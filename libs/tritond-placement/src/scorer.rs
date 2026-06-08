// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Built-in [`crate::Scorer`] implementations (RFD 00005 doc 02
//! §"The built-in scorers").
//!
//! Each scorer returns a normalised `0.0..=1.0` contribution; the
//! chain runner multiplies it by the resolved weight and sums
//! across scorers. Out-of-range returns are clamped (and logged)
//! by the runner; NaN is treated as `0.0`.
//!
//! ## Capacity-based (8)
//!
//! * [`ScoreRamHeadroom`]
//! * [`ScoreDiskHeadroom`]
//! * [`ScoreSpreadByFaultDomain`]
//! * [`ScorePackByFaultDomain`]
//! * [`ScoreAffinityPreferred`]
//! * [`ScorePlatformCurrent`]
//! * [`ScoreFewerCotenantZones`]
//! * [`ScoreUniformRandom`]
//!
//! ## CH-load-based (4)
//!
//! Each gates on `cn-load-summary.stale` -- when stale (or
//! absent), the contribution is `0.0` and the chain runner /
//! `ExplainReport` notes the skip.
//!
//! * [`ScoreAvoidHotNow`]
//! * [`ScoreAvoidPeaky`]
//! * [`ScorePreferLowBaseline`]
//! * [`ScoreDiurnalFit`] -- off by default (input signal rare)
//!
//! ## Strategy presets
//!
//! [`default_scorer_chain`] returns the registration order plus
//! the per-scorer `default_weight()`; [`strategy_overrides`]
//! returns the per-strategy weight overrides applied on top. The
//! engine builder (PL-5+) layers these in order:
//!
//! 1. Every scorer at its `default_weight()`.
//! 2. Strategy preset overrides ([`strategy_overrides`]).
//! 3. Operator overrides from `PlacementConfig::active_scorers`.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use tritond_store::AffinityScope;
use uuid::Uuid;

use crate::types::{ChainContext, CnView, PlacementRequest, Scorer, Strategy, StrategyWeights};

// ---------------------------------------------------------------------------
// 1. score-ram-headroom
// ---------------------------------------------------------------------------

pub struct ScoreRamHeadroom;
impl Scorer for ScoreRamHeadroom {
    fn name(&self) -> &'static str {
        "score-ram-headroom"
    }
    fn default_weight(&self) -> f32 {
        2.0
    }
    fn score(&self, cn: &CnView, req: &PlacementRequest, _ctx: &ChainContext) -> f32 {
        let Some(cap) = cn.capacity.as_ref() else {
            return 0.0;
        };
        if cap.ram_total_mb == 0 {
            return 0.0;
        }
        let reserved: u64 = cn.active_reservations.iter().map(|r| r.ram_mb).sum();
        let assigned: u64 = cn.assigned_instances.iter().map(|i| i.ram_mb).sum();
        let post = (cap.ram_total_mb)
            .saturating_sub(reserved)
            .saturating_sub(assigned)
            .saturating_sub(req.ram_mb) as f32;
        (post / cap.ram_total_mb as f32).clamp(0.0, 1.0)
    }
}

// ---------------------------------------------------------------------------
// 2. score-disk-headroom
//
// Per the RFD: "More free disk remains on the chosen pool". For
// requests that touch multiple pools, the score is the minimum
// across pools (the most-constrained pool's headroom).
// ---------------------------------------------------------------------------

pub struct ScoreDiskHeadroom;
impl Scorer for ScoreDiskHeadroom {
    fn name(&self) -> &'static str {
        "score-disk-headroom"
    }
    fn default_weight(&self) -> f32 {
        1.0
    }
    fn score(&self, cn: &CnView, req: &PlacementRequest, _ctx: &ChainContext) -> f32 {
        let Some(cap) = cn.capacity.as_ref() else {
            return 0.0;
        };
        if req.disk.is_empty() {
            return 0.5;
        }
        // Per-pool reservation pressure.
        let mut reserved_per_pool: std::collections::BTreeMap<&str, u64> =
            std::collections::BTreeMap::new();
        for r in &cn.active_reservations {
            for (pool, bytes) in &r.disk {
                *reserved_per_pool.entry(pool.as_str()).or_insert(0) += *bytes;
            }
        }
        let mut min_ratio = 1.0_f32;
        for (pool, want) in &req.disk {
            let Some(zpool) = cap.zpools.iter().find(|z| &z.name == pool) else {
                return 0.0;
            };
            if zpool.total_bytes == 0 {
                return 0.0;
            }
            let reserved = reserved_per_pool.get(pool.as_str()).copied().unwrap_or(0);
            let post = zpool
                .free_bytes
                .saturating_sub(reserved)
                .saturating_sub(*want) as f32;
            let ratio = (post / zpool.total_bytes as f32).clamp(0.0, 1.0);
            if ratio < min_ratio {
                min_ratio = ratio;
            }
        }
        min_ratio
    }
}

