// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Integration tests for the chain runner end-to-end -- the
//! `default_filter_chain()` + `default_scorer_chain()` wiring,
//! plus the strategy preset's effect on the chosen CN when the
//! relevant scorers are the only tie-breaker.

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::{Duration, Utc};
use tritond_placement::filter::CnApprovedAndLive;
use tritond_placement::scorer::{
    ScorePackByFaultDomain, ScoreSpreadByFaultDomain, ScoreUniformRandom, resolved_weights,
};
use tritond_placement::types::StorageTier;
use tritond_placement::{
    AssignedInstanceView, CapacityView, ChainContext, ChainRunner, CnRoleView, CnStateView, CnView,
    NumaNodeView, OverprovisionDefaults, PlacementPolicyView, PlacementRequest, Scorer,
    SiblingInstanceView, Strategy, StrategyWeights, UnderlayCapability, ZpoolView,
    default_filter_chain, default_scorer_chain,
};
use tritond_store::InstanceAffinity;
use uuid::Uuid;

fn nil() -> Uuid {
    Uuid::nil()
}

fn now() -> chrono::DateTime<Utc> {
    Utc::now()
}

fn make_cn(uuid: Uuid, fault_domain: Option<&str>) -> CnView {
    let mut policy = PlacementPolicyView::default();
    policy.fault_domain = fault_domain.map(str::to_string);
    CnView {
        server_uuid: uuid,
        hostname: format!("cn-{}", uuid.simple()),
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
        placement: policy,
        active_reservations: Vec::new(),
        load_summary: None,
        assigned_instances: Vec::new(),
    }
}

fn make_req(tenant: Uuid) -> PlacementRequest {
    let id = Uuid::new_v4();
    PlacementRequest {
        instance_id: id,
        silo_uuid: nil(),
        tenant_uuid: tenant,
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
        affinity: InstanceAffinity::empty(id, tenant, now()),
        strategy_override: None,
        force_cn: None,
        ignore_scope_pin: false,
        deadline: now() + Duration::minutes(5),
        avoid_cn: Vec::new(),
        migration: None,
    }
}

fn build_runner(strategy: Strategy) -> ChainRunner {
    let mut runner = ChainRunner::empty(strategy);
    for f in default_filter_chain() {
        runner = runner.with_filter(f);
    }
    let weights = resolved_weights(strategy);
    for (scorer, _default) in default_scorer_chain() {
        let name = scorer.name();
        let w = weights.get(name).unwrap_or_else(|| scorer.default_weight());
        runner = runner.with_scorer(scorer, w);
    }
    runner
}

#[test]
fn default_chain_picks_an_eligible_cn() {
    let runner = build_runner(Strategy::Spread);
    let cn_a = make_cn(
        Uuid::parse_str("11111111-1111-4111-8111-111111111111").unwrap(),
        Some("rack-a"),
    );
    let cn_b = make_cn(
        Uuid::parse_str("22222222-2222-4222-8222-222222222222").unwrap(),
        Some("rack-b"),
    );
    let weights = resolved_weights(Strategy::Spread);
    let req = make_req(nil());
    let ctx = ChainContext {
        now: now(),
        cluster_overprovision: OverprovisionDefaults::default(),
        load_staleness_secs: 180,
        agent_heartbeat_threshold_secs: 60,
        strategy_weights: &weights,
        sibling_instances: &[],
    };
    let (chosen, _report) = runner.pick(&[cn_a.clone(), cn_b.clone()], &req, &ctx);
    assert!(matches!(chosen, Some(uuid) if uuid == cn_a.server_uuid || uuid == cn_b.server_uuid));
}

