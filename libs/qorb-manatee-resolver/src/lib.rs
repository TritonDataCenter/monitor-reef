// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! A qorb Resolver implementation for Manatee/ZooKeeper service discovery.
//!
//! This resolver watches a ZooKeeper cluster to determine the PostgreSQL
//! primary from a Manatee-managed replication set. It implements the qorb
//! [`Resolver`] trait, publishing backend updates via a watch channel.
//!
//! # Architecture
//!
//! The resolver runs a background task that:
//! 1. Connects to ZooKeeper (with automatic reconnection on failure)
//! 2. Watches the Manatee cluster state node for changes
//! 3. Parses the JSON state to extract the primary's IP and port
//! 4. Publishes backend updates via a tokio watch channel
//!
//! # Example
//!
//! ```ignore
//! use qorb_manatee_resolver::ManateeResolver;
//! use qorb::resolver::Resolver;
//!
//! let mut resolver = ManateeResolver::new(
//!     "127.0.0.1:2181",
//!     "/manatee/1.moray.my-region.example.com",
//! )?;
//!
//! let rx = resolver.monitor();
//! // Use rx with a qorb Pool
//! ```

use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use qorb::backend::{Backend, Name};
use qorb::resolver::{AllBackends, Resolver};
use serde::Deserialize;
use thiserror::Error;
use tokio::sync::watch;
use tracing::{debug, error, info, warn};
use url::Url;
use zookeeper_client as zk;

/// Errors that can occur in the Manatee resolver.
#[derive(Error, Debug)]
pub enum ManateeResolverError {
    #[error("Empty ZooKeeper connect string")]
    EmptyConnectString,

    #[error("Invalid ZooKeeper address: {0}")]
    InvalidAddress(String),

