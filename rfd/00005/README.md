# RFD 00005: VM placement — the `tritond-placement` engine, the `cn-capacity` / `cn-load-summary` model, and the `designate` saga

> Status: SUPERSEDED — this is the original draft, frozen as written.
> The canonical, maintained copy (with the implementation-reality
> table) is `mariana-trench/rfd/00011`. Known deviations from this
> draft as built: no materializer leader election (every tritond
> writes, last-writer-wins); 5m + 1d windows only (7-day dropped,
> `score-avoid-peaky` retired — 11 scorers); the materializer lives at
> `services/tritond/src/load_materializer.rs`, not behind a cargo
> feature in the placement crate; age staleness is enforced read-side;
> self-metrics are gauges `tritond_placement_load_materializer_seconds`
> / `tritond_placement_load_summary_stale_rows`.
>
> Parent docs: [`../../DESIGN.md`](../../DESIGN.md) §4 (Substrates — FDB), §14 (API
> surface — long-running operations and sagas), §18 (Audit log), §22 (Platform upgrades —
> currently undesigned; placement is one input);
> [`../00004/README.md`](../00004/README.md) (`tritond-saga` — placement runs as a
> saga action with explicit compensation, see invariants 1 / 8 / 9 there);
> [`../../IMDS_DESIGN.md`](../../IMDS_DESIGN.md) (the Silo → Tenant → Project →
> Instance hierarchy a pinned CN narrows against);
> [`../../VM_BACKLOG.md`](../../VM_BACKLOG.md) §12 (which already calls out a
> `POST /v2/instances/recommend-placement` endpoint and is the user-facing surface
> this RFD makes durable);
> [`../../STATUS.md`](../../STATUS.md) Locked Decisions #4 (FDB-only), #15 / #30
> (Silo → Tenant → Project URL shape — placement scopes match), #36 (per-CN agent
> binding — the agent reports capacity).
>
> Source repos referenced: `monitor-reef/` (`services/tritond`, `services/tritonagent`,
> `cli/tcadm`, `libs/tritond-{store,saga,audit,auth,metrics}`); the legacy
> Designation API at `~/workspace/triton/sdc-designation/` (DAPI — the canonical prior
> art, ~40 filter/scorer plugins, JS, plugin chain configured in CNAPI's SAPI manifest)
> and `~/workspace/triton/sdc-cnapi/` (heartbeats + the `POST /allocate` entry point
> that calls DAPI); `admin/` (the operator console — `backend/` Axum + `frontend/`
> React); the ClickHouse metrics zone at `10.199.199.75:8123` (per-zone live metrics
> the materialiser rolls up).

## Summary

Today, `tritond` picks a compute node by counting instances. The whole of placement is
`select_tenant_cn_for_instance` in
`monitor-reef/services/tritond/src/lifecycle.rs` (≈ 25 lines): list every
`Approved` tenant CN that has a recent heartbeat, count assigned instances per CN, pick
the lowest, break ties by `server_uuid`. There is no capacity model (CPU cores, RAM,
disk pools, NICs, GPUs, NUMA, fault domain are all invisible to the picker), no
overprovisioning, no traits, no operator override, no affinity / anti-affinity, no
topology spread, no silo or tenant pinning, no capacity reservation across a
multi-step provision, no `tcadm` surface, no admin-console screen, and no
explainability — when the picker chooses a CN, the operator's only recourse is to
read `tritond` logs. Concurrent provisions can double-book the same CN because
nothing reserves capacity across the saga (the saga catalog is RFD 00004; placement
must run inside it, not in front of it).

Legacy Triton's DAPI got most of the structure right: a typed sequence of *hard
filters* (drop ineligible CNs with a reason), a *calculator* layer (overprovisioned
free CPU / RAM / disk per CN), a sequence of *soft filters* (best-effort locality),
and *scorers* (weighted contributions; highest sum wins). DAPI's plugin chain is
configured per deployment in CNAPI's SAPI manifest and operators extend it by adding
files under `lib/algorithms/`. It does not track NUMA, NICs, GPUs / SR-IOV, fault
domain, live load, or any silo concept; its server objects come from CNAPI heartbeats
and its allocator is a synchronous library call from `vmapi`. The 40-algorithm chain
is its strength (a documented design surface — every leak DAPI doesn't have is
spelled out by the filter that drops it) and its forcing function: a missing
algorithm is a placement gap. We adopt the shape.

It is also what competitors clear or beat. **VMware DRS** runs continuous vMotion-
driven rebalance plus reservations / limits / shares, VM-VM and VM-Host affinity /
anti-affinity rules, maintenance-mode evacuation, and predictive DRS; **Nutanix AHV
ADS** runs anomaly-driven rebalance with storage data locality (the CVM and the VM
co-locate); **AWS EC2** ships placement groups (cluster / partition / spread),
Dedicated Hosts, and Capacity Reservations; **Kubernetes** ships predicates /
priorities, taints / tolerations, topology spread, and pod affinity / anti-affinity.
None of these are a v1 differentiator on their own; together they are the *table
stakes* an enterprise control plane is judged against. v1 of this RFD must clear
DAPI on every axis and clear the table-stakes set on initial placement
(reservations, affinity / anti-affinity, topology spread, silo / tenant pinning,
GPU / SR-IOV / NUMA, fault domain, operator override, explainability). It must not
ship DRS-style live-migration rebalance — that depends on bhyve live migration
landing first (RFD 00006, future) — but it must leave every seam open for it.

This RFD locks the architecture for **`tritond-placement`** — a new workspace
library that exposes `Filter` and `Scorer` Rust traits, a small set of built-ins,
a chain runner that produces an `--explain` decision report, and a leader-elected
background **load materialiser** that rolls per-CN load history from the existing
ClickHouse metrics zone into FDB-backed summaries the scorers read on the hot path
without a synchronous CH dep. It locks **a structured CN capacity / placement
model** in FDB (`cn-capacity` published by `tritonagent`, `cn-placement`
operator-edited, `cn-reservation` saga-owned, `cn-load-summary` materialiser-owned,
`instance-affinity` per-instance) that subsumes today's opaque `sysinfo` blob and
the implicit "instance count" capacity proxy. It locks **a `designate` saga
action** (in the RFD 00004 catalog) that pins a CN, takes a reservation ticket
inside one FDB transaction so concurrent provisions can't collide, and unwinds
cleanly on saga failure. It locks **a complete `tcadm placement` / `tcadm cn`
surface** and an **adminui Placement section** (fleet heatmap with CH window
selector, chain editor, simulation, reservations table) plus a Placement tab on
the CN detail page and on the instance detail page — every primitive is operable
from both, and `--explain` is the killer debug tool DAPI's logs-only "reasons
hash" never was.

The bright line for the v1 cut: **initial placement plus admin override**, with
reservations correct under concurrent provisions and explainability everywhere.
Continuous rebalance (DRS / ADS analog), predictive placement, storage placement
(Storage DRS analog), and silo / tenant capacity *reservations* (vSphere
"reservations" / AWS "Capacity Reservations" — distinct from this RFD's per-CN
*pins*) are deferred to RFDs 00006 / 00007 / 00008 / 00009 with the seams in place.

This is a design RFD with an attached implementation plan; the first slice
(`tritond-placement` crate scaffolding + the four FDB keyspaces) is built against
it.

## Document set

Read in order.

| File | Purpose |
|---|---|
| [`README.md`](./README.md) | Entry point, invariants, the engine choice, decisions table, slice map, open questions. |
| [`01-data-model.md`](./01-data-model.md) | The FDB keyspaces: `cn-capacity` (agent-published structured capacity), `cn-placement` (operator-edited per-CN policy: reserved / cordoned / traits / overprovision / fault domain / silo pin / tenant pin), `cn-reservation` (saga-owned per-saga reservation ticket), `cn-load-summary` (materialiser-owned per-CN ClickHouse rollup), `instance-affinity` (per-instance affinity / anti-affinity / topology spread rules). Key encoding, the typed Rust shapes, the `Store` trait additions, the transactional invariants (single-txn pick + reserve in `designate`, single-txn refresh in the materialiser). **Slices PL-1 + PL-2.** |
| [`02-pipeline.md`](./02-pipeline.md) | The engine: `Filter` and `Scorer` traits, the `Verdict` and `ExplainReport` types, the chain runner, the seventeen default filters and the eight + four built-in scorers (capacity-based and CH-load-based), the `Strategy` presets (Spread / Pack / Balanced), the `cn-load-not-overheating` opt-in guardrail filter. The `designate` saga action and its `undesignate` compensation, slotted into the RFD 00004 catalog (`services/tritond/src/sagas/`). The load materialiser — leader-elected via the existing `tritond` cluster-leader primitive, queries the ClickHouse metrics zone, writes `cn-load-summary` rows. The agent-side change: a structured `POST /v2/agent/capacity` reporter on top of the existing heartbeat. **Slices PL-3 + PL-4 + PL-5 + PL-6.** |
| [`03-admin-surface.md`](./03-admin-surface.md) | The operator surface, end-to-end. The `tcadm` inventory (`tcadm placement pick / explain / strategy / config / materialiser / simulate`, `tcadm cn capacity / load / reserve / unreserve / pin / unpin / cordon / uncordon / trait / overprovision / fault-domain / drain`, `tcadm instance affinity / force-place`, `tcadm placement reservations`). The Dropshot endpoints in `tritond-api` they bind to. The adminui inventory: the new `/admin/placement` overview (fleet heatmap with scope + window + metric selectors, strategy + chain editor, simulator, reservations table), the CN-detail Placement tab, the instance-detail Placement tab, the create-instance wizard additions. The Cedar actions and audit routing (per-tenant vs. fleet, mirroring RFD 00004 invariant D-Sg-11). The metrics `tritond-metrics` adds. **Slices PL-7 + PL-8 + PL-9.** |

## Non-negotiable invariants

1. **Pick and reserve are one FDB transaction.** `designate` reads the per-CN view
   (capacity + active reservations + assigned instances), runs the filter / scorer
   chain, and on a successful pick writes the `cn-reservation/<cn>/<saga>` row plus
   the `Instance.host_cn_uuid` row in **the same transaction**. The capacity used in
   the filter step is the post-reservation residual computed inside the txn. There
   is no "pick now, reserve in a follow-up txn" window. Concurrent `designate`s for
   the same CN either both see their own reservation and at least one fails the
   capacity filter, or one transaction wins and the other re-runs from `pick`. The
   bin-packer being replaced has neither check.
2. **Placement runs inside a saga, not in front of it.** `designate` and its
   compensation `undesignate` are catalog entries in `services/tritond/src/sagas/`
   per RFD 00004 D-Sg-3; they thread the SEC's `(sec_id, epoch)` fencing tuple
   (RFD 00004 D-Sg-8) into every store mutation, so a stale-but-not-yet-known-stale
   SEC's `designate` cannot land a reservation after takeover. Operator force-place
   is an alternate body of the same `designate` action (skips the filter chain,
   still takes the reservation), not a side door around the saga. There is no
   placement code path that mutates state outside a saga action.
3. **The hot path never blocks on ClickHouse.** Scorers read `cn-load-summary` rows
   from FDB; the rows are refreshed by a background materialiser that queries
   ClickHouse on a 60 s tick. A summary stale beyond `staleness_ticks × interval`
   is marked `stale = true`; load-history scorers contribute zero for stale rows
   and the `--explain` report names the skip. ClickHouse being down or slow slows
   down freshness, never `designate`. The materialiser is leader-elected via the
   existing `tritond` cluster-leader primitive (the same primitive `peer_invalidations`
   and the sweepers use); peers without leadership write nothing.
4. **Every filter rejection and every scorer contribution is observable.** The
   chain runner emits an `ExplainReport` for every pick: per-CN, per-filter
   `Accept | Reject { reason }`, per-scorer `f32` contribution, the strategy +
   weight vector, the staleness of each `cn-load-summary` consulted. The report is
   the body of `tcadm placement pick --explain`, the body of `GET /v2/placement/...
   ?explain=true` on dry-run endpoints, and a row in the audit log on every
   real pick (truncated to the chosen CN plus the rejected-by-filter counts so the
   audit row stays bounded). DAPI's "reasons hash that buries the reasons in
   `tritond` logs" is the failure mode to beat.
5. **Silo and tenant are first-class scopes; project is not.** A CN may be pinned
   to a single silo (`cn-placement.pinned_silo_uuid`) or to a single tenant
   (`pinned_tenant_uuid`); a tenant pin implicitly requires the silo pin to match
   that tenant's silo (or be null), enforced at write time, audited on rejection.
   No project-level pinning — operators who need it put the project into a
   single-tenant silo. The `PlacementRequest` carries the requesting principal's
   `(silo_uuid, tenant_uuid, project_uuid)`, which the `cn-scope-match` filter
   reads. The hierarchy is the one IMDS already established
   (`IMDS_DESIGN.md`); placement does not introduce a parallel scoping model.
6. **Operators extend the engine at compile time, not at runtime.** Built-in
   filters / scorers live in `monitor-reef/libs/tritond-placement/src/{filter,scorer}.rs`;
   out-of-tree built-ins live in sibling workspace crates pulled in behind cargo
   features (e.g. `--features placement-acme-rack-locality`). The active chain and
   weight vector are FDB-backed cluster settings; switching them does not require a
   `tritond` restart but every active name must already be registered in the
   process at boot. v1 does not ship a WASM loader or a gRPC plugin protocol — the
   blast radius and the per-pick latency cost are both unjustified for the demand
   (none observed); revisit if operators ask.
7. **The capacity reporter is the only structured source of CN hardware truth.**
   `tritonagent` posts the structured `cn-capacity` row at startup and on hardware
   change (NUMA topology, RAM total, zpool layout, NIC tags, devices). The opaque
   `Cn.sysinfo` blob stays for now (legacy `tcadm cn show` reads it) but placement
   never reads it; a CN with no `cn-capacity` row is invisible to placement and
   surfaces on `tcadm cn list` with a "no capacity report" badge. There is no
   second derivation path (e.g. parsing sysinfo into capacity inside `tritond`) —
   that is the current bin-packer's failure mode, not its fix.
8. **No `vnext` in code identifiers.** Crate = `tritond-placement`; types =
   `CnView`, `PlacementRequest`, `Strategy`, `Verdict`, `ExplainReport`, `Filter`,
   `Scorer`, `CnLoadSummary`, `CnCapacity`, `CnPlacement`, `CnReservation`,
   `InstanceAffinity`. Saga names: `designate` / `undesignate`. Prose use of
   "vnext" is fine; struct names and FDB key prefixes are not. Per
   `feedback_no_vnext_in_code`.

## Why a typed pipeline, and why in-tree Rust traits

A handler that says "pick the CN with the most free RAM that also has the right
NIC tag" looks simple right up to the point an operator asks "why this one and
not that one", at which point you wish you had a list of rejected candidates and
the score contributions. DAPI is the cautionary tale on the JS side: the
architecture is right but the explainability is shallow (a `reasons` hash buried
in service logs). The Kubernetes scheduler is the cautionary tale on the typed
side: every reasonable extension point is a separate plugin interface (Filter /
Score / Reserve / Permit / PreBind / Bind / PostBind) and the framework swallows
most of the budget that should have gone into the filters themselves.

We take the middle. The `Filter` and `Scorer` traits in `tritond-placement` are
the minimum surface that lets us write the seventeen built-in filters and the
twelve built-in scorers cleanly; the chain runner is one function, the
`ExplainReport` is the byproduct, and the audit log row is one projection of it.
Operators who want to add a filter add a Rust struct that implements one trait
and a registration call; the chain order and weights they pick are FDB settings.
That is the same shape `tritond-saga`'s catalog uses for actions and the same
shape `tritond-store` uses for the `Store` trait — no new architectural
vocabulary.

The argument against WASM (or gRPC-extender) loaders: every plugin interface
that crosses an ABI boundary in this codebase has cost two things — the obvious
sandbox / IPC engineering and the less-obvious "now your debugger doesn't follow
the call". For built-ins shipped by the project, in-tree Rust is correct. For
*third-party* operator plugins, the WASM case is real but speculative; we will
revisit when an operator surfaces it. Until then, the cargo-feature seam is
enough.

The argument against a separate `placementd` daemon: placement is on the hot
provision path; a network hop per `designate` is not free; the blast radius of
placement bugs inside `tritond` is small (a saga unwinds), and the blast radius
of placement bugs inside a separate process is larger because now the
`tritond ↔ placementd` protocol must also be correct. DAPI's
"library called by CNAPI" shape is the right precedent. We can hoist later if
operational reality forces it; we cannot easily un-hoist.

## Decisions (D-Pl-*)

| # | Decision | Rationale |
|---|----------|-----------|
| D-Pl-1 | `tritond-placement` is a new in-tree Rust crate exposing `Filter` and `Scorer` traits. Built-ins live in `src/{filter,scorer}.rs`; out-of-tree built-ins live in sibling workspace crates pulled in behind cargo features on `tritond`. No WASM, no gRPC extender, no separate `placementd` daemon. | A typed in-tree pipeline is the smallest credible step that beats DAPI on both correctness and explainability without paying the ABI / IPC tax. Operators who want to add a filter add a Rust struct and a registration line; the chain and weights they pick are FDB settings. The seams to hoist or to add a plugin loader stay open. |
| D-Pl-2 | Pick and reserve are one FDB transaction inside the `designate` saga action. The transaction reads `cn-capacity` + `cn-placement` + the existing `cn-reservation` rows for each candidate, runs the filter / scorer chain on the residual, and writes `cn-reservation/<cn>/<saga>` + `Instance.host_cn_uuid` if a CN wins. Failure to pick (no eligible CN) fails the saga at this action; saga unwind via `undesignate` releases the reservation. | Concurrent provisions are the failure mode the bin-packer cannot survive. A "pick, then reserve in a follow-up txn" shape has a guaranteed race window; the one-txn shape closes it. The reservation row is the durable record of in-flight capacity consumption that scorers on the *next* pick read; without it, two concurrent provisions both see the CN as "free" and both land. |
| D-Pl-3 | CN capacity, placement policy, reservations, load summaries, and per-instance affinity rules are five distinct FDB keyspaces (`cn-capacity`, `cn-placement`, `cn-reservation`, `cn-load-summary`, `instance-affinity`), each with a typed Rust shape in `tritond-store::types`. The legacy opaque `Cn.sysinfo` blob stays read-only for compatibility; placement never reads it. | One key per concern is cheap in FDB and lets each writer be independent (the agent owns capacity, the operator owns placement, the saga owns reservations, the materialiser owns load summaries). Mixing them into one row would couple the writers, break the audit-of-writes shape, and force the materialiser to overwrite operator edits or vice versa. The structured `cn-capacity` row exists because the implicit "derive capacity from sysinfo" path is the bin-packer's failure mode, not its fix. |
| D-Pl-4 | The hot path never blocks on ClickHouse. A background **load materialiser** task in `tritond`, leader-elected via the existing cluster-leader primitive, queries the ClickHouse metrics zone on a 60 s tick (configurable cluster setting) and writes per-CN summaries (`cpu_p50_5m / 1d / 7d`, `cpu_p95_*`, RAM, per-pool disk, NIC tx / rx) into `cn-load-summary`. Scorers read those rows from FDB only. A row stale beyond `staleness_ticks × interval` is marked `stale = true` and load-history scorers contribute zero for it; placement degrades to capacity-only and `--explain` names the skip. | A synchronous CH query per `designate` would couple `tritond`'s provisioning availability to ClickHouse's availability, add multi-second latency on the hot path, and re-derive a value every time that changes on minute scales at best. Materialising once per minute into FDB pays the CH read once for every CN that uses the summary; gracefully degrading on staleness keeps placement working when the metrics tier is down. Leader-electing the materialiser means N `tritond` peers don't independently DoS ClickHouse. |
| D-Pl-5 | Silo and tenant are first-class pinning scopes on `cn-placement` (`pinned_silo_uuid`, `pinned_tenant_uuid`); project is intentionally not a pinning scope. Setting `pinned_tenant_uuid` requires `pinned_silo_uuid` to match (or be null); the constraint is enforced at write time and the rejection is audited. The `cn-scope-match` filter consumes both. Operator force-place may bypass scope-match only with `--ignore-scope-pin` (audited). | Silo / Tenant / Project / Instance is the hierarchy IMDS already established (`IMDS_DESIGN.md`) and STATUS.md Locked Decisions #15 / #30 carries through the URL shape. A CN reserved "for one tenant" is the operator's natural unit (matching legacy DAPI's `owner_uuid` field); a CN reserved "for one silo" is the operator's natural unit for a managed-customer arrangement. A *project*-pinned CN is a strictly smaller blast-radius operator action that no enterprise has asked for and that operators who do need it can express by putting the project into a single-tenant silo. Closing the scope at silo and tenant keeps the data model and the filter narrow; adding project later is a strict superset and not a migration. |
| D-Pl-6 | `Strategy` (`Spread` / `Pack` / `Balanced`) is a preset weight vector applied on top of the same scorers, not a separate chain. A package may pin a strategy; a request may override per provision. | A single chain is easier to debug than three near-identical ones; the strategies differ in *weights*, not in *which scorers run*. DAPI's `alloc_server_spread` package field is the prior art and operators already think this way. The default is `Spread` (matching the implicit behaviour DAPI's default weights produce); `Pack` is for capacity-planning corners and dev clusters. |
| D-Pl-7 | The `Designate` action is a catalog entry in `services/tritond/src/sagas/`; the compensation `Undesignate` releases the reservation, clears `Instance.host_cn_uuid`, and emits the audit row. The action and its undo thread the SEC's `(sec_id, epoch)` fencing tuple per RFD 00004 D-Sg-8 through every store mutation. Per-action timeout default matches the sweeper's stale-claim threshold (RFD 00004 D-Sg-9); a `designate` that cannot pick within the timeout fails the saga and unwinds. | Placement runs inside the saga catalog; it is not a side door around it. The fencing tuple is the difference between "concurrent provisions race cleanly" and "a stale SEC double-books a CN after takeover". The action timeout is the difference between "no eligible CN under transient load" surfacing as a 5xx the operator can see and the saga sitting forever. |
| D-Pl-8 | Operator force-placement is a flag on the same `designate` action body, not a separate handler. `tcadm instance force-place` and the adminui "Force CN" action call the same action with `force_cn: Some(uuid)`, optionally `ignore_scope_pin: true`. The chain is skipped, the reservation is still taken in the same FDB transaction, the audit row records the override and the chosen-but-not-checked CN. | A separate "skip placement, write `host_cn_uuid` directly" path was DAPI's leak (the `params.server_uuid` short-circuit in vmapi): it bypassed the capacity bookkeeping and the audit, so a forced placement looked the same as a regular one in the FDB row but didn't decrement free capacity, and a concurrent automatic pick on the same CN could double-book. Routing force-place through the same `designate` body fixes all three: the reservation row exists, the audit row is distinguishable, and concurrent picks see the consumption. |
| D-Pl-9 | The `cn-load-not-overheating` hard filter is shipped but off by default. Operators enable it via `tcadm placement config set` with the threshold in cluster settings. | The filter is correct ("never place on a CN whose 5-min p95 CPU is above X"); the default-on failure mode ("misconfigured threshold under fleet-wide load → no eligible CN → provisioning outage") is worse than the default-off failure mode ("misconfigured weights → suboptimal placement → operator notices on the heatmap and adjusts"). Soft penalties via the load-history scorers cover the common case; the hard filter is the explicit opt-in for operators who want a guardrail. |
| D-Pl-10 | The `ExplainReport` is the engine's primary output, not a debug afterthought. Every `pick` call produces one; `tcadm placement pick / explain --json` returns it verbatim; the adminui "Simulate" panel renders it as a table; the audit log writes a bounded projection (chosen CN + per-filter reject counts + winning scorer breakdown). DAPI's reasons-in-logs shape is the failure mode to beat. | A placement decision the operator cannot inspect is one the operator cannot trust. The cost of always producing the report is a small allocation per CN per pick — negligible against the pick itself; the cost of *not* producing it is every "why did this provision land here" question costs a `tritond` log archaeology session. |

