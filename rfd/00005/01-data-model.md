# RFD 00005 · Doc 01 — Data model: the five FDB keyspaces

> Slice PL-2 implements this document. Companion crate:
> `monitor-reef/libs/tritond-store/` (new key prefixes, typed shapes,
> `Store` trait additions). Modelled on the existing `tritond-store`
> shape (per-resource key prefix, typed row in `src/types.rs`,
> `MemStore`/`FdbStore` parity, single-transaction-per-write
> discipline).

Placement reads and writes five FDB keyspaces. Each keyspace has a
single writer (in the operational sense — there is at most one role
authoritatively responsible for a row), a typed Rust shape, and a
small set of `Store` trait methods. Mixing them into one row would
couple the writers and force one of them to overwrite another's
edits; one row per concern is cheap in FDB and keeps the audit-of-
writes story clean (D-Pl-3).

## Key layout

```
cn-capacity/<server_uuid>          — written by tritonagent at startup + on hw change
cn-placement/<server_uuid>         — written by operator (tcadm / adminui)
cn-reservation/<server_uuid>/<saga_id>
                                   — written by the designate saga action; deleted by undesignate or by the reaper
cn-load-summary/<server_uuid>      — written by the load materialiser leader
instance-affinity/<instance_id>    — written at instance create; editable by operator
```

All keys are range-scannable (`cn-capacity/` enumerates every CN's
capacity row; `cn-reservation/<cn>/` enumerates every reservation
against one CN; `cn-reservation/` enumerates the entire fleet's
in-flight reservations for the adminui reservations table). FDB tuple
encoding is the same as the rest of `tritond-store`; values are
`serde_json` (PL-2) with a `serde_cbor` move-over deferred until the
whole crate moves.

## `cn-capacity/<server_uuid>` — structured hardware truth

Written by `tritonagent`. The agent has the sysinfo blob, the
`tritond-cn-platform::smartos::zoneadm` wrapper, and the zpool tool
under its hand; it is the only thing that knows the live hardware
view of a CN. The legacy opaque `Cn.sysinfo` blob stays read-only
for compatibility with the existing `tcadm cn show` command;
placement never reads it (invariant 7).

```rust
// tritond-store/src/types.rs
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct CnCapacity {
    pub server_uuid:        Uuid,
    pub cpu_cores_physical: u32,
    pub cpu_threads_logical:u32,
    pub numa_nodes:         Vec<NumaNode>,        // per-node cores + RAM; len() == 1 on a UMA box
    pub ram_total_mb:       u64,
    pub zpools:             Vec<ZpoolCapacity>,   // each: name, total_bytes, free_bytes, tier
    pub nic_tags:           Vec<String>,          // each entry is one tritond-known NIC tag this CN can carry
    pub underlay:           UnderlayCapability,   // { ipv4: bool, ipv6: bool }
    pub devices:            Vec<DeviceCapacity>,  // GPU model + free count, SR-IOV VFs free — see open question in README
    pub platform_version:   String,               // SmartOS / illumos platform ID; matches the legacy DAPI min-platform field
    pub reported_at:        chrono::DateTime<chrono::Utc>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct NumaNode { pub node_id: u8, pub cores: u32, pub ram_mb: u64 }

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ZpoolCapacity {
    pub name:        String,
    pub total_bytes: u64,
    pub free_bytes:  u64,
    pub tier:        StorageTier,                  // Ssd | Nvme | Hdd | Mixed
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum StorageTier { Ssd, Nvme, Hdd, Mixed }

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct UnderlayCapability { pub ipv4: bool, pub ipv6: bool }

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct DeviceCapacity {
    pub kind:       DeviceKind,    // Gpu | SrIovVf
    pub model:      String,        // "a100-80gb", "intel-x710-vf"
    pub free_count: u32,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum DeviceKind { Gpu, SrIovVf }
```

**Invariants.**

