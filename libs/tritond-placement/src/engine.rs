// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! The chain runner and the [`ExplainReport`].
//!
//! PL-3 replaces PL-1's "first eligible" baseline with a
//! purely-filter-driven model: an empty runner accepts every CN,
//! and a runner with the default-chain ([`crate::filter::default_chain`])
//! registered enforces the seventeen built-ins from RFD 00005
//! doc 02. Scorers (PL-4) layer on top. The trait surface and the
//! [`ExplainReport`] shape PL-1 froze are unchanged.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::types::{ChainContext, CnView, Filter, PlacementRequest, Scorer, Strategy, Verdict};

/// The placement engine. Owned by `tritond`; rebuilt on every
/// cluster-settings change.
pub struct ChainRunner {
    /// Hard-filter chain; runs in registration order. The runner
    /// records every filter's verdict per CN so the
    /// [`ExplainReport`] is complete even if an early filter
    /// rejected the CN.
    filters: Vec<Arc<dyn Filter>>,

    /// Soft-score chain. Each tuple is the scorer plus its resolved
    /// weight (after strategy preset + operator overrides).
    scorers: Vec<(Arc<dyn Scorer>, f32)>,

    /// The strategy preset this runner was built for. Surfaces in
    /// the [`ExplainReport`].
    strategy: Strategy,
}

impl ChainRunner {
    /// Build an empty runner. PL-3 + PL-4 add `with_filter` /
    /// `with_scorer` builders alongside; PL-1 callers test the
    /// passthrough by constructing one with no filters or scorers.
    pub fn empty(strategy: Strategy) -> Self {
        Self {
            filters: Vec::new(),
            scorers: Vec::new(),
            strategy,
        }
    }

    /// Register a filter. Order is significant — filters run in the
    /// order they were registered, and the chain config's
    /// `active_filters` list pins that order at load time.
    pub fn with_filter(mut self, filter: Arc<dyn Filter>) -> Self {
        self.filters.push(filter);
        self
    }

    /// Register a scorer with the resolved weight. Order is not
    /// significant for the score sum but is preserved so the
    /// `ExplainReport`'s `scorer_results` is deterministic.
    pub fn with_scorer(mut self, scorer: Arc<dyn Scorer>, weight: f32) -> Self {
        self.scorers.push((scorer, weight));
        self
    }