## New / changed crates

| Crate | Path | Role |
|---|---|---|
| `tritond-placement` (new) | `libs/tritond-placement/` | The engine layer. Dependencies: `serde`, `serde_json`, `thiserror`, `tracing`, `tokio`, `chrono`, `uuid`; path dep on `tritond-store` for the typed FDB shapes (`CnCapacity`, `CnPlacement`, `CnReservation`, `CnLoadSummary`, `InstanceAffinity`); path dep on `tritond-saga` for the `SagaContext` / `SagaRequestCtx` shapes the `designate` action consumes. **No** dep on `tritond` the service. The crate ships: the `Filter` / `Scorer` / `Strategy` / `Verdict` / `ExplainReport` types (`src/types.rs`), the seventeen built-in filters (`src/filter.rs`), the eight capacity scorers + the four CH-load scorers (`src/scorer.rs`), the chain runner (`src/engine.rs`), the config shape that maps cluster settings → active chain + weight vector (`src/config.rs`), and the load materialiser (`src/load_materializer.rs`, behind a `materializer` feature so the test crate doesn't pull the CH client). Tests: golden inputs + expected `ExplainReport` outputs in `tests/`. |
| `tritond` (changed) | `services/tritond/` | Gains `src/placement/` (the glue: chain-loader that reads cluster settings, the `pick` entry point that loads `CnView` rows from the `Store`, the materialiser-task wiring, the operator-facing `/v2/placement/*` and `/v2/admin/cn-placement/*` Dropshot handlers); gains `src/sagas/designate.rs` (the catalog entry — the saga action that calls `placement::pick` inside a single FDB transaction and writes the reservation, plus the `undesignate` compensation; registered in `sagas/mod.rs::register_all_actions` per RFD 00004 doc 02); deletes `select_tenant_cn_for_instance` from `lifecycle.rs` (replaced by the `designate` action — the handler enqueues the `instance-create` saga, which calls `designate` first); rewrites `edge_cluster.rs::select_edge_cn_for_nat_gateway` as a `PlacementRequest` against the same engine with edge-specific filter set (IPv6 underlay required, `cn_role` includes Edge). |
| `tritond-store` (changed) | `libs/tritond-store/` | Adds the five new key prefixes (`cn_capacity`, `cn_placement`, `cn_reservation`, `cn_load_summary`, `instance_affinity`) and their typed Rust shapes (`src/types.rs`); adds `Store` trait methods for the read / list / write / CAS operations placement needs (see doc 01 for the surface). |
| `tritonagent` (changed) | `services/tritonagent/` | Adds a structured capacity reporter — once at startup, again on hardware change — that posts the new `cn-capacity` row over the existing agent → tritond authenticated channel. No new live-load reporter; the existing per-zone metrics pipeline already feeds ClickHouse, which the materialiser already consumes. |
| `tritond-api` (changed) | `apis/tritond-api/` | Adds the `/v2/placement/*` (operator: pick, explain, materialiser status) and `/v2/admin/cn-placement/*` (operator: cn capacity / placement / reservation / load reads + edits) endpoint surfaces and their request / response types (`PlacementRequest`, `PlacementPickResponse`, `ExplainReport`, `CnPlacementEdit`, …). |
| `tritond-client` (regen) | `clients/internal/tritond-client/` | Progenitor regen after the API additions; `tcadm` and `admin-backend` use it. |
| `tcadm` (changed) | `cli/tcadm/` | Adds the `tcadm placement *`, `tcadm cn placement *`, `tcadm cn capacity`, `tcadm cn load`, `tcadm instance force-place`, `tcadm instance affinity *` commands (see doc 03). |
| `admin-backend` (changed) | `admin/backend/` | Adds the Placement-related handlers / proxies the adminui frontend calls (see doc 03). |
| `admin-frontend` (changed) | `admin/frontend/` | Adds the `/admin/placement` overview page, the CN-detail Placement tab, the instance-detail Placement tab, and the create-instance wizard's placement controls (see doc 03). |

No new external runtime dependency on the hot path. The materialiser may pull the
existing CH client crate the metrics pipeline already uses (re-confirm in PL-6;
budget no second client). `make audit` exceptions: the four pre-existing
`monitor-reef/CLAUDE.md` entries; budget an additional entry only if a new
transitive dep trips the advisory db.

## Slice map (PL-0 … PL-9)

| Slice | Scope | Acceptance |
|---|---|---|
| **PL-0** | This RFD: `rfd/00005/README.md` + `01-data-model.md` + `02-pipeline.md` + `03-admin-surface.md`. Doc-only commit. | Files render; cross-links resolve; matches the house RFD style. **(this commit)** |
| **PL-1** | `libs/tritond-placement` crate scaffolding: workspace member, `Cargo.toml` (deps as listed above, `materializer` cargo feature gating the CH client), `src/lib.rs` re-exports, `src/types.rs` — `Filter` / `Scorer` / `Strategy` / `Verdict` / `ExplainReport` / `CnView` / `PlacementRequest` types with `serde` derives and no implementations beyond the trait shells. A trivial passthrough `engine.rs::pick` stub that returns the first eligible CN; one smoke test that drives it against a synthetic `CnView`. | `cargo test -p tritond-placement` green; `make package-test PACKAGE=tritond-placement` and `make package-build PACKAGE=tritond-placement` clean. The crate compiles in isolation, exposes the trait shapes the rest of the slices build against, and has no `tritond` service deps. |
| **PL-2** | The five FDB keyspaces in `tritond-store`: prefixes (`cn_capacity` / `cn_placement` / `cn_reservation` / `cn_load_summary` / `instance_affinity`), typed Rust shapes in `src/types.rs`, `Store` trait additions (read / list / write / CAS — see doc 01 for the exact surface), `MemStore` and `FdbStore` impls, integration tests for the CAS invariants (pin-conflict rejection, reservation-row uniqueness per `(cn, saga)`, materialiser refresh idempotency). | `cargo test -p tritond-store --features foundationdb` (with the local FDB cluster) green; an integration test exercises a concurrent reservation race (two writers, one cn, both reservations land but the post-residual fits only one — second writer's residual check fails inside the txn, surfaces as `StoreError::CapacityExhausted`). |
| **PL-3** | The seventeen built-in filters in `tritond-placement/src/filter.rs`: each filter is one `impl Filter` block with a per-filter unit test (accept case + every distinct reject case). The chain runner in `engine.rs` produces an `ExplainReport` with per-CN, per-filter verdicts. No scorers yet; `pick` returns "first remaining after filters" deterministically by `server_uuid` for tie-break. | `cargo test -p tritond-placement` covers each filter; a chain-runner test asserts the report shape against a 5-CN fixture with a mix of reject reasons. |
| **PL-4** | The eight capacity scorers + the four CH-load scorers in `src/scorer.rs`, the `Strategy` preset weight vectors, the `Score` aggregation in the chain runner, the `score-uniform-random` deterministic-seed tie-break. The `ExplainReport` grows per-scorer contributions per CN. | Per-scorer unit tests assert relative ordering on synthetic `CnView`s; a strategy test asserts `Spread` vs. `Pack` pick the opposite CNs of two equally-eligible candidates with the spread-relevant attribute differing. |
| **PL-5** | The `designate` saga action and its `undesignate` compensation, registered in `services/tritond/src/sagas/designate.rs`. Replaces `select_tenant_cn_for_instance` in `lifecycle.rs` — the handler enqueues the `instance-create` saga (RFD 00004 doc 02) which calls `designate` first. The `tritond` Dropshot endpoints `/v2/placement/pick`, `/v2/placement/explain`, and `/v2/admin/cn-placement/*` (read-only at this slice — edits land in PL-7) under the existing Cedar gating. The agent capacity reporter on `tritonagent`. | Integration test: a provision picks a CN, the reservation row lands, the saga unwind path releases it. The bin-packer's tests are deleted and replaced with capacity-aware tests covering the same shapes (single CN, two CNs, no eligible CN). |
| **PL-6** | The load materialiser in `tritond-placement/src/load_materializer.rs`: leader election piggybacks on the existing cluster-leader primitive, the CH query set, the per-CN `cn-load-summary` write, the staleness tracking, the metrics (`tritond_placement_load_materializer_seconds`, `tritond_placement_load_summary_stale_total`). The materialiser is started from `tritond`'s `bootstrap.rs`. | `tritond` integration test with a stub CH server: the materialiser polls, writes the summary, the load-history scorers contribute. The same test with the stub server returning errors: the summary goes stale, scorers contribute zero, `--explain` flags the skip; `placement::pick` still returns a CN. A leader-failover test (in-process two-peer harness) confirms the second peer takes over within one tick after the first peer's lease lapses. |
| **PL-7** | The `cn-placement` operator edits: the write endpoints under `/v2/admin/cn-placement/{cn}` (reserve / unreserve / pin / unpin / cordon / uncordon / trait set / overprovision / fault-domain), the Cedar action additions (`Cn::Reserve`, `Cn::Pin`, `Cn::Unpin`, …), the audit-row shape, the pin-conflict enforcement (D-Pl-5). `tcadm cn reserve / unreserve / pin / unpin / cordon / uncordon / trait / overprovision / fault-domain` and `tcadm instance affinity / force-place`. | Integration test per write endpoint (happy path + Cedar-deny path + audit-row shape); pin-conflict test (set silo S1 on a CN already pinned to a tenant in S2 → 409 with the conflict reason, no FDB mutation, audit row of the rejection); force-place inserts a reservation and is distinguishable in the audit row from an automatic pick. |
| **PL-8** | The `tcadm placement *` commands (pick / explain / strategy / config / materialiser / simulate / reservations), the corresponding read endpoints in `tritond-api`, the `ExplainReport` rendering (human + `--json`). | `cargo test -p tcadm` for the rendering; a manual smoke against the live cluster (`.10` / `.40` / `.41`) produces an `ExplainReport` that names every CN, every filter verdict, and every scorer contribution. |
| **PL-9** | The adminui Placement section: `/admin/placement` overview (fleet heatmap with scope + window + metric selectors, chain editor, strategy editor, simulator, reservations table), the CN-detail Placement tab, the instance-detail Placement tab, the create-instance wizard's placement controls. `admin-backend` proxies under `/api/admin/placement/*` and the front-end pages. | Browser-driven QA (the `qa` skill): the heatmap renders, the scope filter narrows CNs by silo, the simulator round-trips a request to `/v2/placement/pick?explain=true` and renders the verdicts, the CN-detail Placement tab edits round-trip to the FDB row, the create-instance wizard's "Force CN" preview calls `pick --dry-run` before submit. |

## Open questions

- **Materialiser leader election: piggyback on which primitive?** RFD 00004's
  SEC has a heartbeat and a CAS-on-takeover model; `peer_invalidations` has its
  own leader; the sweepers run on every peer. The materialiser only needs "one
  writer per tick" — does it share the SEC's lease (cheap, but couples
  materialiser availability to saga-engine availability), get its own short
  cluster-wide lease key (clean, but a third leader to operate), or run on every
  peer with a deterministic CN-to-peer assignment (no leader at all)? Decide in
  PL-6 once the SEC's lease interface is concrete.
- **Per-package strategy override storage.** D-Pl-6 says a package may pin a
  strategy; the package catalog itself doesn't exist yet (see backlog —
  `VM_BACKLOG.md` flags package modelling as future). Until packages land,
  strategy overrides ride on the per-request `PlacementRequest.strategy` field
  only; revisit when the package catalog adds its FDB shape.