    #[error("ZooKeeper error: {0}")]
    ZooKeeper(#[from] zk::Error),

    #[error("Invalid JSON in ZooKeeper data: {0}")]
    InvalidJson(#[from] serde_json::Error),

    #[error("Missing field in ZooKeeper data: {0}")]
    MissingField(&'static str),

    #[error("Invalid field in ZooKeeper data: {field}: {message}")]
    InvalidField {
        field: &'static str,
        message: String,
    },

    #[error("Invalid URL: {0}")]
    InvalidUrl(#[from] url::ParseError),
}

/// A parsed ZooKeeper connect string (comma-separated list of host:port pairs).
#[derive(Debug, Clone)]
pub struct ZkConnectString(Vec<SocketAddr>);

impl ZkConnectString {
    /// Parse a connect string like "host1:port1,host2:port2".
    pub fn parse(s: &str) -> Result<Self, ManateeResolverError> {
        if s.is_empty() {
            return Err(ManateeResolverError::EmptyConnectString);
        }

        let addrs: Result<Vec<SocketAddr>, _> = s
            .split(',')
            .map(|addr| {
                SocketAddr::from_str(addr.trim())
                    .map_err(|_| ManateeResolverError::InvalidAddress(addr.to_string()))
            })
            .collect();

        Ok(ZkConnectString(addrs?))
    }

    /// Returns the connect string in a format suitable for zookeeper-client.
    fn to_connect_string(&self) -> String {
        self.0
            .iter()
            .map(|a| a.to_string())
            .collect::<Vec<_>>()
            .join(",")
    }
}

impl FromStr for ZkConnectString {
    type Err = ManateeResolverError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

/// Partial structure of the Manatee cluster state JSON.
///
/// We only parse the fields we need (primary.ip and primary.pgUrl).
#[derive(Debug, Deserialize)]
struct ManateeClusterState {
    primary: Option<ManateePeer>,
}

#[derive(Debug, Deserialize)]
struct ManateePeer {
    ip: Option<String>,
    #[serde(rename = "pgUrl")]
    pg_url: Option<String>,
}

/// Tracks parse staleness for ZooKeeper data.
///
/// This is used to detect when ZooKeeper data has been unparseable for an
/// extended period, which could indicate a configuration or data format issue.
struct ParseStalenessTracker {
    /// Monotonic timestamp (in seconds since tracker creation) of last successful parse.
    /// Uses u64 for atomic operations. 0 means never successfully parsed.
    last_successful_parse_secs: AtomicU64,
    /// When the tracker was created (for calculating relative timestamps)
    start_instant: Instant,
    /// Threshold after which to warn about staleness (in seconds)
    staleness_threshold_secs: u64,
}

impl ParseStalenessTracker {
    fn new(staleness_threshold_secs: u64) -> Self {
        Self {
            last_successful_parse_secs: AtomicU64::new(0),
            start_instant: Instant::now(),
            staleness_threshold_secs,
        }
    }

    fn record_successful_parse(&self) {
        let now_secs = self.start_instant.elapsed().as_secs();
        self.last_successful_parse_secs
            .store(now_secs, Ordering::Relaxed);
    }

    /// Check if data has been unparseable for longer than the threshold.
    /// Returns Some(duration_secs) if stale, None if not.
    fn check_staleness(&self) -> Option<u64> {
        let last_success = self.last_successful_parse_secs.load(Ordering::Relaxed);
        let now_secs = self.start_instant.elapsed().as_secs();

        // If we've never had a successful parse, check against start time
        let stale_duration = if last_success == 0 {
            now_secs
        } else {
            now_secs.saturating_sub(last_success)
        };

        if stale_duration >= self.staleness_threshold_secs {
            Some(stale_duration)
        } else {
            None
        }
    }
}

/// A qorb Resolver that watches a Manatee/ZooKeeper cluster for the primary.
///
/// This resolver watches the ZooKeeper node at `<shard_path>/state` and
/// extracts the primary PostgreSQL server's address from the JSON data.
pub struct ManateeResolver {
    /// Sender for publishing backend updates
    tx: watch::Sender<AllBackends>,

    /// Handle to the background watcher task
    task_handle: Option<tokio::task::JoinHandle<()>>,

    /// Signal to stop the background task
    shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
}

impl ManateeResolver {
    /// Creates a new ManateeResolver and starts the background watcher task.
    ///
    /// # Arguments
    ///
    /// * `connect_string` - Comma-separated list of ZooKeeper addresses
    ///   (e.g., "10.0.0.1:2181,10.0.0.2:2181")
    /// * `shard_path` - The ZooKeeper path for the Manatee shard
    ///   (e.g., "/manatee/1.moray.my-region.example.com")
    ///
    /// # Errors
    ///
    /// Returns an error if the connect string is invalid.
    pub fn new(connect_string: &str, shard_path: &str) -> Result<Self, ManateeResolverError> {
        let zk_addrs = ZkConnectString::parse(connect_string)?;
        let state_path = format!("{}/state", shard_path);

        // Create the watch channel with an empty initial backend set
        let (tx, _rx) = watch::channel(Arc::new(BTreeMap::new()));
        let tx_clone = tx.clone();

        // Create shutdown channel
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();

        // Spawn the background watcher task
        let task_handle = tokio::spawn(async move {
            watcher_loop(zk_addrs, state_path, tx_clone, shutdown_rx).await;
        });

        Ok(ManateeResolver {
            tx,
            task_handle: Some(task_handle),
            shutdown_tx: Some(shutdown_tx),
        })
    }
}

#[async_trait]
impl Resolver for ManateeResolver {
    fn monitor(&mut self) -> watch::Receiver<AllBackends> {
        self.tx.subscribe()
    }

    async fn terminate(&mut self) {
        // Signal the background task to stop
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }

        // Wait for the task to finish
        if let Some(handle) = self.task_handle.take() {
            let _ = handle.await;
        }
    }
}

impl Drop for ManateeResolver {
    fn drop(&mut self) {
        // Signal shutdown (non-blocking)
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        // Note: We can't await the task in Drop, so it will be aborted
        // when the JoinHandle is dropped. For clean shutdown, call
        // terminate() before dropping.
    }
}

/// Default staleness threshold in seconds (5 minutes).
/// If ZooKeeper data has been unparseable for this long, log a warning.
const DEFAULT_STALENESS_THRESHOLD_SECS: u64 = 300;

/// The main watcher loop that runs in the background.
///
/// This function handles:
/// - Connecting to ZooKeeper with exponential backoff on failure
/// - Watching the state node for changes
/// - Parsing the JSON data and publishing backend updates
/// - Tracking and warning about parse staleness
async fn watcher_loop(
    zk_addrs: ZkConnectString,
    state_path: String,
    tx: watch::Sender<AllBackends>,
    mut shutdown_rx: tokio::sync::oneshot::Receiver<()>,
) {
    let connect_string = zk_addrs.to_connect_string();
    let mut backoff = ExponentialBackoff::new();
    let staleness_tracker = Arc::new(ParseStalenessTracker::new(DEFAULT_STALENESS_THRESHOLD_SECS));

    loop {
        // Check for shutdown signal
        if shutdown_rx.try_recv().is_ok() {
            info!("ManateeResolver received shutdown signal");
            return;
        }

        // Connect to ZooKeeper
        info!(connect_string = %connect_string, "Connecting to ZooKeeper");
        let client = match zk::Client::connect(&connect_string).await {
            Ok(client) => {
                info!("Connected to ZooKeeper");
                backoff.reset();
                client
            }
            Err(e) => {
                error!(error = %e, "Failed to connect to ZooKeeper");
                let delay = backoff.next_backoff();
                tokio::select! {
                    _ = tokio::time::sleep(delay) => continue,
                    _ = &mut shutdown_rx => {
                        info!("ManateeResolver received shutdown signal during backoff");
                        return;
                    }
                }
            }
        };

        // Watch loop: repeatedly watch and process changes
        if let Err(e) = watch_state_node(
            &client,
            &state_path,
            &tx,
            &mut shutdown_rx,
            &staleness_tracker,
        )
        .await
        {
            error!(error = %e, "Watch loop error, will reconnect");
            let delay = backoff.next_backoff();
            tokio::select! {
                _ = tokio::time::sleep(delay) => {}
                _ = &mut shutdown_rx => {
                    info!("ManateeResolver received shutdown signal during backoff");
                    return;
                }
            }
        }
    }
}

/// Watches the state node and processes changes.
///
/// Returns when the connection is lost or an unrecoverable error occurs.
async fn watch_state_node(
    client: &zk::Client,
    state_path: &str,
    tx: &watch::Sender<AllBackends>,
    shutdown_rx: &mut tokio::sync::oneshot::Receiver<()>,
    staleness_tracker: &ParseStalenessTracker,
) -> Result<(), ManateeResolverError> {
    loop {
        // Check for shutdown
        if shutdown_rx.try_recv().is_ok() {
            return Ok(());
        }

        // Get data and set up watch
        debug!(path = %state_path, "Getting data and setting watch");
        let result = client.get_and_watch_data(state_path).await;

        match result {
            Ok((data, _stat, watcher)) => {
                // Process the data
                // arch-lint: allow(no-error-swallowing) reason="Continue watching; data may become valid; staleness tracked"
                match process_zk_data(&data, tx) {
                    Ok(()) => {
                        staleness_tracker.record_successful_parse();
                    }
                    Err(e) => {
                        warn!(error = %e, "Failed to process ZooKeeper data");
                        // Check if we've been unable to parse for too long
                        if let Some(stale_secs) = staleness_tracker.check_staleness() {
                            warn!(
                                stale_duration_secs = stale_secs,
                                "ZooKeeper data has been unparseable for an extended period. \
                                 This may indicate a configuration or data format issue."
                            );
                        }
                    }
                }

                // Wait for change
                debug!("Waiting for node change");
                tokio::select! {
                    event = watcher.changed() => {
                        debug!(event_type = ?event.event_type, "Node changed");
                        // Loop back to get new data
                    }
                    _ = &mut *shutdown_rx => {
                        info!("ManateeResolver received shutdown signal while watching");
                        return Ok(());
                    }
                }
            }
            Err(zk::Error::NoNode) => {
                // Node doesn't exist yet - wait and retry
                info!(path = %state_path, "State node does not exist, waiting");
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(5)) => {}
                    _ = &mut *shutdown_rx => {
                        return Ok(());
                    }
                }
            }
            Err(e) => {
                // Connection error or other issue - return to reconnect
                return Err(ManateeResolverError::ZooKeeper(e));
            }
        }
    }
}

/// Parses ZooKeeper data and publishes the backend if changed.
///
/// The expected JSON format is:
/// ```json
/// {
///     "primary": {
///         "ip": "10.0.0.1",
///         "pgUrl": "tcp://postgres@10.0.0.1:5432/postgres"
///     }
/// }
/// ```
fn process_zk_data(
    data: &[u8],
    tx: &watch::Sender<AllBackends>,
) -> Result<(), ManateeResolverError> {
    // Parse the JSON
    let state: ManateeClusterState = serde_json::from_slice(data)?;

    // Extract primary info
    let primary = state
        .primary
        .ok_or(ManateeResolverError::MissingField("primary"))?;

    // Get IP address
    let ip_str = primary
        .ip
        .ok_or(ManateeResolverError::MissingField("primary.ip"))?;
    let ip: IpAddr = ip_str
        .parse()
        .map_err(|_| ManateeResolverError::InvalidField {
            field: "primary.ip",
            message: format!("invalid IP address: {}", ip_str),
        })?;

    // Get port from pgUrl
    let pg_url_str = primary
        .pg_url
        .ok_or(ManateeResolverError::MissingField("primary.pgUrl"))?;
    let pg_url = Url::parse(&pg_url_str)?;
    let port = pg_url.port().ok_or(ManateeResolverError::InvalidField {
        field: "primary.pgUrl",
        message: "missing port in URL".to_string(),
    })?;

    // Create the backend
    let addr = SocketAddr::new(ip, port);
    let name = Name::new(addr);
    let backend = Backend::new(addr);

    info!(address = %addr, "Found Manatee primary");

    // Publish the update
    let mut backends = BTreeMap::new();
    backends.insert(name, backend);
    let _ = tx.send(Arc::new(backends));

    Ok(())
}

/// Simple exponential backoff helper.
struct ExponentialBackoff {
    current: Duration,
    max: Duration,
    multiplier: f64,
}

impl ExponentialBackoff {
    fn new() -> Self {
        Self {
            current: Duration::from_millis(100),
            max: Duration::from_secs(60),
            multiplier: 2.0,
        }
    }

    fn next_backoff(&mut self) -> Duration {
        let result = self.current;
        let next = Duration::from_secs_f64(self.current.as_secs_f64() * self.multiplier);
        self.current = next.min(self.max);
        result
    }

    fn reset(&mut self) {
        self.current = Duration::from_millis(100);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_zk_connect_string_parse() {
        let cs = ZkConnectString::parse("127.0.0.1:2181").unwrap();
        assert_eq!(cs.0.len(), 1);
        assert_eq!(cs.0[0].to_string(), "127.0.0.1:2181");

        let cs = ZkConnectString::parse("10.0.0.1:2181,10.0.0.2:2181").unwrap();
        assert_eq!(cs.0.len(), 2);
    }

    #[test]
    fn test_zk_connect_string_empty() {
        let result = ZkConnectString::parse("");
        assert!(matches!(
            result,
            Err(ManateeResolverError::EmptyConnectString)
        ));
    }

    #[test]
    fn test_zk_connect_string_invalid() {
        let result = ZkConnectString::parse("not-a-valid-address");
        assert!(matches!(
            result,
            Err(ManateeResolverError::InvalidAddress(_))
        ));
    }

    #[test]
    fn test_process_zk_data_valid() {
        let json = r#"{
            "generation": 1,
            "primary": {
                "id": "10.77.77.28:5432:12345",
                "ip": "10.77.77.28",
                "pgUrl": "tcp://postgres@10.77.77.28:5432/postgres",
                "zoneId": "f47c4766-1857-4bdc-97f0-c1fd009c955b",
                "backupUrl": "http://10.77.77.28:12345"
            },
            "sync": {
                "id": "10.77.77.21:5432:12345",
                "ip": "10.77.77.21",
                "pgUrl": "tcp://postgres@10.77.77.21:5432/postgres",
                "zoneId": "f8727df9-c639-4152-a861-c77a878ca387",
                "backupUrl": "http://10.77.77.21:12345"
            },
            "async": [],
            "deposed": [],
            "initWal": "0/16522D8"
        }"#;

        let (tx, rx) = watch::channel(Arc::new(BTreeMap::new()));
        process_zk_data(json.as_bytes(), &tx).unwrap();

        let backends = rx.borrow();
        assert_eq!(backends.len(), 1);

        let expected_addr: SocketAddr = "10.77.77.28:5432".parse().unwrap();
        let name = Name::new(expected_addr);
        let backend = backends.get(&name).unwrap();
        assert_eq!(backend.address, expected_addr);
    }

    #[test]
    fn test_process_zk_data_no_primary() {
        let json = r#"{"generation": 1}"#;
        let (tx, _rx) = watch::channel(Arc::new(BTreeMap::new()));
        let result = process_zk_data(json.as_bytes(), &tx);
        assert!(matches!(
            result,
            Err(ManateeResolverError::MissingField("primary"))
        ));
    }

    #[test]
    fn test_process_zk_data_no_ip() {
        let json = r#"{
            "primary": {
                "pgUrl": "tcp://postgres@10.77.77.28:5432/postgres"
            }
        }"#;
        let (tx, _rx) = watch::channel(Arc::new(BTreeMap::new()));
        let result = process_zk_data(json.as_bytes(), &tx);
        assert!(matches!(
            result,
            Err(ManateeResolverError::MissingField("primary.ip"))
        ));
    }

    #[test]
    fn test_process_zk_data_invalid_ip() {
        let json = r#"{
            "primary": {
                "ip": "not-an-ip",
                "pgUrl": "tcp://postgres@10.77.77.28:5432/postgres"
            }
        }"#;
        let (tx, _rx) = watch::channel(Arc::new(BTreeMap::new()));
        let result = process_zk_data(json.as_bytes(), &tx);
        assert!(matches!(
            result,
            Err(ManateeResolverError::InvalidField {
                field: "primary.ip",
                ..
            })
        ));
    }

