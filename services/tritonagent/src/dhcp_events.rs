// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Per-CN DHCP-event ticker.
//!
//! On illumos, drains the Proteus host event ring on a fixed interval
//! and forwards the DHCP requests it found to tritond's
//! `/v2/agent/dhcp-lease-activity` endpoint, which refreshes each lease
//! record's `last_renewed_at` so the reconciler's idle-GC heuristic
//! doesn't mistake a long-lived VM for an orphaned lease.
//!
//! Best-effort, like the metrics and log tickers: a missing
//! `/dev/proteus`, a `SubscribeEvents` failure, or a transient 5xx from
//! tritond logs a warning and the loop moves on; nothing here is fatal
//! to the agent. On non-illumos there is no kernel transport, so the
//! ticker is a no-op (`tritonagent` builds and runs on dev hosts for
//! the integration suite). The ioctl drain is a microsecond-scale
//! kernel call (it copies a bounded `VecDeque` out under a kmutex), so
//! it runs inline in the async task between `tokio::time` ticks rather
//! than on a blocking pool.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use proteus_api::event::{Event, EventKind};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tracing::warn;
use tritond_client::Client;
use tritond_client::types::DhcpLeaseActivity;

/// Drain cadence. Fast enough that a boot-storm of DHCP requests is
/// forwarded within a few hundred milliseconds; slow enough that the
/// idle case is one cheap empty ioctl every half second.
pub const DEFAULT_DHCP_EVENT_INTERVAL: Duration = Duration::from_millis(500);

/// Spawn the DHCP-event ticker. Returns a [`DhcpEventsHandle`] callers
/// `shutdown().await` to drain the in-flight poll before exit.
pub fn spawn(client: Arc<Client>, proteus_dev: PathBuf, interval: Duration) -> DhcpEventsHandle {
    let (tx, rx) = oneshot::channel::<()>();
    let join = tokio::spawn(run_loop(client, proteus_dev, interval, rx));
    DhcpEventsHandle {
        join: Some(join),
        shutdown: Some(tx),
    }
}

/// JoinHandle + shutdown signal pair, mirroring the metrics / log
/// tickers. Drop-safe: the task ends when the signal arrives or its
/// sender drops.
pub struct DhcpEventsHandle {
    join: Option<JoinHandle<()>>,
    shutdown: Option<oneshot::Sender<()>>,
}

impl DhcpEventsHandle {
    /// Cleanly stop the ticker: signal, then await the in-flight poll.
    pub async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(handle) = self.join.take()
            && let Err(e) = handle.await
        {
            warn!(error = %e, "DHCP-event ticker join failed");
        }
    }
}

/// Map a drained event batch to the tritond report shape. Pure — keeps
/// the event-kind filtering + MAC formatting unit-testable without a
/// kernel transport or an HTTP client.
#[cfg_attr(not(any(target_os = "illumos", test)), allow(dead_code))]
pub(crate) fn dhcp_activity_from_events(events: Vec<Event>) -> Vec<DhcpLeaseActivity> {
    events
        .into_iter()
        .filter_map(|ev| match ev.kind {
            EventKind::DhcpRequest {
                msg_type,
                client_mac,
                xid,
                ..
            } => Some(DhcpLeaseActivity {
                port_id: ev.port_id.0,
                client_mac: format_mac(&client_mac),
                msg_type: msg_type as u8,
                xid,
            }),
            _ => None,
        })
        .collect()
}

#[cfg_attr(not(any(target_os = "illumos", test)), allow(dead_code))]
fn format_mac(b: &[u8; 6]) -> String {
    format!(
        "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        b[0], b[1], b[2], b[3], b[4], b[5],
    )
}

