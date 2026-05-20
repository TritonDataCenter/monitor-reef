// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Trait surface + projection shapes.
//!
//! RFD 00005 doc 02 §"The traits" is the canonical reference; this
//! module is the Rust embodiment. The projection types ([`CnView`],
//! [`CapacityView`], [`PlacementPolicyView`], [`CnLoadSummaryView`],
//! [`ReservationView`]) are *not* the FDB row types — they are the
//! engine's read-only view of those rows. PL-2 introduces the
//! canonical row types in `tritond-store::types` and a Store method
//! that materialises a `CnView` from them inside one read snapshot.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::config::OverprovisionDefaults;

// ---------------------------------------------------------------------------
// Traits.
// ---------------------------------------------------------------------------

/// One stage of the hard-filter phase. Filters are pure functions
/// over the projection shapes; they reject ineligible CNs with a
/// reason that lands in the [`ExplainReport`].
///
/// Built-in filters live in [`crate::filter`] (PL-3 fills the
/// module). Out-of-tree filters live in sibling workspace crates
/// pulled in behind cargo features on `tritond` (D-Pl-1).
pub trait Filter: Send + Sync {
    /// Kebab-case stable id. Used in the FDB-backed chain config and
    /// in every [`ExplainReport`] entry; must be unique across the
    /// process registry.
    fn name(&self) -> &'static str;

    /// Pure function. Must not perform I/O, must not panic on
    /// well-formed input. A panic is treated as a synthetic
    /// `Reject { reason: "filter panicked: …" }` by the chain
    /// runner when running on a `panic = "unwind"` profile; this
    /// workspace builds with `panic = "abort"` so panicking is a
    /// hard crash — write your filter to handle every input.
    fn evaluate(&self, cn: &CnView, req: &PlacementRequest, ctx: &ChainContext) -> Verdict;
}

/// One stage of the soft-score phase. Scorers run only on CNs that
/// passed every filter; each returns a normalised 0.0..=1.0
/// contribution that the chain runner multiplies by the configured
/// weight and sums (D-Pl-1).
pub trait Scorer: Send + Sync {
    /// Kebab-case stable id. Same uniqueness requirement as
    /// [`Filter::name`].
    fn name(&self) -> &'static str;

    /// Default weight when the active [`crate::PlacementConfig`]
    /// doesn't pin a specific weight for this scorer. The strategy
    /// preset ([`Strategy`]) may override this default before the
    /// runner is built.
    fn default_weight(&self) -> f32;

    /// Pure function returning a 0.0..=1.0 contribution. Out-of-range
    /// returns are clamped (and logged) by the runner; NaN is
    /// treated as 0.0.
    fn score(&self, cn: &CnView, req: &PlacementRequest, ctx: &ChainContext) -> f32;
}

// ---------------------------------------------------------------------------
// Filter verdict + chain context.
// ---------------------------------------------------------------------------

/// What a [`Filter`] returns. `Skip` means "this filter does not
/// apply to this CN/request" — neither accept nor reject; the
/// `ExplainReport` notes the skip but treats it as a pass for the
/// purposes of advancing to the next filter.
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case", tag = "verdict")]
pub enum Verdict {
    Accept,
    Reject { reason: String },
    Skip,
}

impl Verdict {
    pub fn is_accept(&self) -> bool {
        matches!(self, Verdict::Accept | Verdict::Skip)
    }

    pub fn reject<S: Into<String>>(reason: S) -> Self {
        Verdict::Reject {
            reason: reason.into(),
        }
    }
}

