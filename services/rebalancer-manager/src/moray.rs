// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Moray client wrapper for the rebalancer manager.
//!
//! This module provides a shard-aware Moray client pool for updating object
//! metadata across multiple Moray shards.

use std::collections::HashMap;
use std::io::Error as IoError;
use std::sync::Arc;

use moray::client::MorayClient;
use moray::objects::{Etag, MethodOptions, MorayObject};
use serde_json::Value;
use slog::{Drain, Logger, o};
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

/// Error type for Moray operations
#[derive(Debug, thiserror::Error)]
pub enum MorayError {
    #[error("Moray client error: {0}")]
    Client(String),

    #[error("Object not found: bucket={bucket} key={key}")]
    NotFound { bucket: String, key: String },

    #[error("Etag conflict: expected={expected}, actual={actual}")]
    EtagConflict { expected: String, actual: String },

    #[error("No Moray client available for shard {0}")]
    NoClientForShard(u32),

    #[error("Failed to parse object metadata: {0}")]
    ParseError(String),
}

impl From<IoError> for MorayError {
    fn from(err: IoError) -> Self {
        MorayError::Client(err.to_string())
    }
}

/// Configuration for the Moray client pool
#[derive(Clone)]
pub struct MorayPoolConfig {
    /// ZooKeeper connect string (e.g., "10.0.0.1:2181,10.0.0.2:2181")
    pub zk_connect_string: String,
    /// Manta domain (e.g., "my-region.example.com")
    pub domain: String,
    /// Minimum shard number
    pub min_shard: u32,
    /// Maximum shard number
    pub max_shard: u32,
}

/// A pool of Moray clients, one per shard
pub struct MorayPool {
    clients: RwLock<HashMap<u32, Arc<MorayClient>>>,
    config: MorayPoolConfig,
    log: Logger,
}

impl MorayPool {
    /// Create a new Moray pool with the given configuration.
    ///
    /// This does not connect to any shards yet - connections are established
    /// lazily when first needed.
    pub fn new(config: MorayPoolConfig) -> Self {
        // Create a slog logger that routes to tracing
        let drain = slog_stdlog::StdLog.fuse();
        let log = Logger::root(drain, o!("component" => "moray-pool"));

        Self {
            clients: RwLock::new(HashMap::new()),
            config,
            log,
        }
    }

    /// Get or create a Moray client for the specified shard.
    pub async fn get_client(&self, shard: u32) -> Result<Arc<MorayClient>, MorayError> {
        // Check if we already have a client for this shard
        {
            let clients = self.clients.read().await;
            if let Some(client) = clients.get(&shard) {
                return Ok(Arc::clone(client));
            }
        }

        // Validate shard number
        if shard < self.config.min_shard || shard > self.config.max_shard {
            return Err(MorayError::NoClientForShard(shard));
        }

        // Create a new client
        let shard_path = format!("/manatee/{}.moray.{}", shard, self.config.domain);

        info!(
            shard = shard,
            shard_path = %shard_path,
            "Creating Moray client for shard"
        );

        let client = MorayClient::with_manatee(
            &self.config.zk_connect_string,
            &shard_path,
            self.log.clone(),
            None,
        )
        .map_err(|e| MorayError::Client(format!("Failed to create client: {}", e)))?;

        let client = Arc::new(client);

        // Store the client
        {
            let mut clients = self.clients.write().await;
            clients.insert(shard, Arc::clone(&client));
        }

        Ok(client)
    }

    /// Get an object from Moray by bucket and key.
    pub async fn get_object(
        &self,
        shard: u32,
        bucket: &str,
        key: &str,
    ) -> Result<MorayObject, MorayError> {
        let client = self.get_client(shard).await?;
        let opts = MethodOptions::default();

        let mut result: Option<MorayObject> = None;

        client
            .get_object(bucket, key, &opts, |obj| {
                result = Some(obj.clone());
                Ok(())
            })
            .await?;

        result.ok_or_else(|| MorayError::NotFound {
            bucket: bucket.to_string(),
            key: key.to_string(),
        })
    }

    /// Put an object to Moray with optional etag for optimistic concurrency.
    pub async fn put_object(
        &self,
        shard: u32,
        bucket: &str,
        key: &str,
        value: Value,
        etag: Option<&str>,
    ) -> Result<String, MorayError> {
        let client = self.get_client(shard).await?;
        let mut opts = MethodOptions::default();

        if let Some(etag_value) = etag {
            opts.etag = Etag::Specified(etag_value.to_string());
        }

        let mut new_etag: Option<String> = None;

        client
            .put_object(bucket, key, value, &opts, |etag| {
                new_etag = Some(etag.to_string());
                Ok(())
            })
            .await?;

        new_etag.ok_or_else(|| MorayError::Client("No etag returned from put_object".to_string()))
    }
}