1. The agent writes this row at startup (after `tritond` registration
   succeeds) and on hardware change. A CN with no `cn-capacity` row
   is invisible to placement (every filter rejects it with reason
   `"cn-capacity row absent"`) and surfaces on `tcadm cn list` with a
   `no-capacity-report` badge.
2. The `reported_at` field is the agent-side clock. Staleness is
   judged against the agent heartbeat freshness (the existing
   `Cn.last_seen` row), not against this field — the agent only
   re-reports on change, so an old `reported_at` plus a fresh
   heartbeat means "hardware is steady", not "row is stale".
3. `nic_tags` is authoritative for the `cn-nic-tags` filter; tritond
   does not re-derive NIC tags from sysinfo.

**`Store` trait additions:**

```rust
async fn put_cn_capacity(&self, ctx: SagaRequestCtx, row: CnCapacity) -> Result<(), StoreError>;
async fn get_cn_capacity(&self, server_uuid: Uuid) -> Result<CnCapacity, StoreError>;
async fn list_cn_capacities(&self) -> Result<Vec<CnCapacity>, StoreError>;
```

`put_cn_capacity` is an unconditional overwrite (the agent is the
single writer; there is no concurrent agent-vs-agent race for one CN
because each CN runs one agent). The `SagaRequestCtx` parameter is
the fencing tuple from RFD 00004 D-Sg-8; for agent writes (not under
a saga), the agent passes `SagaRequestCtx::agent()` (no fencing —
matches handler writes per RFD 00004 SG-1).

## `cn-placement/<server_uuid>` — operator-edited per-CN policy

Written by the operator via `tcadm cn` / adminui. Carries the
soft / hard policy a CN advertises to the placement engine.

```rust
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct CnPlacement {
    pub server_uuid:        Uuid,
    pub reserved:           bool,                          // excluded from automatic placement; force-place still works
    pub reserved_reason:    Option<String>,
    pub traits:             std::collections::BTreeMap<String, String>,
                                                           // operator labels: `gpu=a100`, `pci=zone-a`, `customer=acme`
    pub overprovision_cpu:  Option<f32>,                   // None → use cluster-default (4.0)
    pub overprovision_ram:  Option<f32>,                   // None → use cluster-default (1.0)
    pub overprovision_disk: Option<f32>,                   // None → use cluster-default (1.0)
    pub fault_domain:       Option<String>,                // free-form: `rack-3`, `pdu-a`, `az-east`
    pub pinned_silo_uuid:   Option<Uuid>,                  // see D-Pl-5
    pub pinned_tenant_uuid: Option<Uuid>,                  // see D-Pl-5
    pub cordoned:           bool,                          // drain-only; existing instances keep running
    pub note:               Option<String>,
    pub updated_at:         chrono::DateTime<chrono::Utc>,
    pub updated_by:         tritond_audit::Principal,      // who set the row; surfaces in audit + adminui
}

impl Default for CnPlacement { /* every field None / false / empty; updated_* set on first write */ }
```

**Invariants.**

1. If `pinned_tenant_uuid` is `Some(t)`, then `pinned_silo_uuid` must
   be either `None` or `Some(s)` where `s` is the silo of tenant `t`.
   Enforced at write time inside the FDB transaction by looking up
   the tenant's silo via the existing identity row; rejection surfaces
   as `StoreError::PinConflict { reason }` and is audited.
2. `reserved` is the operator-level "out of service for placement"
   flag; force-place still works (the operator is overriding the
   chain explicitly). `cordoned` is the operator-level "drain-only"
   flag; existing instances keep running, restart still hits the
   same CN. Both default `false`.
3. Setting `overprovision_*` to `None` (the default) means "use the
   cluster default"; the placement engine reads the cluster setting
   at chain build time, not at row write time.

**`Store` trait additions:**

```rust
async fn put_cn_placement(&self, ctx: SagaRequestCtx, row: CnPlacement) -> Result<(), StoreError>;
async fn get_cn_placement(&self, server_uuid: Uuid) -> Result<CnPlacement, StoreError>;
async fn list_cn_placements(&self) -> Result<Vec<CnPlacement>, StoreError>;
```