    /// Run the chain over `cns` and return the chosen CN plus the
    /// `ExplainReport`. Returns `None` for the choice when no CN
    /// passed every filter; the report still names every CN with
    /// its reject reasons.
    ///
    /// **Force-place semantics (D-Pl-8):** when `req.force_cn` is
    /// `Some(cn)`, the chain is restricted to the scope-pin filter
    /// (`cn-scope-match`) only -- every other filter is skipped
    /// from the chain for that pick. The reservation is still
    /// taken in the same FDB transaction by the saga action at
    /// PL-5; this runner enforces only the filter-side of the
    /// invariant. Filters not in the force-place set produce no
    /// `ExplainReport` entry on the force path (the report names
    /// the bypass via a synthetic `force-place` entry instead).
    pub fn pick(
        &self,
        cns: &[CnView],
        req: &PlacementRequest,
        ctx: &ChainContext<'_>,
    ) -> (Option<Uuid>, ExplainReport) {
        let started = Instant::now();
        let mut per_cn: Vec<ExplainPerCn> = Vec::with_capacity(cns.len());
        let force = req.force_cn;

        // Filter phase.
        for cn in cns {
            let mut filter_results: Vec<FilterVerdict> = Vec::with_capacity(self.filters.len());
            let mut accepted = true;

            // Force-place narrows the chain to the scope-pin
            // filter only (D-Pl-8). Every other filter is skipped
            // and the report names it.
            if let Some(forced_cn) = force {
                if forced_cn != cn.server_uuid {
                    accepted = false;
                    filter_results.push(FilterVerdict::new(
                        "force-place",
                        Verdict::reject(format!(
                            "force_cn=Some({forced_cn}) targets a different CN"
                        )),
                    ));
                } else {
                    filter_results.push(FilterVerdict::new("force-place", Verdict::Accept));
                }
            }

            for f in &self.filters {
                if force.is_some() && f.name() != "cn-scope-match" {
                    // Other filters are bypassed on the force-place
                    // path; the report notes the skip.
                    filter_results.push(FilterVerdict::new(f.name(), Verdict::Skip));
                    continue;
                }
                let v = f.evaluate(cn, req, ctx);
                if matches!(v, Verdict::Reject { .. }) {
                    accepted = false;
                }
                filter_results.push(FilterVerdict::new(f.name(), v));
            }

            per_cn.push(ExplainPerCn {
                server_uuid: cn.server_uuid,
                filter_results,
                scorer_results: Vec::new(),
                total_score: None,
                load_summary_stale: cn.load_summary.as_ref().map(|s| s.stale).unwrap_or(true),
                capacity_present: cn.capacity.is_some(),
                accepted,
            });
        }

        // Score phase. Only CNs that passed every filter contribute.
        for entry in per_cn.iter_mut() {
            if !entry.accepted {
                continue;
            }
            let cn = match cns.iter().find(|c| c.server_uuid == entry.server_uuid) {
                Some(c) => c,
                None => continue,
            };
            let mut sum = 0.0_f32;
            for (scorer, weight) in &self.scorers {
                let raw = scorer.score(cn, req, ctx);
                let normalised = if raw.is_nan() {
                    0.0
                } else {
                    raw.clamp(0.0, 1.0)
                };
                let contribution = normalised * weight;
                sum += contribution;
                entry.scorer_results.push(ScorerContribution {
                    name: scorer.name().to_string(),
                    raw: normalised,
                    weight: *weight,
                    contribution,
                });
            }
            entry.total_score = Some(sum);
        }

        // Choose: highest total_score wins; ties broken by ascending
        // server_uuid (deterministic). When no scorers are
        // registered (PL-1), every accepted CN ties at zero and the
        // uuid sort produces a stable first-eligible pick.
        let mut chosen: Option<Uuid> = None;
        let mut best_score: Option<f32> = None;
        let mut sorted: Vec<&ExplainPerCn> = per_cn.iter().filter(|e| e.accepted).collect();
        sorted.sort_by(|a, b| a.server_uuid.cmp(&b.server_uuid));
        for entry in sorted {
            let s = entry.total_score.unwrap_or(0.0);
            match best_score {
                None => {
                    best_score = Some(s);
                    chosen = Some(entry.server_uuid);
                }
                Some(bs) if s > bs => {
                    best_score = Some(s);
                    chosen = Some(entry.server_uuid);
                }
                _ => {}
            }
        }

        let report = ExplainReport {
            request: req.clone(),
            strategy: self.strategy,
            weights: ctx
                .strategy_weights
                .to_report()
                .into_iter()
                .collect::<BTreeMap<String, f32>>(),
            per_cn,
            chosen,
            elapsed: started.elapsed(),
            generated_at: ctx.now,
        };
        (chosen, report)
    }
}

/// What `pick` returns alongside the chosen CN. Every filter
/// rejection and every scorer contribution lands here — D-Pl-10
/// makes this the engine's primary output, not a debug afterthought.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExplainReport {
    pub request: PlacementRequest,
    pub strategy: Strategy,

    /// Resolved weight vector applied to the score phase. Keys are
    /// scorer `name()`s.
    pub weights: BTreeMap<String, f32>,

    pub per_cn: Vec<ExplainPerCn>,

    /// `None` when no CN passed every filter.
    pub chosen: Option<Uuid>,

    pub elapsed: Duration,
    pub generated_at: DateTime<Utc>,
}

impl ExplainReport {
    /// Bounded projection for the audit log row. RFD 00005 invariant
    /// 4 requires the audit row stay bounded — the projection drops
    /// per-CN verdicts that didn't contribute to a reject and
    /// truncates per-scorer contributions to the winning CN.
    pub fn bounded_for_audit(&self) -> AuditExplain {
        let chosen_breakdown = self
            .chosen
            .and_then(|uuid| {
                self.per_cn
                    .iter()
                    .find(|e| e.server_uuid == uuid)
                    .map(|e| e.scorer_results.clone())
            })
            .unwrap_or_default();

        let mut reject_counts: BTreeMap<String, u32> = BTreeMap::new();
        for entry in &self.per_cn {
            for fv in &entry.filter_results {
                if matches!(fv.verdict, Verdict::Reject { .. }) {
                    *reject_counts.entry(fv.filter.to_string()).or_default() += 1;
                }
            }
        }

        AuditExplain {
            chosen: self.chosen,
            chosen_breakdown,
            reject_counts,
            strategy: self.strategy,
            generated_at: self.generated_at,
        }
    }
}

