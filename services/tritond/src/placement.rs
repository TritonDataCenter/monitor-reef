// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Glue between `tritond_store`'s placement keyspaces and the
//! `tritond_placement` engine.
//!
//! The store owns the FDB row shapes (`CnCapacity`, `CnPlacement`,
//! `CnReservation`, `CnLoadSummary`, `InstanceAffinity`, plus the
//! joined `CnPickSnapshot` and `TenantInstanceProjection`); the
//! engine owns its projection types (`CnView`, `CapacityView`,
//! `PlacementPolicyView`, ...). This module performs the
//! conversion -- the placement crate never takes a path dep on the
//! store, and the store never takes one on the placement crate, so
//! the directional dep tree stays a leaf-leaf shape.

use std::sync::Arc;

use chrono::Utc;
use tritond_placement::types::DeviceReservation as PlacementDeviceReservation;
use tritond_placement::{
    AssignedInstanceView, CapacityView, ChainContext, ChainRunner, CnLoadSummaryView, CnRoleView,
    CnStateView, CnView, DeviceKind as PlacementDeviceKind, DeviceView, ExplainReport,
    NumaNodeView, OverprovisionDefaults, PlacementPolicyView, PlacementRequest, ReservationView,
    SiblingInstanceView, Strategy, StrategyWeights, UnderlayCapability, ZpoolView,
    default_filter_chain, default_scorer_chain, resolved_weights,
};
use tritond_store::{
    CnPickSnapshot, CnReservation, CnRole, CnState, DeviceKind as StoreDeviceKind, StorageTier,
    Store, StoreError, TenantInstanceProjection,
};
use uuid::Uuid;

/// Sized against the agent heartbeater's ~5 s tick.
pub const DEFAULT_AGENT_HEARTBEAT_THRESHOLD_SECS: u64 = 60;

/// 3 ticks at the 60 s load-summary interval — the default product of
/// `placement.load_materializer.{staleness_ticks,interval_secs}`.
/// [`pick`] computes the live value from [`Settings`]; this const is
/// for callers without a settings read in hand.
pub const DEFAULT_LOAD_STALENESS_SECS: u64 = 180;

/// Convert a [`CnPickSnapshot`] from the store to a
/// [`CnView`] the placement engine consumes.
///
/// `load_staleness_secs` is the read-side age gate for the CN's
/// `cn-load-summary` row: a row whose `last_refreshed_at` is older is
/// projected `stale = true` regardless of its written flag, so a dead
/// materializer cannot leave frozen rows scoring as fresh.
pub fn snapshot_to_cn_view(
    snap: CnPickSnapshot,
    now: chrono::DateTime<Utc>,
    load_staleness_secs: u64,
) -> CnView {
    // RAM-normalisation denominator for the load projection, captured
    // before `capacity` is moved into its own view.
    let ram_total_mb = snap.capacity.as_ref().map(|c| c.ram_total_mb);
    let capacity = snap.capacity.map(capacity_view);
    let placement = placement_policy_view(snap.placement);
    // Backstop for crashed/leaked reservations: a saga that dies after
    // writing its `cn-reservation` row but before its success/undo
    // release would otherwise wedge the CN's capacity forever (no
    // reaper exists yet, and the snapshot reads every row for the CN).
    // Dropping past-deadline rows here lets the residual self-heal once
    // the reservation TTL lapses. The healthy path still releases
    // explicitly on saga success/unwind, so this only catches crashes.
    let active_reservations = snap
        .reservations
        .into_iter()
        .filter(|r| r.expires_at > now)
        .map(reservation_view)
        .collect();
    let load_summary = snap
        .load_summary
        .map(|s| load_summary_view(s, ram_total_mb, now, load_staleness_secs));
    let assigned_instances = snap
        .assigned_instances
        .into_iter()
        .map(assigned_instance_view)
        .collect();
    CnView {
        server_uuid: snap.cn.server_uuid,
        hostname: snap.cn.hostname,
        state: cn_state_view(snap.cn.state),
        role: cn_role_view(snap.cn.role),
        last_seen: snap.cn.last_seen,
        capacity,
        placement,
        active_reservations,
        load_summary,
        assigned_instances,
    }
}

/// Convert a [`TenantInstanceProjection`] to a
/// [`SiblingInstanceView`].
pub fn projection_to_sibling_view(p: TenantInstanceProjection) -> SiblingInstanceView {
    SiblingInstanceView {
        instance_id: p.instance.id,
        silo_uuid: silo_for_tenant_placeholder(p.instance.tenant_id),
        tenant_uuid: p.instance.tenant_id,
        project_uuid: p.instance.project_id,
        host_cn_uuid: p.instance.host_cn_uuid,
        host_fault_domain: p.host_fault_domain,
    }
}