- **Affinity rule language.** The `instance-affinity` shape in doc 01 mirrors the
  Kubernetes podAffinity primitives (`required` / `preferred`, `vm_to_vm` /
  `vm_to_host` / `topology`, `in` / `not_in` / `spread`). DAPI's Docker-style
  `affinity:container==foo` strings are a strict subset and we will accept them
  in the request schema as sugar that desugars to the typed rules. Decide in
  PL-7 whether to expose the desugared rules on `tcadm instance affinity show`
  or to preserve the original syntax.
- **GPU / SR-IOV inventory granularity.** `cn-capacity.devices[]` carries GPU
  model + free count and SR-IOV VF free counts, but the agent's discovery of
  these is platform-specific (the `tritond-cn-platform::smartos` wrapper does
  not yet expose them). PL-5 ships the data model; the actual agent reporter
  for GPU / SR-IOV is deferred to the slice that lands the first GPU-capable CN
  (out of scope for this RFD).
- **Force-place audit row scope.** D-Pl-8 says force-place is distinguishable in
  the audit row. Open: does the audit row live in the per-silo chain (the
  tenant whose instance was placed) or the fleet chain (the operator who issued
  the override)? RFD 00004 D-Sg-11 splits saga lifecycle (fleet) from side
  effects (per-silo); force-place is a side effect so it follows the per-silo
  rule, but the operator-identity portion of it deserves a fleet-chain entry
  too. Decide in PL-7 against the existing audit conventions; default is "both
  chains with the saga `operation_id` cross-link".
- **Drain semantics in v1.** `tcadm cn drain` in v1 is "stop + restart on a new
  CN" since live migration isn't here yet. Open: does drain stop and immediately
  re-provision each instance serially (simple, slow for a fat CN), in batches
  (faster, but capacity sloshing), or by tenant policy ("never more than N
  instances of one tenant down at once")? Default for PL-7: serial, with a
  `--parallelism` flag clamped to a small N for impatient operators. Revisit
  when live migration lands.

## Commit discipline

Same as RFD 00001 / 00002 / 00003 / 00004: doc-only commits run the closest
available validation (markdown link check). Each implementation slice changes one
logical behaviour, updates the relevant doc in this set, adds / updates tests,
and runs the workspace gates (`make format && make clippy && make package-test
PACKAGE=… && make package-build PACKAGE=…`, plus `make openapi-check` / `make
clients-check` once API changes land in PL-5 / PL-7 / PL-8). Audit exceptions:
the four pre-existing `monitor-reef/CLAUDE.md` entries; budget an additional
entry only if a transitive dep introduced by the load materialiser's CH client
trips the advisory db.