/// The audit-log projection of an [`ExplainReport`]. Bounded by
/// design so the audit chain stays compact (RFD 00005 invariant 4).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AuditExplain {
    pub chosen: Option<Uuid>,
    pub chosen_breakdown: Vec<ScorerContribution>,

    /// Number of CNs each filter rejected. Cheap to scan even when
    /// the fleet is large.
    pub reject_counts: BTreeMap<String, u32>,

    pub strategy: Strategy,
    pub generated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExplainPerCn {
    pub server_uuid: Uuid,

    pub filter_results: Vec<FilterVerdict>,

    /// Populated only for CNs that passed every filter.
    #[serde(default)]
    pub scorer_results: Vec<ScorerContribution>,

    /// `None` when filtered out.
    #[serde(default)]
    pub total_score: Option<f32>,

    pub load_summary_stale: bool,
    pub capacity_present: bool,

    /// Whether every filter accepted. Cached so consumers don't
    /// re-scan `filter_results`.
    pub accepted: bool,
}

/// One filter's verdict on one CN, as it appears in the
/// [`ExplainReport`]. The filter name is stored as an owned
/// `String` so the report is freely (de)serialisable.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FilterVerdict {
    pub filter: String,
    pub verdict: Verdict,
}

impl FilterVerdict {
    pub fn new(filter: impl Into<String>, verdict: Verdict) -> Self {
        Self {
            filter: filter.into(),
            verdict,
        }
    }
}