// ---------------------------------------------------------------------------
// 3. score-spread-by-fault-domain
//
// Higher when fewer tenant siblings already live in this CN's
// fault domain. Uses `ctx.sibling_instances` (tenant-scoped
// projection populated by the saga step at PL-5).
// ---------------------------------------------------------------------------

pub struct ScoreSpreadByFaultDomain;
impl Scorer for ScoreSpreadByFaultDomain {
    fn name(&self) -> &'static str {
        "score-spread-by-fault-domain"
    }
    fn default_weight(&self) -> f32 {
        1.5
    }
    fn score(&self, cn: &CnView, req: &PlacementRequest, ctx: &ChainContext) -> f32 {
        let Some(this_fd) = cn.placement.fault_domain.as_deref() else {
            // CN without a fault-domain tag is "alone" -- spread
            // is trivially satisfied.
            return 1.0;
        };
        let tenant = req.tenant_uuid;
        let mut tenant_total = 0_u32;
        let mut same_fd = 0_u32;
        for sib in ctx.sibling_instances {
            if sib.tenant_uuid != tenant {
                continue;
            }
            tenant_total += 1;
            if sib.host_fault_domain.as_deref() == Some(this_fd) {
                same_fd += 1;
            }
        }
        if tenant_total == 0 {
            return 1.0;
        }
        // 1 - (same_fd / tenant_total): all in same FD -> 0;
        // none in same FD -> 1.
        (1.0 - (same_fd as f32 / tenant_total as f32)).clamp(0.0, 1.0)
    }
}

// ---------------------------------------------------------------------------
// 4. score-pack-by-fault-domain (inverse of spread)
// ---------------------------------------------------------------------------

pub struct ScorePackByFaultDomain;
impl Scorer for ScorePackByFaultDomain {
    fn name(&self) -> &'static str {
        "score-pack-by-fault-domain"
    }
    fn default_weight(&self) -> f32 {
        0.0
    }
    fn score(&self, cn: &CnView, req: &PlacementRequest, ctx: &ChainContext) -> f32 {
        // 1 - spread = pack.
        1.0 - ScoreSpreadByFaultDomain.score(cn, req, ctx)
    }
}

// ---------------------------------------------------------------------------
// 5. score-affinity-preferred
//
// Counts soft-affinity rules satisfied by this CN, normalised by
// rule count. `Required` rules are the `cn-affinity-required`
// filter's job; this scorer only walks `Preferred`.
// ---------------------------------------------------------------------------

pub struct ScoreAffinityPreferred;
impl Scorer for ScoreAffinityPreferred {
    fn name(&self) -> &'static str {
        "score-affinity-preferred"
    }
    fn default_weight(&self) -> f32 {
        1.0
    }
    fn score(&self, cn: &CnView, req: &PlacementRequest, _ctx: &ChainContext) -> f32 {
        use tritond_store::{AffinityOp, AffinitySelector};
        let prefs: Vec<_> = req
            .affinity
            .rules
            .iter()
            .filter(|r| matches!(r.scope, AffinityScope::Preferred))
            .collect();
        if prefs.is_empty() {
            return 0.5;
        }
        let mut satisfied = 0_u32;
        for rule in &prefs {
            let ok = match (&rule.selector, rule.op) {
                (AffinitySelector::CnUuids(uuids), AffinityOp::In) => {
                    uuids.contains(&cn.server_uuid)
                }
                (AffinitySelector::CnUuids(uuids), AffinityOp::NotIn) => {
                    !uuids.contains(&cn.server_uuid)
                }
                (AffinitySelector::CnTraitMatch { key, value }, AffinityOp::In) => {
                    cn.placement.traits.get(key) == Some(value)
                }
                (AffinitySelector::CnTraitMatch { key, value }, AffinityOp::NotIn) => {
                    cn.placement.traits.get(key) != Some(value)
                }
                (AffinitySelector::InstanceIds(ids), AffinityOp::In) => {
                    let assigned: std::collections::BTreeSet<Uuid> = cn
                        .assigned_instances
                        .iter()
                        .map(|i| i.instance_id)
                        .collect();
                    ids.iter().any(|id| assigned.contains(id))
                }
                (AffinitySelector::InstanceIds(ids), AffinityOp::NotIn) => {
                    let assigned: std::collections::BTreeSet<Uuid> = cn
                        .assigned_instances
                        .iter()
                        .map(|i| i.instance_id)
                        .collect();
                    !ids.iter().any(|id| assigned.contains(id))
                }
                _ => continue,
            };
            if ok {
                satisfied += 1;
            }
        }
        (satisfied as f32 / prefs.len() as f32).clamp(0.0, 1.0)
    }
}

