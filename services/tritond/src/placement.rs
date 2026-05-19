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

use tritond_placement::types::DeviceReservation as PlacementDeviceReservation;
use tritond_placement::{
    AssignedInstanceView, CapacityView, ChainContext, ChainRunner, CnLoadSummaryView, CnRoleView,
    CnStateView, CnView, DeviceKind as PlacementDeviceKind, DeviceView, NumaNodeView,
    OverprovisionDefaults, PlacementPolicyView, PlacementRequest, ReservationView,
    SiblingInstanceView, Strategy, StrategyWeights, UnderlayCapability, ZpoolView,
    default_filter_chain, default_scorer_chain, resolved_weights,
};
use tritond_store::{
    CnPickSnapshot, CnRole, CnState, DeviceKind as StoreDeviceKind, StorageTier,
    TenantInstanceProjection,
};

/// Default heartbeat-staleness threshold for the
/// `cn-approved-and-live` filter. PL-7 will move this to the
/// FDB-backed cluster settings shape; PL-5 hardcodes the value the
/// existing agent heartbeater (~5 s tick) is sized against.
pub const DEFAULT_AGENT_HEARTBEAT_THRESHOLD_SECS: u64 = 60;

/// Default load-summary staleness threshold (3 ticks at the
/// default 60 s interval, per RFD 00005 doc 02 §"The load
/// materialiser"). PL-7 moves this to cluster settings; PL-5
/// hardcodes.
pub const DEFAULT_LOAD_STALENESS_SECS: u64 = 180;

/// Convert a [`CnPickSnapshot`] from the store to a
/// [`CnView`] the placement engine consumes.
pub fn snapshot_to_cn_view(snap: CnPickSnapshot) -> CnView {
    let capacity = snap.capacity.map(capacity_view);
    let placement = placement_policy_view(snap.placement);
    let active_reservations = snap
        .reservations
        .into_iter()
        .map(reservation_view)
        .collect();
    let load_summary = snap.load_summary.map(load_summary_view);
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

/// Build a [`ChainRunner`] with the default filter chain and the
/// default scorer chain, with weights resolved against `strategy`.
///
/// PL-5 ships this as the only way to construct a runner; PL-7
/// adds an `active_filters` / `active_scorers` configuration
/// surface that lets operators rearrange the chain via cluster
/// settings.
pub fn build_default_runner(strategy: Strategy) -> ChainRunner {
    let mut runner = ChainRunner::empty(strategy);
    for filter in default_filter_chain() {
        runner = runner.with_filter(filter);
    }
    let weights = resolved_weights(strategy);
    for (scorer, _default) in default_scorer_chain() {
        let name = scorer.name();
        let w = weights.get(name).unwrap_or_else(|| scorer.default_weight());
        runner = runner.with_scorer(scorer, w);
    }
    runner
}

/// Build a [`ChainContext`] suitable for the default chain.
///
/// `strategy_weights` and `sibling_instances` are borrows; the
/// caller owns the lifetime. PL-5's saga action constructs the
/// `StrategyWeights` from [`resolved_weights`] and the sibling
/// slice from [`projection_to_sibling_view`].
pub fn build_chain_context<'a>(
    now: chrono::DateTime<chrono::Utc>,
    strategy_weights: &'a StrategyWeights,
    sibling_instances: &'a [SiblingInstanceView],
) -> ChainContext<'a> {
    ChainContext {
        now,
        cluster_overprovision: OverprovisionDefaults::default(),
        load_staleness_secs: DEFAULT_LOAD_STALENESS_SECS,
        agent_heartbeat_threshold_secs: DEFAULT_AGENT_HEARTBEAT_THRESHOLD_SECS,
        strategy_weights,
        sibling_instances,
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

fn load_summary_view(s: tritond_store::CnLoadSummary) -> CnLoadSummaryView {
    CnLoadSummaryView {
        last_refreshed_at: s.last_refreshed_at,
        stale: s.stale,
        cpu_p50_5m: s.cpu_p50_5m,
        cpu_p95_5m: s.cpu_p95_5m,
        cpu_p50_1d: s.cpu_p50_1d,
        cpu_p95_1d: s.cpu_p95_1d,
        cpu_p95_7d: s.cpu_p95_7d,
        ram_used_p95_5m: (s.ram_used_p95_5m as f32) / 1.0e9, // bytes -> ~normalised; better
        // ratio when the engine also has
        // `ram_total_mb`. PL-5 ships the
        // projection; PL-7 refines.
        nic_tx_bps_p95_5m: s.nic_tx_bps_p95_5m,
        nic_rx_bps_p95_5m: s.nic_rx_bps_p95_5m,
    }
}

fn assigned_instance_view(i: tritond_store::Instance) -> AssignedInstanceView {
    AssignedInstanceView {
        instance_id: i.id,
        // The Instance row carries `tenant_id` and `project_id`;
        // it does NOT carry `silo_id` directly. The placement
        // engine's cotenant scorer reads `silo_uuid` to weight
        // "same silo, different tenant" lower. For PL-5b we set
        // silo to the nil UUID -- this de-weights the silo
        // dimension of the cotenant penalty (it's still computed
        // but with a constant value). PL-5c will join the silo
        // via project -> tenant -> silo when the action wires
        // through `instance-create`.
        silo_uuid: silo_for_tenant_placeholder(i.tenant_id),
        tenant_uuid: i.tenant_id,
        cpu_units: (i.cpu as u32) * 100,
        ram_mb: i.memory_bytes / 1_048_576,
    }
}

/// Placeholder: returns the nil UUID. The full silo join lands in
/// PL-5c via `Store::get_project(p).silo_id` (transitively, since
/// projects carry the silo id). PL-5b accepts the de-weighting in
/// the cotenant scorer documented in [`assigned_instance_view`].
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
        Cn, CnCapacity, CnPlacement, CnReservation, DeviceCapacity, NumaNode, StorageTier,
        UnderlayCapability as StoreUnderlay, ZpoolCapacity,
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
        let view = snapshot_to_cn_view(snap);
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

    #[test]
    fn build_default_runner_lays_down_the_full_chain() {
        let runner = build_default_runner(Strategy::Spread);
        let weights = resolved_weights(Strategy::Spread);
        let ctx = build_chain_context(Utc::now(), &weights, &[]);
        let view = snapshot_to_cn_view(make_snapshot());
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
        };
        let (chosen, report) = runner.pick(&[view.clone()], &req, &ctx);
        assert_eq!(chosen, Some(view.server_uuid));
        // 18 filters in the default chain + 12 scorers.
        assert_eq!(report.per_cn[0].filter_results.len(), 18);
        assert_eq!(report.per_cn[0].scorer_results.len(), 12);
    }
}