`put_cn_placement` is the entry point every operator edit funnels
through; the handlers in `tritond` build the row, look up the silo if
`pinned_tenant_uuid` is set, validate the pin conflict invariant, and
call the store. The store is the second line of defence — it
re-validates the invariant inside the FDB transaction so a racing
edit can't sneak past.

## `cn-reservation/<server_uuid>/<saga_id>` — in-flight provisions

Written by the `designate` saga action, deleted by `undesignate` or
by the reaper. The reservation row is the durable record of in-flight
capacity consumption: scorers on the *next* `designate` read the
reservation row and subtract its resources from the CN's free capacity.

```rust
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct CnReservation {
    pub server_uuid:    Uuid,
    pub saga_id:        steno::SagaId,
    pub instance_id:    Uuid,
    pub cpu_units:      u32,                              // 100 = 1 vCPU at 1.0 overprovision; matches the legacy DAPI cpu_cap convention
    pub ram_mb:         u64,
    pub disk:           std::collections::BTreeMap<String, u64>, // per-zpool reservation in bytes
    pub devices:        Vec<DeviceReservation>,
    pub created_at:     chrono::DateTime<chrono::Utc>,
    pub expires_at:     chrono::DateTime<chrono::Utc>,    // saga deadline + slack; reaped if reached without saga termination
    pub created_by_sec: tritond_saga::SecId,              // RFD 00004 D-Sg-8 fencing tuple
    pub created_at_epoch: tritond_saga::SecEpoch,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct DeviceReservation {
    pub kind:  DeviceKind,
    pub model: String,
    pub count: u32,
}
```

**Invariants.**

1. The row is uniquely keyed by `(server_uuid, saga_id)`. The
   `designate` action's FDB transaction inserts it; a second
   `designate` for the same saga is a programming error (the saga
   catalog never calls `designate` twice) and surfaces as a `Store::
   AlreadyExists` if it happens.
2. The `cn-reservation` rows for a CN are part of the residual the
   filter chain reads. The CAS shape is "read every reservation, sum
   its resources, subtract from capacity, run filters, insert this
   reservation, write `Instance.host_cn_uuid`" — all in one FDB
   transaction. A concurrent `designate` against the same CN either
   sees this row in its read set (in which case its residual is
   smaller) or does not, in which case the FDB MVCC retry triggers
   and it re-runs from the start. The second writer never observes
   stale capacity.
3. `expires_at` is a safety net for sagas that lose their SEC and
   never reach `undesignate`. The reaper (in `sweeper.rs`) deletes
   reservations whose `expires_at` has passed *and* whose owning
   saga is in a terminal state (`Done` / `Unwound` / `Stuck`) per
   `tritond-saga`'s record; reservations whose saga is still
   `Running` or `Unwinding` are left alone — the SEC reassignment
   sweep (RFD 00004 D-Sg-4) is the right mechanism to advance them.
4. `created_by_sec` + `created_at_epoch` carry the fencing tuple per
   RFD 00004 D-Sg-8. The store does not require them to match the
   *current* SEC (a reservation written by a now-dead SEC is still
   the durable record of consumed capacity); the audit log records
   the SEC that wrote it.

**`Store` trait additions:**

```rust
async fn reserve_cn_capacity(
    &self,
    ctx: SagaRequestCtx,
    row: CnReservation,
) -> Result<(), StoreError>;

async fn release_cn_reservation(
    &self,
    ctx: SagaRequestCtx,
    server_uuid: Uuid,
    saga_id: steno::SagaId,
) -> Result<(), StoreError>;

async fn list_cn_reservations(
    &self,
    server_uuid: Option<Uuid>,        // None → fleet-wide
) -> Result<Vec<CnReservation>, StoreError>;

async fn get_cn_view_for_pick(
    &self,
    ctx: SagaRequestCtx,
    server_uuid: Uuid,
) -> Result<CnView, StoreError>;
```