/// The bag of "things every filter and scorer needs but that don't
/// belong on [`CnView`] or [`PlacementRequest`]".
///
/// Borrowed from the call site so the runner doesn't pay an Arc
/// clone per filter; the runner builds one of these at the top of
/// [`crate::ChainRunner::pick`] and threads it through every trait
/// call.
#[derive(Clone, Debug)]
pub struct ChainContext<'a> {
    /// Clock the runner snapshots at the top of `pick` so every
    /// filter / scorer sees the same `now` even if the chain takes
    /// non-trivial wall time.
    pub now: DateTime<Utc>,

    /// Cluster-default overprovision ratios. Per-CN
    /// [`PlacementPolicyView::overprovision_cpu`] /
    /// [`PlacementPolicyView::overprovision_ram`] override these
    /// per CN when set.
    pub cluster_overprovision: OverprovisionDefaults,

    /// Seconds beyond which a [`CnLoadSummaryView`] is considered
    /// stale. Load-history scorers contribute zero on stale rows
    /// and the explain report names the skip.
    pub load_staleness_secs: u64,

    /// Seconds beyond which a `cn.last_seen` heartbeat marks the
    /// CN as not-live for the `cn-approved-and-live` filter. The
    /// agent heartbeats every ~5 s (Slice D); the placement
    /// threshold is a multiple to absorb transient gaps.
    pub agent_heartbeat_threshold_secs: u64,

    /// Resolved weights for the active strategy. The runner consults
    /// this on every scorer call; switching strategy rebuilds the
    /// runner with a different vector.
    pub strategy_weights: &'a StrategyWeights,

    /// Tenant-scoped instance projections used by spread / cotenant
    /// scorers. The slice is built by the caller (the saga step) by
    /// listing every `Instance` whose tenant matches
    /// `request.tenant_uuid` (and, for silo-scope contributions,
    /// every instance whose silo matches `request.silo_uuid`).
    pub sibling_instances: &'a [SiblingInstanceView],
}

// ---------------------------------------------------------------------------
// Strategy preset.
// ---------------------------------------------------------------------------

/// Preset weight vectors applied on top of the same scorer set
/// (D-Pl-6). Default is `Spread`.
#[derive(
    Clone, Copy, Debug, Default, Eq, PartialEq, Hash, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum Strategy {
    /// Spread tenant instances across fault domains; the default
    /// (matches what legacy DAPI's default weights produced).
    #[default]
    Spread,

    /// Pack tenant instances onto fewer CNs; capacity-planning
    /// corners and dev clusters. Load-history scorers stay on —
    /// packing into a p95-90% CN is still wrong.
    Pack,

    /// Even compromise — half spread, half pack.
    Balanced,
}