// ---------------------------------------------------------------------------
// 6. score-platform-current
//
// Higher when the CN's platform_version is closer to the fleet
// max. PL-4 ships a self-contained version: the scorer has no
// way to know the fleet max (the runner does, but doesn't pass
// it). v1 returns a constant 0.5; a future slice threads the
// fleet-max into `ChainContext` and lets this scorer compare.
// ---------------------------------------------------------------------------

pub struct ScorePlatformCurrent;
impl Scorer for ScorePlatformCurrent {
    fn name(&self) -> &'static str {
        "score-platform-current"
    }
    fn default_weight(&self) -> f32 {
        0.5
    }
    fn score(&self, cn: &CnView, _req: &PlacementRequest, _ctx: &ChainContext) -> f32 {
        // Stand-in semantic: a CN with a non-empty platform_version
        // gets 0.5; missing capacity gets 0.0. Real fleet-max
        // comparison lands when ChainContext carries it.
        match cn.capacity.as_ref() {
            Some(cap) if !cap.platform_version.is_empty() => 0.5,
            _ => 0.0,
        }
    }
}

// ---------------------------------------------------------------------------
// 7. score-fewer-cotenant-zones
//
// Higher when fewer instances of the same tenant (and, with a
// smaller weight, the same silo) already live on this CN.
// ---------------------------------------------------------------------------

pub struct ScoreFewerCotenantZones;
impl Scorer for ScoreFewerCotenantZones {
    fn name(&self) -> &'static str {
        "score-fewer-cotenant-zones"
    }
    fn default_weight(&self) -> f32 {
        0.5
    }
    fn score(&self, cn: &CnView, req: &PlacementRequest, _ctx: &ChainContext) -> f32 {
        if cn.assigned_instances.is_empty() {
            return 1.0;
        }
        let total = cn.assigned_instances.len() as f32;
        let same_tenant = cn
            .assigned_instances
            .iter()
            .filter(|i| i.tenant_uuid == req.tenant_uuid)
            .count() as f32;
        let same_silo = cn
            .assigned_instances
            .iter()
            .filter(|i| i.silo_uuid == req.silo_uuid && i.tenant_uuid != req.tenant_uuid)
            .count() as f32;
        // Tenant penalty weighted heavier than silo penalty.
        // Normalise by total assigned: a packed CN with all
        // same-tenant scores 0; an empty CN scores 1.
        let penalty = (same_tenant * 1.0 + same_silo * 0.5) / total;
        (1.0 - penalty).clamp(0.0, 1.0)
    }
}

// ---------------------------------------------------------------------------
// 8. score-uniform-random (deterministic tie-break)
//
// Hashes (request.instance_id, server_uuid) into a stable
// `0.0..=1.0` value. The seed is request-stable so a `--dry-run`
// and the real `designate` that follows return the same CN for
// the same inputs.
// ---------------------------------------------------------------------------

pub struct ScoreUniformRandom;
impl Scorer for ScoreUniformRandom {
    fn name(&self) -> &'static str {
        "score-uniform-random"
    }
    fn default_weight(&self) -> f32 {
        0.1
    }
    fn score(&self, cn: &CnView, req: &PlacementRequest, _ctx: &ChainContext) -> f32 {
        let mut hasher = DefaultHasher::new();
        req.instance_id.hash(&mut hasher);
        cn.server_uuid.hash(&mut hasher);
        let h = hasher.finish();
        // Map u64 to f32 in [0, 1). u64::MAX as f32 loses precision
        // -- normalise to [0, 1) by dividing the top 24 bits.
        let top = (h >> 40) as f32;
        let denom = (1u64 << 24) as f32;
        (top / denom).clamp(0.0, 1.0)
    }
}

// ---------------------------------------------------------------------------
// 9. score-avoid-hot-now (CH-load gated)
// ---------------------------------------------------------------------------