pub fn build_default_runner(strategy: Strategy) -> ChainRunner {
    build_runner_with_weights(strategy, &resolved_weights(strategy))
}

/// Build the default filter/scorer chain but with explicit scorer
/// weights (e.g. resolved from the active placement profile). Each
/// scorer takes its weight from `weights`, falling back to the
/// scorer's built-in default when the map omits it.
pub fn build_runner_with_weights(strategy: Strategy, weights: &StrategyWeights) -> ChainRunner {
    let mut runner = ChainRunner::empty(strategy);
    for filter in default_filter_chain() {
        runner = runner.with_filter(filter);
    }
    for (scorer, _default) in default_scorer_chain() {
        let name = scorer.name();
        let w = weights.get(name).unwrap_or_else(|| scorer.default_weight());
        runner = runner.with_scorer(scorer, w);
    }
    runner
}

/// Resolve the scorer weights for a pick: an explicit per-request
/// `strategy_override` wins (migration / dry-run intent); otherwise the
/// cluster's active placement profile; otherwise the Spread default.
fn resolve_pick_weights(
    request_override: Option<Strategy>,
    profiles: &tritond_store::PlacementProfiles,
) -> (Strategy, StrategyWeights, Option<String>) {
    if let Some(s) = request_override {
        return (s, resolved_weights(s), None);
    }
    if let Some(p) = profiles.active_profile() {
        // Key the weights by the engine's own &'static scorer names
        // (StrategyWeights::set requires them); look each up in the
        // profile's map. Profile entries for unknown scorer names are
        // ignored; scorers the profile omits keep their default weight.
        let mut w = StrategyWeights::new();
        for (scorer, _default) in default_scorer_chain() {
            let name = scorer.name();
            if let Some(weight) = p.weights.get(name) {
                w.set(name, *weight);
            }
        }
        return (Strategy::Spread, w, Some(p.name.clone()));
    }
    (Strategy::Spread, resolved_weights(Strategy::Spread), None)
}

pub fn build_chain_context<'a>(
    now: chrono::DateTime<chrono::Utc>,
    strategy_weights: &'a StrategyWeights,
    sibling_instances: &'a [SiblingInstanceView],
) -> ChainContext<'a> {
    ChainContext {
        now,
        cluster_overprovision: OverprovisionDefaults::default(),
        agent_heartbeat_threshold_secs: DEFAULT_AGENT_HEARTBEAT_THRESHOLD_SECS,
        strategy_weights,
        sibling_instances,
    }
}

/// What [`pick`] writes when `commit == Commit::Yes`.
#[derive(Debug, Clone)]
pub struct PickCommit {
    /// `CnReservation` row inserted under `cn_reservation/<server_uuid>/<saga_id>`.
    pub reservation: CnReservation,
    /// Instance row after `set_instance_host_cn` lands.
    pub instance: tritond_store::Instance,
}