/// Update the sharks array in an object's metadata.
///
/// This reads the current object, replaces the from_shark with dest_shark
/// in the sharks array, and writes it back with etag-based concurrency control.
pub async fn update_object_sharks(
    pool: &MorayPool,
    shard: u32,
    key: &str,
    from_shark: &str,
    dest_shark: &str,
    dest_datacenter: &str,
    expected_etag: &str,
) -> Result<(), MorayError> {
    const BUCKET: &str = "manta";

    debug!(
        shard = shard,
        key = %key,
        from_shark = %from_shark,
        dest_shark = %dest_shark,
        "Updating object sharks"
    );

    // Get the current object
    let obj = pool.get_object(shard, BUCKET, key).await?;

    // Verify etag matches
    if obj._etag != expected_etag {
        warn!(
            key = %key,
            expected_etag = %expected_etag,
            actual_etag = %obj._etag,
            "Etag mismatch when updating object"
        );
        return Err(MorayError::EtagConflict {
            expected: expected_etag.to_string(),
            actual: obj._etag.clone(),
        });
    }

    // Parse the value and update sharks
    let mut value = obj.value.clone();

    let sharks = value
        .get_mut("sharks")
        .and_then(|v| v.as_array_mut())
        .ok_or_else(|| MorayError::ParseError("Missing or invalid sharks array".to_string()))?;

    // Find and replace the from_shark
    let mut found = false;
    for shark in sharks.iter_mut() {
        if let Some(msid) = shark.get("manta_storage_id").and_then(|v| v.as_str())
            && msid == from_shark
        {
            // Replace the shark entry
            *shark = serde_json::json!({
                "manta_storage_id": dest_shark,
                "datacenter": dest_datacenter
            });
            found = true;
            break;
        }
    }

    if !found {
        error!(
            key = %key,
            from_shark = %from_shark,
            "Source shark not found in object metadata"
        );
        return Err(MorayError::ParseError(format!(
            "Source shark {} not found in object metadata",
            from_shark
        )));
    }

    // Write the updated object back
    let new_etag = pool
        .put_object(shard, BUCKET, key, value, Some(&obj._etag))
        .await?;

    info!(
        key = %key,
        from_shark = %from_shark,
        dest_shark = %dest_shark,
        new_etag = %new_etag,
        "Successfully updated object sharks"
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    #[test]
    fn test_sharks_array_parsing() {
        let value = json!({
            "objectId": "test-id",
            "key": "/test/path",
            "contentLength": 1024,
            "sharks": [
                {"manta_storage_id": "1.stor.domain", "datacenter": "dc1"},
                {"manta_storage_id": "2.stor.domain", "datacenter": "dc2"}
            ]
        });

        let sharks = value.get("sharks").unwrap().as_array().unwrap();
        assert_eq!(sharks.len(), 2);

        let shark0 = &sharks[0];
        assert_eq!(
            shark0.get("manta_storage_id").unwrap().as_str().unwrap(),
            "1.stor.domain"
        );
    }

    #[test]
    fn test_sharks_array_mutation() {
        let mut value = json!({
            "objectId": "test-id",
            "sharks": [
                {"manta_storage_id": "1.stor.domain", "datacenter": "dc1"},
                {"manta_storage_id": "2.stor.domain", "datacenter": "dc2"}
            ]
        });

        let sharks = value.get_mut("sharks").unwrap().as_array_mut().unwrap();

        for shark in sharks.iter_mut() {
            if let Some(msid) = shark.get("manta_storage_id").and_then(|v| v.as_str())
                && msid == "1.stor.domain"
            {
                *shark = json!({
                    "manta_storage_id": "3.stor.domain",
                    "datacenter": "dc3"
                });
                break;
            }
        }

        let sharks = value.get("sharks").unwrap().as_array().unwrap();
        assert_eq!(
            sharks[0].get("manta_storage_id").unwrap().as_str().unwrap(),
            "3.stor.domain"
        );
        assert_eq!(
            sharks[0].get("datacenter").unwrap().as_str().unwrap(),
            "dc3"
        );
    }
}