The last method is the single read the chain runner needs per
candidate CN: it returns the joined `(CnCapacity, CnPlacement,
Vec<CnReservation>, Vec<Instance>, Option<CnLoadSummary>)` snapshot
inside the same FDB transaction the `reserve_cn_capacity` will close.
Implementing it as one method, not five reads, is the difference
between "one txn per pick" and "five round trips per pick" — the
former composes correctly with the reservation write; the latter
opens a race window.

## `cn-load-summary/<server_uuid>` — materialiser-owned CH rollup

Written by the leader-elected load materialiser (doc 02). Carries
per-CN load history at three windows (5 min, 24 h, 7 d) so the
load-history scorers can read FDB instead of querying ClickHouse on
the hot path.

```rust
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct CnLoadSummary {
    pub server_uuid:        Uuid,

    // CPU utilisation (0.0 .. 1.0 — fraction of total physical cores busy)
    pub cpu_p50_5m:  f32, pub cpu_p95_5m:  f32, pub cpu_max_5m:  f32,
    pub cpu_p50_1d:  f32, pub cpu_p95_1d:  f32, pub cpu_max_1d:  f32,
    pub cpu_p50_7d:  f32, pub cpu_p95_7d:  f32, pub cpu_max_7d:  f32,

    // RAM used (bytes)
    pub ram_used_p95_5m: u64, pub ram_used_p95_1d: u64, pub ram_used_p95_7d: u64,

    // Disk used per zpool (bytes); BTreeMap so the keys match CnCapacity.zpools[].name
    pub disk_used_bytes_p95_5m: std::collections::BTreeMap<String, u64>,
    pub disk_used_bytes_p95_1d: std::collections::BTreeMap<String, u64>,
    pub disk_used_bytes_p95_7d: std::collections::BTreeMap<String, u64>,

    // NIC throughput (bytes/sec)
    pub nic_tx_bps_p95_5m: u64, pub nic_tx_bps_p95_1d: u64, pub nic_tx_bps_p95_7d: u64,
    pub nic_rx_bps_p95_5m: u64, pub nic_rx_bps_p95_1d: u64, pub nic_rx_bps_p95_7d: u64,

    // Sample-thinness gate: if any of these is below a per-window minimum, the row is treated as stale.
    pub samples_5m: u32, pub samples_1d: u32, pub samples_7d: u32,

    pub last_refreshed_at: chrono::DateTime<chrono::Utc>,
    pub stale:             bool,
}
```

**Invariants.**

1. The materialiser writes the row unconditionally on every tick that
   produces fresh data (capacity-only scorers don't care; load
   scorers gate on `stale`). It does *not* try to be clever about
   "only write if changed" — the row is small and FDB MVCC handles
   the no-op write cheaply.
