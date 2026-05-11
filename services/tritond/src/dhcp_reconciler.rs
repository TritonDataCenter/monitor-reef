// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! DHCP lease reconciliation worker (γ.3).
//!
//! A tokio task that periodically walks every active lease from
//! [`tritond_store::Store::list_all_dhcp_leases`] and removes the
//! orphaned ones. A lease is GC-eligible when **all** of the
//! following are true:
//!
//! 1. The instance the lease was bound to no longer exists. The γ.4
//!    pre-assignment hook stamps `instance_id` on every lease at
//!    creation time, so a lookup with [`Store::get_instance`] is the
//!    authoritative liveness check. (Instances are sticky-reusable
//!    across re-creations only via *reservations*, so the lease
//!    record itself becomes load-bearing only while its instance is
//!    alive.)
//!
//! 2. No reservation pins the MAC. Operator-pinned `(vpc_id, mac) →
//!    ipv4` reservations are explicitly meant to outlive instance
//!    deletion, so removing a lease whose MAC is reserved would
//!    contradict the sticky-by-MAC contract — a re-created instance
//!    with the same MAC would still get the right IP via the
//!    reservation, but observability would lose continuity.
//!
//! 3. The lease's last activity is older than the GC threshold.
//!    "Last activity" is `last_renewed_at.unwrap_or(created_at)`.
//!    The threshold is operator-tunable via
//!    `TRITOND_DHCP_LEASE_GC_THRESHOLD_SECS` and defaults to 7 days
//!    — long enough to outlive any plausible "instance vanished but
//!    a re-create with sticky-by-MAC is imminent" window without
//!    holding orphaned IPv4 addresses out of the allocator
//!    indefinitely.
//!
//! Each delete emits an audit event with actor `"tritond-dhcp-
//! reconciler"` so the operator can trace reconciler-driven removals
//! distinct from explicit `delete_dhcp_lease` calls.
//!
//! ## Configuration
//!
//! * `TRITOND_DHCP_RECONCILE_INTERVAL_SECS` — how often the
//!   reconciler wakes (default 300 = 5 min).
//! * `TRITOND_DHCP_LEASE_GC_THRESHOLD_SECS` — minimum
//!   `now - last_activity` before a lease is GC-eligible (default
//!   604_800 = 7 days).
//!
//! ## Why not delete on instance-delete directly
//!
//! The naïve "delete the lease in the same transaction that deletes
//! the instance" approach loses sticky-by-MAC for the (rare but
//! real) re-create-with-same-MAC operator workflow, where the
//! desired behaviour is to reuse the previously assigned IP. By
//! waiting `threshold` before reaping, the lease record stays
//! available for sticky reuse during the typical "operator
//! mis-clicked, re-creates within minutes" window, and is reaped
//! once the operator clearly isn't coming back.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use tokio::time::interval;
use tracing::{debug, info, warn};

use tritond_audit::Outcome as AuditOutcome;
use tritond_store::{DhcpLease, Store, StoreError};

use crate::audit::AuditService;
use crate::auth::{Action, Principal};

/// Identifier used as the actor on audit events emitted by the
/// reconciler. Lets operators reading the audit chain distinguish
/// between reconciler-driven and operator-driven lease removals.
const RECONCILER_ACTOR: &str = "tritond-dhcp-reconciler";

/// Default cadence: walk every 5 minutes. Reconciliation is
/// idempotent and cheap (one range-scan + per-lease O(1) lookups),
/// so 5 min is a comfortable balance — frequent enough to keep
/// orphan churn bounded, infrequent enough that the work disappears
/// in the noise of a healthy cluster.
pub const DEFAULT_RECONCILE_INTERVAL: Duration = Duration::from_secs(300);

/// Default GC threshold: 7 days. Generous enough for the
/// "operator deletes, re-creates same MAC days later" sticky-by-MAC
/// case, tight enough to keep the IPAM table from filling with
/// long-dead leases.
pub const DEFAULT_LEASE_GC_THRESHOLD: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// Cadence and staleness threshold for the reconciler.
#[derive(Debug, Clone, Copy)]
pub struct ReconcilerConfig {
    pub interval: Duration,
    pub gc_threshold: Duration,
}

impl Default for ReconcilerConfig {
    fn default() -> Self {
        Self {
            interval: DEFAULT_RECONCILE_INTERVAL,
            gc_threshold: DEFAULT_LEASE_GC_THRESHOLD,
        }
    }
}

