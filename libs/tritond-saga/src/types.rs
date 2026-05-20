// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Shared value types persisted in the SecStore and threaded through
//! `SagaContext`. See RFD 00004 doc 01.

use std::fmt;

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use steno::SagaId;
use uuid::Uuid;

/// Stable identifier for a saga execution coordinator (one
/// `tritond` instance). Persisted in `SagaRecord.creator_sec` and
/// `current_sec`; used to scope `by_sec` indices and heartbeats.
#[derive(
    Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct SecId(pub Uuid);

impl SecId {
    pub const fn new(id: Uuid) -> Self {
        Self(id)
    }

    pub fn random() -> Self {
        Self(Uuid::new_v4())
    }

    pub fn as_uuid(&self) -> Uuid {
        self.0
    }
}

impl fmt::Display for SecId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

/// Monotonic fence epoch for a saga's current owner SEC. Bumped on
/// every adoption and every recovery hop. Every action-issued side
/// effect carries `(sec_id, epoch)` so receivers can reject calls
/// from a stale owner. Invariant 8 / D-Sg-8.
#[derive(
    Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct SecEpoch(pub u64);

impl SecEpoch {
    pub const fn new(v: u64) -> Self {
        Self(v)
    }

    pub const ZERO: Self = Self(0);

    pub fn bump(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

impl fmt::Display for SecEpoch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

/// Fencing tuple action bodies thread into store mutations and
/// `enqueue_job` calls. See RFD 00004 doc 01.
#[derive(Clone, Copy, Debug)]
pub struct SagaRequestCtx {
    pub saga_id: SagaId,
    pub sec_id: SecId,
    pub epoch: SecEpoch,
}

impl SagaRequestCtx {
    pub fn new(saga_id: SagaId, sec_id: SecId, epoch: SecEpoch) -> Self {
        Self {
            saga_id,
            sec_id,
            epoch,
        }
    }
}

/// Stable identifier for a resource scope a saga can touch. Used
/// in [`ResourceRef`] to index sagas by resource so a per-resource
/// view (the VM detail page's "operations" subtab, a CN's saga
/// log, a tenant's audit pane) can resolve "what sagas touched
/// me" without scanning every saga.
///
/// Wire form is `snake_case` so the FDB index key + the JSON
/// representation are the same string and easy to grep:
///
/// ```text
/// saga/by_ref/instance/<uuid>/<inverted_ms>/<saga_id>
/// ```
///
/// Adding a scope is non-breaking — old records with unknown
/// scopes deserialise as `Other(String)` via the `#[serde(other)]`
/// catch-all and stay queryable by their raw kebab name.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ResourceScope {
    /// Fleet-wide. Used by sagas that touch global state
    /// (`node_join`, `image_import` of a public image).
    Fleet,
    Silo,
    Tenant,
    Project,
    Vpc,
    Subnet,
    /// A compute node (`CN`). The scope id is the server UUID.
    Cn,
    Instance,
    Nic,
    Disk,
    Image,
    FloatingIp,
    NatGateway,
    Route,
    RouteTable,
    /// An `EdgeCluster` (route_with_nat_edge / nat_gateway sagas).
    EdgeCluster,
    /// A `ProvisioningJob` row the saga enqueued. Lets the
    /// per-job view show the saga that produced it.
    Job,
}

impl ResourceScope {
    /// Stable kebab string used as the FDB key segment. Must match
    /// the `serde(rename_all = "snake_case")` output exactly so the
    /// wire JSON and the index key agree.
    pub fn as_str(self) -> &'static str {
        match self {
            ResourceScope::Fleet => "fleet",
            ResourceScope::Silo => "silo",
            ResourceScope::Tenant => "tenant",
            ResourceScope::Project => "project",
            ResourceScope::Vpc => "vpc",
            ResourceScope::Subnet => "subnet",
            ResourceScope::Cn => "cn",
            ResourceScope::Instance => "instance",
            ResourceScope::Nic => "nic",
            ResourceScope::Disk => "disk",
            ResourceScope::Image => "image",
            ResourceScope::FloatingIp => "floating_ip",
            ResourceScope::NatGateway => "nat_gateway",
            ResourceScope::Route => "route",
            ResourceScope::RouteTable => "route_table",
            ResourceScope::EdgeCluster => "edge_cluster",
            ResourceScope::Job => "job",
        }
    }

    /// Parse a kebab string back into a scope. Returns `None` for
    /// an unknown scope (callers map this to a 400 — we don't want
    /// a silent fall-through).
    pub fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "fleet" => ResourceScope::Fleet,
            "silo" => ResourceScope::Silo,
            "tenant" => ResourceScope::Tenant,
            "project" => ResourceScope::Project,
            "vpc" => ResourceScope::Vpc,
            "subnet" => ResourceScope::Subnet,
            "cn" => ResourceScope::Cn,
            "instance" => ResourceScope::Instance,
            "nic" => ResourceScope::Nic,
            "disk" => ResourceScope::Disk,
            "image" => ResourceScope::Image,
            "floating_ip" => ResourceScope::FloatingIp,
            "nat_gateway" => ResourceScope::NatGateway,
            "route" => ResourceScope::Route,
            "route_table" => ResourceScope::RouteTable,
            "edge_cluster" => ResourceScope::EdgeCluster,
            "job" => ResourceScope::Job,
            _ => return None,
        })
    }
}