/// Per-scorer breakdown on one CN. Surfaces in both the full
/// [`ExplainReport`] and the bounded [`AuditExplain`] projection.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScorerContribution {
    pub name: String,

    /// The clamped-and-NaN-scrubbed value the scorer returned.
    pub raw: f32,

    /// Resolved weight at chain build time.
    pub weight: f32,

    /// `raw × weight`. Cached so the audit reader doesn't have to
    /// re-multiply.
    pub contribution: f32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::OverprovisionDefaults;
    use crate::types::{
        CapacityView, CnRoleView, CnStateView, NumaNodeView, PlacementPolicyView, StorageTier,
        StrategyWeights, UnderlayCapability, ZpoolView,
    };
    use std::collections::BTreeMap;

    fn nil_uuid() -> Uuid {
        Uuid::nil()
    }

    fn make_cn(server_uuid: Uuid, state: CnStateView, capacity: bool) -> CnView {
        CnView {
            server_uuid,
            hostname: format!("cn-{}", server_uuid.simple()),
            state,
            role: CnRoleView::Tenant,
            last_seen: Some(Utc::now()),
            capacity: if capacity {
                Some(CapacityView {
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
                    nic_tags: vec!["admin".into(), "external".into()],
                    underlay: UnderlayCapability {
                        ipv4: true,
                        ipv6: false,
                    },
                    devices: Vec::new(),
                    platform_version: "20260501T000000Z".into(),
                    reported_at: Utc::now(),
                    hvm_supported: true,
                    vmm_protocol_version: None,
                    cpu_features: Vec::new(),
                    tsc_offset_ns: None,
                    zpool_props: BTreeMap::new(),
                })
            } else {
                None
            },
            placement: PlacementPolicyView::default(),
            active_reservations: Vec::new(),
            load_summary: None,
            assigned_instances: Vec::new(),
        }
    }

    fn make_request() -> PlacementRequest {
        let instance_id = Uuid::new_v4();
        PlacementRequest {
            instance_id,
            silo_uuid: nil_uuid(),
            tenant_uuid: nil_uuid(),
            project_uuid: nil_uuid(),
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
            affinity: tritond_store::InstanceAffinity::empty(instance_id, nil_uuid(), Utc::now()),
            strategy_override: None,
            force_cn: None,
            ignore_scope_pin: false,
            deadline: Utc::now() + chrono::Duration::minutes(5),
            avoid_cn: Vec::new(),
            migration: None,
        }
    }

    fn default_runner() -> ChainRunner {
        let mut r = ChainRunner::empty(Strategy::Spread);
        for f in crate::filter::default_filter_chain() {
            r = r.with_filter(f);
        }
        r
    }

    #[test]
    fn default_chain_picks_first_eligible_cn() {
        let weights = StrategyWeights::new();
        let ctx = ChainContext {
            now: Utc::now(),
            cluster_overprovision: OverprovisionDefaults::default(),
            load_staleness_secs: 180,
            agent_heartbeat_threshold_secs: 60,
            strategy_weights: &weights,
            sibling_instances: &[],
        };
        let runner = default_runner();

        let a = Uuid::parse_str("11111111-1111-4111-8111-111111111111").unwrap();
        let b = Uuid::parse_str("22222222-2222-4222-8222-222222222222").unwrap();
        let c = Uuid::parse_str("33333333-3333-4333-8333-333333333333").unwrap();
        let cns = vec![
            make_cn(b, CnStateView::Approved, true),
            make_cn(a, CnStateView::Pending, true), // rejected by cn-approved-and-live
            make_cn(c, CnStateView::Approved, false), // rejected by cn-capacity-present
        ];

        let req = make_request();
        let (chosen, report) = runner.pick(&cns, &req, &ctx);

        // Only `b` survives the default chain (`a` pending,
        // `c` no capacity row).
        assert_eq!(chosen, Some(b));
        assert_eq!(report.per_cn.len(), 3);

        let by_id: BTreeMap<Uuid, &ExplainPerCn> =
            report.per_cn.iter().map(|e| (e.server_uuid, e)).collect();
        assert!(by_id[&b].accepted);
        assert!(!by_id[&a].accepted);
        assert!(!by_id[&c].accepted);
        assert!(by_id[&c].filter_results.iter().any(|fv| matches!(
            fv.verdict,
            Verdict::Reject { ref reason } if reason.contains("cn-capacity row absent")
        )));
        assert_eq!(report.strategy, Strategy::Spread);
    }

    #[test]
    fn no_eligible_cn_returns_none() {
        let weights = StrategyWeights::new();
        let ctx = ChainContext {
            now: Utc::now(),
            cluster_overprovision: OverprovisionDefaults::default(),
            load_staleness_secs: 180,
            agent_heartbeat_threshold_secs: 60,
            strategy_weights: &weights,
            sibling_instances: &[],
        };
        let runner = default_runner();

        let cns = vec![make_cn(Uuid::new_v4(), CnStateView::Pending, true)];
        let req = make_request();

        let (chosen, report) = runner.pick(&cns, &req, &ctx);
        assert!(chosen.is_none());
        assert_eq!(report.per_cn.len(), 1);
        assert!(!report.per_cn[0].accepted);
    }

    #[test]
    fn bounded_for_audit_summarises_reject_counts() {
        let weights = StrategyWeights::new();
        let ctx = ChainContext {
            now: Utc::now(),
            cluster_overprovision: OverprovisionDefaults::default(),
            load_staleness_secs: 180,
            agent_heartbeat_threshold_secs: 60,
            strategy_weights: &weights,
            sibling_instances: &[],
        };
        let runner = default_runner();
        let cns = vec![
            make_cn(Uuid::new_v4(), CnStateView::Pending, true),
            make_cn(Uuid::new_v4(), CnStateView::Pending, false),
        ];
        let (chosen, report) = runner.pick(&cns, &make_request(), &ctx);
        assert!(chosen.is_none());
        let audit = report.bounded_for_audit();
        // Both CNs are pending, so both get rejected by
        // `cn-approved-and-live` -- the audit projection
        // collapses that into one entry with count 2.
        assert_eq!(audit.reject_counts.get("cn-approved-and-live"), Some(&2));
        assert!(audit.chosen.is_none());
        assert!(audit.chosen_breakdown.is_empty());
    }
}