#[test]
fn spread_vs_pack_pick_opposite_cns_when_only_fault_domain_breaks_tie() {
    // Setup: two CNs in different fault domains. The tenant has
    // one existing sibling in rack-a. Spread should prefer rack-b;
    // Pack should prefer rack-a. Everything else is equal.
    let tenant = Uuid::new_v4();
    let cn_a = make_cn(
        Uuid::parse_str("aaaaaaaa-1111-4111-8111-111111111111").unwrap(),
        Some("rack-a"),
    );
    let cn_b = make_cn(
        Uuid::parse_str("bbbbbbbb-2222-4222-8222-222222222222").unwrap(),
        Some("rack-b"),
    );

    let siblings = vec![SiblingInstanceView {
        instance_id: Uuid::new_v4(),
        silo_uuid: nil(),
        tenant_uuid: tenant,
        project_uuid: nil(),
        host_cn_uuid: Some(Uuid::new_v4()),
        host_fault_domain: Some("rack-a".into()),
    }];

    // Use a minimal runner with just the filter that lets both
    // CNs through, the spread / pack scorers, and a low-weight
    // uniform-random for determinism. We avoid the full default
    // scorer chain because score-platform-current would tie at
    // 0.5 on both, but other scorers we don't care about
    // (score-uniform-random tie-break) could bias toward one of
    // them when the two strategy-relevant scorers are equal --
    // we want a clean spread-vs-pack signal.
    let req = make_req(tenant);

    // Build Spread runner.
    let weights_spread = resolved_weights(Strategy::Spread);
    let mut runner_spread = ChainRunner::empty(Strategy::Spread);
    runner_spread = runner_spread.with_filter(Arc::new(CnApprovedAndLive));
    let w_spread_for = |name: &str| weights_spread.get(name).unwrap_or(0.0);
    runner_spread = runner_spread.with_scorer(
        Arc::new(ScoreSpreadByFaultDomain),
        w_spread_for("score-spread-by-fault-domain"),
    );
    runner_spread = runner_spread.with_scorer(
        Arc::new(ScorePackByFaultDomain),
        w_spread_for("score-pack-by-fault-domain"),
    );
    runner_spread = runner_spread.with_scorer(Arc::new(ScoreUniformRandom), 0.001);

    // Build Pack runner.
    let weights_pack = resolved_weights(Strategy::Pack);
    let mut runner_pack = ChainRunner::empty(Strategy::Pack);
    runner_pack = runner_pack.with_filter(Arc::new(CnApprovedAndLive));
    let w_pack_for = |name: &str| weights_pack.get(name).unwrap_or(0.0);
    runner_pack = runner_pack.with_scorer(
        Arc::new(ScoreSpreadByFaultDomain),
        w_pack_for("score-spread-by-fault-domain"),
    );
    runner_pack = runner_pack.with_scorer(
        Arc::new(ScorePackByFaultDomain),
        w_pack_for("score-pack-by-fault-domain"),
    );
    runner_pack = runner_pack.with_scorer(Arc::new(ScoreUniformRandom), 0.001);

    let ctx_spread = ChainContext {
        now: now(),
        cluster_overprovision: OverprovisionDefaults::default(),
        load_staleness_secs: 180,
        agent_heartbeat_threshold_secs: 60,
        strategy_weights: &weights_spread,
        sibling_instances: &siblings,
    };
    let ctx_pack = ChainContext {
        now: now(),
        cluster_overprovision: OverprovisionDefaults::default(),
        load_staleness_secs: 180,
        agent_heartbeat_threshold_secs: 60,
        strategy_weights: &weights_pack,
        sibling_instances: &siblings,
    };

    let (chosen_spread, _) = runner_spread.pick(&[cn_a.clone(), cn_b.clone()], &req, &ctx_spread);
    let (chosen_pack, _) = runner_pack.pick(&[cn_a.clone(), cn_b.clone()], &req, &ctx_pack);

    // Spread: tenant already in rack-a -> prefer rack-b.
    assert_eq!(chosen_spread, Some(cn_b.server_uuid));
    // Pack: prefer the fault-domain that already has the tenant -> rack-a.
    assert_eq!(chosen_pack, Some(cn_a.server_uuid));
}

#[test]
fn explain_report_per_cn_carries_filter_and_scorer_breakdown() {
    let runner = build_runner(Strategy::Spread);
    let cn = make_cn(Uuid::new_v4(), Some("rack-a"));
    let weights = resolved_weights(Strategy::Spread);
    let req = make_req(nil());
    let ctx = ChainContext {
        now: now(),
        cluster_overprovision: OverprovisionDefaults::default(),
        load_staleness_secs: 180,
        agent_heartbeat_threshold_secs: 60,
        strategy_weights: &weights,
        sibling_instances: &[],
    };
    let (chosen, report) = runner.pick(&[cn.clone()], &req, &ctx);
    assert_eq!(chosen, Some(cn.server_uuid));
    assert_eq!(report.per_cn.len(), 1);
    let entry = &report.per_cn[0];
    assert!(entry.accepted);
    // Default chain ships 23 filters (18 RFD-00005 + 5 LM-0
    // migration filters); every one ran.
    assert_eq!(entry.filter_results.len(), 23);
    // Default scorer chain has 11 entries; all 11 contributions land.
    assert_eq!(entry.scorer_results.len(), 11);
    // Total score is the sum of contributions.
    let sum: f32 = entry.scorer_results.iter().map(|c| c.contribution).sum();
    let total = entry.total_score.unwrap_or(0.0);
    assert!((sum - total).abs() < 1e-3);
    // Strategy is recorded.
    assert_eq!(report.strategy, Strategy::Spread);
    // Bounded audit projection shape.
    let audit = report.bounded_for_audit();
    assert_eq!(audit.chosen, Some(cn.server_uuid));
    assert_eq!(audit.chosen_breakdown.len(), 11);
}

#[test]
fn assigned_instance_view_field_is_accessible() {
    // Compile-time check that AssignedInstanceView is reachable
    // from the integration crate.
    let _ = AssignedInstanceView {
        instance_id: Uuid::new_v4(),
        silo_uuid: nil(),
        tenant_uuid: nil(),
        cpu_units: 100,
        ram_mb: 1024,
    };
    let _ = StrategyWeights::new();
}