2. `stale` is set by the materialiser when (a) the last refresh is
   older than `staleness_ticks × interval`, (b) the sample count for
   a window is below a per-window minimum (catches a CN that came
   online minutes ago — its 7d window is genuinely empty, not "the
   materialiser is broken"), or (c) the ClickHouse query for this CN
   returned an error. The materialiser writes the row anyway with
   `stale = true` so the heatmap can distinguish "no data" from "data
   says zero".
3. Load-history scorers (`score-avoid-hot-now`, `score-avoid-peaky`,
   `score-prefer-low-baseline`, `score-diurnal-fit`) check `stale`
   and contribute 0.0 when true; the `ExplainReport` names the skip.

**`Store` trait additions:**

```rust
async fn put_cn_load_summary(&self, ctx: SagaRequestCtx, row: CnLoadSummary) -> Result<(), StoreError>;
async fn get_cn_load_summary(&self, server_uuid: Uuid) -> Result<Option<CnLoadSummary>, StoreError>;
async fn list_cn_load_summaries(&self) -> Result<Vec<CnLoadSummary>, StoreError>;
```

The read returns `Option` because a CN that just registered has no
summary yet — distinct from a `stale` row (which exists but is not
trustworthy).

## `instance-affinity/<instance_id>` — per-instance rules

Written at instance create (the affinity / anti-affinity / topology
spread the request carried), editable by the operator via
`tcadm instance affinity set`. Future restart / move actions read
this row; v1 reads it during the initial `designate`.

```rust
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct InstanceAffinity {
    pub instance_id: Uuid,
    pub rules:       Vec<AffinityRule>,
    pub spread:      Option<TopologySpread>,
    pub updated_at:  chrono::DateTime<chrono::Utc>,
    pub updated_by:  tritond_audit::Principal,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct AffinityRule {
    pub kind:     AffinityKind,
    pub scope:    AffinityScope,            // Required | Preferred
    pub op:       AffinityOp,               // In | NotIn | Spread (Spread is only valid on TopologySpread)
    pub selector: AffinitySelector,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case", tag = "kind", content = "value")]
pub enum AffinityKind { VmToVm, VmToHost, Topology }

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum AffinityScope { Required, Preferred }

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum AffinityOp { In, NotIn }

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum AffinitySelector {
    InstanceIds(Vec<Uuid>),                            // vm_to_vm
    InstanceTagMatch { key: String, value: String },   // vm_to_vm by tag
    CnUuids(Vec<Uuid>),                                // vm_to_host
    CnTraitMatch { key: String, value: String },       // vm_to_host by trait
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct TopologySpread {
    pub key:      TopologyKey,           // FaultDomain | CnUuid | Trait(String)
    pub max_skew: u32,
    pub scope:    AffinityScope,         // Required | Preferred
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case", tag = "kind", content = "value")]
pub enum TopologyKey { FaultDomain, CnUuid, Trait(String) }
```

**Invariants.**

1. A `Required` rule that cannot be satisfied is a `Reject` from the
   `cn-affinity-required` filter; `Preferred` rules are inputs to
   `score-affinity-preferred`.
2. DAPI-style `affinity:container==foo` request strings are sugar at
   the API boundary; the request handler desugars them into
   `AffinityRule` rows before persisting, and `tcadm instance
   affinity show` renders the desugared form. (See open question in
   the README about whether to preserve the original syntax.)
3. The `instance-affinity` row is created in the same FDB transaction
   as the `Instance` row; an instance with no affinity rules has an
   empty `rules: []` row, not the absence of a row, so the read path
   is a single get rather than "get-or-default".

**`Store` trait additions:**

```rust
async fn put_instance_affinity(&self, ctx: SagaRequestCtx, row: InstanceAffinity) -> Result<(), StoreError>;
async fn get_instance_affinity(&self, instance_id: Uuid) -> Result<InstanceAffinity, StoreError>;
async fn list_instance_affinities_for_tenant(
    &self,
    tenant_id: Uuid,
) -> Result<Vec<InstanceAffinity>, StoreError>;
```

The `list_instance_affinities_for_tenant` read backs the
`score-fewer-cotenant-zones` and topology-spread scorers — they need
to know every existing instance's rules to compute "would placing
this new instance here keep the spread within skew?".

## The joined view: `CnView`

The chain runner consumes one `CnView` per candidate CN, returned by
`Store::get_cn_view_for_pick` inside the `designate` transaction:

```rust
// tritond-placement/src/types.rs
pub struct CnView {
    pub cn:               Cn,                          // existing tritond-store row (state, role, last_seen, ...)
    pub capacity:         Option<CnCapacity>,          // None → cn-capacity row missing; every filter rejects
    pub placement:        CnPlacement,                 // Default::default() if no row
    pub reservations:     Vec<CnReservation>,          // every row under cn-reservation/<server_uuid>/
    pub instances:        Vec<Instance>,               // every Instance with host_cn_uuid = this cn (for affinity scoring)
    pub load_summary:     Option<CnLoadSummary>,       // None → never materialised; load scorers contribute 0.0
    pub computed_at:      chrono::DateTime<chrono::Utc>,
}

pub struct PlacementRequest {
    pub instance_id:           Uuid,                    // the instance being placed
    pub silo_uuid:             Uuid,
    pub tenant_uuid:           Uuid,
    pub project_uuid:          Uuid,
    pub cpu_units:             u32,                     // package shape (D-Pl-6 open question on package store)
    pub ram_mb:                u64,
    pub disk:                  std::collections::BTreeMap<String, u64>,   // per-pool — empty means "any pool with room"
    pub required_traits:       std::collections::BTreeMap<String, String>,
    pub required_nic_tags:     Vec<String>,
    pub required_underlay:     UnderlayCapability,
    pub required_devices:      Vec<DeviceReservation>,  // GPU model + count, SR-IOV VF model + count
    pub needs_hvm:             bool,
    pub min_platform:          Option<String>,
    pub affinity:              InstanceAffinity,        // the row that will be persisted on success
    pub strategy:              Strategy,                // Spread | Pack | Balanced
    pub fault_domain_spread:   bool,                    // top-level toggle layered on top of strategy
    pub force_cn:              Option<Uuid>,            // operator override (D-Pl-8)
    pub ignore_scope_pin:      bool,                    // operator override (D-Pl-5)
}
```

The `CnView` is the engine's only input; the `PlacementRequest` is
the only thing that varies per call. Making both pure data types
means every filter / scorer is a pure function over them, and the
test suite can drive any pick with a hand-built `CnView` slice and
a hand-built `PlacementRequest`.

## Audit rows

Each `cn-placement` edit, each `instance-affinity` edit, each
force-place, and each `designate` writes an audit row through the
existing `tritond-audit` substrate. Saga lifecycle events
(`operation_started` / `_step` / `_finished` for `designate` and
`undesignate`) follow RFD 00004 D-Sg-11: saga lifecycle to the
fleet chain (`audit/saga/fleet`), the side-effect rows
(`cn-placement` edit, reservation insert, `Instance.host_cn_uuid`
write, `instance-affinity` write) to the per-silo chain of the
silo whose instance was placed, cross-linked by `operation_id`.

The `cn-placement` and CN-edit rows (`Cn::Reserve`, `Cn::Pin`, …)
land in the fleet chain (they are operator actions on fleet
infrastructure, not on a tenant's resource); the `instance-
affinity` and force-place rows land in the per-silo chain (they
mutate a tenant's instance state).

## What `MemStore` does

The in-memory `MemStore` ships matching impls for every method
above. The reservation race test in PL-2 runs against `MemStore`
with synthetic concurrency (two `tokio::spawn`s that both try to
reserve the same CN); the `MemStore` re-validates the residual on
every write under a `parking_lot::Mutex` so the test reproduces the
FDB MVCC retry shape without needing the FDB binary. `FdbStore`'s
implementation uses real FDB transactions; the trait is identical.

## What goes wrong if we cut corners

- **One row mixing capacity + placement + reservations.** The agent
  and the operator and the saga all write to the same row, and
  every write becomes "read-modify-write of the giant row". A
  concurrent agent capacity refresh blows away the operator's pin
  edit; the saga's reservation write blows away the materialiser's
  load summary. Five rows, one writer each, no fight.
- **Reservation as a counter rather than a row.** A counter saves
  one read but loses every other property: which saga holds the
  reservation, when it was taken, when it expires, what kind of
  device / pool was reserved. The reaper can't tell what's safe to
  free; the audit log can't tell who took capacity. A row per
  reservation is small and right.
- **`pick` and `reserve` in two transactions.** The window between
  them is small but real, and the failure mode (two provisions
  land on the same CN, exceed capacity, agent provisioning fails
  late) is exactly the operator pain DAPI also has on its
  not-yet-acked picks. One transaction or it's not a fix.
- **Synchronous ClickHouse query per pick.** `tritond` becomes
  unavailable when ClickHouse is unavailable; every provision adds
  a multi-second range query; the CH cluster sees `N_tritond_peers
  × N_picks_per_second` traffic. Materialise once per minute, read
  from FDB; degrade cleanly on staleness.