    #[test]
    fn test_process_zk_data_no_port() {
        let json = r#"{
            "primary": {
                "ip": "10.77.77.28",
                "pgUrl": "tcp://postgres@10.77.77.28/postgres"
            }
        }"#;
        let (tx, _rx) = watch::channel(Arc::new(BTreeMap::new()));
        let result = process_zk_data(json.as_bytes(), &tx);
        assert!(matches!(
            result,
            Err(ManateeResolverError::InvalidField {
                field: "primary.pgUrl",
                ..
            })
        ));
    }

    #[test]
    fn test_process_zk_data_invalid_json() {
        let json = b"not valid json";
        let (tx, _rx) = watch::channel(Arc::new(BTreeMap::new()));
        let result = process_zk_data(json, &tx);
        assert!(matches!(result, Err(ManateeResolverError::InvalidJson(_))));
    }

    #[test]
    fn test_exponential_backoff() {
        let mut backoff = ExponentialBackoff::new();

        let d1 = backoff.next_backoff();
        assert_eq!(d1, Duration::from_millis(100));

        let d2 = backoff.next_backoff();
        assert_eq!(d2, Duration::from_millis(200));

        let d3 = backoff.next_backoff();
        assert_eq!(d3, Duration::from_millis(400));

        // Test reset
        backoff.reset();
        let d4 = backoff.next_backoff();
        assert_eq!(d4, Duration::from_millis(100));
    }

    #[test]
    fn test_exponential_backoff_max() {
        let mut backoff = ExponentialBackoff::new();
        backoff.current = Duration::from_secs(50);

        let d1 = backoff.next_backoff();
        assert_eq!(d1, Duration::from_secs(50));

        // Should be capped at max (60s)
        let d2 = backoff.next_backoff();
        assert_eq!(d2, Duration::from_secs(60));

        let d3 = backoff.next_backoff();
        assert_eq!(d3, Duration::from_secs(60));
    }
}