#[cfg(target_os = "illumos")]
async fn run_loop(
    client: Arc<Client>,
    proteus_dev: PathBuf,
    interval: Duration,
    mut shutdown: oneshot::Receiver<()>,
) {
    use proteus_api::event::{ReadEventsRequest, SubscribeEventsRequest};
    use proteus_ioctl::{Client as ProteusClient, KernelTransport};
    use tracing::{debug, info};
    use tritond_client::types::DhcpLeaseActivityReport;

    /// Ring capacity requested at `SubscribeEvents`. 4096 events
    /// absorbs many boot storms before the host drops the oldest
    /// entry — and a drop only costs a `last_renewed_at` refresh.
    const RING_CAPACITY: u32 = 4096;
    /// Per-poll drain batch cap. Matches the host's `MAX_EVENTS_PER_READ`.
    const READ_BATCH: u32 = 256;

    let proteus = match KernelTransport::open_path(&proteus_dev) {
        Ok(t) => ProteusClient::new(t),
        Err(_) => {
            // No Proteus device — renewal bookkeeping is just disabled;
            // DHCP itself still works (the lease is written at instance
            // create), and the reconciler falls back to `created_at`.
            warn!(
                dev = %proteus_dev.display(),
                "DHCP-event reader: cannot open Proteus device; lease renewal bookkeeping disabled",
            );
            return;
        }
    };
    match proteus.subscribe_events(&SubscribeEventsRequest {
        capacity: RING_CAPACITY,
    }) {
        Ok(resp) => info!(
            capacity = resp.installed_capacity,
            "DHCP-event reader subscribed to the Proteus event ring",
        ),
        Err(_) => {
            warn!("DHCP-event reader: SubscribeEvents failed; lease renewal bookkeeping disabled");
            return;
        }
    }

    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                let resp = match proteus.read_events(&ReadEventsRequest { max_events: READ_BATCH }) {
                    Ok(r) => r,
                    Err(e) => { warn!(error = %e, "ReadEvents ioctl failed"); continue; }
                };
                if resp.dropped_since_last_read > 0 {
                    warn!(
                        dropped = resp.dropped_since_last_read,
                        "Proteus event ring dropped events (ring under-sized or reader fell behind)",
                    );
                }
                let items = dhcp_activity_from_events(resp.events);
                if items.is_empty() {
                    continue;
                }
                let forwarded = items.len();
                match client
                    .agent_report_dhcp_lease_activity()
                    .body(DhcpLeaseActivityReport { items })
                    .send()
                    .await
                {
                    Ok(_) => debug!(forwarded, "forwarded DHCP request events to tritond"),
                    Err(e) => warn!(error = %e, "forwarding DHCP request events to tritond failed"),
                }
            }
            _ = &mut shutdown => {
                debug!("DHCP-event reader shutdown");
                return;
            }
        }
    }
}

#[cfg(not(target_os = "illumos"))]
async fn run_loop(
    _client: Arc<Client>,
    proteus_dev: PathBuf,
    _interval: Duration,
    _shutdown: oneshot::Receiver<()>,
) {
    tracing::debug!(
        dev = %proteus_dev.display(),
        "DHCP-event reader: Proteus kernel transport unavailable on this platform; disabled",
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use proteus_api::event::DhcpMessageType;
    use proteus_api::ids::{Generation, PortId};
    use uuid::Uuid;

    fn dhcp_event(port: Uuid, msg: DhcpMessageType, mac: [u8; 6], xid: u32) -> Event {
        Event::header(
            PortId(port),
            Generation::new(1),
            1,
            0,
            EventKind::DhcpRequest {
                msg_type: msg,
                client_mac: mac,
                requested_ip: None,
                xid,
                observed_at_nsec: 0,
            },
        )
    }

    fn other_event(port: Uuid) -> Event {
        Event::header(
            PortId(port),
            Generation::new(1),
            2,
            0,
            EventKind::UnderlayChanged,
        )
    }

    #[test]
    fn maps_only_dhcp_requests() {
        let port = Uuid::from_u128(0xdace);
        let events = vec![
            dhcp_event(
                port,
                DhcpMessageType::Discover,
                [0x02, 0x08, 0x20, 0xab, 0xcd, 0xef],
                0xdead_beef,
            ),
            other_event(port),
            dhcp_event(
                port,
                DhcpMessageType::Request,
                [0x02, 0x08, 0x20, 0x00, 0x00, 0x07],
                0x1234_5678,
            ),
        ];
        let items = dhcp_activity_from_events(events);
        assert_eq!(items.len(), 2, "the UnderlayChanged event is dropped");
        assert_eq!(items[0].port_id, port);
        assert_eq!(items[0].client_mac, "02:08:20:ab:cd:ef");
        assert_eq!(items[0].msg_type, 1); // DISCOVER
        assert_eq!(items[0].xid, 0xdead_beef);
        assert_eq!(items[1].msg_type, 3); // REQUEST
        assert_eq!(items[1].client_mac, "02:08:20:00:00:07");
    }

    #[test]
    fn empty_or_non_dhcp_batch_maps_to_empty() {
        assert!(dhcp_activity_from_events(vec![]).is_empty());
        assert!(dhcp_activity_from_events(vec![other_event(Uuid::nil())]).is_empty());
    }
}