/// `chosen` is `Some` when a CN passed every filter; `report` carries
/// every CN's verdict either way.
#[derive(Debug, Clone)]
pub struct PickOutcome {
    pub chosen: Option<Uuid>,
    pub report: ExplainReport,
    pub committed: Option<PickCommit>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum Commit {
    /// Dry-run: explain only, no state changes.
    No,
    /// Insert the reservation and pin `Instance.host_cn_uuid`.
    Yes {
        saga_id: Uuid,
        sec_id: Uuid,
        sec_epoch: u64,
    },
    /// Reservation only — leave `Instance.host_cn_uuid` alone.
    /// Live migration uses this: source pin stays until cutover, but
    /// target capacity must be held so a concurrent `instance-create`
    /// can't steal it.
    ReservationOnly {
        saga_id: Uuid,
        sec_id: Uuid,
        sec_epoch: u64,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum PickError {
    /// `report` carries every rejected CN's filter verdicts.
    #[error("no eligible CN")]
    NoEligibleCn { report: Box<ExplainReport> },
    #[error("store: {0}")]
    Store(#[from] StoreError),
}

/// Run the placement chain end-to-end against a live store.
pub async fn pick(
    store: &Arc<dyn Store>,
    request: PlacementRequest,
    commit: Commit,
) -> Result<PickOutcome, PickError> {
    // 1. List approved CNs. Pending / Disabled rows are invisible
    //    to placement; the `cn-approved-and-live` filter would
    //    reject them anyway, but pre-filtering here keeps the
    //    snapshot loop tight on large fleets.
    let cns = store.list_cns(Some(CnState::Approved)).await?;

    // Cluster settings drive both the load-staleness age gate (read
    // live so `placement.load_materializer.staleness_ticks` takes
    // effect without a restart) and the active profile's weights.
    let settings = store.get_settings().await?;
    let load_staleness_secs = settings
        .placement_load_materializer_staleness_ticks
        .saturating_mul(settings.placement_load_materializer_interval_secs)
        .max(1);

    // 2. Build CnView projections for each. Skip CNs whose
    //    snapshot read fails (e.g. the row was concurrently
    //    deleted between list and read); the report will not
    //    contain them, which is intended -- a placement run does
    //    not have to span every momentary fleet shape.
    let now = Utc::now();
    let mut cn_views: Vec<CnView> = Vec::with_capacity(cns.len());
    for cn in &cns {
        match store.get_cn_pick_snapshot(cn.server_uuid).await {
            Ok(snap) => cn_views.push(snapshot_to_cn_view(snap, now, load_staleness_secs)),
            Err(StoreError::NotFound) => continue,
            Err(e) => return Err(PickError::Store(e)),
        }
    }

    // 3. Tenant sibling slice + host_fault_domain join.
    let siblings_raw = store
        .list_tenant_instance_projections(request.tenant_uuid)
        .await?;
    let siblings: Vec<SiblingInstanceView> = siblings_raw
        .into_iter()
        .map(projection_to_sibling_view)
        .collect();

    // 4. + 5. Runner + chain context, then pick. Scorer weights come
    // from the active placement profile (cluster setting, read live so
    // a profile change takes effect on the next pick without a
    // restart); a per-request strategy_override still wins.
    let (strategy, weights, profile) =
        resolve_pick_weights(request.strategy_override, &settings.placement_profiles);
    let runner = build_runner_with_weights(strategy, &weights);
    let ctx = build_chain_context(now, &weights, &siblings);
    let (chosen, mut report) = runner.pick(&cn_views, &request, &ctx);
    // Record which named profile produced these weights (the runner
    // only knows the Strategy label, not the profile name).
    report.profile = profile;

    // Commit: reservation + Instance pin run as two sequential
    // writes (single FDB txn wrapper not landed).
    let committed = match (chosen, commit) {
        (
            Some(cn_uuid),
            Commit::Yes {
                saga_id,
                sec_id,
                sec_epoch,
            },
        ) => {
            let reservation = CnReservation {
                server_uuid: cn_uuid,
                saga_id,
                instance_id: request.instance_id,
                cpu_units: request.cpu_units,
                ram_mb: request.ram_mb,
                disk: request.disk.clone(),
                devices: request
                    .required_devices
                    .iter()
                    .map(|d| tritond_store::DeviceReservation {
                        kind: store_device_kind(d.kind),
                        model: d.model.clone(),
                        count: d.count,
                    })
                    .collect(),
                created_at: Utc::now(),
                expires_at: request.deadline,
                created_by_sec_id: sec_id,
                created_at_epoch: sec_epoch,
            };
            store.reserve_cn_capacity(reservation.clone()).await?;
            let instance = store
                .set_instance_host_cn(request.instance_id, Some(cn_uuid))
                .await?;
            Some(PickCommit {
                reservation,
                instance,
            })
        }
        (
            Some(cn_uuid),
            Commit::ReservationOnly {
                saga_id,
                sec_id,
                sec_epoch,
            },
        ) => {
            let reservation = CnReservation {
                server_uuid: cn_uuid,
                saga_id,
                instance_id: request.instance_id,
                cpu_units: request.cpu_units,
                ram_mb: request.ram_mb,
                disk: request.disk.clone(),
                devices: request
                    .required_devices
                    .iter()
                    .map(|d| tritond_store::DeviceReservation {
                        kind: store_device_kind(d.kind),
                        model: d.model.clone(),
                        count: d.count,
                    })
                    .collect(),
                created_at: Utc::now(),
                expires_at: request.deadline,
                created_by_sec_id: sec_id,
                created_at_epoch: sec_epoch,
            };
            store.reserve_cn_capacity(reservation.clone()).await?;
            // Read the instance for the PickCommit shape but do
            // NOT call set_instance_host_cn — migration keeps
            // source pin until the cutover step.
            let instance = store.get_instance(request.instance_id).await?;
            Some(PickCommit {
                reservation,
                instance,
            })
        }
        (None, Commit::Yes { .. }) | (None, Commit::ReservationOnly { .. }) => {
            return Err(PickError::NoEligibleCn {
                report: Box::new(report),
            });
        }
        _ => None,
    };

    Ok(PickOutcome {
        chosen,
        report,
        committed,
    })
}

/// Idempotent: `Ok(())` whether the row existed or was already
/// released by a concurrent unwind.
pub async fn release_reservation(
    store: &Arc<dyn Store>,
    server_uuid: Uuid,
    saga_id: Uuid,
    instance_id: Uuid,
) -> Result<(), StoreError> {
    match store.release_cn_reservation(server_uuid, saga_id).await {
        Ok(()) | Err(StoreError::NotFound) => {}
        Err(e) => return Err(e),
    }
    match store.set_instance_host_cn(instance_id, None).await {
        Ok(_) | Err(StoreError::NotFound) => Ok(()),
        Err(e) => Err(e),
    }
}

fn store_device_kind(k: PlacementDeviceKind) -> StoreDeviceKind {
    match k {
        PlacementDeviceKind::Gpu => StoreDeviceKind::Gpu,
        PlacementDeviceKind::SrIovVf => StoreDeviceKind::SrIovVf,
    }
}

// ---------------------------------------------------------------------------
// Projection helpers (private; one per FDB row).
// ---------------------------------------------------------------------------

fn capacity_view(c: tritond_store::CnCapacity) -> CapacityView {
    CapacityView {
        cpu_cores_physical: c.cpu_cores_physical,
        cpu_threads_logical: c.cpu_threads_logical,
        numa_nodes: c
            .numa_nodes
            .into_iter()
            .map(|n| NumaNodeView {
                node_id: n.node_id,
                cores: n.cores,
                ram_mb: n.ram_mb,
            })
            .collect(),
        ram_total_mb: c.ram_total_mb,
        ram_available_mb: c.ram_available_mb,
        cpu_utilization_pct: c.cpu_utilization_pct,
        zpools: c
            .zpools
            .into_iter()
            .map(|z| ZpoolView {
                name: z.name,
                total_bytes: z.total_bytes,
                free_bytes: z.free_bytes,
                tier: storage_tier_view(z.tier),
            })
            .collect(),
        nic_tags: c.nic_tags,
        underlay: UnderlayCapability {
            ipv4: c.underlay.ipv4,
            ipv6: c.underlay.ipv6,
        },
        devices: c
            .devices
            .into_iter()
            .map(|d| DeviceView {
                kind: device_kind_view(d.kind),
                model: d.model,
                free_count: d.free_count,
            })
            .collect(),
        platform_version: c.platform_version,
        reported_at: c.reported_at,
        hvm_supported: c.hvm_supported,
        // Migration compatibility fingerprint. Filters Skip when a
        // field is absent so instance-create runs unaffected.
        vmm_protocol_version: c.vmm_protocol_version,
        cpu_features: c.cpu_features,
        tsc_offset_ns: c.tsc_offset_ns,
        zpool_props: c
            .zpool_props
            .into_iter()
            .map(|(pool, props)| {
                (
                    pool,
                    tritond_placement::types::ZpoolPropFingerprint {
                        encryption: props.encryption,
                        compression: props.compression,
                        recordsize_bytes: props.recordsize_bytes,
                    },
                )
            })
            .collect(),
    }
}

fn placement_policy_view(p: tritond_store::CnPlacement) -> PlacementPolicyView {
    PlacementPolicyView {
        reserved: p.reserved,
        cordoned: p.cordoned,
        cordoned_reason: p.cordoned_reason,
        pinned_silo_uuid: p.pinned_silo_uuid,
        pinned_tenant_uuid: p.pinned_tenant_uuid,
        traits: p.traits,
        overprovision_cpu: p.overprovision_cpu,
        overprovision_ram: p.overprovision_ram,
        fault_domain: p.fault_domain,
    }
}

fn reservation_view(r: tritond_store::CnReservation) -> ReservationView {
    ReservationView {
        saga_id: r.saga_id,
        instance_id: r.instance_id,
        cpu_units: r.cpu_units,
        ram_mb: r.ram_mb,
        disk: r.disk,
        devices: r
            .devices
            .into_iter()
            .map(|d| PlacementDeviceReservation {
                kind: device_kind_view(d.kind),
                model: d.model,
                count: d.count,
            })
            .collect(),
        deadline: r.expires_at,
    }
}

fn load_summary_view(
    s: tritond_store::CnLoadSummary,
    ram_total_mb: Option<u64>,
    now: chrono::DateTime<Utc>,
    load_staleness_secs: u64,
) -> CnLoadSummaryView {
    // Read-side age gate: the materializer only sets `stale` while it
    // is alive, so an un-refreshed row must go stale by age here.
    let age_stale =
        now.signed_duration_since(s.last_refreshed_at).num_seconds() > load_staleness_secs as i64;
    // RAM utilisation as a true fraction of the CN's total RAM. The
    // row stores bytes; without a capacity row there is no denominator,
    // so the projection has no RAM signal and the row is treated stale
    // (mirrors the materializer's unknown-cores rule for CPU).
    let ram_fraction = ram_total_mb
        .filter(|mb| *mb > 0)
        .map(|mb| ((s.ram_used_p95_5m as f64) / (mb as f64 * 1_048_576.0)).clamp(0.0, 1.0) as f32);
    CnLoadSummaryView {
        last_refreshed_at: s.last_refreshed_at,
        stale: s.stale || age_stale || ram_fraction.is_none(),
        cpu_p50_5m: s.cpu_p50_5m,
        cpu_p95_5m: s.cpu_p95_5m,
        cpu_p50_1d: s.cpu_p50_1d,
        cpu_p95_1d: s.cpu_p95_1d,
        ram_used_p95_5m: ram_fraction.unwrap_or(0.0),
        nic_tx_bps_p95_5m: s.nic_tx_bps_p95_5m,
        nic_rx_bps_p95_5m: s.nic_rx_bps_p95_5m,
    }
}

fn assigned_instance_view(i: tritond_store::Instance) -> AssignedInstanceView {
    AssignedInstanceView {
        instance_id: i.id,
        // Instance carries tenant/project but not silo; placeholder
        // de-weights the cotenant scorer until the project→tenant→silo
        // join lands.
        silo_uuid: silo_for_tenant_placeholder(i.tenant_id),
        tenant_uuid: i.tenant_id,
        cpu_units: (i.cpu as u32) * 100,
        ram_mb: i.memory_bytes / 1_048_576,
    }
}

fn silo_for_tenant_placeholder(_tenant_id: uuid::Uuid) -> uuid::Uuid {
    uuid::Uuid::nil()
}

fn cn_state_view(s: CnState) -> CnStateView {
    // Both enums are `#[non_exhaustive]`; future variants map to
    // `Pending` (placement-invisible until the projection learns
    // the new state).
    match s {
        CnState::Pending => CnStateView::Pending,
        CnState::Approved => CnStateView::Approved,
        CnState::Disabled => CnStateView::Disabled,
        _ => CnStateView::Pending,
    }
}

fn cn_role_view(r: CnRole) -> CnRoleView {
    match r {
        CnRole::Tenant => CnRoleView::Tenant,
        CnRole::Edge => CnRoleView::Edge,
        CnRole::Both => CnRoleView::Both,
        _ => CnRoleView::Tenant,
    }
}

fn storage_tier_view(t: StorageTier) -> tritond_placement::types::StorageTier {
    // `StorageTier` is `#[non_exhaustive]`; a future variant
    // here maps to `Mixed` as the conservative default until the
    // projection is taught about it.
    match t {
        StorageTier::Ssd => tritond_placement::types::StorageTier::Ssd,
        StorageTier::Nvme => tritond_placement::types::StorageTier::Nvme,
        StorageTier::Hdd => tritond_placement::types::StorageTier::Hdd,
        StorageTier::Mixed => tritond_placement::types::StorageTier::Mixed,
        _ => tritond_placement::types::StorageTier::Mixed,
    }
}

fn device_kind_view(k: StoreDeviceKind) -> PlacementDeviceKind {
    // `DeviceKind` is `#[non_exhaustive]`; a future variant maps
    // to `Gpu` as the conservative placeholder (it will never
    // match a real device asks because the engine compares on
    // both `kind` and `model`).
    match k {
        StoreDeviceKind::Gpu => PlacementDeviceKind::Gpu,
        StoreDeviceKind::SrIovVf => PlacementDeviceKind::SrIovVf,
        _ => PlacementDeviceKind::Gpu,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use std::collections::BTreeMap;
    use tritond_store::{
        Cn, CnCapacity, CnLoadSummary, CnPlacement, CnReservation, DeviceCapacity, NumaNode,
        StorageTier, UnderlayCapability as StoreUnderlay, ZpoolCapacity,
    };
    use uuid::Uuid;

    fn make_snapshot() -> CnPickSnapshot {
        let cn = Cn {
            server_uuid: Uuid::new_v4(),
            hostname: "cn".into(),
            admin_ip: None,
            state: CnState::Approved,
            role: CnRole::Tenant,
            registered_at: Utc::now(),
            approved_at: Some(Utc::now()),
            last_seen: Some(Utc::now()),
            sysinfo: serde_json::json!({}),
            claim_code: None,
            claim_code_expires_at: None,
            poll_token: String::new(),
            bound_api_key_id: None,
            pending_credential: None,
            last_status: None,
            console_listen_port: None,
            console_tls_spki_sha256: None,
            console_ticket_key: None,
            imds_token_key: None,
            migrate_ticket_key: None,
        };
        let capacity = CnCapacity {
            server_uuid: cn.server_uuid,
            cpu_cores_physical: 8,
            cpu_threads_logical: 16,
            numa_nodes: vec![NumaNode {
                node_id: 0,
                cores: 8,
                ram_mb: 32_768,
            }],
            ram_total_mb: 32_768,
            ram_available_mb: 30_000,
            cpu_utilization_pct: 0.10,
            zpools: vec![ZpoolCapacity {
                name: "zones".into(),
                total_bytes: 500_000_000_000,
                free_bytes: 400_000_000_000,
                tier: StorageTier::Ssd,
            }],
            nic_tags: vec!["admin".into()],
            underlay: StoreUnderlay {
                ipv4: true,
                ipv6: false,
            },
            devices: vec![DeviceCapacity {
                kind: StoreDeviceKind::Gpu,
                model: "a100".into(),
                free_count: 2,
            }],
            platform_version: "20260501T000000Z".into(),
            hvm_supported: true,
            reported_at: Utc::now(),
            vmm_protocol_version: None,
            cpu_features: Vec::new(),
            tsc_offset_ns: None,
            zpool_props: std::collections::BTreeMap::new(),
        };
        let placement = CnPlacement::fresh(cn.server_uuid, Utc::now());
        let reservation = CnReservation {
            server_uuid: cn.server_uuid,
            saga_id: Uuid::new_v4(),
            instance_id: Uuid::new_v4(),
            cpu_units: 200,
            ram_mb: 4096,
            disk: BTreeMap::new(),
            devices: Vec::new(),
            created_at: Utc::now(),
            expires_at: Utc::now() + chrono::Duration::minutes(5),
            created_by_sec_id: Uuid::nil(),
            created_at_epoch: 0,
        };
        CnPickSnapshot {
            cn,
            capacity: Some(capacity),
            placement,
            reservations: vec![reservation],
            load_summary: None,
            assigned_instances: Vec::new(),
            computed_at: Utc::now(),
        }
    }

    #[test]
    fn snapshot_round_trips_into_cn_view() {
        let snap = make_snapshot();
        let server_uuid = snap.cn.server_uuid;
        let view = snapshot_to_cn_view(snap, Utc::now(), DEFAULT_LOAD_STALENESS_SECS);
        assert_eq!(view.server_uuid, server_uuid);
        assert!(matches!(view.state, CnStateView::Approved));
        assert!(matches!(view.role, CnRoleView::Tenant));
        let cap = view.capacity.as_ref().expect("capacity present");
        assert_eq!(cap.ram_total_mb, 32_768);
        assert_eq!(cap.zpools.len(), 1);
        assert_eq!(cap.devices.len(), 1);
        assert_eq!(view.active_reservations.len(), 1);
        assert_eq!(view.active_reservations[0].cpu_units, 200);
        assert_eq!(view.active_reservations[0].ram_mb, 4096);
    }

    fn make_load_summary(server_uuid: Uuid, refreshed: chrono::DateTime<Utc>) -> CnLoadSummary {
        CnLoadSummary {
            server_uuid,
            cpu_p50_5m: 0.2,
            cpu_p95_5m: 0.4,
            cpu_max_5m: 0.5,
            cpu_p50_1d: 0.1,
            cpu_p95_1d: 0.3,
            cpu_max_1d: 0.6,
            // 16 GiB used of the fixture's 32 GiB total -> 0.5.
            ram_used_p95_5m: 16 * 1024 * 1024 * 1024,
            ram_used_p95_1d: 16 * 1024 * 1024 * 1024,
            disk_used_bytes_p95_5m: BTreeMap::new(),
            disk_used_bytes_p95_1d: BTreeMap::new(),
            nic_tx_bps_p95_5m: 0,
            nic_tx_bps_p95_1d: 0,
            nic_rx_bps_p95_5m: 0,
            nic_rx_bps_p95_1d: 0,
            samples_5m: 5,
            samples_1d: 100,
            last_refreshed_at: refreshed,
            stale: false,
        }
    }

    // RAM p95 must reach the scorer as a 0..1 fraction of the CN's
    // total, not raw GB -- a >=1 GB-used CN previously saturated the
    // scorer's clamp at 1.0.
    #[test]
    fn load_view_normalises_ram_to_a_fraction_of_total() {
        let now = Utc::now();
        let mut snap = make_snapshot();
        snap.load_summary = Some(make_load_summary(snap.cn.server_uuid, now));
        let view = snapshot_to_cn_view(snap, now, DEFAULT_LOAD_STALENESS_SECS);
        let load = view.load_summary.expect("load summary present");
        assert!(!load.stale);
        assert!(
            (load.ram_used_p95_5m - 0.5).abs() < 1e-3,
            "16 GiB of 32 GiB must project as ~0.5, got {}",
            load.ram_used_p95_5m
        );
    }

    // A row the materializer stopped refreshing must go stale by age
    // at read time, regardless of its written `stale = false`.
    #[test]
    fn load_view_goes_stale_by_age() {
        let now = Utc::now();
        let mut snap = make_snapshot();
        snap.load_summary = Some(make_load_summary(
            snap.cn.server_uuid,
            now - chrono::Duration::seconds(DEFAULT_LOAD_STALENESS_SECS as i64 + 1),
        ));
        let view = snapshot_to_cn_view(snap, now, DEFAULT_LOAD_STALENESS_SECS);
        assert!(view.load_summary.expect("present").stale);
    }

    // No capacity row -> no RAM denominator -> the load row cannot be
    // trusted as a fraction; project it stale.
    #[test]
    fn load_view_without_capacity_is_stale() {
        let now = Utc::now();
        let mut snap = make_snapshot();
        snap.load_summary = Some(make_load_summary(snap.cn.server_uuid, now));
        snap.capacity = None;
        let view = snapshot_to_cn_view(snap, now, DEFAULT_LOAD_STALENESS_SECS);
        let load = view.load_summary.expect("present");
        assert!(load.stale);
        assert_eq!(load.ram_used_p95_5m, 0.0);
    }

    #[test]
    fn build_default_runner_lays_down_the_full_chain() {
        let runner = build_default_runner(Strategy::Spread);
        let weights = resolved_weights(Strategy::Spread);
        let ctx = build_chain_context(Utc::now(), &weights, &[]);
        let view = snapshot_to_cn_view(make_snapshot(), Utc::now(), DEFAULT_LOAD_STALENESS_SECS);
        // Use a synthetic request that nothing in the default
        // chain rejects.
        let req_id = Uuid::new_v4();
        let req = PlacementRequest {
            instance_id: req_id,
            silo_uuid: Uuid::nil(),
            tenant_uuid: Uuid::nil(),
            project_uuid: Uuid::nil(),
            role: CnRoleView::Tenant,
            cpu_units: 100,
            ram_mb: 1024,
            disk: BTreeMap::new(),
            required_traits: BTreeMap::new(),
            required_nic_tags: Vec::new(),
            required_underlay: UnderlayCapability {
                ipv4: true,
                ipv6: false,
            },
            required_devices: Vec::new(),
            needs_hvm: false,
            min_platform: None,
            affinity: tritond_store::InstanceAffinity::empty(req_id, Uuid::nil(), Utc::now()),
            strategy_override: None,
            force_cn: None,
            ignore_scope_pin: false,
            deadline: Utc::now() + chrono::Duration::minutes(5),
            avoid_cn: Vec::new(),
            migration: None,
        };
        let (chosen, report) = runner.pick(&[view.clone()], &req, &ctx);
        assert_eq!(chosen, Some(view.server_uuid));
        // 23 filters (17 base + cn-capacity-present + 5 migration) +
        // 11 scorers in the default chain (PL-6 retired avoid-peaky).
        assert_eq!(report.per_cn[0].filter_results.len(), 23);
        assert_eq!(report.per_cn[0].scorer_results.len(), 11);
    }

    // ---------------------------------------------------------------------
    // pick() end-to-end tests
    // ---------------------------------------------------------------------

    use tritond_store::{MemStore, NewSilo, NewTenant};

    fn make_placement_request(tenant: Uuid) -> PlacementRequest {
        let id = Uuid::new_v4();
        PlacementRequest {
            instance_id: id,
            silo_uuid: Uuid::nil(),
            tenant_uuid: tenant,
            project_uuid: Uuid::nil(),
            role: CnRoleView::Tenant,
            cpu_units: 100,
            ram_mb: 2_048,
            disk: BTreeMap::new(),
            required_traits: BTreeMap::new(),
            required_nic_tags: Vec::new(),
            required_underlay: UnderlayCapability {
                ipv4: true,
                ipv6: false,
            },
            required_devices: Vec::new(),
            needs_hvm: false,
            min_platform: None,
            affinity: tritond_store::InstanceAffinity::empty(id, tenant, Utc::now()),
            strategy_override: None,
            force_cn: None,
            ignore_scope_pin: false,
            deadline: Utc::now() + chrono::Duration::minutes(5),
            avoid_cn: Vec::new(),
            migration: None,
        }
    }

    async fn make_store_with_one_approved_cn() -> (Arc<dyn Store>, Uuid) {
        let mem = MemStore::new();
        let _silo = mem
            .create_silo(NewSilo {
                name: "s".into(),
                description: None,
            })
            .await
            .unwrap();
        let server_uuid = Uuid::new_v4();
        mem.register_cn(
            server_uuid,
            "cn-1".into(),
            None,
            serde_json::json!({}),
            Utc::now(),
        )
        .await
        .unwrap();
        // Force the CN into Approved by minting + attaching a key
        // -- the existing `approve_cn` flow needs a bound key, but
        // for the placement test we just need state == Approved
        // with a recent last_seen and a CnCapacity row.
        {
            let inner_field_writer = &mem;
            // Use the public approve_cn API.
            inner_field_writer
                .approve_cn(
                    server_uuid,
                    Uuid::new_v4(),
                    "pwd".into(),
                    [0u8; 32],
                    [0u8; 32],
                    [0u8; 32],
                    Utc::now(),
                )
                .await
                .unwrap();
        }
        // Publish capacity.
        mem.put_cn_capacity(tritond_store::CnCapacity {
            server_uuid,
            cpu_cores_physical: 8,
            cpu_threads_logical: 16,
            numa_nodes: vec![NumaNode {
                node_id: 0,
                cores: 8,
                ram_mb: 65_536,
            }],
            ram_total_mb: 65_536,
            ram_available_mb: 60_000,
            cpu_utilization_pct: 0.10,
            zpools: vec![ZpoolCapacity {
                name: "zones".into(),
                total_bytes: 1_000_000_000_000,
                free_bytes: 800_000_000_000,
                tier: StorageTier::Ssd,
            }],
            nic_tags: vec!["admin".into()],
            underlay: StoreUnderlay {
                ipv4: true,
                ipv6: false,
            },
            devices: Vec::new(),
            platform_version: "20260501T000000Z".into(),
            hvm_supported: true,
            reported_at: Utc::now(),
            vmm_protocol_version: None,
            cpu_features: Vec::new(),
            tsc_offset_ns: None,
            zpool_props: std::collections::BTreeMap::new(),
        })
        .await
        .unwrap();
        (Arc::new(mem) as Arc<dyn Store>, server_uuid)
    }

    #[tokio::test]
    async fn pick_picks_the_only_approved_cn_dry_run() {
        let (store, cn) = make_store_with_one_approved_cn().await;
        let req = make_placement_request(Uuid::new_v4());
        let out = pick(&store, req, Commit::No).await.unwrap();
        assert_eq!(out.chosen, Some(cn));
        assert!(out.committed.is_none());
        // No reservation should have landed.
        let reservations = store.list_cn_reservations(None).await.unwrap();
        assert!(reservations.is_empty());
    }

    #[tokio::test]
    async fn pick_no_eligible_cn_returns_explain_report() {
        // Empty fleet: pick must surface NoEligibleCn with a
        // report carrying zero per-CN rows when Commit::Yes is
        // asked (an empty fleet is a failure on the commit path).
        let store: Arc<dyn Store> = Arc::new(MemStore::new());
        let req = make_placement_request(Uuid::new_v4());
        match pick(
            &store,
            req,
            Commit::Yes {
                saga_id: Uuid::new_v4(),
                sec_id: Uuid::new_v4(),
                sec_epoch: 0,
            },
        )
        .await
        {
            Err(PickError::NoEligibleCn { report }) => {
                assert!(report.chosen.is_none());
                assert_eq!(report.per_cn.len(), 0);
            }
            other => panic!("expected NoEligibleCn, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn pick_no_eligible_cn_dry_run_returns_outcome_without_error() {
        // Commit::No must return a no-pick outcome instead of erroring
        // so the explain UI renders the empty fleet without a 5xx.
        let store: Arc<dyn Store> = Arc::new(MemStore::new());
        let req = make_placement_request(Uuid::new_v4());
        let out = pick(&store, req, Commit::No).await.unwrap();
        assert!(out.chosen.is_none());
        assert!(out.committed.is_none());
        assert_eq!(out.report.per_cn.len(), 0);
    }

    // End-to-end "pick + commit" coverage lives in integration tests
    // that wire the full create-instance tree; reserve_cn_capacity and
    // set_instance_host_cn are covered individually in tritond-store.
}