/// Spawn the reconciler task and detach. Returns the handle for
/// callers that want to await shutdown; production drops it on the
/// floor and lets the runtime shut everything down at process exit.
pub fn spawn(
    store: Arc<dyn Store>,
    audit: Arc<AuditService>,
    cfg: ReconcilerConfig,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(run(store, audit, cfg))
}

async fn run(store: Arc<dyn Store>, audit: Arc<AuditService>, cfg: ReconcilerConfig) {
    info!(
        interval_secs = cfg.interval.as_secs(),
        gc_threshold_secs = cfg.gc_threshold.as_secs(),
        "dhcp lease reconciler starting",
    );
    // The first tick of `tokio::time::interval` fires immediately,
    // which would race the rest of bootstrap on a freshly started
    // tritond. Skip it so the first reconcile happens after one
    // full interval.
    let mut tick = interval(cfg.interval);
    tick.tick().await;
    loop {
        tick.tick().await;
        let outcome = reconcile_once(store.as_ref(), audit.as_ref(), cfg.gc_threshold).await;
        match outcome {
            Ok(stats) if stats.reaped == 0 => {
                debug!(
                    examined = stats.examined,
                    pinned_kept = stats.pinned_kept,
                    instance_alive_kept = stats.instance_alive_kept,
                    fresh_kept = stats.fresh_kept,
                    "dhcp reconcile pass: nothing to reap",
                );
            }
            Ok(stats) => {
                info!(
                    examined = stats.examined,
                    reaped = stats.reaped,
                    pinned_kept = stats.pinned_kept,
                    instance_alive_kept = stats.instance_alive_kept,
                    fresh_kept = stats.fresh_kept,
                    "dhcp reconcile pass complete",
                );
            }
            Err(e) => {
                warn!(error = %e, "dhcp reconcile pass failed; will retry next interval");
            }
        }
    }
}

/// Aggregate counts for one reconcile pass. Surfaced for tests and
/// for the operator-visible info-level summary.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ReconcileStats {
    /// Total leases visited this pass.
    pub examined: usize,
    /// Leases successfully deleted.
    pub reaped: usize,
    /// Leases kept because a reservation pins the MAC.
    pub pinned_kept: usize,
    /// Leases kept because the instance is still alive.
    pub instance_alive_kept: usize,
    /// Leases kept because they're younger than the GC threshold.
    pub fresh_kept: usize,
}

/// Run exactly one reconcile pass. Public so tests can drive the
/// state machine deterministically without spinning up a tokio
/// `interval`.
pub async fn reconcile_once(
    store: &dyn Store,
    audit: &AuditService,
    gc_threshold: Duration,
) -> Result<ReconcileStats, StoreError> {
    let leases = store.list_all_dhcp_leases().await?;
    let mut stats = ReconcileStats::default();
    let cutoff_dur = chrono::Duration::from_std(gc_threshold).map_err(|e| {
        StoreError::Backend(format!("gc threshold doesn't fit chrono::Duration: {e}"))
    })?;
    let cutoff = Utc::now() - cutoff_dur;

    for lease in leases {
        stats.examined += 1;
        let last_activity = lease.last_renewed_at.unwrap_or(lease.created_at);
        if last_activity > cutoff {
            stats.fresh_kept += 1;
            continue;
        }
        // Reservation pin overrides everything else: a pinned MAC's
        // lease is by design allowed to outlive its instance, so we
        // never reap it here. Operator releases via
        // delete_dhcp_lease if they want it gone.
        match store.get_dhcp_reservation(lease.vpc_id, &lease.mac).await {
            Ok(_) => {
                stats.pinned_kept += 1;
                continue;
            }
            Err(StoreError::NotFound) => {}
            Err(e) => return Err(e),
        }
        // Instance liveness check.
        match store.get_instance(lease.instance_id).await {
            Ok(_) => {
                stats.instance_alive_kept += 1;
                continue;
            }
            Err(StoreError::NotFound) => {}
            Err(e) => return Err(e),
        }
        // All three conditions met: orphaned + unpinned + stale.
        match reap_one(store, audit, &lease).await {
            Ok(true) => stats.reaped += 1,
            // Race: operator or another reconciler tick beat us
            // to the delete. Final state matches what we wanted
            // either way, so don't increment reaped (we didn't
            // do the work) and don't warn.
            Ok(false) => {}
            Err(e) => {
                warn!(
                    vpc_id = %lease.vpc_id,
                    mac = %lease.mac,
                    error = %e,
                    "failed to reap stale dhcp lease; will retry next pass",
                );
            }
        }
    }
    Ok(stats)
}