/// A resource a saga touches. Catalog modules export a
/// `build_references(params)` helper that returns one of these per
/// known resource at saga-create time. Stored on the saga record
/// AND indexed in `saga/by_ref/<scope>/<id>/...` so resource-scoped
/// queries are O(scan-on-index) instead of O(scan-all-sagas).
///
/// Resources allocated *during* the saga (e.g. the instance_id
/// created by `instance-create.create_record`) can't be on the
/// initial ref list. Two ways to handle that, in order of
/// preference:
///   1. Pre-allocate the UUID in the handler and pass it in as a
///      param so it's known at create time. The store
///      mutation's CAS protects uniqueness.
///   2. Defer the ref; it shows up only after action 1 succeeds
///      and a sweeper-style backfill writes it. Not implemented
///      yet — deferred sagas just have an incomplete ref list.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct ResourceRef {
    pub scope: ResourceScope,
    pub id: Uuid,
}

impl ResourceRef {
    pub const fn new(scope: ResourceScope, id: Uuid) -> Self {
        Self { scope, id }
    }
}

/// The persisted "what saga is this" record, indexed by saga id.
/// Sized to fit in one FDB value (well under the 100 KB FDB
/// value-size budget). Event log lives separately under
/// `saga/event/<saga_id>/...` and is paginated; see
/// [`crate::secstore::TritondSecStore::load_recoverable`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SagaRecord {
    pub id: SagaId,
    pub name: String,
    /// D-Sg-10. Bumped on any change to action ids, DAG shape, or
    /// `Params` shape. The registry keeps the previous N=2 versions
    /// registered so a rolling deploy is safe.
    pub version: u32,
    /// The SEC that originally created the saga.
    pub creator_sec: SecId,
    /// The SEC that currently owns the saga. CAS-on-write enforces
    /// "one owner at a time" (D-Sg-4).
    pub current_sec: SecId,
    /// Fencing epoch for the current owner. Bumped on every
    /// adoption and recovery hop. Invariant 8.
    pub current_epoch: SecEpoch,
    /// How many times this saga has been adopted by a SEC; aids
    /// debugging.
    pub adopt_generation: u64,
    /// The serialised DAG + params Steno needs to rebuild the
    /// executor on recovery.
    pub dag: serde_json::Value,
    /// Steno's cached state: Running / Unwinding / Done.
    pub state: SagaCachedStatePersist,
    pub time_created: DateTime<Utc>,
    pub time_done: Option<DateTime<Utc>>,
    /// Set when a terminal saga ends "stuck" because at least one
    /// undo itself errored, or because the registered version is
    /// missing (`SagaError::UnknownVersion`). Operator-visible.
    pub stuck_reason: Option<String>,
    /// Resources this saga touches, written at create time. Used
    /// by per-resource saga views — see `ResourceRef`. Empty for
    /// pre-resource-index records (those just don't show up in
    /// per-resource queries; no migration needed).
    #[serde(default)]
    pub references: Vec<ResourceRef>,
}

/// Local mirror of `steno::SagaCachedState` so we can derive serde +
/// JsonSchema without depending on Steno's serde impl in stable
/// schema positions.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SagaCachedStatePersist {
    Running,
    Unwinding,
    Done,
}

impl From<steno::SagaCachedState> for SagaCachedStatePersist {
    fn from(value: steno::SagaCachedState) -> Self {
        match value {
            steno::SagaCachedState::Running => Self::Running,
            steno::SagaCachedState::Unwinding => Self::Unwinding,
            steno::SagaCachedState::Done => Self::Done,
        }
    }
}

impl From<SagaCachedStatePersist> for steno::SagaCachedState {
    fn from(value: SagaCachedStatePersist) -> Self {
        match value {
            SagaCachedStatePersist::Running => Self::Running,
            SagaCachedStatePersist::Unwinding => Self::Unwinding,
            SagaCachedStatePersist::Done => Self::Done,
        }
    }
}

/// Everything `SecClient::saga_resume` needs in one bundle.
/// `load_recoverable` returns these for sagas the local SEC owns
/// that aren't yet terminal.
#[derive(Clone, Debug)]
pub struct RecoverableSaga {
    pub record: SagaRecord,
    pub events: Vec<steno::SagaNodeEvent>,
}

/// Per-SEC heartbeat side-table row. Written by the local heartbeat
/// task; read by every SEC's sweeper to find stale owners. D-Sg-4 /
/// D-Sg-8 (the epoch is carried so observers know which fencing
/// generation a heartbeat is paired with).
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct SecHeartbeat {
    pub sec_id: SecId,
    pub epoch: SecEpoch,
    pub at: DateTime<Utc>,
}
