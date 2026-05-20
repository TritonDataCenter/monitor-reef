# RFD 00005 · Doc 02 — The pipeline: filters, scorers, the `designate` saga action, the load materialiser

> Slices PL-3 (filters + chain runner + `ExplainReport`), PL-4 (scorers
> + strategies), PL-5 (the `designate` saga action + the agent
> capacity reporter), PL-6 (the load materialiser) implement this
> document. Companion code: `monitor-reef/libs/tritond-placement/src/`,
> `monitor-reef/services/tritond/src/sagas/designate.rs`,
> `monitor-reef/services/tritonagent/src/capacity_reporter.rs`.

The engine is two phases (filter, then score) over the `CnView` /
`PlacementRequest` shapes from doc 01, plus a small set of supporting
machinery: the chain runner, the `ExplainReport`, the strategy preset
weight vectors, the `designate` saga action that wraps `pick` in one
FDB transaction, and the load materialiser that turns ClickHouse
metrics into the `cn-load-summary` rows scorers read.

## The traits

```rust
// tritond-placement/src/types.rs
pub trait Filter: Send + Sync {
    /// kebab-case stable id; used in chain config and ExplainReport.
    fn name(&self) -> &'static str;

    /// Pure function over (CnView, PlacementRequest, ChainContext).
    fn evaluate(&self, cn: &CnView, req: &PlacementRequest, ctx: &ChainContext) -> Verdict;
}

pub trait Scorer: Send + Sync {
    fn name(&self) -> &'static str;

    /// Default weight when this scorer is registered without an explicit weight override.
    fn default_weight(&self) -> f32;

    /// Pure function. Return a normalised 0.0..=1.0 contribution; the chain runner multiplies by
    /// the configured weight and sums. Out-of-range returns are clamped (and logged).
    fn score(&self, cn: &CnView, req: &PlacementRequest, ctx: &ChainContext) -> f32;
}

pub enum Verdict {
    Accept,
    Reject { reason: String },
    Skip,                                  // filter doesn't apply to this CN/request — neither accept nor reject; ExplainReport notes it
}

pub struct ChainContext<'a> {
    pub now:                 chrono::DateTime<chrono::Utc>,
    pub cluster_overprovision: OverprovisionDefaults,    // cluster-default ratios when CnPlacement leaves them None
    pub load_staleness_secs: u64,                        // for the load-summary stale check
    pub strategy_weights:    &'a StrategyWeights,        // resolved weights for the active strategy
    pub sibling_instances:   &'a [Instance],             // every tenant-scoped Instance, for spread / cotenant scorers
}

pub struct StrategyWeights(pub std::collections::BTreeMap<&'static str, f32>);

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Strategy { Spread, Pack, Balanced }
```