/// Resolved per-scorer weight vector after the strategy preset and
/// any operator overrides have been applied.
///
/// Indexed by scorer `name()`. The runner consults this on every
/// scorer call; a scorer missing from the map contributes zero.
#[derive(Clone, Debug, Default)]
pub struct StrategyWeights(pub BTreeMap<&'static str, f32>);

impl StrategyWeights {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&mut self, name: &'static str, weight: f32) -> &mut Self {
        self.0.insert(name, weight);
        self
    }

    pub fn get(&self, name: &str) -> Option<f32> {
        self.0.get(name).copied()
    }

    /// Snapshot the weight vector into a serialisable form for the
    /// [`ExplainReport`]. Keys are owned strings so the report can
    /// outlive the runner.
    pub fn to_report(&self) -> BTreeMap<String, f32> {
        self.0
            .iter()
            .map(|(name, weight)| ((*name).to_string(), *weight))
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Placement request.
// ---------------------------------------------------------------------------

/// Everything the engine needs to know about the workload being
/// placed. Built by the saga step (`designate`) from the
/// instance-create params; built by the materialised dry-run path
/// from the `/v2/placement/pick` request body.
///
/// The Silo / Tenant / Project triple comes straight from the
/// requesting principal (`IMDS_DESIGN.md`'s hierarchy, also locked
/// in STATUS.md #15 / #30). Project is *not* a placement scope
/// (D-Pl-5) — it rides on the request only so spread / cotenant
/// scorers can count project-scoped siblings.
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PlacementRequest {
    pub instance_id: Uuid,
    pub silo_uuid: Uuid,
    pub tenant_uuid: Uuid,
    pub project_uuid: Uuid,
    pub role: CnRoleView,

    /// 1 vCPU = 100 cpu_units (legacy DAPI's `cpu_cap` convention).
    pub cpu_units: u32,
    pub ram_mb: u64,

    /// Per-pool disk asks. The empty map means "any pool, any size"
    /// (only valid for placement holds that don't allocate disk);
    /// callers normally translate the package's disk shape into a
    /// non-empty map.
    #[serde(default)]
    pub disk: BTreeMap<String, u64>,

    #[serde(default)]
    pub required_traits: BTreeMap<String, String>,
    #[serde(default)]
    pub required_nic_tags: Vec<String>,

    pub required_underlay: UnderlayCapability,

    #[serde(default)]
    pub required_devices: Vec<DeviceReservation>,

    #[serde(default)]
    pub needs_hvm: bool,

    /// Matches legacy DAPI `min_platform`. `None` means "no
    /// constraint".
    #[serde(default)]
    pub min_platform: Option<String>,

    /// Affinity / anti-affinity / topology-spread rules for the
    /// instance being placed. The `cn-affinity-required` filter
    /// reads the hard half; the `score-affinity-preferred` scorer
    /// (PL-4) reads the soft half.
    pub affinity: tritond_store::InstanceAffinity,

    /// Per-request strategy override. `None` means "use cluster
    /// default" - the runner resolves this at the top of `pick`.
    #[serde(default)]
    pub strategy_override: Option<Strategy>,

    /// Operator-initiated force-place: skip the chain and place on
    /// this specific CN. The scope-pin filter still runs unless
    /// `ignore_scope_pin` is also set. The reservation is still
    /// taken in the same FDB transaction; the audit row carries
    /// `force_cn: true` (D-Pl-8).
    #[serde(default)]
    pub force_cn: Option<Uuid>,

    /// Bypass the silo / tenant scope pin. Only honoured together
    /// with [`Self::force_cn`]; an `ignore_scope_pin` without a
    /// force is a programming error rejected at the API edge
    /// (D-Pl-5).
    #[serde(default)]
    pub ignore_scope_pin: bool,

    /// Deadline by which the reservation row this pick takes must
    /// be reaped if the saga doesn't complete (D-Pl-7's stale-claim
    /// threshold). The store layer sets the reservation TTL from
    /// this.
    pub deadline: DateTime<Utc>,

    /// CNs that must not be picked by this placement request,
    /// regardless of every other filter. The migration designate
    /// action populates this with the source CN's uuid so the chain
    /// cannot pick the same host the VM already lives on. Honored
    /// even under [`Self::force_cn`]: an explicit force at a CN in
    /// this list rejects.
    ///
    /// Default empty — `instance-create` doesn't set it.
    #[serde(default)]
    pub avoid_cn: Vec<Uuid>,

    /// Migration-specific compatibility fingerprint of the source CN
    /// + dataset, set on placement requests originating from the
    /// `designate_for_migration` saga action. Used by the
    /// `cn-bhyve-compatible`, `cn-cpu-feature-superset`,
    /// `cn-time-synced`, and `cn-zfs-compatible` filters to reject
    /// targets the source can't safely hand off to.
    ///
    /// Default `None` — `instance-create` doesn't set it, and the
    /// four migration filters return `Verdict::Skip` when it is
    /// absent.
    #[serde(default)]
    pub migration: Option<MigrationCompat>,
}

/// Source-side compatibility fingerprint for live-migration target
/// selection. Built by the migration saga from the source CN's
/// agent-reported [`CapacityView`] (with its `migration` fields
/// populated by the LM-0 capability probe) plus the source dataset
/// properties read at saga begin time.
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MigrationCompat {
    /// vmm-migrate wire protocol the source's tritonagent speaks
    /// (e.g. `"vmm-migrate-ron/0"`). Target must report the same
    /// string for the handshake to succeed.
    pub vmm_protocol_version: String,

    /// CPU feature flags the bhyve userspace exposes to the guest on
    /// the source (e.g. `["vmx", "avx2", "sse4_2", "aes"]`). Target
    /// must report a superset; a missing flag would cause the guest
    /// to `#UD` on resume.
    pub cpu_features: Vec<String>,

    /// Source's NTP offset relative to UTC in nanoseconds. Migration
    /// is rejected if the target's offset differs by more than 100ms
    /// (bhyve TSC import is sensitive to large skews).
    pub tsc_offset_ns: i64,

    /// Source dataset properties the target zpool must support. Keyed
    /// by zpool name on the source. The migration only carries the
    /// zpool that actually backs the VM (in v1, the single `zones`
    /// pool); the field is map-shaped so multi-dataset migrations can
    /// extend it without a schema bump.
    #[serde(default)]
    pub zpool_props: BTreeMap<String, ZpoolPropFingerprint>,

    /// Whether the source's primary dataset has `encryption=on`. v1
    /// punts encrypted-source migration; the saga handler rejects at
    /// the API edge so this never reaches the chain. The flag is
    /// carried here for symmetry with the wire types and so a future
    /// `cn-not-encrypted-source` filter can short-circuit the chain.
    #[serde(default)]
    pub source_dataset_encrypted: bool,
}

/// Per-zpool ZFS properties the migration compares between source
/// and target. Only the values that affect on-disk format
/// compatibility live here — performance-only knobs (atime, sync,
/// etc.) are out of scope.
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ZpoolPropFingerprint {
    /// `zfs get encryption`: `"off"`, `"on"`, or `"aes-256-gcm"` etc.
    pub encryption: String,
    /// `zfs get compression`: `"off"`, `"lz4"`, `"zstd"`, etc.
    pub compression: String,
    /// `zfs get recordsize` in bytes (e.g. `131072` for 128K).
    pub recordsize_bytes: u32,
}

/// Per-device reservation ask. PL-1 ships the shape; the agent-side
/// inventory of GPUs / SR-IOV VFs is deferred to a later slice (see
/// RFD 00005 README open question).
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DeviceReservation {
    pub kind: DeviceKind,
    pub model: String,
    pub count: u32,
}

// ---------------------------------------------------------------------------
// `CnView` — the engine's projection over all five FDB sources.
// ---------------------------------------------------------------------------

/// What a filter / scorer sees of one compute node.
///
/// The projection bundles every input the chain needs from FDB:
/// the basic `Cn` registration (state / role / last_seen), the
/// agent-published structured capacity, the operator-edited
/// placement policy, the saga-owned in-flight reservations, the
/// materialiser-owned ClickHouse rollup, and the host-bound sibling
/// instances. PL-2 ships the Store method that builds one of these
/// inside a single FDB read snapshot; PL-1 lets callers build them
/// from scratch for tests.
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CnView {
    pub server_uuid: Uuid,
    pub hostname: String,
    pub state: CnStateView,
    pub role: CnRoleView,

    /// `None` until Slice D's heartbeater has reported. The
    /// `cn-approved-and-live` filter rejects rows older than the
    /// agent heartbeat threshold.
    pub last_seen: Option<DateTime<Utc>>,

    /// Agent-published structured capacity. `None` for a CN that
    /// has not yet posted a `cn-capacity` row (every filter rejects
    /// such rows with reason "cn-capacity row absent" per RFD 00005
    /// invariant 7).
    pub capacity: Option<CapacityView>,

    /// Operator-edited placement policy. Always present (defaults
    /// are the "fresh CN" shape).
    pub placement: PlacementPolicyView,

    /// In-flight reservations against this CN (saga-owned). The
    /// runner sums these into the residual the capacity filters
    /// check against.
    #[serde(default)]
    pub active_reservations: Vec<ReservationView>,

    /// Materialiser-owned ClickHouse rollup. `None` when no
    /// summary exists; load-history scorers contribute zero in that
    /// case and the `ExplainReport` notes the skip.
    pub load_summary: Option<CnLoadSummaryView>,

    /// Instances already host-bound to this CN. Carries enough per
    /// instance for the resource filters (cpu_units + ram_mb sum
    /// into the residual) and for the cotenant / affinity scorers
    /// (silo_uuid + tenant_uuid). PL-5's Store join populates this
    /// from the existing `instance/in_host_cn` membership index.
    #[serde(default)]
    pub assigned_instances: Vec<AssignedInstanceView>,
}

/// Projection of one `Instance` row already host-bound to a CN.
/// Only the fields the filters and scorers need; the join from the
/// canonical `tritond_store::Instance` row happens at PL-5's
/// `get_cn_view_for_pick`.
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AssignedInstanceView {
    pub instance_id: Uuid,
    pub silo_uuid: Uuid,
    pub tenant_uuid: Uuid,
    /// 1 vCPU = 100 cpu_units (legacy DAPI's `cpu_cap` convention).
    pub cpu_units: u32,
    pub ram_mb: u64,
}

/// Mirror of `tritond_store::CnState` so the placement crate doesn't
/// have to take a path dep on `tritond-store` at PL-1 just to name
/// the lifecycle states. PL-2 reconciles by either taking the dep
/// (and `pub use`-ing the canonical enum) or keeping the mirror and
/// supplying a `From` impl on the store side — decide at PL-2.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CnStateView {
    Pending,
    Approved,
    Disabled,
}

/// Mirror of `tritond_store::CnRole`. Same PL-2 reconciliation note
/// as [`CnStateView`].
#[derive(
    Clone, Copy, Debug, Default, Eq, PartialEq, Hash, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum CnRoleView {
    #[default]
    Tenant,
    Edge,
    Both,
}

impl CnRoleView {
    /// Does a CN holding this role satisfy a request that asks for
    /// `required`? `Both` accepts every ask; `Tenant`/`Edge` accept
    /// only their kind.
    pub fn satisfies(self, required: CnRoleView) -> bool {
        matches!(
            (self, required),
            (CnRoleView::Both, _)
                | (CnRoleView::Tenant, CnRoleView::Tenant)
                | (CnRoleView::Edge, CnRoleView::Edge)
        )
    }
}

/// Structured capacity reported by `tritonagent`. The shape mirrors
/// the canonical `tritond_store::CnCapacity` row PL-2 introduces;
/// PL-1 keeps it inline so the engine has something to compile
/// against. The PL-2 reconciliation is the same as the [`CnStateView`]
/// note.
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CapacityView {
    pub cpu_cores_physical: u32,
    pub cpu_threads_logical: u32,

    /// Length == 1 on a UMA box; multi-node boxes carry per-node
    /// cores + RAM.
    pub numa_nodes: Vec<NumaNodeView>,
    pub ram_total_mb: u64,

    pub zpools: Vec<ZpoolView>,
    pub nic_tags: Vec<String>,
    pub underlay: UnderlayCapability,
    pub devices: Vec<DeviceView>,

    /// SmartOS / illumos platform ID; matches legacy DAPI
    /// `min_platform`.
    pub platform_version: String,

    /// `cn-capacity` agent-side clock when the row was last
    /// published. Used to gate stale capacity reports.
    pub reported_at: DateTime<Utc>,

    /// Whether the CN's CPU advertises hardware virtualisation
    /// extensions (VMX / SVM). The `cn-hvm-supported` filter
    /// consults this for bhyve / KVM brands.
    #[serde(default)]
    pub hvm_supported: bool,

    // ----- migration-compatibility fingerprint -----
    //
    // These four fields are populated by the LM-0 tritonagent
    // capability probe and consumed by the migration filters. They
    // are `Option` / empty-default-shaped so that CNs whose agents
    // haven't been upgraded to ship them yet still pass through the
    // chain on non-migration requests; the migration filters
    // explicitly `Skip` when the matching field is absent.
    /// vmm-migrate wire protocol the agent's userspace speaks
    /// (e.g. `"vmm-migrate-ron/0"`). `None` until the agent reports
    /// the capability; the `cn-bhyve-compatible` filter rejects in
    /// that case when a migration request is in flight.
    #[serde(default)]
    pub vmm_protocol_version: Option<String>,

    /// CPU feature flags bhyve exposes to guests on this CN. Default
    /// empty; the `cn-cpu-feature-superset` filter rejects when the
    /// source asks for a feature not in this set.
    #[serde(default)]
    pub cpu_features: Vec<String>,

    /// CN's NTP offset relative to UTC in nanoseconds. `None` when
    /// the agent hasn't reported a probe; the `cn-time-synced`
    /// filter rejects in that case for migration requests.
    #[serde(default)]
    pub tsc_offset_ns: Option<i64>,

    /// Per-zpool ZFS property fingerprint, keyed by pool name.
    /// Default empty; the `cn-zfs-compatible` filter requires every
    /// source pool to be present here with a compatible value.
    #[serde(default)]
    pub zpool_props: BTreeMap<String, ZpoolPropFingerprint>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct NumaNodeView {
    pub node_id: u8,
    pub cores: u32,
    pub ram_mb: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ZpoolView {
    pub name: String,
    pub total_bytes: u64,
    pub free_bytes: u64,
    pub tier: StorageTier,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum StorageTier {
    Ssd,
    Nvme,
    Hdd,
    Mixed,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct UnderlayCapability {
    pub ipv4: bool,
    pub ipv6: bool,
}

impl UnderlayCapability {
    /// Is this CN's underlay sufficient for a request that requires
    /// `req`? "Sufficient" means every protocol the request needs
    /// is also reported by the CN.
    pub fn satisfies(self, req: UnderlayCapability) -> bool {
        (!req.ipv4 || self.ipv4) && (!req.ipv6 || self.ipv6)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DeviceView {
    pub kind: DeviceKind,
    pub model: String,
    pub free_count: u32,
}

#[derive(
    Clone,
    Copy,
    Debug,
    Eq,
    PartialEq,
    Ord,
    PartialOrd,
    Hash,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum DeviceKind {
    Gpu,
    SrIovVf,
}

/// Operator-edited per-CN placement policy. Defaults are the
/// "fresh CN" shape: not reserved, not cordoned, no pins, no traits,
/// no overprovision overrides, no fault-domain tag.
#[derive(Clone, Debug, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PlacementPolicyView {
    #[serde(default)]
    pub reserved: bool,

    #[serde(default)]
    pub cordoned: bool,

    /// "Why cordoned" — set to `"drain"` by `tcadm cn drain` (PL-7).
    /// Until drain lands, `cn-not-evacuating` is effectively
    /// `cn-not-cordoned`.
    #[serde(default)]
    pub cordoned_reason: Option<String>,

    #[serde(default)]
    pub pinned_silo_uuid: Option<Uuid>,
    #[serde(default)]
    pub pinned_tenant_uuid: Option<Uuid>,

    #[serde(default)]
    pub traits: BTreeMap<String, String>,

    #[serde(default)]
    pub overprovision_cpu: Option<f32>,
    #[serde(default)]
    pub overprovision_ram: Option<f32>,

    /// Free-form operator label used by the spread / pack scorers.
    /// Two CNs with the same `fault_domain` are co-located from
    /// placement's perspective.
    #[serde(default)]
    pub fault_domain: Option<String>,
}

/// A `cn-reservation/<cn>/<saga>` row, projected. The runner sums
/// `cpu_units` and `ram_mb` across active reservations to compute
/// the residual the capacity filters check against (D-Pl-2).
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ReservationView {
    pub saga_id: Uuid,
    pub instance_id: Uuid,
    pub cpu_units: u32,
    pub ram_mb: u64,
    pub disk: BTreeMap<String, u64>,
    pub devices: Vec<DeviceReservation>,
    pub deadline: DateTime<Utc>,
}

/// A `cn-load-summary/<cn>` row, projected. Materialiser-owned
/// (PL-6); read by load-history scorers. `stale = true` rows are
/// still written so the heatmap can distinguish "data says nothing"
/// from "no data exists" (RFD 00005 doc 02 §"Staleness").
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CnLoadSummaryView {
    pub last_refreshed_at: DateTime<Utc>,
    pub stale: bool,

    /// CPU utilisation (0.0..=1.0) median / 95th percentile over the
    /// last 5 minutes / 1 day / 7 days.
    pub cpu_p50_5m: f32,
    pub cpu_p95_5m: f32,
    pub cpu_p50_1d: f32,
    pub cpu_p95_1d: f32,
    pub cpu_p95_7d: f32,

    /// RAM utilisation (0.0..=1.0) 95th percentile over the last
    /// 5 minutes.
    pub ram_used_p95_5m: f32,

    /// NIC tx + rx in bytes/sec averaged over the last 5 minutes.
    /// Used by load-aware scorers in PL-4; PL-1 carries the field
    /// so the projection shape is stable.
    pub nic_tx_bps_p95_5m: u64,
    pub nic_rx_bps_p95_5m: u64,
}

/// Projection of an `Instance` row for spread / cotenant scorers.
/// Only the fields the scorers need.
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SiblingInstanceView {
    pub instance_id: Uuid,
    pub silo_uuid: Uuid,
    pub tenant_uuid: Uuid,
    pub project_uuid: Uuid,

    /// `None` if the instance is mid-saga and not yet pinned.
    #[serde(default)]
    pub host_cn_uuid: Option<Uuid>,

    /// `CnPlacement.fault_domain` of the host CN, if any.
    /// Populated by the saga step that builds the slice (PL-5);
    /// the `score-spread-by-fault-domain` and
    /// `score-pack-by-fault-domain` scorers compare it against
    /// the candidate CN's fault_domain.
    #[serde(default)]
    pub host_fault_domain: Option<String>,
}