pub struct ScoreAvoidHotNow;
impl Scorer for ScoreAvoidHotNow {
    fn name(&self) -> &'static str {
        "score-avoid-hot-now"
    }
    fn default_weight(&self) -> f32 {
        1.5
    }
    fn score(&self, cn: &CnView, _req: &PlacementRequest, _ctx: &ChainContext) -> f32 {
        let Some(load) = cn.load_summary.as_ref() else {
            return 0.0;
        };
        if load.stale {
            return 0.0;
        }
        // Combine cpu_p95_5m and ram_used_p95_5m (both 0..1 in
        // the projection). Low values = "not hot" = high score.
        let busy =
            (load.cpu_p95_5m.max(0.0).min(1.0) + load.ram_used_p95_5m.max(0.0).min(1.0)) / 2.0;
        (1.0 - busy).clamp(0.0, 1.0)
    }
}

// ---------------------------------------------------------------------------
// 10. score-avoid-peaky
// ---------------------------------------------------------------------------

pub struct ScoreAvoidPeaky;
impl Scorer for ScoreAvoidPeaky {
    fn name(&self) -> &'static str {
        "score-avoid-peaky"
    }
    fn default_weight(&self) -> f32 {
        1.0
    }
    fn score(&self, cn: &CnView, _req: &PlacementRequest, _ctx: &ChainContext) -> f32 {
        let Some(load) = cn.load_summary.as_ref() else {
            return 0.0;
        };
        if load.stale {
            return 0.0;
        }
        (1.0 - load.cpu_p95_7d.clamp(0.0, 1.0)).clamp(0.0, 1.0)
    }
}

// ---------------------------------------------------------------------------
// 11. score-prefer-low-baseline
// ---------------------------------------------------------------------------

pub struct ScorePreferLowBaseline;
impl Scorer for ScorePreferLowBaseline {
    fn name(&self) -> &'static str {
        "score-prefer-low-baseline"
    }
    fn default_weight(&self) -> f32 {
        0.75
    }
    fn score(&self, cn: &CnView, _req: &PlacementRequest, _ctx: &ChainContext) -> f32 {
        let Some(load) = cn.load_summary.as_ref() else {
            return 0.0;
        };
        if load.stale {
            return 0.0;
        }
        (1.0 - load.cpu_p50_1d.clamp(0.0, 1.0)).clamp(0.0, 1.0)
    }
}

// ---------------------------------------------------------------------------
// 12. score-diurnal-fit (off by default)
//
// Per the RFD: "When request.affinity carries an expected-load
// hint (or a sibling reference), prefers CNs whose 24-hour quiet
// window overlaps the workload's busy window." The expected-load
// hint isn't modeled on the request yet; PL-4 ships the scorer
// as an always-zero stub that the operator can opt into via the
// chain config once the input signal is wired up.
// ---------------------------------------------------------------------------

pub struct ScoreDiurnalFit;
impl Scorer for ScoreDiurnalFit {
    fn name(&self) -> &'static str {
        "score-diurnal-fit"
    }
    fn default_weight(&self) -> f32 {
        0.0
    }
    fn score(&self, _cn: &CnView, _req: &PlacementRequest, _ctx: &ChainContext) -> f32 {
        0.0
    }
}

// ---------------------------------------------------------------------------
// Default chain + strategy presets
// ---------------------------------------------------------------------------

/// Default scorer registration order with each scorer's
/// `default_weight()`. The chain config (PL-7) lets operators
/// override the weights; the [`Strategy`] preset is applied on
/// top of these defaults by [`strategy_overrides`].
pub fn default_scorer_chain() -> Vec<(Arc<dyn Scorer>, f32)> {
    let entries: Vec<Arc<dyn Scorer>> = vec![
        Arc::new(ScoreRamHeadroom),
        Arc::new(ScoreDiskHeadroom),
        Arc::new(ScoreSpreadByFaultDomain),
        Arc::new(ScorePackByFaultDomain),
        Arc::new(ScoreAffinityPreferred),
        Arc::new(ScorePlatformCurrent),
        Arc::new(ScoreFewerCotenantZones),
        Arc::new(ScoreUniformRandom),
        Arc::new(ScoreAvoidHotNow),
        Arc::new(ScoreAvoidPeaky),
        Arc::new(ScorePreferLowBaseline),
        Arc::new(ScoreDiurnalFit),
    ];
    entries
        .into_iter()
        .map(|s| {
            let w = s.default_weight();
            (s, w)
        })
        .collect()
}