`ChainContext` is the small bag of "things every filter and scorer
needs but that don't belong on `CnView` or `PlacementRequest`" — the
clock, the cluster default overprovision ratios, the resolved weights
the strategy presets compile to, and the cross-CN sibling-instance
slice the spread scorers need (every instance owned by the requesting
tenant or silo, used to compute "how many of my instances already
live on this fault domain").

## The chain runner

```rust
// tritond-placement/src/engine.rs
pub struct ChainRunner {
    filters: Vec<Arc<dyn Filter>>,
    scorers: Vec<(Arc<dyn Scorer>, f32)>,                // scorer + resolved weight (after strategy preset overrides)
}

pub struct ExplainReport {
    pub request:        PlacementRequest,
    pub strategy:       Strategy,
    pub weights:        std::collections::BTreeMap<&'static str, f32>,
    pub per_cn:         Vec<ExplainPerCn>,
    pub chosen:         Option<Uuid>,
    pub elapsed:        std::time::Duration,
    pub generated_at:   chrono::DateTime<chrono::Utc>,
}

pub struct ExplainPerCn {
    pub server_uuid:    Uuid,
    pub filter_results: Vec<(/* filter name */ &'static str, Verdict)>,
    pub scorer_results: Vec<ScorerContribution>,         // only populated for CNs that passed every filter
    pub total_score:    Option<f32>,                     // None when filtered out
    pub load_summary_stale: bool,
    pub capacity_present:   bool,
}

pub struct ScorerContribution {
    pub name:         &'static str,
    pub raw:          f32,           // the 0..1 the scorer returned
    pub weight:       f32,           // the resolved weight at chain time
    pub contribution: f32,           // raw × weight
}
```

`ChainRunner::pick(&self, cns: &[CnView], req: &PlacementRequest, ctx: &ChainContext)`
returns `(Option<Uuid>, ExplainReport)` — the `Option` is `None` when
no CN passed every filter (the report still names every CN with its
reject reasons). The runner is the only place that knows about the
loop; filters and scorers are pure.

**Tie-break.** When two CNs have identical `total_score` within an
epsilon (1e-6), the deterministic `score-uniform-random` scorer
(seeded with `(request.instance_id, server_uuid)`) breaks the tie.
The seed is request-stable, so a `--dry-run` and the real `designate`
that follows return the same CN for the same inputs.

**Errors inside a filter or scorer.** A trait impl that panics is a
bug; the runner catches panics with `std::panic::catch_unwind` and
records a synthetic `Reject { reason: "scorer panicked: …" }` so one
buggy scorer cannot break the whole pick. A scorer that returns NaN
is treated as 0.0 with a log.

## The seventeen built-in filters

| Name | Rejects when | Notes |
|---|---|---|
| `cn-approved-and-live` | `cn.state != Approved` *or* `cn.last_seen` is older than the agent heartbeat threshold | The current bin-packer already checks this; the filter is the formal home. |
| `cn-role-matches` | `cn.role` doesn't include the request's role need (tenant / edge / both) | Edge sagas (NAT gateway) ask for `CnRole::Edge`; tenant sagas ask for `CnRole::Tenant`. |
| `cn-not-reserved` | `placement.reserved == true` *and* `request.force_cn` is not this CN | Operator out-of-service flag. Force-place still works. |
| `cn-not-cordoned` | `placement.cordoned == true` | Drain-only — existing instances stay, new placements skip. |
| `cn-scope-match` | (`placement.pinned_silo_uuid` is `Some(s)` and `s != request.silo_uuid`) *or* (`placement.pinned_tenant_uuid` is `Some(t)` and `t != request.tenant_uuid`); bypassed only if `request.ignore_scope_pin && request.force_cn == Some(cn.server_uuid)` | D-Pl-5. The bypass requires *both* the request flag and the force-place; an `ignore_scope_pin` without a force is a programming error and is rejected at the API edge. |
| `cn-platform-min` | `capacity.platform_version` is older than `request.min_platform` | Matches legacy DAPI `min_platform`. |
| `cn-traits-required` | any key in `request.required_traits` is missing from `placement.traits` or carries a different value | Equality match per key. |
| `cn-nic-tags` | any tag in `request.required_nic_tags` is missing from `capacity.nic_tags` | Set membership per tag. |
| `cn-underlay` | `capacity.underlay` does not satisfy `request.required_underlay` (e.g. v6 needed, only v4 available) | Edge NAT gateway needs IPv6. |
| `cn-zpool-has-space` | for every pool in `request.disk`, no zpool in `capacity.zpools` has enough free after reservations and after the cluster overprovision ratio | Per-pool, not aggregate. A request that asks for "any pool with 50 GB" passes if any one pool fits; a request that asks for a specific pool must hit that pool. |
| `cn-ram-available` | `request.ram_mb` exceeds `(capacity.ram_total_mb / overprovision_ram) - reserved_ram - assigned_ram` | The overprovision divisor is `placement.overprovision_ram` if set, else cluster default. |
| `cn-cpu-available` | `request.cpu_units` exceeds `(capacity.cpu_threads_logical * 100 / overprovision_cpu) - reserved_cpu - assigned_cpu` | 1 vCPU = 100 cpu_units, matching legacy DAPI's `cpu_cap` convention. |
| `cn-numa-fits` | request requires NUMA pinning *and* no single `capacity.numa_nodes[i]` has enough free cores+RAM after subtracting same-NUMA-node consumption | Only runs when `request.affinity` carries a NUMA constraint or when the package shape declares one (future package field). |
| `cn-device-available` | for any `(model, count)` in `request.required_devices`, no `capacity.devices[i]` of the right model has `free_count - reserved_count >= count` | GPU model + SR-IOV VF model are the v1 device kinds. |
| `cn-hvm-supported` | `request.needs_hvm == true` and `capacity` does not report an HVM-capable CPU | Bhyve / KVM brands need HVM. |
| `cn-affinity-required` | any `request.affinity.rules` with `scope == Required` is not satisfiable on this CN — `In` rules with no matching sibling on this CN, `NotIn` rules with a matching sibling on this CN, `Required` topology spread that would exceed `max_skew` | The hard half of the affinity story; soft rules are scored by `score-affinity-preferred`. |
| `cn-not-evacuating` | a drain operation is in progress on this CN (a future field on `cn-placement.cordoned_reason = "drain"` set by `tcadm cn drain`) | Until drain lands (PL-7), this filter is effectively `cn-not-cordoned`. |

The opt-in filter `cn-load-not-overheating` is shipped but not in the
default chain (D-Pl-9). When operators enable it via
`tcadm placement config set --chain ...,cn-load-not-overheating,...`,
its threshold reads `placement.load_filter.cpu_p95_5m_max` from
cluster settings (default 0.90).

## The built-in scorers

Each returns a normalised 0.0..=1.0 contribution. The chain runner
multiplies by the resolved weight and sums.

### Capacity-based (8)

| Name | Default weight | Returns higher when |
|---|---|---|
| `score-ram-headroom` | 2.0 | More free RAM remains after this provision (free / total). |
| `score-disk-headroom` | 1.0 | More free disk remains on the chosen pool (free / total). |
| `score-spread-by-fault-domain` | 1.5 | Fewer instances of `request.tenant_uuid` already live in this CN's fault domain. |
| `score-pack-by-fault-domain` | 0.0 | (Inverse of the spread scorer; `Pack` strategy turns this on.) |
| `score-affinity-preferred` | 1.0 | Each preferred affinity rule (vm-to-vm, vm-to-host) satisfied by this CN; normalised by rule count. |
| `score-platform-current` | 0.5 | Higher `capacity.platform_version` relative to the fleet max. |
| `score-fewer-cotenant-zones` | 0.5 | Fewer instances belonging to the same tenant (high weight) or same silo (lower weight) already live on this CN. Weighted by scope distance (same tenant > same silo > unrelated). |
| `score-uniform-random` | 0.1 | Deterministic per `(request.instance_id, server_uuid)`; tie-break only. |

### CH-load-based (4)

Each gates on `cn-load-summary.stale` — when stale, returns 0.0 and
the `ExplainReport` notes the skip.

| Name | Default weight | Returns higher when |
|---|---|---|
| `score-avoid-hot-now` | 1.5 | `cpu_p95_5m` and `ram_used_p95_5m` (normalised by capacity) are *low*. The "free on paper, slammed in reality" guardrail. |
| `score-avoid-peaky` | 1.0 | `cpu_p95_7d` is low. Avoids landing next to a noisy neighbour's recurring weekly peak even if the CN is quiet right now. |
| `score-prefer-low-baseline` | 0.75 | `cpu_p50_1d` is low. Picks the genuinely under-used CN over the merely currently-idle one. |
| `score-diurnal-fit` | 0.0 | (Off by default.) When `request.affinity` carries an expected-load hint (or a sibling reference), prefers CNs whose 24-hour quiet window overlaps the workload's busy window. Opt-in because the input signal is rarely present. |

### Strategy presets

```rust
// tritond-placement/src/config.rs
fn strategy_weights(strategy: Strategy) -> StrategyWeights {
    use Strategy::*;
    let mut w = StrategyWeights::default();        // every scorer at default_weight()
    match strategy {
        Spread => {
            // defaults already favour spread (score-spread-by-fault-domain at 1.5);
            // turn off the pack scorer explicitly.
            w.set("score-pack-by-fault-domain", 0.0);
        }
        Pack => {
            w.set("score-spread-by-fault-domain", 0.0);
            w.set("score-pack-by-fault-domain",   1.5);
            // load-history scorers still on — packing into a CN that's already at p95 90% is wrong.
        }
        Balanced => {
            w.set("score-spread-by-fault-domain", 0.75);
            w.set("score-pack-by-fault-domain",   0.75);
        }
    }
    w
}
```

The strategy is one preset weight vector applied on top of the same
chain. `tcadm placement strategy set` overrides individual weights;
the resulting `StrategyWeights` is what every `pick` uses until the
next setting change.

## The chain config

```rust
// tritond-placement/src/config.rs
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct PlacementConfig {
    pub active_filters: Vec<String>,               // kebab-case filter names in evaluation order
    pub active_scorers: Vec<ScorerConfig>,
    pub strategy:       Strategy,                  // default strategy for requests that don't override
    pub overprovision:  OverprovisionDefaults,     // cluster-default ratios
    pub materialiser:   MaterialiserConfig,
    pub updated_at:     chrono::DateTime<chrono::Utc>,
    pub updated_by:     tritond_audit::Principal,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ScorerConfig { pub name: String, pub weight: f32 }

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct MaterialiserConfig {
    pub interval_seconds:   u32,    // default 60
    pub staleness_ticks:    u32,    // default 3
    pub clickhouse_url:     String, // defaults to the metrics zone URL
    pub min_samples_5m:     u32,    // default 3
    pub min_samples_1d:     u32,    // default 12
    pub min_samples_7d:     u32,    // default 24
}
```

`PlacementConfig` lives in the FDB-backed cluster settings (same
substrate `tcadm config` and adminui Settings already use). Loading
it builds the `ChainRunner` (looking up each named filter / scorer in
the registry); a setting change rebuilds the runner without
restarting `tritond`. An active name that is not in the registry is
a hard error at load time — the load fails loudly, the setting
change is rejected, and `tcadm placement config set` returns the
unknown name in the error.

## The `designate` saga action

Registered in `services/tritond/src/sagas/designate.rs`; one of the
catalog entries listed in RFD 00004 doc 02.

```rust
// services/tritond/src/sagas/designate.rs
declare_saga_actions! {
    designate;
    DESIGNATE -> "host_cn_uuid" {
        + sd_designate
        - sd_undesignate
    }
}

async fn sd_designate(action_ctx: SagaActionContext) -> Result<Uuid, ActionError> {
    let saga_ctx = action_ctx.user_data();
    let params: DesignateParams = action_ctx.saga_params()?;
    let req_ctx = saga_ctx.request_ctx(action_ctx.saga_id());

    // Single FDB transaction: load CnViews, run the chain, insert the reservation, write host_cn_uuid.
    let chain = saga_ctx.placement.runner_for(params.strategy_override.unwrap_or(saga_ctx.placement.default_strategy())).await?;
    let outcome = saga_ctx.store
        .designate_in_txn(req_ctx, |store_txn| async move {
            let cns = store_txn.list_cn_uuids_for_role(params.role).await?;
            let mut views = Vec::with_capacity(cns.len());
            for cn in cns { views.push(store_txn.get_cn_view_for_pick(cn).await?); }
            let request = build_placement_request(&params, &saga_ctx.store, &store_txn).await?;
            let (chosen, report) = chain.pick(&views, &request, &saga_ctx.chain_context().await?);
            let cn = chosen.ok_or_else(|| StoreError::CapacityExhausted { explain: report.clone() })?;
            store_txn.reserve_cn_capacity(req_ctx, build_reservation(cn, &request, params.deadline)).await?;
            store_txn.set_instance_host_cn(req_ctx, request.instance_id, cn).await?;
            store_txn.put_instance_affinity(req_ctx, request.affinity.clone()).await?;
            Ok((cn, report))
        })
        .await
        .map_err(action_error_from_store)?;

    saga_ctx.audit.emit_designate(&params, &outcome.0, &outcome.1.bounded_for_audit()).await;
    saga_ctx.metrics.observe_pick(&outcome.1);
    Ok(outcome.0)
}

async fn sd_undesignate(action_ctx: SagaActionContext) -> Result<(), ActionError> {
    let saga_ctx = action_ctx.user_data();
    let params:   DesignateParams = action_ctx.saga_params()?;
    let cn:       Uuid            = action_ctx.lookup("host_cn_uuid")?;
    let req_ctx = saga_ctx.request_ctx(action_ctx.saga_id());

    saga_ctx.store.release_cn_reservation(req_ctx, cn, action_ctx.saga_id()).await
        .or_else(swallow_not_found)?;
    saga_ctx.store.clear_instance_host_cn(req_ctx, params.instance_id).await
        .or_else(swallow_not_found)?;
    saga_ctx.audit.emit_undesignate(&params, &cn).await;
    Ok(())
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct DesignateParams {
    pub instance_id:         Uuid,
    pub silo_uuid:           Uuid,
    pub tenant_uuid:         Uuid,
    pub project_uuid:        Uuid,
    pub role:                CnRole,
    pub cpu_units:           u32,
    pub ram_mb:              u64,
    pub disk:                std::collections::BTreeMap<String, u64>,
    pub required_traits:     std::collections::BTreeMap<String, String>,
    pub required_nic_tags:   Vec<String>,
    pub required_underlay:   UnderlayCapability,
    pub required_devices:    Vec<DeviceReservation>,
    pub needs_hvm:           bool,
    pub min_platform:        Option<String>,
    pub affinity:            InstanceAffinity,
    pub strategy_override:   Option<Strategy>,
    pub fault_domain_spread: bool,
    pub force_cn:            Option<Uuid>,
    pub ignore_scope_pin:    bool,
    pub deadline:            chrono::DateTime<chrono::Utc>,
}
```

**Force-place is the same action with `force_cn: Some(uuid)`.** The
action body checks for `force_cn` first; if set, it skips the chain,
loads the one `CnView`, runs *only* the scope-pin filter (unless
`ignore_scope_pin` is also set with the force), and on accept writes
the reservation row exactly as the automatic path does. The audit
row carries `force_cn: true` so the operator-initiated pick is
distinguishable from an automatic one (D-Pl-8).

**The single-transaction shape is the `designate_in_txn` helper.**
The store exposes a closure-based transaction surface so the read
phase (every `CnView`) and the write phase (the reservation + the
two `Instance` field updates) share one FDB read version. The
closure body cannot perform arbitrary `tokio::spawn` work; it can
only call the `StoreTxn` methods, and the store retries the closure
on a `not_committed` (1020) error. This is the same shape the
existing single-resource CAS helpers in `tritond-store` use, just
generalised to multi-key reads + multi-key writes.

**The reservation row's resources are computed from
`PlacementRequest` once, before the closure runs**, so the closure
body is cheap to retry. A retry re-reads every `CnView` (FDB MVCC
sees the latest), re-runs the chain, and picks again — the *same*
CN may not win the second time around, and that is correct (the
reservation that was already inserted by the previous attempt is
not visible to its own retry because the write was rolled back, but
*other* concurrent writers' reservations are).

## The load materialiser

A leader-elected background task in `tritond` that turns ClickHouse
metrics into `cn-load-summary` rows.

```rust
// tritond-placement/src/load_materializer.rs
pub struct LoadMaterialiser {
    pub store:        Arc<dyn Store>,
    pub clickhouse:   ClickHouseClient,                    // the existing CH client the metrics pipeline uses; reused
    pub config:       MaterialiserConfig,
    pub leader:       Arc<dyn ClusterLeader>,              // the existing tritond cluster-leader primitive
    pub metrics:      Arc<PlacementMetrics>,
    pub log:          tracing::Span,
}

impl LoadMaterialiser {
    pub async fn run(self, cancel: tokio_util::sync::CancellationToken) {
        let mut tick = tokio::time::interval(Duration::from_secs(self.config.interval_seconds.into()));
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = tick.tick() => {
                    if !self.leader.am_leader().await { continue; }   // non-leaders do nothing
                    if let Err(e) = self.tick_once().await {
                        tracing::warn!(error = %e, "load materialiser tick failed");
                        self.metrics.materialiser_failures.inc();
                    }
                }
            }
        }
    }

    async fn tick_once(&self) -> Result<(), MaterialiserError> {
        let start = std::time::Instant::now();
        let cns = self.store.list_cn_capacities().await?;
        let summaries = self.clickhouse.query_per_cn_summaries(&self.config, &cns).await?;
        for s in summaries {
            self.store.put_cn_load_summary(SagaRequestCtx::handler(), s).await?;
        }
        self.metrics.materialiser_seconds.observe(start.elapsed().as_secs_f64());
        Ok(())
    }
}
```

**The ClickHouse query shape.** One `SELECT … GROUP BY server_uuid`
per metric per window, ranged on the right time bucket; the
materialiser stitches the results into `CnLoadSummary` rows. The
exact SQL lives in `ClickHouseClient::query_per_cn_summaries` and
matches the schema the existing metrics pipeline writes; the
materialiser does not introduce a new schema.

**Staleness.** Each `CnLoadSummary` carries
`last_refreshed_at + stale`. The `stale` flag is set when (a) the
last refresh is older than `staleness_ticks × interval_seconds`,
(b) the per-window sample count is below `min_samples_*`, or (c)
the ClickHouse query for this CN returned an error. The materialiser
still writes the row with `stale = true` so the heatmap can show
"data says nothing" distinctly from "no data exists".

**Leader election: open question.** The README flags this; the
default is to share the SEC lease (cheap, but couples to the
saga engine) — decide at PL-6 once the SEC's lease interface is
concrete. The materialiser's correctness does not depend on the
choice: if two peers think they're the leader for one tick, they
write the same summary twice and FDB MVCC makes one of them a no-op.

**Metrics.**

| Metric | Type | Labels | Meaning |
|---|---|---|---|
| `tritond_placement_load_materializer_seconds` | histogram | — | Per-tick wall time. |
| `tritond_placement_load_summary_stale_total` | counter | — | Incremented per CN whose tick produced a `stale = true` row. |
| `tritond_placement_pick_seconds` | histogram | `outcome=success|capacity_exhausted|force` | Per-pick wall time. |
| `tritond_placement_filter_reject_total` | counter | `filter=<name>` | Which filter is starving the fleet. |
| `tritond_placement_score_chosen` | counter | `scorer=<name>` | Sum of contributions per scorer on winning picks; helps tune weights. |
| `tritond_placement_reservations_active` | gauge | — | Length of `cn-reservation/` range scan. |

All metrics flow through the existing `tritond-metrics` substrate;
the Settings / Metrics page in adminui graphs them once the page
knows the names.

## The agent capacity reporter

`tritonagent` gains a small reporter that posts the structured
`cn-capacity` row over the existing authenticated agent → tritond
channel. The reporter runs once at startup (after registration
succeeds) and on hardware change events from the platform layer
(numa topology change, zpool add / remove, NIC tag change, device
hot-plug — the same triggers the existing sysinfo path watches).

```rust
// services/tritonagent/src/capacity_reporter.rs
pub async fn report_capacity(client: &TritondClient, sysinfo: &Sysinfo, zpools: &[Zpool], nics: &[Nic], devices: &[Device]) -> Result<()> {
    let cap = build_cn_capacity(sysinfo, zpools, nics, devices)?;
    client.post_agent_capacity(&cap).await?;
    Ok(())
}
```

The agent's existing live-metrics pipeline (per-zone CPU% / RSS /
disk usage / NIC tx/rx) is unchanged — it already feeds ClickHouse,
and the load materialiser already consumes the rollups. There is no
new live-load reporter; one channel for per-tick live metrics, one
channel for change-driven structured capacity (D-Pl-3).

## Replacing the bin-packer

`select_tenant_cn_for_instance` in `lifecycle.rs` is deleted. The
`POST /v2/tenants/{t}/projects/{p}/instances` handler now does what
the saga catalog dictates (RFD 00004 doc 02): build
`InstanceCreateParams` (including the `Idempotency-Key`), call
`SagaExecutor::saga_execute::<SagaInstanceCreate>`. The
`instance-create` saga's first action (per the SG-2 design) is
`designate`; subsequent actions allocate NICs / IPs / disks /
enqueue the provision job; the unwind tail releases everything in
reverse, with `undesignate` as the final unwind step.

`edge_cluster.rs::select_edge_cn_for_nat_gateway` is rewritten as a
`PlacementRequest` against the same engine with
`role = CnRole::Edge`, `required_underlay = { ipv4: false, ipv6:
true }`, and no per-instance affinity. The `nat-gateway-create`
saga (RFD 00004 SG-5) carries this as its `designate` step the same
way `instance-create` does.

## What the chain does not (yet) do

- **Continuous rebalance.** A future RFD 00006 (DRS) consumes the
  same `cn-load-summary` rows and the same `ChainRunner` (with the
  current CN excluded) to find a destination for an overprovisioned
  CN; live migration is the missing primitive. The hooks are in
  place — every scorer is already pure and re-entrant.
- **Storage placement.** v1's `cn-zpool-has-space` is per-pool but
  not per-replica; a future RFD 00007 adds a Storage-DRS-style chain
  layer that places disks across pools and replicas.
- **Predictive placement.** A future RFD 00008 trains on the
  `cn-load-summary` history to bias scorers by predicted load
  rather than observed load.
- **Silo / tenant capacity reservations.** A future RFD 00009 adds
  a `silo-capacity-reservation` keyspace (distinct from this RFD's
  per-CN `cn-reservation`) and a `silo-quota-guarantee` filter that
  enforces "this silo has 100 vCPUs guaranteed across the fleet
  even when the fleet is hot".

All four future RFDs slot into the engine without changing the
trait shapes; the engine's job is to remain the right shape for
them to extend.
