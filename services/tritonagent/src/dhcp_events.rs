// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Per-CN Proteus event-ring ticker.
//!
//! On illumos, drains the Proteus host event ring on a fixed interval
//! and dispatches each event by kind:
//!
//! * `DhcpRequest` -> forwarded to tritond's
//!   `/v2/agent/dhcp-lease-activity` endpoint, refreshing each lease
//!   record's `last_renewed_at` so the reconciler's idle-GC heuristic
//!   doesn't mistake a long-lived VM for an orphaned lease.
//! * `PeerResolveNeeded` -> calls tritond's
//!   `/v2/agent/peer?vni=&ip=` endpoint, then issues an
//!   `AddPeerEntry` ioctl back into the kmod's per-port v2p cache.
//!   The kmod-side cache's single-flight gate already collapses
//!   duplicate misses for the same `(port, vni, peer_ip)`; the
//!   agent runs each resolution sequentially so we don't issue
//!   parallel tritond queries for the same peer across CN boots.
//!   See `PROTEUS_PLAN.md` §11.7.1.
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

/// Spawn the Proteus event-ring ticker. Returns a
/// [`DhcpEventsHandle`] callers `shutdown().await` to drain the
/// in-flight poll before exit. `peer_resolver_enabled` is the
/// per-CN `--peer-resolver` rollback toggle: when `false`, miss
/// events are silently dropped (forwarding falls back to the
/// pre-shipped peer_table on each port blueprint).
pub fn spawn(
    client: Arc<Client>,
    proteus_dev: PathBuf,
    interval: Duration,
    peer_resolver_enabled: bool,
) -> DhcpEventsHandle {
    let (tx, rx) = oneshot::channel::<()>();
    let join = tokio::spawn(run_loop(
        client,
        proteus_dev,
        interval,
        rx,
        peer_resolver_enabled,
    ));
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
pub(crate) fn dhcp_activity_from_events(events: &[Event]) -> Vec<DhcpLeaseActivity> {
    events
        .iter()
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

/// One peer-resolve request the agent should service. Pulled from
/// a drained batch; the resolver issues a tritond query + kmod
/// AddPeerEntry ioctl per item. Carries everything the resolver
/// needs so the event-loop body stays a simple `for r in resolves`.
#[cfg_attr(not(any(target_os = "illumos", test)), allow(dead_code))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PeerResolveJob {
    pub port_id: uuid::Uuid,
    pub vni: u32,
    pub family: proteus_api::PeerAddrFamily,
    pub peer_ip: [u8; 16],
}

#[cfg_attr(not(any(target_os = "illumos", test)), allow(dead_code))]
pub(crate) fn peer_resolve_jobs_from_events(events: &[Event]) -> Vec<PeerResolveJob> {
    events
        .iter()
        .filter_map(|ev| match ev.kind {
            EventKind::PeerResolveNeeded {
                vni,
                family,
                peer_ip,
            } => Some(PeerResolveJob {
                port_id: ev.port_id.0,
                vni,
                family,
                peer_ip,
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

/// Agent-side negative cache. Per `(port_id, vni, peer_ip)`,
/// remembers the next time the resolver may re-attempt after a
/// tritond failure. Prevents a tight stampede when a guest is
/// retrying ARP at sub-second cadence against a peer the control
/// plane can't (yet) resolve. Backoff doubles up to a cap; an
/// entry self-expires once the backoff window passes, letting the
/// next miss re-try cleanly.
///
/// Lives entirely in agent memory; survives only as long as the
/// resolver task. On agent restart the kmod's inflight markers
/// may still be set, but the next agent's first resolve attempt
/// for each entry will clear them via the standard path.
#[cfg(any(target_os = "illumos", test))]
#[derive(Default)]
struct NegativeCache {
    /// (port_id, vni, peer_ip) -> (next_retry_at_nsec, last_backoff_ms).
    entries: std::collections::HashMap<NegativeKey, NegativeSlot>,
}

#[cfg(any(target_os = "illumos", test))]
#[derive(Hash, PartialEq, Eq, Clone)]
struct NegativeKey {
    port_id: uuid::Uuid,
    vni: u32,
    peer_ip: [u8; 16],
}

#[cfg(any(target_os = "illumos", test))]
#[derive(Clone, Copy)]
struct NegativeSlot {
    next_retry_at_nsec: u64,
    /// Last backoff in milliseconds; doubles on subsequent
    /// failures (capped).
    backoff_ms: u64,
}

#[cfg(any(target_os = "illumos", test))]
const NEG_INITIAL_BACKOFF_MS: u64 = 1_000;
#[cfg(any(target_os = "illumos", test))]
const NEG_MAX_BACKOFF_MS: u64 = 60_000;

#[cfg(any(target_os = "illumos", test))]
impl NegativeCache {
    fn should_skip(&mut self, key: &NegativeKey, now_nsec: u64) -> bool {
        match self.entries.get(key) {
            Some(slot) if now_nsec < slot.next_retry_at_nsec => true,
            Some(_) => {
                // Window passed; drop the entry so a hit on the
                // retry can clear it cleanly via `note_success`.
                self.entries.remove(key);
                false
            }
            None => false,
        }
    }

    fn note_failure(&mut self, key: NegativeKey, now_nsec: u64) {
        let prev = self.entries.get(&key).copied();
        let backoff_ms = match prev {
            Some(slot) => (slot.backoff_ms * 2).min(NEG_MAX_BACKOFF_MS),
            None => NEG_INITIAL_BACKOFF_MS,
        };
        let next_retry_at_nsec = now_nsec.saturating_add(backoff_ms * 1_000_000);
        self.entries.insert(
            key,
            NegativeSlot {
                next_retry_at_nsec,
                backoff_ms,
            },
        );
    }

    fn note_success(&mut self, key: &NegativeKey) {
        self.entries.remove(key);
    }
}

#[cfg(target_os = "illumos")]
async fn run_loop(
    client: Arc<Client>,
    proteus_dev: PathBuf,
    interval: Duration,
    mut shutdown: oneshot::Receiver<()>,
    peer_resolver_enabled: bool,
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
    let mut neg_cache = NegativeCache::default();

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
                let items = dhcp_activity_from_events(&resp.events);
                let peer_jobs = if peer_resolver_enabled {
                    peer_resolve_jobs_from_events(&resp.events)
                } else {
                    // Resolver rollback path: drop miss events on
                    // the floor. The kmod's inflight markers stay
                    // set until TTL expires or the cache is dumped;
                    // intra-VPC forwarding falls back to the pre-
                    // shipped peer_table on each per-port blueprint.
                    Vec::new()
                };
                if !items.is_empty() {
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
                for job in peer_jobs {
                    let key = NegativeKey {
                        port_id: job.port_id,
                        vni: job.vni,
                        peer_ip: job.peer_ip,
                    };
                    let now_nsec = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_nanos() as u64)
                        .unwrap_or(0);
                    if neg_cache.should_skip(&key, now_nsec) {
                        tracing::debug!(
                            peer_ip = ?job.peer_ip,
                            vni = job.vni,
                            "peer-resolve: in negative-cache window; skipping",
                        );
                        // Clear the kmod inflight marker so a
                        // subsequent miss after the backoff window
                        // re-fires; the kmod won't emit another
                        // event until we do.
                        clear_inflight_after_failure(&proteus, &job);
                        continue;
                    }
                    let success = resolve_one_peer(client.as_ref(), &proteus, &job).await;
                    if success {
                        neg_cache.note_success(&key);
                    } else {
                        neg_cache.note_failure(key, now_nsec);
                    }
                }
            }
            _ = &mut shutdown => {
                debug!("DHCP-event reader shutdown");
                return;
            }
        }
    }
}

/// Service one [`PeerResolveJob`]: ask tritond, hand the answer to
/// the kmod via `AddPeerEntry`. Returns `true` on a successful
/// resolution + AddPeerEntry; `false` on any failure (so the
/// caller can populate the agent-side negative cache). Failures
/// also fire `InvalidatePeerEntry` to clear the kmod's single-
/// flight marker.
#[cfg(target_os = "illumos")]
async fn resolve_one_peer(
    client: &Client,
    proteus: &proteus_ioctl::Client<proteus_ioctl::KernelTransport>,
    job: &PeerResolveJob,
) -> bool {
    use tracing::{debug, info};
    use tritond_client::ClientInfo;

    let ip_str = format_peer_ip(job.family, &job.peer_ip);
    // The auto-generated client doesn't expose this endpoint yet
    // (regen-spec runs separately and commits the new typed call);
    // until then we issue the GET directly via the underlying
    // reqwest::Client. Once the typed `agent_peer_resolve()`
    // method is generated, swap this block for the typed call.
    let baseurl = client.baseurl().to_string();
    let url = format!(
        "{baseurl}/v2/agent/peer?vni={vni}&ip={ip}",
        vni = job.vni,
        ip = urlencoding::encode(&ip_str),
    );
    let resp = match client.client().get(&url).send().await {
        Ok(r) => r,
        Err(e) => {
            warn!(error = %e, peer_ip = %ip_str, vni = job.vni,
                "peer-resolve HTTP failed; negative cache will back off");
            clear_inflight_after_failure(proteus, job);
            return false;
        }
    };
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        // No realized NIC owns this IP. Clear the kmod's
        // single-flight marker so a subsequent guest retry (which
        // might land *after* a peer comes online) re-fires the
        // miss event cleanly. Phase B's negative cache (item 6)
        // will rate-limit the retries; for now we rely on the
        // guest's own ARP cadence.
        debug!(peer_ip = %ip_str, vni = job.vni,
            "peer-resolve: tritond 404 (no realized NIC); clearing inflight");
        clear_inflight_after_failure(proteus, job);
        return false;
    }
    if !resp.status().is_success() {
        warn!(
            status = resp.status().as_u16(),
            peer_ip = %ip_str,
            vni = job.vni,
            "peer-resolve: tritond non-success status",
        );
        clear_inflight_after_failure(proteus, job);
        return false;
    }
    let body: tritond_api::AgentPeerResolveResponse = match resp.json().await {
        Ok(b) => b,
        Err(e) => {
            warn!(error = %e, "peer-resolve: malformed tritond response");
            clear_inflight_after_failure(proteus, job);
            return false;
        }
    };

    let Some(entry) = parse_resolve_response(job.family, &job.peer_ip, &body) else {
        warn!(
            peer_ip = %ip_str,
            vni = job.vni,
            "peer-resolve: malformed mac/underlay in tritond response",
        );
        clear_inflight_after_failure(proteus, job);
        return false;
    };

    let req = proteus_api::peer::AddPeerEntryRequest {
        port_id: proteus_api::ids::PortId(job.port_id),
        vni: job.vni,
        entry,
        ttl_seconds: body.ttl_seconds,
    };
    match proteus.add_peer_entry(&req) {
        Ok(_) => {
            info!(peer_ip = %ip_str, vni = job.vni, "peer-resolve: cached");
            true
        }
        Err(e) => {
            warn!(error = %e, peer_ip = %ip_str, vni = job.vni,
                "peer-resolve: AddPeerEntry ioctl failed");
            clear_inflight_after_failure(proteus, job);
            false
        }
    }
}

/// On any resolver failure (404, 5xx, malformed body, ioctl error),
/// fire `InvalidatePeerEntry` to clear the kmod's single-flight
/// marker. The invalidate path treats "slot absent" as a no-op, so
/// this is cheap regardless of cache state; what we care about is
/// the side effect of `inflight_v{4,6}.remove`, which lets the next
/// miss event fire instead of being silently swallowed.
#[cfg(target_os = "illumos")]
fn clear_inflight_after_failure(
    proteus: &proteus_ioctl::Client<proteus_ioctl::KernelTransport>,
    job: &PeerResolveJob,
) {
    let req = proteus_api::peer::InvalidatePeerEntryRequest {
        port_id: proteus_api::ids::PortId(job.port_id),
        vni: job.vni,
        family: job.family,
        addr: job.peer_ip,
    };
    if let Err(e) = proteus.invalidate_peer_entry(&req) {
        tracing::debug!(error = %e, "clear_inflight: InvalidatePeerEntry failed");
    }
}

#[cfg(target_os = "illumos")]
fn format_peer_ip(family: proteus_api::PeerAddrFamily, ip: &[u8; 16]) -> String {
    match family {
        proteus_api::PeerAddrFamily::V4 => {
            format!("{}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3])
        }
        proteus_api::PeerAddrFamily::V6 => {
            let octets: [u8; 16] = *ip;
            std::net::Ipv6Addr::from(octets).to_string()
        }
    }
}

#[cfg(target_os = "illumos")]
fn parse_resolve_response(
    family: proteus_api::PeerAddrFamily,
    peer_ip: &[u8; 16],
    body: &tritond_api::AgentPeerResolveResponse,
) -> Option<proteus_api::peer::PeerEntry> {
    let mut mac = [0u8; 6];
    let mut count = 0;
    for (i, part) in body.guest_mac.split(':').enumerate() {
        if i >= 6 || part.len() != 2 {
            return None;
        }
        mac[i] = u8::from_str_radix(part, 16).ok()?;
        count += 1;
    }
    if count != 6 {
        return None;
    }
    let underlay_addr: std::net::Ipv6Addr = body.underlay.parse().ok()?;
    Some(proteus_api::peer::PeerEntry {
        family,
        addr: *peer_ip,
        guest_mac: mac,
        underlay: underlay_addr.octets(),
    })
}

#[cfg(not(target_os = "illumos"))]
async fn run_loop(
    _client: Arc<Client>,
    proteus_dev: PathBuf,
    _interval: Duration,
    _shutdown: oneshot::Receiver<()>,
    _peer_resolver_enabled: bool,
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
        let items = dhcp_activity_from_events(&events);
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
        assert!(dhcp_activity_from_events(&[]).is_empty());
        assert!(dhcp_activity_from_events(&[other_event(Uuid::nil())]).is_empty());
    }

    fn peer_event(port: Uuid, peer_ip: [u8; 4], vni: u32) -> Event {
        let mut padded = [0u8; 16];
        padded[..4].copy_from_slice(&peer_ip);
        Event::header(
            PortId(port),
            Generation::new(1),
            1,
            0,
            EventKind::PeerResolveNeeded {
                vni,
                family: proteus_api::PeerAddrFamily::V4,
                peer_ip: padded,
            },
        )
    }

    #[test]
    fn peer_resolve_jobs_filtered_from_mixed_batch() {
        let port = Uuid::new_v4();
        let events = vec![
            dhcp_event(port, DhcpMessageType::Discover, [0x02; 6], 0xaa),
            peer_event(port, [10, 42, 1, 3], 0x1234),
            other_event(Uuid::nil()),
            peer_event(port, [10, 42, 1, 4], 0x1234),
        ];
        let jobs = peer_resolve_jobs_from_events(&events);
        assert_eq!(jobs.len(), 2);
        assert_eq!(&jobs[0].peer_ip[..4], &[10, 42, 1, 3]);
        assert_eq!(jobs[0].vni, 0x1234);
        assert_eq!(jobs[0].port_id, port);
        assert!(matches!(jobs[0].family, proteus_api::PeerAddrFamily::V4));
        assert_eq!(&jobs[1].peer_ip[..4], &[10, 42, 1, 4]);
    }

    #[test]
    fn peer_resolve_jobs_empty_batch_yields_empty() {
        assert!(peer_resolve_jobs_from_events(&[]).is_empty());
        assert!(peer_resolve_jobs_from_events(&[dhcp_event(
            Uuid::nil(),
            DhcpMessageType::Discover,
            [0; 6],
            0,
        )])
        .is_empty());
    }

    fn key(port: Uuid, vni: u32, peer: [u8; 4]) -> NegativeKey {
        let mut p = [0u8; 16];
        p[..4].copy_from_slice(&peer);
        NegativeKey {
            port_id: port,
            vni,
            peer_ip: p,
        }
    }

    #[test]
    fn negative_cache_first_attempt_is_not_skipped() {
        let mut nc = NegativeCache::default();
        let k = key(Uuid::nil(), 0x1234, [10, 0, 0, 1]);
        assert!(!nc.should_skip(&k, 0));
    }

    #[test]
    fn negative_cache_after_failure_skips_within_window() {
        let mut nc = NegativeCache::default();
        let k = key(Uuid::nil(), 0x1234, [10, 0, 0, 2]);
        nc.note_failure(k.clone(), 0);
        // Within the initial 1s backoff window.
        assert!(nc.should_skip(&k, 500_000_000));
    }

    #[test]
    fn negative_cache_clears_after_window_passes() {
        let mut nc = NegativeCache::default();
        let k = key(Uuid::nil(), 0x1234, [10, 0, 0, 3]);
        nc.note_failure(k.clone(), 0);
        // Past the initial 1s window.
        let later = 2 * 1_000_000_000u64;
        assert!(!nc.should_skip(&k, later));
        // Entry was reaped.
        assert!(nc.entries.is_empty());
    }

    #[test]
    fn negative_cache_doubles_backoff_on_repeated_failure() {
        let mut nc = NegativeCache::default();
        let k = key(Uuid::nil(), 0x1234, [10, 0, 0, 4]);
        nc.note_failure(k.clone(), 0);
        let first_backoff = nc.entries.get(&k).unwrap().backoff_ms;
        // Repeat failure (e.g., resolver retried during slot, but
        // we test by directly calling); backoff doubles.
        nc.note_failure(k.clone(), 0);
        let second_backoff = nc.entries.get(&k).unwrap().backoff_ms;
        assert_eq!(second_backoff, first_backoff * 2);
    }

    #[test]
    fn negative_cache_backoff_clamps_at_cap() {
        let mut nc = NegativeCache::default();
        let k = key(Uuid::nil(), 0x1234, [10, 0, 0, 5]);
        // Force backoff to saturate.
        for _ in 0..20 {
            nc.note_failure(k.clone(), 0);
        }
        assert_eq!(nc.entries.get(&k).unwrap().backoff_ms, NEG_MAX_BACKOFF_MS);
    }

    #[test]
    fn negative_cache_success_clears_entry() {
        let mut nc = NegativeCache::default();
        let k = key(Uuid::nil(), 0x1234, [10, 0, 0, 6]);
        nc.note_failure(k.clone(), 0);
        assert!(nc.entries.contains_key(&k));
        nc.note_success(&k);
        assert!(!nc.entries.contains_key(&k));
    }
}