/// Returns `Ok(true)` when the reconciler actually deleted the
/// lease and emitted an audit event. Returns `Ok(false)` when
/// somebody else raced us to the delete (NotFound from the store)
/// — the desired end state still holds, so we skip the audit
/// event and don't count it as our work.
async fn reap_one(
    store: &dyn Store,
    audit: &AuditService,
    lease: &DhcpLease,
) -> Result<bool, StoreError> {
    match store.delete_dhcp_lease(lease.vpc_id, &lease.mac).await {
        Ok(()) => {}
        Err(StoreError::NotFound) => return Ok(false),
        Err(other) => return Err(other),
    }
    audit
        .record_mutation(
            &Principal::Anonymous,
            Action::DhcpLeaseDelete,
            None,
            Some(format!("DhcpLease::\"{}/{}\"", lease.vpc_id, lease.mac)),
            AuditOutcome::Success {
                resource: Some(format!("DhcpLease::\"{}/{}\"", lease.vpc_id, lease.mac)),
            },
            serde_json::json!({
                "vpc_id": lease.vpc_id,
                "mac": lease.mac,
                "ipv4": lease.ipv4,
                "instance_id": lease.instance_id,
                "actor": RECONCILER_ACTOR,
                "reason": "stale_orphaned_lease",
            }),
        )
        .await;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    use chrono::Duration as ChronoDuration;
    use tritond_audit::MemChain;
    use tritond_store::{
        DhcpLease, MemStore, NewDhcpReservation, NewProject, NewSilo, NewTenant, NewVpc,
    };
    use uuid::Uuid;

    fn audit_for_test() -> Arc<AuditService> {
        Arc::new(AuditService::new(Arc::new(MemChain::new())))
    }

    /// Construct a DhcpLease record with the supplied "minutes ago"
    /// for both `created_at` and `last_renewed_at` (None means
    /// `last_renewed_at` is left None — `effective_last_activity`
    /// will fall back to `created_at`).
    fn lease(
        vpc_id: Uuid,
        mac: &str,
        ipv4: Ipv4Addr,
        instance_id: Uuid,
        created_minutes_ago: i64,
        renewed_minutes_ago: Option<i64>,
    ) -> DhcpLease {
        let now = Utc::now();
        DhcpLease {
            vpc_id,
            mac: mac.into(),
            ipv4,
            instance_id,
            nic_id: Uuid::new_v4(),
            last_msg_type: None,
            last_xid: None,
            last_renewed_at: renewed_minutes_ago.map(|m| now - ChronoDuration::minutes(m)),
            created_at: now - ChronoDuration::minutes(created_minutes_ago),
        }
    }

    #[tokio::test]
    async fn reconcile_reaps_stale_orphaned_unpinned_lease() {
        let store: Arc<dyn Store> = Arc::new(MemStore::new());
        let audit = audit_for_test();
        let vpc_id = fixture_via_store(store.as_ref()).await;
        // Seed: one orphaned, unpinned, very-stale lease.
        let dead_instance = Uuid::new_v4();
        store
            .record_dhcp_lease(lease(
                vpc_id,
                "02:08:20:aa:bb:01",
                "10.99.0.10".parse().unwrap(),
                dead_instance,
                60 * 24 * 14, // 14 days old
                None,
            ))
            .await
            .unwrap();

        let stats = reconcile_once(store.as_ref(), audit.as_ref(), Duration::from_secs(60))
            .await
            .unwrap();
        assert_eq!(stats.examined, 1);
        assert_eq!(stats.reaped, 1);
        assert_eq!(stats.pinned_kept, 0);
        assert_eq!(stats.fresh_kept, 0);

        // Lease is gone.
        let leases = store.list_all_dhcp_leases().await.unwrap();
        assert!(
            leases.is_empty(),
            "stale orphan lease should have been reaped"
        );
    }

    #[tokio::test]
    async fn reconcile_keeps_lease_when_reservation_pins_mac() {
        let store: Arc<dyn Store> = Arc::new(MemStore::new());
        let audit = audit_for_test();
        let vpc_id = fixture_via_store(store.as_ref()).await;
        let pinned_mac = "02:08:20:aa:bb:02";
        let reserved_ip: Ipv4Addr = "10.99.0.50".parse().unwrap();
        store
            .create_dhcp_reservation(
                vpc_id,
                NewDhcpReservation {
                    mac: pinned_mac.into(),
                    ipv4: reserved_ip,
                    hostname: None,
                    per_mac_options: vec![],
                },
            )
            .await
            .unwrap();
        let dead_instance = Uuid::new_v4();
        store
            .record_dhcp_lease(lease(
                vpc_id,
                pinned_mac,
                reserved_ip,
                dead_instance,
                60 * 24 * 30, // 30 days old — definitely stale
                None,
            ))
            .await
            .unwrap();

        let stats = reconcile_once(store.as_ref(), audit.as_ref(), Duration::from_secs(60))
            .await
            .unwrap();
        assert_eq!(stats.examined, 1);
        assert_eq!(stats.reaped, 0);
        assert_eq!(stats.pinned_kept, 1);
        // Lease still there.
        assert_eq!(store.list_all_dhcp_leases().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn reconcile_keeps_fresh_lease_even_if_orphaned() {
        let store: Arc<dyn Store> = Arc::new(MemStore::new());
        let audit = audit_for_test();
        let vpc_id = fixture_via_store(store.as_ref()).await;
        let dead_instance = Uuid::new_v4();
        // 5 minutes old — well under any reasonable threshold.
        store
            .record_dhcp_lease(lease(
                vpc_id,
                "02:08:20:aa:bb:03",
                "10.99.0.20".parse().unwrap(),
                dead_instance,
                5,
                None,
            ))
            .await
            .unwrap();

        // Threshold = 1 hour: lease younger than threshold, so kept
        // even though instance is gone and there's no reservation.
        let stats = reconcile_once(store.as_ref(), audit.as_ref(), Duration::from_secs(60 * 60))
            .await
            .unwrap();
        assert_eq!(stats.examined, 1);
        assert_eq!(stats.reaped, 0);
        assert_eq!(stats.fresh_kept, 1);
        assert_eq!(stats.pinned_kept, 0);
    }

    #[tokio::test]
    async fn reconcile_uses_last_renewed_at_when_present_to_keep_recently_active_lease() {
        // A lease created 30 days ago but renewed 5 minutes ago
        // is fresh — last_renewed_at supersedes created_at. The
        // reconciler must NOT reap it.
        let store: Arc<dyn Store> = Arc::new(MemStore::new());
        let audit = audit_for_test();
        let vpc_id = fixture_via_store(store.as_ref()).await;
        let dead_instance = Uuid::new_v4();
        store
            .record_dhcp_lease(lease(
                vpc_id,
                "02:08:20:aa:bb:04",
                "10.99.0.30".parse().unwrap(),
                dead_instance,
                60 * 24 * 30,
                Some(5),
            ))
            .await
            .unwrap();

        let stats = reconcile_once(store.as_ref(), audit.as_ref(), Duration::from_secs(60 * 60))
            .await
            .unwrap();
        assert_eq!(stats.fresh_kept, 1);
        assert_eq!(stats.reaped, 0);
    }

    /// Same as `fixture_vpc` but takes `&dyn Store` so the
    /// reconciler tests that operate on `Arc<dyn Store>` can share
    /// it without juggling concrete-type access.
    async fn fixture_via_store(store: &dyn Store) -> Uuid {
        let silo = store
            .create_silo(NewSilo {
                name: "recon-silo".into(),
                description: None,
            })
            .await
            .unwrap();
        let tenant = store
            .create_tenant(
                silo.id,
                NewTenant {
                    name: "recon-tenant".into(),
                    description: None,
                },
            )
            .await
            .unwrap();
        let project = store
            .create_project(
                tenant.id,
                NewProject {
                    name: "recon-proj".into(),
                    description: None,
                },
            )
            .await
            .unwrap();
        let vpc = store
            .create_vpc(
                tenant.id,
                project.id,
                NewVpc {
                    name: "recon-vpc".into(),
                    description: None,
                    ipv4_block: Some("10.99.0.0/16".parse().unwrap()),
                    ipv6_block: None,
                },
            )
            .await
            .unwrap();
        vpc.id
    }
}