/// Resolve the per-strategy weight overrides applied on top of
/// each scorer's `default_weight()`. The map's keys are the
/// scorer names; absent scorers keep their default.
///
/// The structural shape mirrors [`crate::config::strategy_weights`]
/// (which was a structural-only PL-1 sketch); PL-4 fills in the
/// concrete scorer names.
pub fn strategy_overrides(strategy: Strategy) -> StrategyWeights {
    let mut w = StrategyWeights::new();
    match strategy {
        Strategy::Spread => {
            // Defaults already favour spread; explicitly disable
            // the pack scorer.
            w.set("score-pack-by-fault-domain", 0.0);
        }
        Strategy::Pack => {
            w.set("score-spread-by-fault-domain", 0.0);
            w.set("score-pack-by-fault-domain", 1.5);
            // Cotenant scorer also flips on Pack: we want to
            // bias TOWARD CNs with cotenants of the same tenant.
            w.set("score-fewer-cotenant-zones", 0.0);
        }
        Strategy::Balanced => {
            w.set("score-spread-by-fault-domain", 0.75);
            w.set("score-pack-by-fault-domain", 0.75);
        }
    }
    w
}

/// Build the resolved per-scorer weight map for [`Strategy`]:
/// every scorer at its `default_weight()`, then the strategy
/// overrides applied on top.
pub fn resolved_weights(strategy: Strategy) -> StrategyWeights {
    let mut w = StrategyWeights::new();
    for (scorer, _) in default_scorer_chain() {
        w.set(scorer.name(), scorer.default_weight());
    }
    for (name, weight) in &strategy_overrides(strategy).0 {
        w.set(*name, *weight);
    }
    w
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::OverprovisionDefaults;
    use crate::types::{
        AssignedInstanceView, CapacityView, CnLoadSummaryView, CnRoleView, CnStateView, CnView,
        NumaNodeView, PlacementPolicyView, PlacementRequest, ReservationView, SiblingInstanceView,
        StorageTier, UnderlayCapability, ZpoolView,
    };
    use chrono::{Duration, Utc};
    use std::collections::BTreeMap;
    use tritond_store::InstanceAffinity;

    fn nil() -> Uuid {
        Uuid::nil()
    }
    fn now() -> chrono::DateTime<Utc> {
        Utc::now()
    }

    fn make_cn() -> CnView {
        CnView {
            server_uuid: Uuid::new_v4(),
            hostname: "cn".into(),
            state: CnStateView::Approved,
            role: CnRoleView::Tenant,
            last_seen: Some(now()),
            capacity: Some(CapacityView {
                cpu_cores_physical: 16,
                cpu_threads_logical: 32,
                numa_nodes: vec![NumaNodeView {
                    node_id: 0,
                    cores: 16,
                    ram_mb: 65_536,
                }],
                ram_total_mb: 65_536,
                ram_available_mb: 60_000,
                cpu_utilization_pct: 0.10,
                zpools: vec![ZpoolView {
                    name: "zones".into(),
                    total_bytes: 1_000_000_000_000,
                    free_bytes: 800_000_000_000,
                    tier: StorageTier::Ssd,
                }],
                nic_tags: vec!["admin".into()],
                underlay: UnderlayCapability {
                    ipv4: true,
                    ipv6: false,
                },
                devices: Vec::new(),
                platform_version: "20260501T000000Z".into(),
                reported_at: now(),
                hvm_supported: true,
                vmm_protocol_version: None,
                cpu_features: Vec::new(),
                tsc_offset_ns: None,
                zpool_props: BTreeMap::new(),
            }),
            placement: PlacementPolicyView::default(),
            active_reservations: Vec::new(),
            load_summary: None,
            assigned_instances: Vec::new(),
        }
    }

    fn make_req() -> PlacementRequest {
        let id = Uuid::new_v4();
        PlacementRequest {
            instance_id: id,
            silo_uuid: nil(),
            tenant_uuid: nil(),
            project_uuid: nil(),
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
            affinity: InstanceAffinity::empty(id, nil(), now()),
            strategy_override: None,
            force_cn: None,
            ignore_scope_pin: false,
            deadline: now() + Duration::minutes(5),
            avoid_cn: Vec::new(),
            migration: None,
        }
    }

    fn make_ctx<'a>(
        weights: &'a StrategyWeights,
        siblings: &'a [SiblingInstanceView],
    ) -> ChainContext<'a> {
        ChainContext {
            now: now(),
            cluster_overprovision: OverprovisionDefaults::default(),
            load_staleness_secs: 180,
            agent_heartbeat_threshold_secs: 60,
            strategy_weights: weights,
            sibling_instances: siblings,
        }
    }

    // ---- score-ram-headroom ----

    #[test]
    fn score_ram_headroom_higher_on_empty_cn() {
        let cn = make_cn();
        let req = make_req();
        let w = StrategyWeights::new();
        let ctx = make_ctx(&w, &[]);
        let s = ScoreRamHeadroom.score(&cn, &req, &ctx);
        assert!(s > 0.9, "expected ~1.0 on empty CN, got {s}");
    }

    #[test]
    fn score_ram_headroom_lower_when_heavily_reserved() {
        let mut cn = make_cn();
        cn.active_reservations.push(ReservationView {
            saga_id: Uuid::new_v4(),
            instance_id: Uuid::new_v4(),
            cpu_units: 0,
            ram_mb: 60_000,
            disk: BTreeMap::new(),
            devices: Vec::new(),
            deadline: now(),
        });
        let req = make_req();
        let w = StrategyWeights::new();
        let ctx = make_ctx(&w, &[]);
        let s = ScoreRamHeadroom.score(&cn, &req, &ctx);
        assert!(s < 0.1, "expected very low score, got {s}");
    }

    // ---- score-disk-headroom ----

    #[test]
    fn score_disk_headroom_neutral_when_request_has_no_disk_ask() {
        let cn = make_cn();
        let req = make_req();
        let w = StrategyWeights::new();
        let ctx = make_ctx(&w, &[]);
        assert!((ScoreDiskHeadroom.score(&cn, &req, &ctx) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn score_disk_headroom_higher_on_emptier_pool() {
        let cn = make_cn(); // 800 GB free of 1 TB
        let mut req = make_req();
        req.disk.insert("zones".into(), 10_000_000_000); // 10 GB
        let w = StrategyWeights::new();
        let ctx = make_ctx(&w, &[]);
        let s = ScoreDiskHeadroom.score(&cn, &req, &ctx);
        assert!(s > 0.7);
    }

    // ---- score-spread-by-fault-domain ----

    #[test]
    fn score_spread_neutral_when_cn_has_no_fault_domain() {
        let cn = make_cn(); // fault_domain = None
        let req = make_req();
        let w = StrategyWeights::new();
        let ctx = make_ctx(&w, &[]);
        assert_eq!(ScoreSpreadByFaultDomain.score(&cn, &req, &ctx), 1.0);
    }

    #[test]
    fn score_spread_lower_when_tenant_already_in_same_fault_domain() {
        let mut cn = make_cn();
        cn.placement.fault_domain = Some("rack-3".into());
        let req = make_req();
        let siblings = vec![
            SiblingInstanceView {
                instance_id: Uuid::new_v4(),
                silo_uuid: nil(),
                tenant_uuid: req.tenant_uuid,
                project_uuid: nil(),
                host_cn_uuid: Some(Uuid::new_v4()),
                host_fault_domain: Some("rack-3".into()),
            },
            SiblingInstanceView {
                instance_id: Uuid::new_v4(),
                silo_uuid: nil(),
                tenant_uuid: req.tenant_uuid,
                project_uuid: nil(),
                host_cn_uuid: Some(Uuid::new_v4()),
                host_fault_domain: Some("rack-3".into()),
            },
        ];
        let w = StrategyWeights::new();
        let ctx = make_ctx(&w, &siblings);
        let s = ScoreSpreadByFaultDomain.score(&cn, &req, &ctx);
        // Both tenant siblings in rack-3 -> 1 - 2/2 = 0.
        assert!(s < 1e-6, "expected ~0, got {s}");
    }

    #[test]
    fn score_spread_higher_when_tenant_in_other_fault_domains() {
        let mut cn = make_cn();
        cn.placement.fault_domain = Some("rack-3".into());
        let req = make_req();
        let siblings = vec![SiblingInstanceView {
            instance_id: Uuid::new_v4(),
            silo_uuid: nil(),
            tenant_uuid: req.tenant_uuid,
            project_uuid: nil(),
            host_cn_uuid: Some(Uuid::new_v4()),
            host_fault_domain: Some("rack-7".into()),
        }];
        let w = StrategyWeights::new();
        let ctx = make_ctx(&w, &siblings);
        assert!((ScoreSpreadByFaultDomain.score(&cn, &req, &ctx) - 1.0).abs() < 1e-6);
    }

    // ---- score-pack-by-fault-domain ----

    #[test]
    fn score_pack_is_inverse_of_spread() {
        let mut cn = make_cn();
        cn.placement.fault_domain = Some("rack-3".into());
        let req = make_req();
        let siblings = vec![SiblingInstanceView {
            instance_id: Uuid::new_v4(),
            silo_uuid: nil(),
            tenant_uuid: req.tenant_uuid,
            project_uuid: nil(),
            host_cn_uuid: Some(Uuid::new_v4()),
            host_fault_domain: Some("rack-3".into()),
        }];
        let w = StrategyWeights::new();
        let ctx = make_ctx(&w, &siblings);
        let spread = ScoreSpreadByFaultDomain.score(&cn, &req, &ctx);
        let pack = ScorePackByFaultDomain.score(&cn, &req, &ctx);
        assert!((spread + pack - 1.0).abs() < 1e-6);
    }

    // ---- score-affinity-preferred ----

    #[test]
    fn score_affinity_preferred_neutral_when_no_preferred_rules() {
        let cn = make_cn();
        let req = make_req();
        let w = StrategyWeights::new();
        let ctx = make_ctx(&w, &[]);
        assert!((ScoreAffinityPreferred.score(&cn, &req, &ctx) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn score_affinity_preferred_max_when_all_satisfied() {
        use tritond_store::{AffinityKind, AffinityOp as Op, AffinityRule, AffinitySelector};
        let cn = make_cn();
        let mut req = make_req();
        req.affinity.rules.push(AffinityRule {
            kind: AffinityKind::VmToHost,
            scope: AffinityScope::Preferred,
            op: Op::In,
            selector: AffinitySelector::CnUuids(vec![cn.server_uuid]),
        });
        let w = StrategyWeights::new();
        let ctx = make_ctx(&w, &[]);
        assert!((ScoreAffinityPreferred.score(&cn, &req, &ctx) - 1.0).abs() < 1e-6);
    }

    // ---- score-platform-current ----

    #[test]
    fn score_platform_current_zero_on_missing_capacity() {
        let mut cn = make_cn();
        cn.capacity = None;
        let req = make_req();
        let w = StrategyWeights::new();
        let ctx = make_ctx(&w, &[]);
        assert_eq!(ScorePlatformCurrent.score(&cn, &req, &ctx), 0.0);
    }

    // ---- score-fewer-cotenant-zones ----

    #[test]
    fn score_fewer_cotenants_max_on_empty_cn() {
        let cn = make_cn(); // no assigned instances
        let req = make_req();
        let w = StrategyWeights::new();
        let ctx = make_ctx(&w, &[]);
        assert_eq!(ScoreFewerCotenantZones.score(&cn, &req, &ctx), 1.0);
    }

    #[test]
    fn score_fewer_cotenants_drops_with_tenant_siblings_on_cn() {
        let mut cn = make_cn();
        let req = make_req();
        cn.assigned_instances.push(AssignedInstanceView {
            instance_id: Uuid::new_v4(),
            silo_uuid: nil(),
            tenant_uuid: req.tenant_uuid,
            cpu_units: 0,
            ram_mb: 0,
        });
        let w = StrategyWeights::new();
        let ctx = make_ctx(&w, &[]);
        let s = ScoreFewerCotenantZones.score(&cn, &req, &ctx);
        // 1 assigned, all same-tenant -> penalty 1.0 -> score 0.
        assert!(s.abs() < 1e-6);
    }

    // ---- score-uniform-random ----

    #[test]
    fn score_uniform_random_is_stable_for_same_inputs() {
        let cn = make_cn();
        let req = make_req();
        let w = StrategyWeights::new();
        let ctx = make_ctx(&w, &[]);
        let a = ScoreUniformRandom.score(&cn, &req, &ctx);
        let b = ScoreUniformRandom.score(&cn, &req, &ctx);
        assert_eq!(a, b);
        assert!((0.0..=1.0).contains(&a));
    }

    #[test]
    fn score_uniform_random_differs_across_cns_for_one_request() {
        let cn_a = make_cn();
        let cn_b = make_cn();
        let req = make_req();
        let w = StrategyWeights::new();
        let ctx = make_ctx(&w, &[]);
        let a = ScoreUniformRandom.score(&cn_a, &req, &ctx);
        let b = ScoreUniformRandom.score(&cn_b, &req, &ctx);
        // Different server_uuids: extremely likely to differ.
        assert!(
            a != b || a == 0.0 || a == 1.0,
            "two CNs hashed to same value"
        );
    }

    // ---- CH-load scorers ----

    fn load(stale: bool, cpu_p95_5m: f32, cpu_p95_7d: f32, cpu_p50_1d: f32) -> CnLoadSummaryView {
        CnLoadSummaryView {
            last_refreshed_at: now(),
            stale,
            cpu_p50_5m: 0.0,
            cpu_p95_5m,
            cpu_p50_1d,
            cpu_p95_1d: 0.0,
            cpu_p95_7d,
            ram_used_p95_5m: 0.0,
            nic_tx_bps_p95_5m: 0,
            nic_rx_bps_p95_5m: 0,
        }
    }

    #[test]
    fn ch_load_scorers_return_zero_when_summary_absent() {
        let cn = make_cn(); // load_summary None
        let req = make_req();
        let w = StrategyWeights::new();
        let ctx = make_ctx(&w, &[]);
        assert_eq!(ScoreAvoidHotNow.score(&cn, &req, &ctx), 0.0);
        assert_eq!(ScoreAvoidPeaky.score(&cn, &req, &ctx), 0.0);
        assert_eq!(ScorePreferLowBaseline.score(&cn, &req, &ctx), 0.0);
        assert_eq!(ScoreDiurnalFit.score(&cn, &req, &ctx), 0.0);
    }

    #[test]
    fn ch_load_scorers_return_zero_when_summary_is_stale() {
        let mut cn = make_cn();
        cn.load_summary = Some(load(true, 0.10, 0.10, 0.10));
        let req = make_req();
        let w = StrategyWeights::new();
        let ctx = make_ctx(&w, &[]);
        assert_eq!(ScoreAvoidHotNow.score(&cn, &req, &ctx), 0.0);
        assert_eq!(ScoreAvoidPeaky.score(&cn, &req, &ctx), 0.0);
        assert_eq!(ScorePreferLowBaseline.score(&cn, &req, &ctx), 0.0);
    }

    #[test]
    fn score_avoid_hot_now_higher_when_quiet() {
        let mut cn = make_cn();
        cn.load_summary = Some(load(false, 0.10, 0.0, 0.0));
        let req = make_req();
        let w = StrategyWeights::new();
        let ctx = make_ctx(&w, &[]);
        let s = ScoreAvoidHotNow.score(&cn, &req, &ctx);
        assert!(s > 0.9);
    }

    #[test]
    fn score_avoid_peaky_lower_when_weekly_peak_high() {
        let mut cn = make_cn();
        cn.load_summary = Some(load(false, 0.10, 0.95, 0.10));
        let req = make_req();
        let w = StrategyWeights::new();
        let ctx = make_ctx(&w, &[]);
        let s = ScoreAvoidPeaky.score(&cn, &req, &ctx);
        assert!(s < 0.1);
    }

    #[test]
    fn score_prefer_low_baseline_lower_when_24h_median_high() {
        let mut cn = make_cn();
        cn.load_summary = Some(load(false, 0.10, 0.0, 0.85));
        let req = make_req();
        let w = StrategyWeights::new();
        let ctx = make_ctx(&w, &[]);
        let s = ScorePreferLowBaseline.score(&cn, &req, &ctx);
        assert!(s < 0.2);
    }

    // ---- strategy presets ----

    #[test]
    fn strategy_spread_disables_pack_scorer() {
        let w = strategy_overrides(Strategy::Spread);
        assert_eq!(w.get("score-pack-by-fault-domain"), Some(0.0));
    }

    #[test]
    fn strategy_pack_flips_spread_and_pack_weights() {
        let w = strategy_overrides(Strategy::Pack);
        assert_eq!(w.get("score-spread-by-fault-domain"), Some(0.0));
        assert_eq!(w.get("score-pack-by-fault-domain"), Some(1.5));
    }

    #[test]
    fn strategy_balanced_half_and_half() {
        let w = strategy_overrides(Strategy::Balanced);
        assert_eq!(w.get("score-spread-by-fault-domain"), Some(0.75));
        assert_eq!(w.get("score-pack-by-fault-domain"), Some(0.75));
    }

    #[test]
    fn resolved_weights_layers_defaults_then_overrides() {
        let w = resolved_weights(Strategy::Spread);
        // Defaults preserved for unaffected scorers.
        assert_eq!(w.get("score-ram-headroom"), Some(2.0));
        // Overridden by strategy.
        assert_eq!(w.get("score-pack-by-fault-domain"), Some(0.0));
    }

    // ---- default chain integration ----

    #[test]
    fn default_scorer_chain_has_twelve_entries() {
        assert_eq!(default_scorer_chain().len(), 12);
    }
}
