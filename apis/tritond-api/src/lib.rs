// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Triton Cloud control-plane API.
//!
//! Phase 0 ships a deliberately small surface — a liveness check and
//! the silo CRUD primitives — that exercises the full Dropshot +
//! OpenAPI + Progenitor + FoundationDB pipeline end to end. Subsequent
//! phases extend the trait with `/v2/instances`, `/v2/audit`, and the
//! rest of DESIGN.md §14.
//!
//! Domain types live in [`tritond_store`] and are re-exported from
//! [`mod@types`] so wire types and storage types never drift.

pub mod types;
pub mod v1;

use chrono::{DateTime, Utc};
use dropshot::{
    HttpError, HttpResponseCreated, HttpResponseDeleted, HttpResponseOk,
    HttpResponseUpdatedNoContent, Path, Query, RequestContext, TypedBody, WebsocketChannelResult,
    WebsocketConnection,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tritond_auth::{ConsoleKind, RedactedString};
use uuid::Uuid;

use crate::types::{
    ApiKeyScope, ApiKeyView, AuditChainHead, AuditEvent, AuditVerifyOutcome, AutoApproveWindow,
    CnRole, CnState, CnView, DhcpLease, DhcpPool, DhcpReservation, Disk, FirewallRule, FloatingIp,
    IdpConfigView, Image, ImdsBindingWire, Instance, JobKind, JobOutcome, LegacyVm,
    ManagedIdentity, NatGateway, NetworkResourceId, NewDhcpPool, NewDhcpReservation,
    NewFirewallRule, NewFloatingIp, NewImage, NewInstance, NewNatGateway, NewProject, NewQuota,
    NewRoute, NewRouteTable, NewSilo, NewSshKey, NewStorageCluster, NewSubnet, NewTenant, NewVpc,
    Nic, Project, ProvisioningJob, Quota, RealizationStatus, RealizerId, Route, RouteTable, Silo,
    SshKey, StorageClusterView, Subnet, Tenant, Vpc,
};

/// Liveness response.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
}

/// Path parameters for endpoints that operate on a single silo.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SiloPath {
    pub silo_id: Uuid,
}

/// Path parameters for endpoints that operate on a single tenant
/// inside a silo. Tenants live under silos in the URL even though
/// the operator-only Tenant CRUD endpoints administer them
/// directly; the silo segment lets a future per-silo operator
/// role surface tenant management without rooting it at `/v2/tenants`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SiloTenantPath {
    pub silo_id: Uuid,
    pub tenant_id: Uuid,
}

/// Path parameters for endpoints that operate on a single API key.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ApiKeyPath {
    pub api_key_id: Uuid,
}

/// Request body for `POST /v2/auth/login`.
///
/// `password` is a [`RedactedString`] so a stray `Debug` of this
/// struct does not print the credential and so the in-memory copy
/// is zeroed when the value drops.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct LoginRequest {
    pub username: String,
    pub password: RedactedString,
}

/// Request body for `POST /v2/auth/refresh`.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct RefreshRequest {
    pub refresh_token: String,
}

/// Response body for both login and refresh.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct TokenResponse {
    pub access_token: String,
    pub refresh_token: String,
    pub access_expires_at: DateTime<Utc>,
    pub refresh_expires_at: DateTime<Utc>,
}

/// Request body for `POST /v2/silos/{silo_id}/image-bundles`.
/// (Bundle ingest stays silo-scoped through slice F; multi-scope
/// bundle ingest is a future concern.)
///
/// `bundle_url` points at a tritond image bundle (an
/// uncompressed tar with `manifest.json` + `content.zfs.gz`,
/// produced by the `tritonimg-build` CLI). tritond fetches the
/// bundle once at registration time, validates the manifest,
/// re-hashes the content against the manifest's claimed sha256,
/// and populates the Image record's `name`, `version`, `os`,
/// `size_bytes`, `sha256`, and `compatibility` from the
/// manifest. Operators don't pass any of those fields by hand
/// — the bundle is the source of truth.
///
/// The bundle URL is also recorded as the Image's `source_url`
/// so the per-CN agent fetches the same bundle at provision
/// time (extracts manifest, sha256-verifies content, ZFS-
/// receives).
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct NewImageFromBundle {
    pub bundle_url: String,
}

/// Request body for `POST /v2/silos/{silo_id}/images/from-imgapi`.
///
/// The IMGAPI v2 manifest is the canonical Joyent / Triton image
/// wire format. Operators (and the `tcadm image fetch-nocloud`
/// pipeline once it lands) upload the binary blob to Manta out
/// of band, then POST this body to register the image with
/// tritond. tritond derives every `Image` field from the
/// manifest plus the operator-supplied integrity metadata.
///
/// ## Why we carry our own sha256
///
/// IMGAPI's `files[].sha1` is the spec-mandated digest; our
/// per-CN agent verifies SHA-256 for defense-in-depth (the
/// existing bundle ingest path uses SHA-256 too). The publisher
/// computes both digests during the same streaming hash on the
/// way to Manta and supplies both, so the agent's existing
/// verifier needs no SHA-1 path.
///
/// ## URL derivation
///
/// `manta_url` is the public HTTPS URL the per-CN agent will
/// fetch the blob from. Conventionally
/// `<imgapi-blob-manta prefix>/<uuid>/file`, but tritond does
/// not enforce a layout — operators may host blobs anywhere
/// HTTPS-reachable. The agent re-hashes the bytes against
/// `sha256` regardless.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct NewImageFromImgapi {
    /// Full IMGAPI v2 manifest as it would appear at
    /// `GET /images/{uuid}` on any IMGAPI server. tritond
    /// re-validates the manifest with `imgapi_manifest::Manifest::validate`
    /// before persisting.
    pub manifest: imgapi_manifest::Manifest,

    /// Public HTTPS URL where the blob in `manifest.files[0]`
    /// can be fetched. Persisted on the Image record as
    /// `source_url`.
    pub manta_url: String,

    /// Lowercase 64-char hex SHA-256 of the blob bytes
    /// (the same bytes whose SHA-1 appears in
    /// `manifest.files[0].sha1`). Required because the agent's
    /// existing integrity check verifies SHA-256.
    pub sha256: String,
}

/// Request body for `POST /v2/auth/api-keys`.
///
/// `scope` defaults to [`ApiKeyScope::Full`] when omitted on the
/// wire — preserves the pre-scope behaviour where every minted
/// key has the full permissions of the owning user. Operators
/// who want a least-privilege key (e.g. for a CI pipeline that
/// only reads audit logs) pass `scope: "read_only"` or
/// `scope: "audit_only"` at create time.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct NewApiKey {
    pub description: String,
    #[serde(default)]
    pub scope: ApiKeyScope,
}

/// Response body for `POST /v2/auth/api-keys`.
///
/// `secret` is the wire-form key. It is shown to the operator
/// **once**; the server retains only a bcrypt hash.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct ApiKeyCreated {
    #[serde(flatten)]
    pub key: ApiKeyView,
    pub secret: String,
}

/// Path parameters for endpoints that operate on a single
/// provisioning job by id.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct AgentJobPath {
    pub job_id: Uuid,
}

/// Path parameters for a single Proteus per-port blueprint.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct AgentPortBlueprintPath {
    pub port_id: Uuid,
}

/// Query parameters for `GET /v2/agent/peer`. The endpoint resolves
/// a single in-VPC peer address to its host CN's underlay address +
/// guest MAC; the bound CN agent calls this on every cache miss
/// (the kmod's v2p cache fires a `PeerResolveNeeded` event with the
/// same shape). See `PROTEUS_PLAN.md` §11.7.1.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct AgentPeerResolveQuery {
    /// VNI of the target VPC. Required so the lookup is bounded to
    /// one tenant's address space (peers are NOT globally unique).
    pub vni: u32,
    /// Peer IP as a string. v4 and v6 both accepted; the resolver
    /// parses and dispatches by family.
    pub ip: String,
}

/// Response body for `GET /v2/agent/peer`. Matches the on-wire
/// shape of [`proteus_api::peer::PeerEntry`] (guest MAC + underlay
/// IPv6) plus a server-suggested TTL the agent should honour when
/// calling [`proteus_api::peer::AddPeerEntryRequest`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AgentPeerResolveResponse {
    /// Resolved peer guest's MAC, `aa:bb:cc:dd:ee:ff`.
    pub guest_mac: String,
    /// Host CN underlay IPv6 the kmod uses as the Geneve outer dst.
    pub underlay: String,
    /// Suggested cache TTL in seconds. The agent clamps + the kmod
    /// clamps again; honoured as a soft upper bound.
    pub ttl_seconds: u32,
}

/// Query parameters for `GET /v2/agent/peer-invalidations`. The
/// agent supplies the last invalidation `seq` it has applied; the
/// response returns everything strictly after that seq.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct AgentPeerInvalidationsQuery {
    /// Last sequence number the agent has applied. `0` on first
    /// call after agent start.
    #[serde(default)]
    pub since: u64,
}

/// One invalidation directive the agent should apply against the
/// kmod's v2p cache. Fired by tritond on NIC teardown / migration.
/// `(vni, peer_ip)` identifies the entry to drop; the agent applies
/// it to every local port that might have cached the peer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AgentPeerInvalidation {
    /// Monotonic per-tritond sequence number. The agent's `since`
    /// cursor uses this to dedup on retry / agent restart.
    pub seq: u64,
    pub vni: u32,
    /// Peer IP (v4 or v6) as a string. Family inferred at parse
    /// time on the agent.
    pub peer_ip: String,
}

/// Response body for `GET /v2/agent/peer-invalidations`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AgentPeerInvalidationsResponse {
    pub invalidations: Vec<AgentPeerInvalidation>,
    /// Highest sequence number returned; the agent passes this as
    /// `since` on its next poll.
    pub tail_seq: u64,
}

/// Optional `?force=true` query parameter on
/// `DELETE /v2/silos/.../instances/{id}`. Default is `false`,
/// which preserves the "must be Stopped or Failed first" gate.
/// Operators set `force=true` to delete an instance from any
/// state — useful when an instance is wedged in `Pending` /
/// `Provisioning` / `Stopping` and the operator needs to clean
/// up without driving each stuck transition by hand.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct InstanceDeleteQuery {
    #[serde(default)]
    pub force: bool,
}

/// Query string for the console `#[channel]` endpoints
/// (`/v2/.../instances/{id}/console`, `/v2/admin/legacy/vms/{uuid}/console`).
///
/// `kind=serial` opens the text console (zone console for native /
/// lx / bhyve, KVM serial UDS for kvm); `kind=vnc` opens the RFB
/// framebuffer and is only valid for `bhyve` / `kvm` brands —
/// requesting it for any other brand is a client error.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ConsoleQuery {
    /// Which console to attach: `serial` or `vnc`.
    pub kind: ConsoleKind,
}

/// Request body for `POST /v2/agent/jobs/claim`.
///
/// `claimed_by` is the agent's own identity — used by the store as
/// the [`ProvisioningJob::claimed_by`] field, and rolled into audit
/// events so concurrent agents can be told apart.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct ClaimJobRequest {
    pub claimed_by: String,
}

/// Response body for `POST /v2/agent/jobs/claim`.
///
/// `job` is `Some(...)` when the queue had a Pending job and the
/// claim succeeded; `None` when the queue is empty. The HTTP status
/// is always `200 OK`; the agent reads the `job` field to decide
/// whether to do work or sleep until the next poll.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct ClaimJobResponse {
    #[serde(default)]
    pub job: Option<ProvisioningJob>,
}

/// Request body for `POST /v2/agent/jobs/{job_id}/complete`.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct CompleteJobRequest {
    pub outcome: JobOutcome,
}

/// Request body for `POST /v2/agent/status`.
///
/// `payload` is opaque to tritond — agents pick the shape — but
/// the Triton-classic shape is `{ vms, zpools, meminfo,
/// diskinfo, boot_time, timestamp }`. Stored verbatim on the
/// `Cn` record's `last_status` field; surfaced via
/// `tcadm cn show`.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct AgentStatusRequest {
    pub payload: serde_json::Value,
}

/// Request body for `POST /v2/agent/network-realization`.
///
/// Agents report one `(resource, realizer)` row at a time. Tritond
/// validates the resource exists and then lets the store enforce
/// monotonic generation reporting for that tuple.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct NetworkRealizationRequest {
    pub resource: NetworkResourceId,
    pub realizer: RealizerId,
    pub generation: u64,
    pub status: RealizationStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// One DHCP message the kmod observed for a guest port, forwarded by
/// the bound CN agent draining the Proteus event ring. Tritond uses
/// these to keep the lease record's `last_renewed_at` fresh so the
/// reconciler's idle-GC heuristic doesn't mistake a long-lived VM for
/// an orphaned lease. RELEASE / DECLINE are recorded as the last
/// message type but never expire the lease (persistent-lease policy);
/// only DISCOVER / REQUEST advance the renewal clock.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct DhcpLeaseActivity {
    /// Proteus port id — equal to the NIC id.
    pub port_id: Uuid,
    /// Client MAC as the kmod saw it, canonical lowercase
    /// colon-separated form (e.g. `02:08:20:ab:cd:ef`). Used for a
    /// cross-check against the stored NIC; not the lookup key.
    pub client_mac: String,
    /// DHCP message type (RFC 2132 option 53): 1 DISCOVER, 2 OFFER,
    /// 3 REQUEST, 4 DECLINE, 5 ACK, 6 NAK, 7 RELEASE, 8 INFORM.
    pub msg_type: u8,
    /// BOOTP transaction id from the request.
    pub xid: u32,
}

/// Request body for `POST /v2/agent/dhcp-lease-activity`. A batch —
/// the agent drains the event ring on each poll and forwards every
/// DHCP request it found in one round-trip.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct DhcpLeaseActivityReport {
    pub items: Vec<DhcpLeaseActivity>,
}

/// Opaque Proteus port blueprint returned to a bound CN agent.
///
/// `blueprint_postcard_base64` is a base64-encoded
/// `proteus_api::blueprint::PortBlueprint`. Tritond keeps it opaque at
/// the public API boundary so the agent can decode it against the same
/// Proteus userspace client version it will apply locally.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct AgentPortBlueprint {
    pub port_id: Uuid,
    pub generation: u64,
    pub blueprint_postcard_base64: String,
}

/// Materialised view of everything the agent needs to act on a
/// claimed [`ProvisioningJob`]. Returned by
/// `GET /v2/agent/jobs/{job_id}/blueprint`.
///
/// The shape is intentionally a flat bundle: instance + image +
/// nics + subnets + disks + ssh public keys, all in one response. That lets
/// the agent issue exactly one round-trip per claimed job, and
/// keeps the queue payload itself opaque to the agent's needs (a
/// Provision job will eventually want different fields than a
/// hypothetical Migrate or Resize, and embedding everything in
/// the [`ProvisioningJob`] would force lockstep schema migration).
///
/// Tritond's `Instance::id` is the canonical identity reused
/// downstream: the agent passes it as the SmartOS zone UUID at
/// `vmadm create`, so subsequent Stop/Restart jobs can address
/// the zone by the same id without a separate mapping table.
///
/// Optional fields reflect the per-`JobKind` shape:
///
/// * `Provision` — `instance`, `image`, `nics`, `disks`, and
///   any `ssh_public_keys` are populated. `subnets` carries the
///   referenced subnet records so the agent can derive static guest
///   network metadata without relying on dataplane DHCP. The agent has
///   everything it needs to call `vmadm create`.
/// * `Stop` / `Restart` — `instance` populated, others may be
///   empty. The agent only needs `instance.id` to call
///   `vmadm stop` / `vmadm reboot`.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct ProvisioningBlueprint {
    pub job_id: Uuid,
    pub kind: JobKind,
    /// `Some(...)` when the underlying instance still exists;
    /// `None` if a concurrent operator delete raced the agent's
    /// claim. The agent must report `Failed` for this case
    /// rather than acting on a phantom instance.
    #[serde(default)]
    pub instance: Option<Instance>,
    /// Boot image record. Populated for Provision jobs; absent
    /// for Stop/Restart.
    #[serde(default)]
    pub image: Option<Image>,
    /// All NICs attached to the instance. Empty for non-Provision
    /// jobs.
    #[serde(default)]
    pub nics: Vec<Nic>,
    /// Subnets referenced by `nics`, deduplicated by id. Empty for
    /// non-Provision jobs.
    #[serde(default)]
    pub subnets: Vec<Subnet>,
    /// All disks attached to the instance. Empty for non-Provision
    /// jobs.
    #[serde(default)]
    pub disks: Vec<Disk>,
    /// Raw openssh-form public keys to inject via the SmartOS
    /// `root_authorized_keys` metadata at zone-create time.
    /// Resolved from `Instance::ssh_key_ids`.
    #[serde(default)]
    pub ssh_public_keys: Vec<String>,
    /// Tamper-evident managed-zone identity. `Some` for `Provision`
    /// jobs (tritond mints + signs at blueprint-fetch time using the
    /// per-deployment HMAC key); `None` for `Stop` / `Restart` /
    /// `Delete` (the zone identity is already on disk from its
    /// original provision) and for any non-instance kind. The agent
    /// stamps these four fields verbatim into the zone's
    /// `internal_metadata` inside the `vmadm create` payload.
    #[serde(default)]
    pub managed_identity: Option<ManagedIdentity>,
    /// Per-port IMDS bindings: `(pseudo_src, port_id, instance_id)`
    /// tuples the agent registers in its `ImdsBindingTable` after a
    /// successful `proteus::apply_blueprint`. tritond populates this
    /// when it has allocated a pseudo-source on the CN for an IMDS-
    /// enabled instance (today: empty; the populate-side commit
    /// lands alongside the proteus stateful DNAT/SNAT compile). See
    /// `IMDS_DESIGN.md` §2.1.
    #[serde(default)]
    pub imds_bindings: Vec<ImdsBindingWire>,
    /// Instance-scope metadata to fold into the SmartOS vmadm-create
    /// payload's `customer_metadata` / `internal_metadata` maps:
    ///   * `triton/instance/*` keys with `guest_visible=true`
    ///     -> `customer_metadata.<suffix>` (cloud-init reads this)
    ///   * `triton/instance/*` keys with `guest_visible=false`
    ///     -> `internal_metadata.<suffix>` (the legacy
    ///     "internal_metadata" shape, where the historical
    ///     `root_pw` lives)
    /// Empty for non-Provision jobs. The agent strips the
    /// `triton/instance/` prefix when folding so an operator who
    /// sets `triton/instance/root_pw` ends up with
    /// `internal_metadata.root_pw` in the create payload, matching
    /// what cloud-init's SmartOS datasource expects.
    #[serde(default)]
    pub provision_metadata: Vec<MetaEntry>,
}

/// Path parameters for endpoints that operate on a single audit event.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct AuditEventPath {
    pub seq: u64,
}

// ---------------------------------------------------------------------
// Operations / sagas (RFD 00004 SG-4)
// ---------------------------------------------------------------------

/// Public state of a long-running operation (RFD 00004 D-Sg-13).
/// String values are stable on the wire.
///
/// The list endpoint (`GET /v2/operations`) projects coarse state —
/// Pending / Running / Unwinding / Done / Stuck — derived from the
/// saga record alone (cheap). The detail endpoint
/// (`GET /v2/operations/{id}`) refines `Done` into one of
/// `Succeeded` / `Failed` / `Unwound` by walking the persisted
/// node-event log. Operators reading a row in the list see the
/// coarse view; expanding the row reveals the refined outcome.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OperationState {
    /// Saga has been created and the record is durable, but no
    /// action body has reported a `started` event yet. Brief
    /// window between `saga_create` and the first action's first
    /// log line.
    Pending,
    /// Saga is in the forward direction with at least one action
    /// having started.
    Running,
    /// Saga has decided to unwind. Some forward action failed; the
    /// undo of each committed action is now running in reverse.
    /// Distinct from `Running` so SDKs and operators can see "this
    /// is going to fail, the compensations are running" without a
    /// log dive (D-Sg-13).
    Unwinding,
    /// Forward chain ran to completion with no failures. Terminal.
    Succeeded,
    /// A forward action failed but no committed actions existed to
    /// compensate, so no undo ran. Terminal.
    Failed,
    /// A forward action failed and one or more committed actions
    /// were compensated by their undos in reverse. Terminal — the
    /// saga's net effect is "nothing happened" if every undo ran
    /// cleanly.
    Unwound,
    /// At least one undo errored or the saga's persisted version
    /// is no longer registered. Terminal in the "operator action
    /// needed" sense — automatic recovery does not retry these.
    Stuck,
    /// Coarse "saga reached Done" used by the list projection
    /// before events are walked. The detail endpoint never returns
    /// this — it refines to one of Succeeded / Failed / Unwound.
    /// Kept on the wire so old clients see a familiar value during
    /// the staged rollout of D-Sg-13.
    Done,
}

/// One operation as it appears in `GET /v2/operations`. Minimal
/// shape suitable for an adminUI list view; clients fetch the
/// detail surface for the full DAG / event log.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct OperationSummary {
    /// Stable operation id (== Steno saga id).
    pub id: Uuid,
    /// Saga `NAME` (`instance-create`, `instance-delete`, …). Kebab.
    pub kind: String,
    /// Saga `VERSION` (RFD 00004 D-Sg-10).
    pub version: u32,
    /// Public state (see [`OperationState`]).
    pub state: OperationState,
    /// The SEC that originally created the saga. Operators looking
    /// at "which tritond ran this" see this here.
    pub creator_sec: Uuid,
    /// The SEC currently driving it. Differs from `creator_sec`
    /// only after a reassignment hop (D-Sg-4).
    pub current_sec: Uuid,
    /// When the saga was created (UTC).
    pub time_created: DateTime<Utc>,
    /// When the saga reached `Done`. `None` if still running.
    pub time_done: Option<DateTime<Utc>>,
    /// Set when the saga ends `Stuck` (Done-with-undo-error or
    /// missing-version). Human-readable.
    pub stuck_reason: Option<String>,
    /// Resources this saga touches. Populated at create time; used
    /// by per-resource saga views to filter. Empty for sagas
    /// created before RFD 00004 SG-4 resource indexing landed.
    #[serde(default)]
    pub references: Vec<ResourceReference>,
}

/// Wire mirror of `tritond_saga::ResourceScope`. Stable
/// snake_case strings; safe to extend (callers that don't
/// recognise a new value fall through to whatever default behavior
/// they have — the value is opaque to most clients).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ResourceScope {
    Fleet,
    Silo,
    Tenant,
    Project,
    Vpc,
    Subnet,
    Cn,
    Instance,
    Nic,
    Disk,
    Image,
    FloatingIp,
    NatGateway,
    Route,
    RouteTable,
    EdgeCluster,
    Job,
}

/// One resource a saga touches. Mirrors `tritond_saga::ResourceRef`
/// on the wire; the type lives here so the API doesn't depend on
/// the saga crate's serde shape directly.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash)]
pub struct ResourceReference {
    pub scope: ResourceScope,
    pub id: Uuid,
}

/// Detail view: the summary plus the persisted DAG + event log.
/// Used by `GET /v2/operations/{operation_id}` and rendered by
/// `tcadm operations get` / adminUI Operations detail panel.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct OperationDetail {
    #[serde(flatten)]
    pub summary: OperationSummary,
    /// Fence epoch for `current_sec`. Bumped on every adoption and
    /// every recovery hop (RFD 00004 D-Sg-8).
    pub current_epoch: u64,
    /// How many times this saga has been adopted by a SEC.
    pub adopt_generation: u64,
    /// The persisted Steno `SagaDag` JSON. Opaque to clients; the
    /// adminUI surfaces it for operators who need the structure.
    pub dag: serde_json::Value,
    /// Step-by-step progress derived from the DAG (total action
    /// nodes) and the node-event log (which have completed, which
    /// is in flight). Computed server-side from the persisted log,
    /// so this value survives `tritond` restarts. RFD 00004
    /// D-Sg-13.
    pub progress: OperationProgress,
    /// One entry per Action node in the DAG, in DAG order. Carries
    /// each step's status, output JSON (on success), error JSON
    /// (on failure), and the unwind state if a compensation ran.
    /// The adminUI Operations detail panel renders this directly.
    /// RFD 00004 SG-4 debug surface.
    pub steps: Vec<OperationStep>,
}

/// Step-by-step progress of an operation. Stable across `tritond`
/// restarts because both inputs (the DAG and the node-event log)
/// are persisted by the SEC.
///
/// * `total_steps` — count of Action nodes in the DAG (Start / End
///   markers are excluded; an operator who wants to count them
///   should walk the raw `dag` themselves).
/// * `completed_steps` — count of Action nodes whose log carries a
///   terminal event (`succeeded` or `failed`). Counts the forward
///   pass only; undos don't push this number up.
/// * `current_step` — label of the action that is currently in
///   flight (a `started` event with no terminal counterpart), or
///   `None` when the saga is terminal or has not started any
///   action yet. The label is the catalog's `label` field, not the
///   internal node name.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct OperationProgress {
    pub completed_steps: u32,
    pub total_steps: u32,
    #[serde(default)]
    pub current_step: Option<String>,
}

/// Per-step status used in [`OperationStep::status`]. Lifecycle:
/// `pending → running → (succeeded | failed)`. If the saga unwinds,
/// each prior-Succeeded step transitions
/// `succeeded → undo_running → (undone | undo_failed)`. A
/// `succeeded` step that didn't need to be unwound (the saga
/// succeeded overall) stays `succeeded`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    /// No events for this node yet.
    Pending,
    /// `Started` event observed; no terminal counterpart yet.
    Running,
    /// Forward action returned Ok.
    Succeeded,
    /// Forward action returned Err. The saga is unwinding or
    /// terminal-failed.
    Failed,
    /// `UndoStarted` for a previously-Succeeded node; the undo is
    /// running.
    UndoRunning,
    /// `UndoFinished` — the compensation completed cleanly.
    Undone,
    /// `UndoFailed` — the compensation itself errored. Saga is
    /// terminal-stuck on this node.
    UndoFailed,
}

/// One forward-or-undo event in a step's lifecycle, surfaced for
/// operators who need to see the exact JSON Steno persisted (output
/// value on Succeeded, structured ActionError on Failed). RFD 00004
/// SG-4 debug surface.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct OperationStep {
    /// Node index in the persisted DAG (`graph.nodes[index]`).
    pub index: u32,
    /// Internal node name (the `name` field on the catalog
    /// declaration — e.g. `"instance"`, `"final_instance"`).
    pub name: String,
    /// Human-friendly label (catalog's `label`, e.g.
    /// `"create_instance_record"`).
    pub label: String,
    /// Fully-qualified action identifier (catalog's `action_name`,
    /// e.g. `"instance_create.create_record"`).
    pub action_name: String,
    /// Projected lifecycle state for this node.
    pub status: StepStatus,
    /// Output JSON from a `Succeeded` event, if any. Opaque to the
    /// adminUI; rendered as `<pre>` for operators.
    #[serde(default)]
    pub output: Option<serde_json::Value>,
    /// Structured error JSON from a `Failed` event, if any. Carries
    /// whatever the action body packed into `ActionError` —
    /// typically `{ "kind": "...", "message": "..." }` for our
    /// catalog (e.g. `decode_store_error_kind` in `instance_create`).
    #[serde(default)]
    pub error: Option<serde_json::Value>,
    /// Short human message extracted from `error` so the row can
    /// render without an expanded JSON view. `None` when the step
    /// hasn't failed.
    #[serde(default)]
    pub error_message: Option<String>,
    /// Structured UndoFailed error JSON, if any. Only set when
    /// `status == UndoFailed`.
    #[serde(default)]
    pub undo_error: Option<serde_json::Value>,
}

/// Path parameters for `/v2/operations/{operation_id}`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct OperationPath {
    pub operation_id: Uuid,
}

// ---------------------------------------------------------------------
// Live migrations (LM-1).
//
// Read-only surface. The mutating endpoint
// (`POST /v2/instances/{id}/actions/migrate`) lands with the
// migration saga (LM-5) so the action handler can dispatch on
// MigrationAction.
// ---------------------------------------------------------------------

/// Path parameters for `/v2/migrations/{migration_id}` + nested
/// routes.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct MigrationPath {
    pub migration_id: Uuid,
}

/// Path parameters for `/v2/instances/{instance_id}/migrations`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct InstanceMigrationsPath {
    pub instance_id: Uuid,
}

/// Query parameters for `GET /v2/migrations`. Pagination follows the
/// same cursor pattern as `/v2/operations`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListMigrationsQuery {
    /// Maximum number of items to return. Defaults to 50 if absent;
    /// the server caps the maximum to bound response size.
    #[serde(default)]
    pub limit: Option<u32>,
    /// Continuation token: return migrations strictly after this
    /// id in the global newest-first ordering. `None` for the first
    /// page.
    #[serde(default)]
    pub after_id: Option<Uuid>,
}

/// Request body for `POST .../instances/{instance_id}/migrate`.
///
/// LM-5 ships `action=begin` only; the other actions (estimate,
/// pause, switch, abort, rollback, finalize) return 501 until
/// LM-6 / LM-8 wire the sub-sagas. The wire shape is fixed now
/// so clients don't have to bump on each LM-* slice.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct MigrateInstanceBody {
    /// Sub-action. Defaults to `Begin` when omitted so the
    /// minimal "just migrate" request body is the empty object.
    #[serde(default = "default_migration_action")]
    pub action: tritond_store::MigrationAction,
    /// Operator-supplied target CN. When set, the placement chain
    /// force-places to this CN (still subject to the migration
    /// filters); operator-only — the tenant-scoped Cedar policy
    /// doesn't grant cross-tenant force-place, so a tenant member
    /// supplying this field gets a 403.
    #[serde(default)]
    pub target_server_uuid: Option<Uuid>,
    /// Operator-supplied affinity rules. LM-5 ignores this; LM-6
    /// threads it through `PlacementRequest.affinity`.
    #[serde(default)]
    pub affinity: Option<Vec<String>>,
    /// Cold-migrate: the source VM is already stopped (or the
    /// operator handled it pre-migration). The saga skips the
    /// live-memory transfer (node 8) and treats the post-
    /// incremental ZFS state as canonical. Defaults to `false`
    /// once LM-7 lands the live path; until LM-7 ships, passing
    /// `false` returns a clear error pointing at LM-7 rather
    /// than running a silently-broken cutover.
    #[serde(default)]
    pub cold: bool,
}

fn default_migration_action() -> tritond_store::MigrationAction {
    tritond_store::MigrationAction::Begin
}

/// Response body for `POST .../instances/{instance_id}/migrate`
/// when the action is `Begin`. The operation id is the Steno
/// saga id; clients poll `GET /v2/operations/{operation_id}` for
/// saga-level progress and `GET /v2/migrations/{migration_id}`
/// for the migration-specific timeline.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MigrateInstanceResponse {
    pub migration_id: Uuid,
    pub operation_id: Uuid,
}

/// Query parameters for `GET /v2/migrations/{migration_id}/progress`.
/// Operators page the per-migration event log by passing the
/// `last_progress_seq` they have already seen as `after_seq`; the
/// server returns events with `seq > after_seq` in ascending order.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListMigrationProgressQuery {
    /// Maximum number of events to return. Defaults to 200; the
    /// server caps the maximum to bound response size.
    #[serde(default)]
    pub limit: Option<u32>,
    /// Return only events whose `seq` is greater than this value.
    /// `None` (or `0`) returns from the beginning.
    #[serde(default)]
    pub after_seq: Option<u64>,
}

/// Response body for `POST /v2/operations/{operation_id}/abandon`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct AbandonResponse {
    /// The operation we abandoned.
    pub id: Uuid,
    /// How many of the saga's DAG nodes the executor poked with
    /// `saga_inject_error`. A higher number means more "future"
    /// nodes were marked to fail; the actual unwind starts at the
    /// first one the saga reaches.
    pub poked_nodes: u64,
}

/// Query parameters for `GET /v2/operations`. Pagination is
/// continuation-token style: pass back the `id` of the last entry
/// from the previous page as `after_id`.
///
/// Resource filtering (RFD 00004 SG-4): pass both `resource_scope`
/// and `resource_id` to restrict the listing to sagas that touched
/// the named resource. Backed by the FDB by_ref index; returns
/// newest-first ordering. Passing only one of the two is a 400.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListOperationsQuery {
    /// Maximum number of items to return. Defaults to 50 if absent;
    /// the server caps the maximum to bound response size.
    #[serde(default)]
    pub limit: Option<u32>,
    /// Continuation token: return operations strictly after this
    /// id. `None` for the first page.
    #[serde(default)]
    pub after_id: Option<Uuid>,
    /// Resource scope to filter by (e.g. `instance`, `cn`,
    /// `tenant`). Must be paired with `resource_id`.
    #[serde(default)]
    pub resource_scope: Option<ResourceScope>,
    /// Resource id to filter by. Must be paired with
    /// `resource_scope`.
    #[serde(default)]
    pub resource_id: Option<Uuid>,
}

// ---------------------------------------------------------------------
// CN registration / approval (slice C)
// ---------------------------------------------------------------------

/// Request body for `POST /v2/agent/register`. Anonymous endpoint
/// (no auth header required); the agent has no credentials yet at
/// this point in its lifecycle.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct RegisterCnRequest {
    /// SmartOS server UUID, read from `/usr/bin/sysinfo`. Identity
    /// for the entire registration record.
    pub server_uuid: Uuid,
    pub hostname: String,
    /// Admin-network IPv4 (best-effort; included for operator
    /// visibility, not used for authentication).
    #[serde(default)]
    pub admin_ip: Option<std::net::Ipv4Addr>,
    /// Raw `/usr/bin/sysinfo` JSON. Opaque to tritond; surfaced via
    /// `tcadm cn show` for operator inspection.
    pub sysinfo: serde_json::Value,
    /// TCP port the agent's on-CN console listener binds on the admin
    /// IP. Reported on every (re-)registration. `None` when the agent
    /// was started without a console listener — serial / VNC consoles
    /// are unavailable for the CN until a registration carries it.
    #[serde(default)]
    pub console_listen_port: Option<u16>,
    /// Lowercase-hex SHA-256 of the agent console listener's TLS
    /// SubjectPublicKeyInfo. tritond pins this when it dials the
    /// listener so a hijacked admin IP cannot MITM the console byte
    /// stream. `None` iff `console_listen_port` is `None`.
    #[serde(default)]
    pub console_tls_spki_sha256_hex: Option<String>,
}

/// Response body for `POST /v2/agent/register`.
///
/// The agent gets back its `poll_token` — needed for every
/// subsequent call to `GET /v2/agent/register/status` — plus, when
/// in Pending state, the `claim_code` it must display on the
/// console for the operator to pair.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct RegisterCnResponse {
    pub server_uuid: Uuid,
    pub state: CnState,
    /// `Some(...)` only when state is Pending. Formatted for human
    /// display: `XXX-XXX`.
    #[serde(default)]
    pub claim_code: Option<String>,
    #[serde(default)]
    pub claim_code_expires_at: Option<DateTime<Utc>>,
    pub poll_token: String,
}

/// Query string for `GET /v2/agent/register/status`. The agent
/// long-polls this endpoint until tritond returns the API key.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct RegisterStatusQuery {
    pub poll_token: String,
}

/// Response body for `GET /v2/agent/register/status`.
///
/// `api_key` is populated exactly once — on the first call after
/// the operator approves (or auto-approve fires). Subsequent calls
/// return `state = Approved` with `api_key = None`. The agent
/// persists the key locally on receipt; if the agent loses the
/// key file, an operator must `tcadm cn disable` and re-approve.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct RegisterStatusResponse {
    pub state: CnState,
    #[serde(default)]
    pub api_key: Option<String>,
    /// Per-CN HS256 console-ticket key, lowercase hex (32 bytes / 64
    /// hex chars). Delivered exactly once, alongside `api_key`, on the
    /// first long-poll after the operator approves (or auto-approve
    /// fires). The agent persists it next to its credential file and
    /// uses it to verify the short-lived console tickets tritond mints
    /// when proxying a serial / VNC session. Secret — never logged.
    /// `None` on every subsequent poll and whenever `api_key` is `None`.
    #[serde(default)]
    pub console_ticket_key_hex: Option<String>,
    /// Per-CN HS256 IMDSv2 session-token key, lowercase hex (32 bytes
    /// / 64 hex chars). Same delivery contract as
    /// `console_ticket_key_hex` -- handed exactly once alongside
    /// `api_key`, then `None` thereafter. The agent uses it to mint +
    /// verify the IMDSv2 session tokens guests obtain via
    /// `PUT /latest/api/token`. Secret -- never logged. See
    /// `IMDS_DESIGN.md` §3.
    #[serde(default)]
    pub imds_token_key_hex: Option<String>,
    /// Per-CN HS256 live-migration ticket key, lowercase hex (32
    /// bytes / 64 hex chars). Same one-shot delivery contract as
    /// `console_ticket_key_hex` / `imds_token_key_hex`. The agent
    /// persists this alongside the other ticket keys and uses it
    /// to verify migrate tickets the source-side agent presents
    /// when dialing the target's `/migrate/{id}` and
    /// `/migrate/{id}/zfs` listener routes. Secret — never logged.
    /// See `tritond_auth::MigrateTicketKey`.
    #[serde(default)]
    pub migrate_ticket_key_hex: Option<String>,
}

/// Path parameter for endpoints that operate on a single CN.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct CnPath {
    pub server_uuid: Uuid,
}

/// One row of the drain-preview migration plan. `target_cn_*` are
/// populated when the placement engine found an eligible CN for the
/// instance; otherwise `reason` carries the no-eligible-CN explanation.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DrainMigrationRow {
    pub instance_id: Uuid,
    pub instance_name: String,
    pub instance_tenant_id: Uuid,
    pub instance_project_id: Uuid,
    /// CPU vCPUs the instance reserves (1 vCPU = 100 placement units).
    pub instance_cpu: u32,
    /// RAM the instance reserves, in MiB.
    pub instance_ram_mb: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_cn_uuid: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_cn_hostname: Option<String>,
    /// Set when no CN could be picked. One-line operator-readable
    /// summary; full explain report is available via the operations
    /// endpoint if the operator wants the chain detail.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Response from `POST /v2/cns/{server_uuid}/drain/preview`. Used by
/// the operator console's `BlastRadiusCard` to show the actual
/// migration plan + capacity / quorum signals before commit.
///
/// `placeable` + `not_placeable` partition the instances currently
/// hosted on the source CN. Iff `not_placeable` is empty the drain
/// can proceed without operator intervention (capacity_ok = true).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DrainPreviewResponse {
    /// Total instances currently pinned to this CN.
    pub instances_on_cn: usize,
    /// Instances the placement engine could find a target for.
    pub placeable: Vec<DrainMigrationRow>,
    /// Instances the placement engine could not place anywhere.
    pub not_placeable: Vec<DrainMigrationRow>,
    /// True when every instance can be placed off this CN.
    pub capacity_ok: bool,
    /// Names of instances whose name matches a quorum-service
    /// heuristic (vault / etcd / consul / tritond-sec / fdb /
    /// cockroach). Coarse — replaces nothing in the placement
    /// engine, just a UI hint that the operator should look twice.
    pub quorum_at_risk: Vec<String>,
    /// True when the heuristic finds no quorum members on this CN.
    pub quorum_ok: bool,
}

/// Optional state filter for `GET /v2/cns`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct CnListQuery {
    #[serde(default)]
    pub state: Option<CnState>,
}

/// Path parameter for endpoints addressing one legacy VM by SmartOS
/// zone uuid.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct LegacyVmPath {
    pub smartos_uuid: Uuid,
}

/// Optional filter for `GET /v2/admin/legacy/vms`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct LegacyVmListQuery {
    /// Restrict to legacy VMs hosted on the given CN.
    #[serde(default)]
    pub host_cn: Option<Uuid>,
}

/// Path parameter for endpoints that operate on a single registered
/// storage cluster (`/v2/storage/clusters/{id}`).
#[derive(Debug, Deserialize, JsonSchema)]
pub struct StorageClusterPath {
    pub id: Uuid,
}

/// Per-node forwarder path, e.g.
/// `/v2/storage/clusters/{id}/nodes/{node_id}`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct StorageClusterNodePath {
    pub id: Uuid,
    /// Numeric storage-node identifier (mirrors `manta_place::NodeId`,
    /// which is a `u32`). Re-serialised verbatim to mantad.
    pub node_id: u32,
}

/// Per-bucket forwarder path,
/// `/v2/storage/clusters/{id}/buckets/{bucket}`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct StorageClusterBucketPath {
    pub id: Uuid,
    pub bucket: String,
}

/// Per-IAM-user forwarder path,
/// `/v2/storage/clusters/{id}/users/{user}`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct StorageClusterUserPath {
    pub id: Uuid,
    pub user: String,
}

/// Per-access-key forwarder path,
/// `/v2/storage/clusters/{id}/access-keys/{access_key_id}`. Note that
/// access-key delete in mantad's API is *not* nested under the user
/// (the AKID alone identifies the key) — the path mirrors that shape.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct StorageClusterAccessKeyPath {
    pub id: Uuid,
    pub access_key_id: String,
}

/// Per-policy forwarder path,
/// `/v2/storage/clusters/{id}/users/{user}/policies/{policy}`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct StorageClusterUserPolicyPath {
    pub id: Uuid,
    pub user: String,
    pub policy: String,
}

// ----- Storage cluster forwarder mirror types -----
//
// These types mirror `mantad_client::types::*` field-for-field so the
// JSON wire shape is byte-identical. The mirror exists because Dropshot
// requires `JsonSchema` for response types and `mantad-client` types
// don't carry `JsonSchema` derives (they're built against schemars
// 1.x in the manta-storage workspace, while this workspace pins
// schemars 0.8 to match Dropshot 0.16). Conversion happens once on
// each side via `From` impls living in `services/tritond/src/lib.rs`.

/// Mirror of `mantad_client::types::ClusterSummary`. Returned by
/// `GET /v2/storage/clusters/{id}/cluster`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct StorageClusterSummary {
    pub version: String,
    pub primary: u32,
    pub this_node: u32,
    pub replication_factor: usize,
    pub nodes_total: usize,
    pub nodes_alive: usize,
    pub buckets: usize,
    pub total_blobs: u64,
    pub total_bytes: u64,
    pub racks: Vec<String>,
    pub query_ms: u64,
}

/// Mirror of `mantad_client::types::Node`. Returned by
/// `GET /v2/storage/clusters/{id}/nodes` (and the per-node lookup).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct StorageNode {
    pub id: u32,
    pub rack: String,
    pub internal_url: String,
    pub alive: bool,
    pub is_primary: bool,
    pub blobs: u64,
    pub bytes: u64,
    pub buckets: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Mirror of `mantad_client::types::PeerEntry`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct StoragePeerEntry {
    pub id: u32,
    pub rack: String,
    pub internal_url: String,
}

/// Mirror of `mantad_client::types::Membership`. Returned by
/// `GET /v2/storage/clusters/{id}/membership` and by every
/// node-mutation endpoint (drain / undrain / reweight / add /
/// remove) that mantad answers with the refreshed view.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct StorageMembership {
    pub version: u64,
    pub peers: Vec<StoragePeerEntry>,
    pub auto_membership: bool,
}

/// Body for `POST /v2/storage/clusters/{id}/nodes`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct StorageAddNodeRequest {
    pub id: u32,
    pub rack: String,
    pub internal_url: String,
}

/// Body for `POST /v2/storage/clusters/{id}/nodes/{node_id}/reweight`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct StorageReweightRequest {
    /// New reweight factor; 1.0 = normal, 0.0 = drained.
    pub factor: f64,
}

/// Mirror of `mantad_client::types::Bucket`. Returned by
/// `GET /v2/storage/clusters/{id}/buckets` (and per-bucket get).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct StorageBucket {
    pub name: String,
    pub owner: String,
    pub created_at: DateTime<Utc>,
    /// Object count, present only when `?stats=1` was forwarded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub object_count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_bytes: Option<u64>,
}

/// Body for `POST /v2/storage/clusters/{id}/buckets`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct StorageCreateBucketRequest {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    /// Raw JSON `Durability` (mantad's type lives in mantas3-meta;
    /// kept opaque to avoid a dep cycle).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub durability: Option<serde_json::Value>,
}

/// Mirror of `mantad_client::types::ObjectsPage`. Paged
/// `GET /v2/storage/clusters/{id}/buckets/{bucket}/objects` response.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct StorageObjectsPage {
    pub objects: Vec<StorageObjectSummary>,
    pub common_prefixes: Vec<String>,
    pub is_truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_continuation_token: Option<String>,
}

/// Mirror of `mantad_client::types::ObjectSummary`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct StorageObjectSummary {
    pub key: String,
    pub size: u64,
    pub etag: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    pub last_modified: DateTime<Utc>,
}

/// Query for the object-listing forwarder. Mirrors
/// `mantad_client::types::ObjectsQuery`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct StorageObjectsQuery {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delimiter: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub continuation_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_keys: Option<u32>,
}

/// Bucket-list query — the only filter mantad exposes is `?stats=1`,
/// which expands the response with object_count + total_bytes per
/// bucket (a more expensive scan).
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct StorageBucketListQuery {
    #[serde(default)]
    pub stats: Option<bool>,
}

/// Mirror of `mantad_client::types::User`. Returned by IAM list +
/// per-user GET.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct StorageUser {
    pub name: String,
    pub created_at: DateTime<Utc>,
}

/// Body for `POST /v2/storage/clusters/{id}/users`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct StorageCreateUserRequest {
    pub name: String,
}

/// Mirror of `mantad_client::types::AccessKey`. `secret_access_key`
/// is `Some(_)` only on the `POST .../access-keys` create response —
/// mantad does not retain the cleartext after that one return.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct StorageAccessKey {
    pub access_key_id: String,
    pub user: String,
    pub created_at: DateTime<Utc>,
    /// `"Active"` or `"Revoked"` (free-form on the wire today).
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_access_key: Option<String>,
}

// ----- Presign + multipart wire types (Stage 6a) -----

/// Body of `POST /v2/storage/clusters/{id}/presigner`. Operator
/// configures the IAM credential tritond signs presigned S3 URLs
/// with. To clear the presigner, send empty strings — the handler
/// treats empty as "unset". `s3_endpoint` is the data-plane URL
/// (port 7443); leave `None` to keep the existing value during
/// credential rotation.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SetPresignerRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub s3_endpoint: Option<String>,
    pub access_key_id: String,
    pub secret_access_key: String,
}

/// Body of `POST /v2/storage/clusters/{id}/s3/presign/put`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PresignPutRequest {
    pub bucket: String,
    pub key: String,
    /// Validity window in seconds; 1..=604_800 (AWS 7-day cap).
    pub expires_secs: u32,
}

/// Body of `POST /v2/storage/clusters/{id}/s3/presign/get`. Same
/// shape as the PUT request — kept as a distinct type for audit and
/// API-doc clarity.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PresignGetRequest {
    pub bucket: String,
    pub key: String,
    pub expires_secs: u32,
}

/// Response shared by `presign/put` and `presign/get`. The browser
/// uses these fields verbatim — the URL is fully signed; `method`
/// tells the client which verb to issue; `headers` is reserved for
/// future use when we sign headers beyond `host` (today empty).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PresignResponse {
    pub url: String,
    /// HTTP verb the URL is signed for (`"PUT"` or `"GET"`).
    pub method: String,
    /// Headers the client must send verbatim. Empty in the current
    /// signer scope; reserved for future Content-Type / x-amz-*
    /// signing.
    #[serde(default)]
    pub headers: std::collections::HashMap<String, String>,
}

/// Body of `POST /v2/storage/clusters/{id}/s3/multipart/initiate`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MultipartInitiateRequest {
    pub bucket: String,
    pub key: String,
    /// Optional `Content-Type` echoed into the `CreateMultipartUpload`
    /// call so mantad records it on the final object.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
}

/// Response shape for `multipart/initiate`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MultipartInitiateResponse {
    /// Opaque mantad-side identifier the browser must echo on every
    /// subsequent multipart call.
    pub upload_id: String,
}

/// Body of `POST /v2/storage/clusters/{id}/s3/multipart/parts`.
/// Returns one presigned URL per part. The browser PUTs each part
/// directly to mantad and tracks the returned `ETag` headers.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MultipartPartsRequest {
    pub bucket: String,
    pub key: String,
    pub upload_id: String,
    /// Number of parts the upload will use. Must be 1..=10_000 per
    /// the S3 spec; mantad inherits the limit.
    pub part_count: u32,
    /// Per-part URL validity window. Treat as a session length —
    /// longer than `expires_secs` for a single-shot PUT because
    /// the browser may need time to upload all parts in sequence.
    pub expires_secs: u32,
}

/// One row in [`MultipartPartsResponse::parts`].
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MultipartPart {
    /// 1-based part number. The browser sends parts in any order
    /// but must echo the same numbers when calling
    /// `multipart/complete`.
    pub part_number: u32,
    /// Presigned PUT URL. Sign-time invariant: each URL embeds
    /// `partNumber` + `uploadId` query params and is valid for
    /// `expires_secs` seconds.
    pub url: String,
}

/// Response shape for `multipart/parts`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MultipartPartsResponse {
    pub parts: Vec<MultipartPart>,
}

/// Per-part metadata the browser captured during upload, supplied
/// to `multipart/complete`. Order matters: the list must be sorted
/// ascending by `part_number`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CompletedMultipartPart {
    pub part_number: u32,
    /// `ETag` value mantad returned on each per-part PUT. mantad
    /// will reject the complete call if any etag mismatches.
    pub etag: String,
}

/// Body of `POST /v2/storage/clusters/{id}/s3/multipart/complete`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MultipartCompleteRequest {
    pub bucket: String,
    pub key: String,
    pub upload_id: String,
    pub parts: Vec<CompletedMultipartPart>,
}

/// Response shape for `multipart/complete`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MultipartCompleteResponse {
    pub bucket: String,
    pub key: String,
    /// Final object etag mantad assigned the assembled upload.
    pub etag: String,
}

/// Body of `POST /v2/storage/clusters/{id}/s3/multipart/abort`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MultipartAbortRequest {
    pub bucket: String,
    pub key: String,
    pub upload_id: String,
}

/// Per-CN summary returned by `GET /v2/admin/legacy/cns`.
///
/// Distinct from [`CnView`]: this view rolls up the discovery
/// classifier's per-CN counts (how many tritond-managed instances
/// vs unmanaged legacy zones) so a fleet-admin operator can spot
/// CNs that still have legacy zones to adopt.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct LegacyCnSummary {
    pub server_uuid: Uuid,
    pub hostname: String,
    pub state: CnState,
    pub last_seen: Option<DateTime<Utc>>,
    /// Count of tritond-managed instances currently placed on this CN.
    pub managed_instance_count: usize,
    /// Count of legacy (unmanaged) zones tritond's classifier has
    /// observed on this CN.
    pub legacy_vm_count: usize,
}

/// Request body for `POST /v2/cns/approve`.
///
/// The operator presents the claim code displayed on the CN's
/// console (or syslog, or the `/var/lib/tritonagent/claim-code`
/// file). Hyphens and case are normalized server-side.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct ApproveCnRequest {
    pub code: String,
}

/// Request body for `POST /v2/cns/{server_uuid}/role`.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct SetCnRoleRequest {
    pub role: CnRole,
}

/// Request body for `POST /v2/cns/auto-approve`.
///
/// Opens (or replaces) the global auto-approve window. Bounded by
/// both wall-time and a remaining-count budget so a forgotten
/// window can't stay open forever; tritond clamps `duration_secs`
/// to the 24h hard cap server-side.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct OpenAutoApproveRequest {
    pub duration_secs: u64,
    /// Maximum number of registrations to auto-approve before the
    /// window closes early. `None` means "time-bound only".
    #[serde(default)]
    pub count: Option<u64>,
}

// ---------------------------------------------------------------------
// Cluster configuration (`/v2/config`)
// ---------------------------------------------------------------------

/// One cluster-wide configuration key with its current value,
/// default, description, and operational metadata. Returned by the
/// `/v2/config` endpoints; consumed by `tcadm config` and the admin
/// console.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ConfigEntry {
    /// Dotted key name, e.g. `sweeper.interval_secs`.
    pub key: String,
    /// Value as stored in FoundationDB (the built-in default when no
    /// value has been written). If `env_override` is set, the daemon
    /// is actually running with the environment variable's value, not
    /// this one.
    pub value: serde_json::Value,
    /// Built-in default for this key.
    pub default: serde_json::Value,
    /// Environment variable currently shadowing this key at boot
    /// (`env > FDB > default`), if any.
    pub env_override: Option<String>,
    /// Whether changing this key requires a `tritond` restart to take
    /// effect. Currently `true` for every key.
    pub restart_required: bool,
    /// One-line human description.
    pub description: String,
}

/// Path parameter for endpoints addressing one configuration key by
/// its dotted name.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ConfigKeyPath {
    /// Dotted key name, e.g. `sweeper.interval_secs`.
    pub key: String,
}

/// Request body for `PUT /v2/config/{key}`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct SetConfigRequest {
    /// New value for the key. Must match the key's type; an ill-typed
    /// value is rejected with `400`.
    pub value: serde_json::Value,
}

/// Path parameters for endpoints that operate on a single tenant.
/// Used by the project-list / project-create endpoints rooted at
/// `/v2/tenants/{tenant_id}/projects`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct TenantPath {
    pub tenant_id: Uuid,
}

/// Path parameters for endpoints that operate on a single tenant's
/// IdP config (the OIDC IdP rooted at `/v2/tenants/{tenant_id}/idp`).
/// Distinct from [`TenantPath`] for shape-clarity in handler
/// signatures even though the field set is identical.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct TenantIdpPath {
    pub tenant_id: Uuid,
}

/// Path parameters for endpoints that operate on a single project
/// inside a tenant.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct TenantProjectPath {
    pub tenant_id: Uuid,
    pub project_id: Uuid,
}

/// Path parameters for endpoints that operate on a single VPC inside a
/// project inside a tenant.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct TenantProjectVpcPath {
    pub tenant_id: Uuid,
    pub project_id: Uuid,
    pub vpc_id: Uuid,
}

/// Path parameters for endpoints that operate on a single subnet
/// inside a VPC inside a project inside a tenant.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct TenantProjectVpcSubnetPath {
    pub tenant_id: Uuid,
    pub project_id: Uuid,
    pub vpc_id: Uuid,
    pub subnet_id: Uuid,
}

/// Path parameters for endpoints that operate on a single route table
/// inside a VPC inside a project inside a tenant.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct TenantProjectVpcRouteTablePath {
    pub tenant_id: Uuid,
    pub project_id: Uuid,
    pub vpc_id: Uuid,
    pub route_table_id: Uuid,
}

/// Path parameters for endpoints that operate on a single route
/// inside a route table inside a VPC inside a project inside a tenant.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct TenantProjectVpcRouteTableRoutePath {
    pub tenant_id: Uuid,
    pub project_id: Uuid,
    pub vpc_id: Uuid,
    pub route_table_id: Uuid,
    pub route_id: Uuid,
}

/// Path parameters for endpoints that operate on a single firewall
/// rule scoped to a VPC.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct TenantProjectVpcFirewallRulePath {
    pub tenant_id: Uuid,
    pub project_id: Uuid,
    pub vpc_id: Uuid,
    pub firewall_rule_id: Uuid,
}

/// Path parameters for DHCP reservation / lease endpoints scoped to a
/// single MAC inside a VPC. Handler re-canonicalises the MAC so any
/// of `02:08:20:ab:cd:ef`, `02-08-20-AB-CD-EF`, or `0208.20ab.cdef`
/// is accepted.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct TenantProjectVpcDhcpMacPath {
    pub tenant_id: Uuid,
    pub project_id: Uuid,
    pub vpc_id: Uuid,
    pub mac: String,
}

/// Path parameters for endpoints that operate on a single NAT gateway
/// inside a VPC inside a project inside a tenant.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct TenantProjectVpcNatGatewayPath {
    pub tenant_id: Uuid,
    pub project_id: Uuid,
    pub vpc_id: Uuid,
    pub nat_gateway_id: Uuid,
}

/// Path parameter for endpoints that operate on a single SSH
/// key by id, regardless of scope. Used by `GET
/// /v2/ssh-keys/{key_id}` and `DELETE /v2/ssh-keys/{key_id}`.
/// Visibility / ownership gating happens in the handler via the
/// visibility predicate.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SshKeyPath {
    pub key_id: Uuid,
}

/// Path parameter for endpoints that operate on a single image
/// by id, regardless of scope. Used by `GET /v2/images/{image_id}`
/// and `DELETE /v2/images/{image_id}`. Visibility / ownership
/// gating happens in the handler via the visibility predicate.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ImagePath {
    pub image_id: Uuid,
}

/// Path parameters for endpoints that operate on a single instance
/// inside a project inside a tenant.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct TenantProjectInstancePath {
    pub tenant_id: Uuid,
    pub project_id: Uuid,
    pub instance_id: Uuid,
}

/// Path parameters for endpoints that operate on a single NIC
/// belonging to an instance.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct TenantProjectInstanceNicPath {
    pub tenant_id: Uuid,
    pub project_id: Uuid,
    pub instance_id: Uuid,
    pub nic_id: Uuid,
}

/// Path parameters for endpoints that operate on a single Disk
/// belonging to an instance.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct TenantProjectInstanceDiskPath {
    pub tenant_id: Uuid,
    pub project_id: Uuid,
    pub instance_id: Uuid,
    pub disk_id: Uuid,
}

/// Path parameters for endpoints that operate on a single
/// FloatingIp inside a project.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct TenantProjectFloatingIpPath {
    pub tenant_id: Uuid,
    pub project_id: Uuid,
    pub floating_ip_id: Uuid,
}

/// Query parameters for metrics range endpoints
/// (`.../instances/{instance_id}/metrics`, `.../cns/{cn_id}/metrics`).
///
/// `range` is a short suffix-encoded duration (`5m`, `15m`, `1h`,
/// `6h`, `24h`, `7d`, `30d`) — these mirror the buttons in the V5
/// dashboard and decouple the wire format from a future re-skin.
/// `schema` selects which timeseries schema to plot; defaults to
/// `triton.cpu_per_zone` (VM detail) or `triton.cpu_per_cn` (node
/// detail), inferred server-side from the URL.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct MetricsRangeQuery {
    /// Short range identifier (e.g. `1h`).
    #[serde(default)]
    pub range: Option<String>,
    /// Schema name. Defaults to `triton.cpu_per_zone`.
    #[serde(default)]
    pub schema: Option<String>,
}

/// Path parameters for `GET /v2/tenants/{tenant_id}/metrics`. Reused
/// for the per-tenant Prometheus exposition; the existing
/// `TenantPath` lives in `types.rs` and is used by `/v2/tenants/{}/...`
/// CRUD endpoints, but the metrics endpoint adds new auth semantics
/// so it gets its own path type for clarity.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct TenantMetricsPath {
    pub tenant_id: Uuid,
}

/// Path parameters for the per-VM log tail endpoint.
/// `source` is `"console"` or `"platform"` (see
/// [`tritond_logs::LogSource`]).
#[derive(Debug, Deserialize, JsonSchema)]
pub struct InstanceLogsPath {
    pub tenant_id: Uuid,
    pub project_id: Uuid,
    pub instance_id: Uuid,
    /// URL-safe source name. `console` or `platform`.
    pub source: String,
}

/// Query parameters for the per-VM log tail endpoint.
///
/// `lines` defaults to 500 and is clamped at 5000 server-side.
/// `before_seq` is the pagination cursor returned in the previous
/// response's oldest line; absent on the first call.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct LogTailQuery {
    #[serde(default)]
    pub lines: Option<usize>,
    #[serde(default)]
    pub before_seq: Option<u64>,
}

/// Body for `POST /attach`. Names the target NIC the FloatingIp
/// should swap onto. The server resolves silo + project + instance
/// from the NIC; cross-tenant targets surface as 404.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AttachFloatingIpRequest {
    pub nic_id: Uuid,
}

/// Query parameters for `GET /v2/audit/events`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct AuditListQuery {
    /// Return events with `seq > after_seq`. Default 0 (start of chain).
    #[serde(default)]
    pub after_seq: Option<u64>,
    /// Maximum events to return. Default 100, max 1000.
    #[serde(default)]
    pub limit: Option<u32>,
}

/// Response body for `GET /v2/audit/events`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct AuditEventList {
    pub events: Vec<AuditEvent>,
    pub head: Option<AuditChainHead>,
}

/// Query parameters for `GET /v2/audit/verify`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct AuditVerifyQuery {
    /// First seq to walk. Default 0.
    #[serde(default)]
    pub from: Option<u64>,
    /// Last seq to walk. Default = current chain head.
    #[serde(default)]
    pub to: Option<u64>,
}

/// Response body for `GET /v2/audit/verify`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct AuditVerifyResponse {
    pub outcome: AuditVerifyOutcome,
    pub head: Option<AuditChainHead>,
}

/// Request body for `POST /v2/tenants/{tenant_id}/idp`. tritond
/// **eagerly** fetches the IdP's discovery document on this call;
/// a 4xx/5xx return means the IdP isn't reachable or doesn't speak
/// OIDC, and the config is not persisted.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct NewIdpConfig {
    pub issuer_url: String,
    pub client_id: String,
    pub client_secret: RedactedString,
    /// Expected `aud` claim. Defaults to `client_id` when omitted.
    #[serde(default)]
    pub audience: Option<String>,
}

/// Path parameters for endpoints that operate on one metadata scope
/// (silo / tenant / project / instance). The scope kind comes from
/// `{scope}` (deserialized as [`MetaScope`]); `{scope_id}` is the
/// UUID of that scope's owning entity. See `IMDS_DESIGN.md` §4.1.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct MetaScopePath {
    pub scope: tritond_store::MetaScope,
    pub scope_id: Uuid,
}

/// Query string for endpoints that operate on a single metadata
/// entry: the metadata key. The key may contain `/` segments
/// (`config/ntp-servers`, `state/active-color`, …), which is why it
/// lives in the query string rather than a URL path segment.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct MetaKeyQuery {
    pub key: String,
}

/// One metadata entry as it appears on the wire (list / get
/// responses). `value` is JSON; the flags + audit fields are flat at
/// the top level so the OpenAPI schema reads cleanly.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MetaEntry {
    pub key: String,
    #[serde(flatten)]
    pub value: tritond_store::MetaValue,
}

/// Request body for `PUT /v2/meta/{scope}/{scope_id}/entry`. `value`
/// is required; the two flags are optional and default to the values
/// from [`tritond_store::default_guest_visible`] (and `false` for
/// `guest_writable`).
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct SetMetaRequest {
    pub value: serde_json::Value,
    #[serde(default)]
    pub guest_visible: Option<bool>,
    #[serde(default)]
    pub guest_writable: Option<bool>,
}

/// Response body for `PUT /v2/meta/{scope}/{scope_id}/entry`. Carries
/// the stored entry and the scope's new generation counter (the
/// realized-view cache key on the agent side).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SetMetaResponse {
    pub entry: MetaEntry,
    pub generation: u64,
}

/// Request body for `PUT /v2/agent/instances/{instance_id}/meta`.
/// The agent-facing variant of [`SetMetaRequest`]; the agent can
/// only ever write `guest/*` keys at instance scope, so the visibility
/// + writable flags are fixed server-side (`guest_visible: true`,
/// `guest_writable: true`) and not under guest control. The agent
/// (and therefore the guest VM speaking to it) only supplies the
/// value.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SetGuestMetaRequest {
    pub value: serde_json::Value,
}

/// Path parameter for endpoints that operate on the realized view of
/// one instance (`/v2/meta/instance/{instance_id}/realized`).
#[derive(Debug, Deserialize, JsonSchema)]
pub struct InstanceRealizedMetaPath {
    pub instance_id: Uuid,
}

/// One leaf of the realized view: the effective key + value + the
/// scope provenance (`silo|tenant|project|instance|system`). The
/// computed system keys (`meta-data/*`, `triton/system/*`) appear with
/// `from = system`; everything else with the storage scope that won
/// the precedence merge. See `IMDS_DESIGN.md` §1.5.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RealizedMetaEntry {
    pub key: String,
    pub value: tritond_store::MetaValue,
    pub from: tritond_store::MetaProvenance,
}

/// Compact reference to an instance, used in the affected-instances
/// payload below. Carries the operator-relevant identity tuple
/// (`tenant_id`, `project_id`, `id`) plus the human-readable `name`
/// so the UI can render the row without a follow-up lookup.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AffectedInstanceRef {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub project_id: Uuid,
    pub name: String,
}

/// One row of the `shadowed` list returned by
/// `GET /v2/meta/{scope}/{scope_id}/affected?key=K`. Carries the
/// instance plus the scope that actually wins for it (a narrower
/// scope's override, or `System` for a computed system key that
/// shadows operator metadata).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ShadowedInstance {
    #[serde(flatten)]
    pub instance: AffectedInstanceRef,
    /// Which scope's value actually wins for this instance, given
    /// that the request-scope's value is being shadowed. Always one
    /// of the *narrower* scopes for a non-System winner; never the
    /// request-scope itself (those instances appear in `wins`).
    pub winner_scope: tritond_store::MetaProvenance,
}

/// Response body for `GET /v2/meta/{scope}/{scope_id}/affected?key=K`
/// — the affected-instances reverse-index. Answers the question
/// "if I edit `key` at this scope, which instances does that change
/// actually flow to, and which are shielded by a narrower override?"
/// See `IMDS_DESIGN.md` §1.5 for the precedence rules.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AffectedInstancesResponse {
    /// The value stored at the request scope (`null` when the key is
    /// not set there). Returned for context — the UI doesn't have to
    /// make a separate `GET /v2/meta/.../entry` call.
    pub value_at_scope: Option<tritond_store::MetaValue>,
    /// Instances under the request scope where this scope's value
    /// is the realized winner (no narrower scope overrides it).
    pub wins: Vec<AffectedInstanceRef>,
    /// Instances under the request scope where a narrower scope
    /// already overrides this key. Each row names the narrower
    /// scope so the UI can render "tenant wins" / "project wins" /
    /// "instance wins" at a glance.
    pub shadowed: Vec<ShadowedInstance>,
}

#[dropshot::api_description]
pub trait TritondApi {
    /// Context type for request handlers.
    type Context: Send + Sync + 'static;

    /// Liveness check. Returns service status and version string.
    #[endpoint {
        method = GET,
        path = "/v2/health",
        tags = ["system"],
    }]
    async fn health(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseOk<HealthResponse>, HttpError>;

    /// List every silo, sorted by `name`. Phase 0 has no per-operator
    /// silo visibility filter so this returns the full set.
    #[endpoint {
        method = GET,
        path = "/v2/silos",
        tags = ["silos"],
    }]
    async fn list_silos(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseOk<Vec<Silo>>, HttpError>;

    /// Create a silo. Returns 201 with the created silo.
    ///
    /// Fails with 409 if a silo with the requested name already exists.
    #[endpoint {
        method = POST,
        path = "/v2/silos",
        tags = ["silos"],
    }]
    async fn create_silo(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<NewSilo>,
    ) -> Result<HttpResponseCreated<Silo>, HttpError>;

    /// Look up a silo by id. Returns 404 if no such silo exists.
    #[endpoint {
        method = GET,
        path = "/v2/silos/{silo_id}",
        tags = ["silos"],
    }]
    async fn get_silo(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloPath>,
    ) -> Result<HttpResponseOk<Silo>, HttpError>;

    /// Exchange username + password for an access/refresh token pair.
    /// Returns 401 if credentials are invalid.
    #[endpoint {
        method = POST,
        path = "/v2/auth/login",
        tags = ["auth"],
    }]
    async fn login(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<LoginRequest>,
    ) -> Result<HttpResponseOk<TokenResponse>, HttpError>;

    /// Exchange a valid refresh token for a fresh access/refresh pair.
    /// Returns 401 if the refresh token is invalid or expired.
    #[endpoint {
        method = POST,
        path = "/v2/auth/refresh",
        tags = ["auth"],
    }]
    async fn refresh(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<RefreshRequest>,
    ) -> Result<HttpResponseOk<TokenResponse>, HttpError>;

    /// Create a long-lived API key for the calling user. The plaintext
    /// secret is included in the response **once** and never shown again.
    #[endpoint {
        method = POST,
        path = "/v2/auth/api-keys",
        tags = ["auth"],
    }]
    async fn create_api_key(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<NewApiKey>,
    ) -> Result<HttpResponseCreated<ApiKeyCreated>, HttpError>;

    /// List the calling user's API keys. The plaintext secret is never
    /// returned by this endpoint.
    #[endpoint {
        method = GET,
        path = "/v2/auth/api-keys",
        tags = ["auth"],
    }]
    async fn list_api_keys(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseOk<Vec<ApiKeyView>>, HttpError>;

    /// Delete one of the calling user's API keys. Returns 404 if the
    /// id does not belong to the calling user.
    #[endpoint {
        method = DELETE,
        path = "/v2/auth/api-keys/{api_key_id}",
        tags = ["auth"],
    }]
    async fn delete_api_key(
        rqctx: RequestContext<Self::Context>,
        path: Path<ApiKeyPath>,
    ) -> Result<HttpResponseDeleted, HttpError>;

    /// Page through audit events. Returns at most `limit` events with
    /// `seq > after_seq` plus the current chain head.
    #[endpoint {
        method = GET,
        path = "/v2/audit/events",
        tags = ["audit"],
    }]
    async fn list_audit_events(
        rqctx: RequestContext<Self::Context>,
        query: Query<AuditListQuery>,
    ) -> Result<HttpResponseOk<AuditEventList>, HttpError>;

    /// Fetch a single audit event by sequence number.
    #[endpoint {
        method = GET,
        path = "/v2/audit/events/{seq}",
        tags = ["audit"],
    }]
    async fn get_audit_event(
        rqctx: RequestContext<Self::Context>,
        path: Path<AuditEventPath>,
    ) -> Result<HttpResponseOk<AuditEvent>, HttpError>;

    /// Walk the audit chain in `[from, to]` and recompute hashes.
    /// Returns the first divergence (if any) plus the current head.
    /// Cheap on small ranges; auditors typically walk the entire
    /// chain once per export.
    #[endpoint {
        method = GET,
        path = "/v2/audit/verify",
        tags = ["audit"],
    }]
    async fn verify_audit_chain(
        rqctx: RequestContext<Self::Context>,
        query: Query<AuditVerifyQuery>,
    ) -> Result<HttpResponseOk<AuditVerifyResponse>, HttpError>;

    /// List long-running operations (RFD 00004 SG-4). Returns
    /// every saga the SecStore knows about — `Running`, `Stuck`,
    /// and `Done` alike. Continuation pagination via the
    /// `after_id` query parameter. Operator-only at SG-4; SG-4b
    /// will add tenant scoping once the catalog has tenant-scoped
    /// sagas with resource references.
    #[endpoint {
        method = GET,
        path = "/v2/operations",
        tags = ["operations"],
    }]
    async fn list_operations(
        rqctx: RequestContext<Self::Context>,
        query: Query<ListOperationsQuery>,
    ) -> Result<HttpResponseOk<Vec<OperationSummary>>, HttpError>;

    /// Detail view for a single operation: summary + persisted DAG
    /// + fence fields. Used by `tcadm operations get` and the
    /// adminUI Operations detail panel.
    #[endpoint {
        method = GET,
        path = "/v2/operations/{operation_id}",
        tags = ["operations"],
    }]
    async fn get_operation(
        rqctx: RequestContext<Self::Context>,
        path: Path<OperationPath>,
    ) -> Result<HttpResponseOk<OperationDetail>, HttpError>;

    /// Operator-initiated unwind (RFD 00004 D-Sg-12). Injects an
    /// error at every pending node of the saga so the next node
    /// the saga reaches fails, triggering the catalog's own undos
    /// in reverse. Any currently-running action body completes its
    /// natural outcome first; there is no preemption of in-flight
    /// work.
    ///
    /// Returns the count of nodes poked. Operator-only.
    #[endpoint {
        method = POST,
        path = "/v2/operations/{operation_id}/abandon",
        tags = ["operations"],
    }]
    async fn abandon_operation(
        rqctx: RequestContext<Self::Context>,
        path: Path<OperationPath>,
    ) -> Result<HttpResponseOk<AbandonResponse>, HttpError>;

    // -----------------------------------------------------------------
    // Live migrations (LM-1, read-only)
    // -----------------------------------------------------------------

    /// List recent live-migration records across the fleet,
    /// newest-first, paged by id cursor. Operator-only at LM-1;
    /// per-tenant scoping is a follow-up once the per-instance
    /// route below covers the customer-facing view.
    #[endpoint {
        method = GET,
        path = "/v2/migrations",
        tags = ["migrations"],
    }]
    async fn list_migrations(
        rqctx: RequestContext<Self::Context>,
        query: Query<ListMigrationsQuery>,
    ) -> Result<HttpResponseOk<Vec<tritond_store::MigrationRecord>>, HttpError>;

    /// One migration by id.
    #[endpoint {
        method = GET,
        path = "/v2/migrations/{migration_id}",
        tags = ["migrations"],
    }]
    async fn get_migration(
        rqctx: RequestContext<Self::Context>,
        path: Path<MigrationPath>,
    ) -> Result<HttpResponseOk<tritond_store::MigrationRecord>, HttpError>;

    /// Page the per-migration progress event log. Operators poll
    /// this from the adminUI / `tcadm migrations get --watch` by
    /// passing the highest `seq` they've already seen as
    /// `after_seq`.
    #[endpoint {
        method = GET,
        path = "/v2/migrations/{migration_id}/progress",
        tags = ["migrations"],
    }]
    async fn list_migration_progress(
        rqctx: RequestContext<Self::Context>,
        path: Path<MigrationPath>,
        query: Query<ListMigrationProgressQuery>,
    ) -> Result<HttpResponseOk<Vec<tritond_store::MigrationProgressEvent>>, HttpError>;

    /// Per-instance migration history (newest first). Includes
    /// terminal records so an operator can see the VM's full
    /// migration timeline.
    #[endpoint {
        method = GET,
        path = "/v2/instances/{instance_id}/migrations",
        tags = ["migrations"],
    }]
    async fn list_instance_migrations(
        rqctx: RequestContext<Self::Context>,
        path: Path<InstanceMigrationsPath>,
    ) -> Result<HttpResponseOk<Vec<tritond_store::MigrationRecord>>, HttpError>;

    /// Atomically claim the next Pending provisioning job.
    /// Returns `200 OK` with `{"job": null}` when the queue is
    /// empty (the agent should sleep before its next poll), and
    /// `200 OK` with `{"job": {...}}` when a job was claimed and
    /// transitioned to `InProgress`. Auth: requires an API key
    /// with [`tritond_store::ApiKeyScope::Agent`].
    ///
    /// The path is `/v2/agent/claim` rather than
    /// `/v2/agent/jobs/claim` because Dropshot's router cannot
    /// disambiguate a literal `claim` segment from a `{job_id}`
    /// path parameter at the same level.
    #[endpoint {
        method = POST,
        path = "/v2/agent/claim",
        tags = ["agent"],
    }]
    async fn agent_claim_job(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<ClaimJobRequest>,
    ) -> Result<HttpResponseOk<ClaimJobResponse>, HttpError>;

    /// Mark a previously-claimed provisioning job terminal.
    /// `outcome` is `Completed` for success or `Failed { reason }`
    /// for an agent-side abort. Auth: requires an API key with
    /// [`tritond_store::ApiKeyScope::Agent`].
    #[endpoint {
        method = POST,
        path = "/v2/agent/jobs/{job_id}/complete",
        tags = ["agent"],
    }]
    async fn agent_complete_job(
        rqctx: RequestContext<Self::Context>,
        path: Path<AgentJobPath>,
        body: TypedBody<CompleteJobRequest>,
    ) -> Result<HttpResponseOk<ProvisioningJob>, HttpError>;

    /// Materialise the full blueprint the agent needs to act on
    /// a claimed job — instance + image + NICs + disks +
    /// authorised SSH public keys, all in one response. Auth:
    /// requires an API key with
    /// [`tritond_store::ApiKeyScope::Agent`].
    #[endpoint {
        method = GET,
        path = "/v2/agent/jobs/{job_id}/blueprint",
        tags = ["agent"],
    }]
    async fn agent_job_blueprint(
        rqctx: RequestContext<Self::Context>,
        path: Path<AgentJobPath>,
    ) -> Result<HttpResponseOk<ProvisioningBlueprint>, HttpError>;

    /// Materialise the Proteus per-port blueprint for a NIC. Auth:
    /// requires a CN-bound API key with
    /// [`tritond_store::ApiKeyScope::Agent`]. The bound CN must have
    /// an in-progress claim for the port's instance.
    #[endpoint {
        method = GET,
        path = "/v2/agent/blueprints/{port_id}",
        tags = ["agent"],
    }]
    async fn agent_port_blueprint(
        rqctx: RequestContext<Self::Context>,
        path: Path<AgentPortBlueprintPath>,
    ) -> Result<HttpResponseOk<AgentPortBlueprint>, HttpError>;

    /// Resolve one in-VPC peer (`vni`, `ip`) -> guest MAC + host
    /// underlay. The bound CN agent calls this on every v2p cache
    /// miss; tritond walks the NIC table, finds the NIC that owns
    /// the IP, looks up its host CN, and returns the CN's underlay
    /// address. Returns 404 when no realized NIC owns the IP.
    /// See `PROTEUS_PLAN.md` §11.7.1.
    #[endpoint {
        method = GET,
        path = "/v2/agent/peer",
        tags = ["agent"],
    }]
    async fn agent_peer_resolve(
        rqctx: RequestContext<Self::Context>,
        query: Query<AgentPeerResolveQuery>,
    ) -> Result<HttpResponseOk<AgentPeerResolveResponse>, HttpError>;

    /// Pull pending v2p invalidations for this CN, strictly after
    /// the supplied `since` cursor. The bound CN agent polls this
    /// on a fixed cadence (default ~10s) and applies each entry
    /// via `InvalidatePeerEntry` on every local port that might
    /// have cached it. Phase A v1: tritond broadcasts NIC-teardown
    /// invalidations to all CNs; Phase B adds per-CN filtering by
    /// tracking which CNs have queried `/v2/agent/peer`. See
    /// `PROTEUS_PLAN.md` §11.7.1.
    #[endpoint {
        method = GET,
        path = "/v2/agent/peer-invalidations",
        tags = ["agent"],
    }]
    async fn agent_peer_invalidations(
        rqctx: RequestContext<Self::Context>,
        query: Query<AgentPeerInvalidationsQuery>,
    ) -> Result<HttpResponseOk<AgentPeerInvalidationsResponse>, HttpError>;

    /// Agent-facing realized metadata view for one instance. Same
    /// body as [`Self::get_instance_realized_meta`] but auth is
    /// the CN-bound `Action::AgentBlueprint` scope (matches
    /// [`Self::agent_peer_resolve`]). The tenant-facing endpoint
    /// requires tenant-member Cedar, which a CN-bound API key
    /// cannot satisfy — but tritonagent's IMDS daemon needs to
    /// read the realized view to answer guest IMDSv2 requests.
    /// The dataplane already enforces locality: the IMDS request
    /// arrives via the guest's vnic on this CN, so any instance
    /// the agent asks about is one currently placed on the
    /// agent's CN.
    #[endpoint {
        method = GET,
        path = "/v2/agent/instances/{instance_id}/realized-meta",
        tags = ["agent"],
    }]
    async fn agent_get_instance_realized_meta(
        rqctx: RequestContext<Self::Context>,
        path: Path<InstanceRealizedMetaPath>,
    ) -> Result<HttpResponseOk<Vec<RealizedMetaEntry>>, HttpError>;

    /// Agent-facing instance-scoped `guest/*` metadata writeback.
    /// The IMDSv2 listener in tritonagent forwards a guest VM's
    /// `PUT /triton/guest/<key>` through this endpoint. The server
    /// enforces:
    ///
    /// * the key must start with `guest/` (no other prefix);
    /// * scope is always Instance, instance_id is the URL path;
    /// * `guest_visible` and `guest_writable` are forced true
    ///   (operator-set entries can flip these via the tenant-facing
    ///   set_meta surface; the guest never controls them);
    /// * the calling agent is CN-bound (`Action::AgentBlueprint`);
    /// * the instance is currently placed on the agent's CN —
    ///   already enforced by the IMDS dataplane (the request
    ///   reaches the agent only via that instance's vnic).
    #[endpoint {
        method = PUT,
        path = "/v2/agent/instances/{instance_id}/meta",
        tags = ["agent"],
    }]
    async fn agent_set_instance_guest_meta(
        rqctx: RequestContext<Self::Context>,
        path: Path<InstanceRealizedMetaPath>,
        query: Query<MetaKeyQuery>,
        body: TypedBody<SetGuestMetaRequest>,
    ) -> Result<HttpResponseOk<SetMetaResponse>, HttpError>;

    /// Heartbeat from a bound agent. Lightweight ping — empty
    /// body, just bumps `Cn.last_seen`. Auth: requires an API
    /// key with [`tritond_store::ApiKeyScope::Agent`] AND a
    /// `bound_to_cn` value (the per-CN keys minted at approval).
    /// Unbound Agent keys (legacy operator-minted) get 403 since
    /// there's no CN to attribute the heartbeat to.
    #[endpoint {
        method = POST,
        path = "/v2/agent/heartbeat",
        tags = ["agent"],
    }]
    async fn agent_heartbeat(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseOk<()>, HttpError>;

    /// Full status sample from a bound agent. Replaces
    /// `Cn.last_status` and bumps `last_seen`. Same auth shape
    /// as `agent_heartbeat`.
    #[endpoint {
        method = POST,
        path = "/v2/agent/status",
        tags = ["agent"],
    }]
    async fn agent_status(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<AgentStatusRequest>,
    ) -> Result<HttpResponseOk<()>, HttpError>;

    /// Report realized network state for a single resource. Auth:
    /// requires a CN-bound API key with
    /// [`tritond_store::ApiKeyScope::Agent`]. Backward generation
    /// reports for the same `(resource, realizer)` are rejected with
    /// `409 Conflict`.
    #[endpoint {
        method = POST,
        path = "/v2/agent/network-realization",
        tags = ["agent"],
    }]
    async fn agent_report_network_realization(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<NetworkRealizationRequest>,
    ) -> Result<HttpResponseOk<()>, HttpError>;

    /// Report DHCP request activity the kmod observed for guest ports.
    /// Auth: requires a CN-bound API key with
    /// [`tritond_store::ApiKeyScope::Agent`]. State-sample traffic —
    /// the response is always `200 OK`; items for unknown ports or
    /// ports with no lease record yet are silently skipped.
    #[endpoint {
        method = POST,
        path = "/v2/agent/dhcp-lease-activity",
        tags = ["agent"],
    }]
    async fn agent_report_dhcp_lease_activity(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<DhcpLeaseActivityReport>,
    ) -> Result<HttpResponseOk<()>, HttpError>;

    /// Self-register a compute node. Anonymous endpoint (no API
    /// key needed) — the agent has none until approval completes.
    /// Tritond creates a [`CnState::Pending`] record with a fresh
    /// claim code unless the global auto-approve window is open,
    /// in which case the record is created directly Approved and
    /// the agent will retrieve its API key on the very next
    /// `/register/status` long-poll.
    ///
    /// Idempotent on `server_uuid`: re-registration of a Pending
    /// record rotates the claim code; re-registration of an
    /// Approved record refreshes sysinfo without re-minting
    /// credentials.
    #[endpoint {
        method = POST,
        path = "/v2/agent/register",
        tags = ["agent"],
    }]
    async fn agent_register(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<RegisterCnRequest>,
    ) -> Result<HttpResponseOk<RegisterCnResponse>, HttpError>;

    /// Long-poll for the per-CN API key. Anonymous endpoint
    /// authenticated only by holding the `poll_token` returned at
    /// registration. Tritond holds the connection open for up to
    /// ~30s waiting for state to flip from Pending to Approved
    /// (or for the auto-approve credential to be wired up); on
    /// timeout the agent re-polls.
    ///
    /// The `api_key` field is populated **once** — on the first
    /// successful retrieval after approval. Subsequent calls
    /// return `state = Approved` with `api_key = None`.
    #[endpoint {
        method = GET,
        path = "/v2/agent/register/status",
        tags = ["agent"],
    }]
    async fn agent_register_status(
        rqctx: RequestContext<Self::Context>,
        query: Query<RegisterStatusQuery>,
    ) -> Result<HttpResponseOk<RegisterStatusResponse>, HttpError>;

    /// List compute nodes, optionally filtered by state. Operator
    /// surface (root + future fleet-scoped operator role).
    #[endpoint {
        method = GET,
        path = "/v2/cns",
        tags = ["cns"],
    }]
    async fn list_cns(
        rqctx: RequestContext<Self::Context>,
        query: Query<CnListQuery>,
    ) -> Result<HttpResponseOk<Vec<CnView>>, HttpError>;

    /// Read a single compute-node record by `server_uuid`.
    #[endpoint {
        method = GET,
        path = "/v2/cns/{server_uuid}",
        tags = ["cns"],
    }]
    async fn get_cn(
        rqctx: RequestContext<Self::Context>,
        path: Path<CnPath>,
    ) -> Result<HttpResponseOk<CnView>, HttpError>;

    /// Approve a Pending compute node by claim code. Mints the
    /// per-CN API key inside the same transaction that flips
    /// state; the plaintext is delivered to the agent via its
    /// long-poll on `/register/status`. Per-source-IP rate-limited.
    ///
    /// Returns 404 for unknown / expired / already-approved
    /// codes (conflated to defeat enumeration). Returns 429
    /// when the per-IP bucket is drained.
    ///
    /// The path is `/v2/cn-approvals` rather than nested under
    /// `/v2/cns/...` because Dropshot's router cannot
    /// disambiguate a literal `approve` segment from the
    /// `{server_uuid}` parameter at the same level.
    #[endpoint {
        method = POST,
        path = "/v2/cn-approvals",
        tags = ["cns"],
    }]
    async fn approve_cn(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<ApproveCnRequest>,
    ) -> Result<HttpResponseOk<CnView>, HttpError>;

    /// Disable a compute node. Revokes the bound API key and
    /// flips state to Disabled. The record is retained for audit
    /// visibility; a second call is idempotent.
    #[endpoint {
        method = POST,
        path = "/v2/cns/{server_uuid}/disable",
        tags = ["cns"],
    }]
    async fn disable_cn(
        rqctx: RequestContext<Self::Context>,
        path: Path<CnPath>,
    ) -> Result<HttpResponseOk<CnView>, HttpError>;

    /// Set the placement role for a compute node. Operator-only.
    #[endpoint {
        method = POST,
        path = "/v2/cns/{server_uuid}/role",
        tags = ["cns"],
    }]
    async fn set_cn_role(
        rqctx: RequestContext<Self::Context>,
        path: Path<CnPath>,
        body: TypedBody<SetCnRoleRequest>,
    ) -> Result<HttpResponseOk<CnView>, HttpError>;

    /// Dry-run the drain plan for one CN. For each instance currently
    /// hosted on this CN the placement engine picks a candidate target
    /// (excluding the source CN); the response partitions into
    /// `placeable` (a target was found) and `not_placeable` (no
    /// eligible CN). Also surfaces a quorum heuristic so the operator
    /// can spot vault / etcd / fdb / etc. members before committing
    /// the drain. Read-only — no reservations are written.
    ///
    /// Used by the operator console's BlastRadiusCard on Compute Node
    /// detail (admin v3 design) to show "12 instances would migrate
    /// to: monroe-r2-s01 × 7, monroe-r2-s02 × 5; capacity OK; quorum
    /// at risk: vault-primary" before the operator commits.
    #[endpoint {
        method = POST,
        path = "/v2/cns/{server_uuid}/drain/preview",
        tags = ["cns"],
    }]
    async fn drain_preview(
        rqctx: RequestContext<Self::Context>,
        path: Path<CnPath>,
    ) -> Result<HttpResponseOk<DrainPreviewResponse>, HttpError>;

    /// Read the current auto-approve window, if open.
    /// Returns `200 OK` with `null` when no window is open.
    ///
    /// Path lives at `/v2/cn-auto-approve` rather than nested
    /// under `/v2/cns/...` for the same reason as `/v2/cn-approvals`:
    /// a literal `auto-approve` segment cannot coexist with the
    /// `{server_uuid}` parameter on Dropshot's router.
    #[endpoint {
        method = GET,
        path = "/v2/cn-auto-approve",
        tags = ["cns"],
    }]
    async fn get_auto_approve_window(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseOk<Option<AutoApproveWindow>>, HttpError>;

    /// Open (or replace) the auto-approve window. While the
    /// window is open, new self-registrations are promoted to
    /// Approved without operator action. Server-side cap:
    /// `duration_secs` is clamped to 24h.
    #[endpoint {
        method = POST,
        path = "/v2/cn-auto-approve",
        tags = ["cns"],
    }]
    async fn open_auto_approve_window(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<OpenAutoApproveRequest>,
    ) -> Result<HttpResponseOk<AutoApproveWindow>, HttpError>;

    /// Close the auto-approve window. Idempotent; no-op when no
    /// window is open.
    #[endpoint {
        method = DELETE,
        path = "/v2/cn-auto-approve",
        tags = ["cns"],
    }]
    async fn close_auto_approve_window(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseDeleted, HttpError>;

    /// List every cluster-wide configuration key with its current
    /// value, built-in default, description, restart requirement, and
    /// whether an environment variable is currently overriding it at
    /// boot. Fleet-admin only.
    #[endpoint {
        method = GET,
        path = "/v2/config",
        tags = ["config"],
    }]
    async fn list_config(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseOk<Vec<ConfigEntry>>, HttpError>;

    /// Read one configuration key. Returns `404` for an unknown key.
    /// Fleet-admin only.
    #[endpoint {
        method = GET,
        path = "/v2/config/{key}",
        tags = ["config"],
    }]
    async fn get_config(
        rqctx: RequestContext<Self::Context>,
        path: Path<ConfigKeyPath>,
    ) -> Result<HttpResponseOk<ConfigEntry>, HttpError>;

    /// Set one configuration key. Returns `404` for an unknown key,
    /// `400` for an ill-typed value, the updated entry otherwise. The
    /// new value is persisted to FoundationDB and takes effect on the
    /// next `tritond` restart. Fleet-admin only; the change is recorded
    /// in the audit log.
    #[endpoint {
        method = PUT,
        path = "/v2/config/{key}",
        tags = ["config"],
    }]
    async fn set_config(
        rqctx: RequestContext<Self::Context>,
        path: Path<ConfigKeyPath>,
        body: TypedBody<SetConfigRequest>,
    ) -> Result<HttpResponseOk<ConfigEntry>, HttpError>;

    /// Reset one configuration key to its built-in default. Returns
    /// `404` for an unknown key, the (default-valued) entry otherwise.
    /// Persisted to FoundationDB; takes effect on the next restart.
    /// Fleet-admin only; recorded in the audit log.
    #[endpoint {
        method = DELETE,
        path = "/v2/config/{key}",
        tags = ["config"],
    }]
    async fn reset_config(
        rqctx: RequestContext<Self::Context>,
        path: Path<ConfigKeyPath>,
    ) -> Result<HttpResponseOk<ConfigEntry>, HttpError>;

    /// Configure the OIDC identity provider for a tenant. Returns
    /// 502 if the discovery document cannot be fetched, 404 if the
    /// tenant does not exist, 409 if a different tenant already
    /// claims the same `issuer_url`, otherwise 201 with the
    /// redacted view of what was persisted.
    #[endpoint {
        method = POST,
        path = "/v2/tenants/{tenant_id}/idp",
        tags = ["tenants", "auth"],
    }]
    async fn put_tenant_idp(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantIdpPath>,
        body: TypedBody<NewIdpConfig>,
    ) -> Result<HttpResponseCreated<IdpConfigView>, HttpError>;

    /// Read the OIDC IdP config for a tenant. The client secret is
    /// never returned. 404 when no IdP is configured.
    #[endpoint {
        method = GET,
        path = "/v2/tenants/{tenant_id}/idp",
        tags = ["tenants", "auth"],
    }]
    async fn get_tenant_idp(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantIdpPath>,
    ) -> Result<HttpResponseOk<IdpConfigView>, HttpError>;

    /// Remove the OIDC IdP config for a tenant. Federated users
    /// in that tenant will fail to authenticate until a new
    /// config is posted.
    #[endpoint {
        method = DELETE,
        path = "/v2/tenants/{tenant_id}/idp",
        tags = ["tenants", "auth"],
    }]
    async fn delete_tenant_idp(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantIdpPath>,
    ) -> Result<HttpResponseDeleted, HttpError>;

    /// List the tenants in a silo. Operator-facing surface; root
    /// can administer tenants directly.
    #[endpoint {
        method = GET,
        path = "/v2/silos/{silo_id}/tenants",
        tags = ["silos", "tenants"],
    }]
    async fn list_silo_tenants(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloPath>,
    ) -> Result<HttpResponseOk<Vec<Tenant>>, HttpError>;

    /// Create a tenant in a silo. Returns 409 if the tenant name is
    /// already in use within the silo, 404 if the silo does not exist.
    #[endpoint {
        method = POST,
        path = "/v2/silos/{silo_id}/tenants",
        tags = ["silos", "tenants"],
    }]
    async fn create_silo_tenant(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloPath>,
        body: TypedBody<NewTenant>,
    ) -> Result<HttpResponseCreated<Tenant>, HttpError>;

    /// Read a single tenant. Returns 404 when the tenant does not
    /// exist or belongs to a different silo (cross-silo probes do
    /// not learn that the resource exists).
    #[endpoint {
        method = GET,
        path = "/v2/silos/{silo_id}/tenants/{tenant_id}",
        tags = ["silos", "tenants"],
    }]
    async fn get_silo_tenant(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloTenantPath>,
    ) -> Result<HttpResponseOk<Tenant>, HttpError>;

    /// Delete a tenant. Returns 404 when the tenant does not exist
    /// or belongs to a different silo. Phase-0 deletion is
    /// permissive (does not check for child projects); the
    /// block-on-children guard belongs in a future cleanup.
    #[endpoint {
        method = DELETE,
        path = "/v2/silos/{silo_id}/tenants/{tenant_id}",
        tags = ["silos", "tenants"],
    }]
    async fn delete_silo_tenant(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloTenantPath>,
    ) -> Result<HttpResponseDeleted, HttpError>;

    /// List the projects inside a tenant.
    #[endpoint {
        method = GET,
        path = "/v2/tenants/{tenant_id}/projects",
        tags = ["projects"],
    }]
    async fn list_tenant_projects(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantPath>,
    ) -> Result<HttpResponseOk<Vec<Project>>, HttpError>;

    /// Create a project in a tenant. Returns 409 if the project
    /// name is already in use within that tenant.
    #[endpoint {
        method = POST,
        path = "/v2/tenants/{tenant_id}/projects",
        tags = ["projects"],
    }]
    async fn create_tenant_project(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantPath>,
        body: TypedBody<NewProject>,
    ) -> Result<HttpResponseCreated<Project>, HttpError>;

    /// Read a single project. Returns 404 when the project does not
    /// exist or belongs to a different tenant (cross-tenant probes do
    /// not learn that other tenants exist).
    #[endpoint {
        method = GET,
        path = "/v2/tenants/{tenant_id}/projects/{project_id}",
        tags = ["projects"],
    }]
    async fn get_tenant_project(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectPath>,
    ) -> Result<HttpResponseOk<Project>, HttpError>;

    /// Delete a project. Returns 404 when the project does not exist
    /// or belongs to a different tenant.
    #[endpoint {
        method = DELETE,
        path = "/v2/tenants/{tenant_id}/projects/{project_id}",
        tags = ["projects"],
    }]
    async fn delete_tenant_project(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectPath>,
    ) -> Result<HttpResponseDeleted, HttpError>;

    /// List the VPCs inside a project. Returns 404 when the tenant or
    /// project does not exist (or the project belongs to a different
    /// tenant).
    #[endpoint {
        method = GET,
        path = "/v2/tenants/{tenant_id}/projects/{project_id}/vpcs",
        tags = ["vpcs"],
    }]
    async fn list_project_vpcs(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectPath>,
    ) -> Result<HttpResponseOk<Vec<Vpc>>, HttpError>;

    /// Create a VPC in a project. Returns 400 if neither `ipv4_block`
    /// nor `ipv6_block` is provided. Returns 409 if a VPC with the
    /// same name already exists in the project. The server assigns
    /// `id`, `vni` (random in `[4096, 2^24)`, unique rack-wide), and
    /// `created_at`.
    #[endpoint {
        method = POST,
        path = "/v2/tenants/{tenant_id}/projects/{project_id}/vpcs",
        tags = ["vpcs"],
    }]
    async fn create_project_vpc(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectPath>,
        body: TypedBody<NewVpc>,
    ) -> Result<HttpResponseCreated<Vpc>, HttpError>;

    /// Read a single VPC. Returns 404 when the VPC does not exist or
    /// belongs to a different tenant or project (cross-tenant probes
    /// do not learn that the resource exists).
    #[endpoint {
        method = GET,
        path = "/v2/tenants/{tenant_id}/projects/{project_id}/vpcs/{vpc_id}",
        tags = ["vpcs"],
    }]
    async fn get_project_vpc(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcPath>,
    ) -> Result<HttpResponseOk<Vpc>, HttpError>;

    /// Delete a VPC. Returns 404 when the VPC does not exist or
    /// belongs to a different tenant or project. Returns 409 if the
    /// VPC still has subnets attached — operators must clear subnets
    /// before deleting the parent VPC (Phase 0 has no cascade).
    #[endpoint {
        method = DELETE,
        path = "/v2/tenants/{tenant_id}/projects/{project_id}/vpcs/{vpc_id}",
        tags = ["vpcs"],
    }]
    async fn delete_project_vpc(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcPath>,
    ) -> Result<HttpResponseDeleted, HttpError>;

    /// List the subnets inside a VPC. Returns 404 when the tenant,
    /// project, or VPC does not exist (or is in the wrong parent).
    #[endpoint {
        method = GET,
        path = "/v2/tenants/{tenant_id}/projects/{project_id}/vpcs/{vpc_id}/subnets",
        tags = ["subnets"],
    }]
    async fn list_vpc_subnets(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcPath>,
    ) -> Result<HttpResponseOk<Vec<Subnet>>, HttpError>;

    /// Create a subnet in a VPC. Returns 400 if neither
    /// `ipv4_block` nor `ipv6_block` is provided. Returns 409 if a
    /// subnet with the same name already exists in the VPC, if a
    /// CIDR is not contained in the parent VPC's matching-family
    /// CIDR, or if a CIDR overlaps an existing subnet's CIDR. The
    /// server assigns `id` and `created_at`.
    #[endpoint {
        method = POST,
        path = "/v2/tenants/{tenant_id}/projects/{project_id}/vpcs/{vpc_id}/subnets",
        tags = ["subnets"],
    }]
    async fn create_vpc_subnet(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcPath>,
        body: TypedBody<NewSubnet>,
    ) -> Result<HttpResponseCreated<Subnet>, HttpError>;

    /// Read a single subnet. Returns 404 when the subnet does not
    /// exist or belongs to a different tenant, project, or VPC.
    #[endpoint {
        method = GET,
        path = "/v2/tenants/{tenant_id}/projects/{project_id}/vpcs/{vpc_id}/subnets/{subnet_id}",
        tags = ["subnets"],
    }]
    async fn get_vpc_subnet(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcSubnetPath>,
    ) -> Result<HttpResponseOk<Subnet>, HttpError>;

    /// Delete a subnet. Returns 404 when the subnet does not exist
    /// or belongs to a different tenant, project, or VPC.
    #[endpoint {
        method = DELETE,
        path = "/v2/tenants/{tenant_id}/projects/{project_id}/vpcs/{vpc_id}/subnets/{subnet_id}",
        tags = ["subnets"],
    }]
    async fn delete_vpc_subnet(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcSubnetPath>,
    ) -> Result<HttpResponseDeleted, HttpError>;

    /// RFD 00007 `GET /v1/vpc-dhcp-pools/{vpc_id}`. Flat single
    /// per-VPC DHCP-pool read. 404s when no pool is set.
    #[endpoint {
        method = GET,
        path = "/v1/vpc-dhcp-pools/{vpc_id}",
        tags = ["dhcp"],
    }]
    async fn get_vpc_dhcp_pool_v1(
        rqctx: RequestContext<Self::Context>,
        path: Path<crate::v1::VpcDhcpPoolPath>,
    ) -> Result<HttpResponseOk<DhcpPool>, HttpError>;

    /// RFD 00007 `GET /v1/vpc-dhcp-leases?vpc=<uuid>`.
    #[endpoint {
        method = GET,
        path = "/v1/vpc-dhcp-leases",
        tags = ["dhcp"],
    }]
    async fn list_dhcp_leases_v1(
        rqctx: RequestContext<Self::Context>,
        query: Query<crate::v1::VpcDhcpQuery>,
    ) -> Result<HttpResponseOk<crate::v1::ResultsPage<DhcpLease>>, HttpError>;

    /// RFD 00007 `GET /v1/vpc-dhcp-leases/{mac}`. Bare-MAC lookup
    /// (MAC is unique by invariant; backed by the AP-1c
    /// `dhcp_lease/by_mac/` index).
    #[endpoint {
        method = GET,
        path = "/v1/vpc-dhcp-leases/{mac}",
        tags = ["dhcp"],
    }]
    async fn get_dhcp_lease_v1(
        rqctx: RequestContext<Self::Context>,
        path: Path<crate::v1::DhcpMacPath>,
    ) -> Result<HttpResponseOk<DhcpLease>, HttpError>;

    /// RFD 00007 `GET /v1/vpc-dhcp-reservations?vpc=<uuid>`.
    #[endpoint {
        method = GET,
        path = "/v1/vpc-dhcp-reservations",
        tags = ["dhcp"],
    }]
    async fn list_dhcp_reservations_v1(
        rqctx: RequestContext<Self::Context>,
        query: Query<crate::v1::VpcDhcpQuery>,
    ) -> Result<HttpResponseOk<crate::v1::ResultsPage<DhcpReservation>>, HttpError>;

    /// RFD 00007 `GET /v1/firewall-rules?vpc=<uuid>`.
    #[endpoint {
        method = GET,
        path = "/v1/firewall-rules",
        tags = ["firewall-rules"],
    }]
    async fn list_firewall_rules_v1(
        rqctx: RequestContext<Self::Context>,
        query: Query<crate::v1::FirewallRuleQuery>,
    ) -> Result<HttpResponseOk<crate::v1::ResultsPage<FirewallRule>>, HttpError>;

    /// RFD 00007 `GET /v1/firewall-rules/{firewall_rule_id}`.
    #[endpoint {
        method = GET,
        path = "/v1/firewall-rules/{firewall_rule_id}",
        tags = ["firewall-rules"],
    }]
    async fn get_firewall_rule_v1(
        rqctx: RequestContext<Self::Context>,
        path: Path<crate::v1::FirewallRulePath>,
    ) -> Result<HttpResponseOk<FirewallRule>, HttpError>;

    /// RFD 00007 `GET /v1/nat-gateways?vpc=<uuid>`.
    #[endpoint {
        method = GET,
        path = "/v1/nat-gateways",
        tags = ["nat-gateways"],
    }]
    async fn list_nat_gateways_v1(
        rqctx: RequestContext<Self::Context>,
        query: Query<crate::v1::NatGatewayQuery>,
    ) -> Result<HttpResponseOk<crate::v1::ResultsPage<NatGateway>>, HttpError>;

    /// RFD 00007 `GET /v1/nat-gateways/{nat_gateway_id}`.
    #[endpoint {
        method = GET,
        path = "/v1/nat-gateways/{nat_gateway_id}",
        tags = ["nat-gateways"],
    }]
    async fn get_nat_gateway_v1(
        rqctx: RequestContext<Self::Context>,
        path: Path<crate::v1::NatGatewayPath>,
    ) -> Result<HttpResponseOk<NatGateway>, HttpError>;

    /// RFD 00007 `GET /v1/route-tables?vpc=<uuid>`.
    #[endpoint {
        method = GET,
        path = "/v1/route-tables",
        tags = ["route-tables"],
    }]
    async fn list_route_tables_v1(
        rqctx: RequestContext<Self::Context>,
        query: Query<crate::v1::RouteTableQuery>,
    ) -> Result<HttpResponseOk<crate::v1::ResultsPage<RouteTable>>, HttpError>;

    /// RFD 00007 `GET /v1/route-tables/{route_table_id}`.
    #[endpoint {
        method = GET,
        path = "/v1/route-tables/{route_table_id}",
        tags = ["route-tables"],
    }]
    async fn get_route_table_v1(
        rqctx: RequestContext<Self::Context>,
        path: Path<crate::v1::RouteTablePath>,
    ) -> Result<HttpResponseOk<RouteTable>, HttpError>;

    /// RFD 00007 `GET /v1/routes?route_table=<uuid>`.
    #[endpoint {
        method = GET,
        path = "/v1/routes",
        tags = ["routes"],
    }]
    async fn list_routes_v1(
        rqctx: RequestContext<Self::Context>,
        query: Query<crate::v1::RouteQuery>,
    ) -> Result<HttpResponseOk<crate::v1::ResultsPage<Route>>, HttpError>;

    /// RFD 00007 `GET /v1/routes/{route_id}`.
    #[endpoint {
        method = GET,
        path = "/v1/routes/{route_id}",
        tags = ["routes"],
    }]
    async fn get_route_v1(
        rqctx: RequestContext<Self::Context>,
        path: Path<crate::v1::RoutePath>,
    ) -> Result<HttpResponseOk<Route>, HttpError>;

    /// RFD 00007 `GET /v1/floating-ips?tenant=&project=`. Flat FIP list.
    #[endpoint {
        method = GET,
        path = "/v1/floating-ips",
        tags = ["floating-ips"],
    }]
    async fn list_floating_ips_v1(
        rqctx: RequestContext<Self::Context>,
        query: Query<crate::v1::FloatingIpQuery>,
    ) -> Result<HttpResponseOk<crate::v1::ResultsPage<FloatingIp>>, HttpError>;

    /// RFD 00007 `GET /v1/floating-ips/{floating_ip_id}`.
    #[endpoint {
        method = GET,
        path = "/v1/floating-ips/{floating_ip_id}",
        tags = ["floating-ips"],
    }]
    async fn get_floating_ip_v1(
        rqctx: RequestContext<Self::Context>,
        path: Path<crate::v1::FloatingIpPath>,
    ) -> Result<HttpResponseOk<FloatingIp>, HttpError>;

    /// RFD 00007 `POST /v1/floating-ips/{floating_ip_id}/attach`.
    #[endpoint {
        method = POST,
        path = "/v1/floating-ips/{floating_ip_id}/attach",
        tags = ["floating-ips"],
    }]
    async fn attach_floating_ip_v1(
        rqctx: RequestContext<Self::Context>,
        path: Path<crate::v1::FloatingIpPath>,
        body: TypedBody<AttachFloatingIpRequest>,
    ) -> Result<HttpResponseOk<FloatingIp>, HttpError>;

    /// RFD 00007 `POST /v1/floating-ips/{floating_ip_id}/detach`.
    #[endpoint {
        method = POST,
        path = "/v1/floating-ips/{floating_ip_id}/detach",
        tags = ["floating-ips"],
    }]
    async fn detach_floating_ip_v1(
        rqctx: RequestContext<Self::Context>,
        path: Path<crate::v1::FloatingIpPath>,
    ) -> Result<HttpResponseOk<FloatingIp>, HttpError>;

    /// RFD 00007 `GET /v1/images?scope=public[&silo=&tenant=&project=]`.
    /// AP-2h: `scope=public` only.
    #[endpoint {
        method = GET,
        path = "/v1/images",
        tags = ["images"],
    }]
    async fn list_images_v1(
        rqctx: RequestContext<Self::Context>,
        query: Query<crate::v1::ImageQuery>,
    ) -> Result<HttpResponseOk<crate::v1::ResultsPage<Image>>, HttpError>;

    /// RFD 00007 `GET /v1/images/{image_id}`. Flat single-image read.
    #[endpoint {
        method = GET,
        path = "/v1/images/{image_id}",
        tags = ["images"],
    }]
    async fn get_image_v1(
        rqctx: RequestContext<Self::Context>,
        path: Path<crate::v1::ImagePath>,
    ) -> Result<HttpResponseOk<Image>, HttpError>;

    /// RFD 00007 `GET /v1/ssh-keys?scope=public[&silo=&tenant=&project=]`.
    #[endpoint {
        method = GET,
        path = "/v1/ssh-keys",
        tags = ["ssh-keys"],
    }]
    async fn list_ssh_keys_v1(
        rqctx: RequestContext<Self::Context>,
        query: Query<crate::v1::SshKeyQuery>,
    ) -> Result<HttpResponseOk<crate::v1::ResultsPage<SshKey>>, HttpError>;

    /// RFD 00007 `GET /v1/ssh-keys/{key_id}`. Flat single-key read.
    #[endpoint {
        method = GET,
        path = "/v1/ssh-keys/{key_id}",
        tags = ["ssh-keys"],
    }]
    async fn get_ssh_key_v1(
        rqctx: RequestContext<Self::Context>,
        path: Path<crate::v1::SshKeyPath>,
    ) -> Result<HttpResponseOk<SshKey>, HttpError>;

    /// RFD 00007 `GET /v1/vpcs?tenant=&project=`. Flat VPC list.
    #[endpoint {
        method = GET,
        path = "/v1/vpcs",
        tags = ["vpcs"],
    }]
    async fn list_vpcs_v1(
        rqctx: RequestContext<Self::Context>,
        query: Query<crate::v1::VpcQuery>,
    ) -> Result<HttpResponseOk<crate::v1::ResultsPage<Vpc>>, HttpError>;

    /// RFD 00007 `GET /v1/vpcs/{vpc_id}`. Flat single-VPC read.
    #[endpoint {
        method = GET,
        path = "/v1/vpcs/{vpc_id}",
        tags = ["vpcs"],
    }]
    async fn get_vpc_v1(
        rqctx: RequestContext<Self::Context>,
        path: Path<crate::v1::VpcPath>,
    ) -> Result<HttpResponseOk<Vpc>, HttpError>;

    /// RFD 00007 `GET /v1/subnets?vpc=<uuid>`. Flat subnet list.
    #[endpoint {
        method = GET,
        path = "/v1/subnets",
        tags = ["subnets"],
    }]
    async fn list_subnets_v1(
        rqctx: RequestContext<Self::Context>,
        query: Query<crate::v1::SubnetQuery>,
    ) -> Result<HttpResponseOk<crate::v1::ResultsPage<Subnet>>, HttpError>;

    /// RFD 00007 `GET /v1/subnets/{subnet_id}`. Flat single-subnet read.
    #[endpoint {
        method = GET,
        path = "/v1/subnets/{subnet_id}",
        tags = ["subnets"],
    }]
    async fn get_subnet_v1(
        rqctx: RequestContext<Self::Context>,
        path: Path<crate::v1::SubnetPath>,
    ) -> Result<HttpResponseOk<Subnet>, HttpError>;

    /// List route tables inside a VPC.
    #[endpoint {
        method = GET,
        path = "/v2/tenants/{tenant_id}/projects/{project_id}/vpcs/{vpc_id}/route-tables",
        tags = ["route-tables"],
    }]
    async fn list_vpc_route_tables(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcPath>,
    ) -> Result<HttpResponseOk<Vec<RouteTable>>, HttpError>;

    /// Create a non-main route table in a VPC. Returns 409 if the
    /// name is already in use within the VPC.
    #[endpoint {
        method = POST,
        path = "/v2/tenants/{tenant_id}/projects/{project_id}/vpcs/{vpc_id}/route-tables",
        tags = ["route-tables"],
    }]
    async fn create_vpc_route_table(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcPath>,
        body: TypedBody<NewRouteTable>,
    ) -> Result<HttpResponseCreated<RouteTable>, HttpError>;

    /// Read a single route table.
    #[endpoint {
        method = GET,
        path = "/v2/tenants/{tenant_id}/projects/{project_id}/vpcs/{vpc_id}/route-tables/{route_table_id}",
        tags = ["route-tables"],
    }]
    async fn get_vpc_route_table(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcRouteTablePath>,
    ) -> Result<HttpResponseOk<RouteTable>, HttpError>;

    /// Delete a non-main route table. Returns 409 while subnets or
    /// routes still reference the table.
    #[endpoint {
        method = DELETE,
        path = "/v2/tenants/{tenant_id}/projects/{project_id}/vpcs/{vpc_id}/route-tables/{route_table_id}",
        tags = ["route-tables"],
    }]
    async fn delete_vpc_route_table(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcRouteTablePath>,
    ) -> Result<HttpResponseDeleted, HttpError>;

    /// List routes inside a route table.
    #[endpoint {
        method = GET,
        path = "/v2/tenants/{tenant_id}/projects/{project_id}/vpcs/{vpc_id}/route-tables/{route_table_id}/routes",
        tags = ["routes"],
    }]
    async fn list_vpc_route_table_routes(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcRouteTablePath>,
    ) -> Result<HttpResponseOk<Vec<Route>>, HttpError>;

    /// Create a route in a route table. Returns 409 if the
    /// destination is already in use in the table.
    #[endpoint {
        method = POST,
        path = "/v2/tenants/{tenant_id}/projects/{project_id}/vpcs/{vpc_id}/route-tables/{route_table_id}/routes",
        tags = ["routes"],
    }]
    async fn create_vpc_route_table_route(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcRouteTablePath>,
        body: TypedBody<NewRoute>,
    ) -> Result<HttpResponseCreated<Route>, HttpError>;

    /// Read a single route.
    #[endpoint {
        method = GET,
        path = "/v2/tenants/{tenant_id}/projects/{project_id}/vpcs/{vpc_id}/route-tables/{route_table_id}/routes/{route_id}",
        tags = ["routes"],
    }]
    async fn get_vpc_route_table_route(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcRouteTableRoutePath>,
    ) -> Result<HttpResponseOk<Route>, HttpError>;

    /// Delete a route.
    #[endpoint {
        method = DELETE,
        path = "/v2/tenants/{tenant_id}/projects/{project_id}/vpcs/{vpc_id}/route-tables/{route_table_id}/routes/{route_id}",
        tags = ["routes"],
    }]
    async fn delete_vpc_route_table_route(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcRouteTableRoutePath>,
    ) -> Result<HttpResponseDeleted, HttpError>;

    // -- Firewall rules (Slice 1: per-VPC flat rule list) -------------

    /// List firewall rules scoped to a VPC, sorted by `priority`
    /// descending (highest evaluates first).
    #[endpoint {
        method = GET,
        path = "/v2/tenants/{tenant_id}/projects/{project_id}/vpcs/{vpc_id}/firewall-rules",
        tags = ["firewall-rules"],
    }]
    async fn list_vpc_firewall_rules(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcPath>,
    ) -> Result<HttpResponseOk<Vec<FirewallRule>>, HttpError>;

    /// Create a firewall rule scoped to a VPC. Slice 1: every NIC in
    /// the VPC inherits every rule (no security-group attachment
    /// yet). The server assigns `id` and `created_at`.
    #[endpoint {
        method = POST,
        path = "/v2/tenants/{tenant_id}/projects/{project_id}/vpcs/{vpc_id}/firewall-rules",
        tags = ["firewall-rules"],
    }]
    async fn create_vpc_firewall_rule(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcPath>,
        body: TypedBody<NewFirewallRule>,
    ) -> Result<HttpResponseCreated<FirewallRule>, HttpError>;

    /// Delete a firewall rule by id.
    #[endpoint {
        method = DELETE,
        path = "/v2/tenants/{tenant_id}/projects/{project_id}/vpcs/{vpc_id}/firewall-rules/{firewall_rule_id}",
        tags = ["firewall-rules"],
    }]
    async fn delete_vpc_firewall_rule(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcFirewallRulePath>,
    ) -> Result<HttpResponseDeleted, HttpError>;

    // -- DHCP / IPAM (γ.1 + γ.4: per-VPC pool, reservations, leases)

    /// Read the per-VPC DHCP pool config. Returns the body wrapped
    /// `Some` when set; returns `None` when the operator hasn't
    /// customised this VPC.
    #[endpoint {
        method = GET,
        path = "/v2/tenants/{tenant_id}/projects/{project_id}/vpcs/{vpc_id}/dhcp/pool",
        tags = ["dhcp"],
    }]
    async fn get_vpc_dhcp_pool(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcPath>,
    ) -> Result<HttpResponseOk<Option<DhcpPool>>, HttpError>;

    /// Create or replace the per-VPC DHCP pool config.
    #[endpoint {
        method = PUT,
        path = "/v2/tenants/{tenant_id}/projects/{project_id}/vpcs/{vpc_id}/dhcp/pool",
        tags = ["dhcp"],
    }]
    async fn set_vpc_dhcp_pool(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcPath>,
        body: TypedBody<NewDhcpPool>,
    ) -> Result<HttpResponseOk<DhcpPool>, HttpError>;

    /// Remove the per-VPC DHCP pool config (revert to defaults).
    #[endpoint {
        method = DELETE,
        path = "/v2/tenants/{tenant_id}/projects/{project_id}/vpcs/{vpc_id}/dhcp/pool",
        tags = ["dhcp"],
    }]
    async fn clear_vpc_dhcp_pool(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcPath>,
    ) -> Result<HttpResponseDeleted, HttpError>;

    /// List DHCP reservations (operator-pinned MAC→IP mappings) in a VPC.
    #[endpoint {
        method = GET,
        path = "/v2/tenants/{tenant_id}/projects/{project_id}/vpcs/{vpc_id}/dhcp/reservations",
        tags = ["dhcp"],
    }]
    async fn list_vpc_dhcp_reservations(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcPath>,
    ) -> Result<HttpResponseOk<Vec<DhcpReservation>>, HttpError>;

    /// Create a DHCP reservation pinning a MAC to a specific IPv4.
    /// Returns 409 if the MAC is already reserved with a different IP.
    #[endpoint {
        method = POST,
        path = "/v2/tenants/{tenant_id}/projects/{project_id}/vpcs/{vpc_id}/dhcp/reservations",
        tags = ["dhcp"],
    }]
    async fn create_vpc_dhcp_reservation(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcPath>,
        body: TypedBody<NewDhcpReservation>,
    ) -> Result<HttpResponseCreated<DhcpReservation>, HttpError>;

    /// Look up a reservation by MAC.
    #[endpoint {
        method = GET,
        path = "/v2/tenants/{tenant_id}/projects/{project_id}/vpcs/{vpc_id}/dhcp/reservations/{mac}",
        tags = ["dhcp"],
    }]
    async fn get_vpc_dhcp_reservation(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcDhcpMacPath>,
    ) -> Result<HttpResponseOk<DhcpReservation>, HttpError>;

    /// Remove a reservation by MAC.
    #[endpoint {
        method = DELETE,
        path = "/v2/tenants/{tenant_id}/projects/{project_id}/vpcs/{vpc_id}/dhcp/reservations/{mac}",
        tags = ["dhcp"],
    }]
    async fn delete_vpc_dhcp_reservation(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcDhcpMacPath>,
    ) -> Result<HttpResponseDeleted, HttpError>;

    /// List active DHCP leases for a VPC. Each entry was written when
    /// tritond pre-assigned an IP to a NIC at instance create.
    #[endpoint {
        method = GET,
        path = "/v2/tenants/{tenant_id}/projects/{project_id}/vpcs/{vpc_id}/dhcp/leases",
        tags = ["dhcp"],
    }]
    async fn list_vpc_dhcp_leases(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcPath>,
    ) -> Result<HttpResponseOk<Vec<DhcpLease>>, HttpError>;

    /// Look up a lease by MAC.
    #[endpoint {
        method = GET,
        path = "/v2/tenants/{tenant_id}/projects/{project_id}/vpcs/{vpc_id}/dhcp/leases/{mac}",
        tags = ["dhcp"],
    }]
    async fn get_vpc_dhcp_lease(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcDhcpMacPath>,
    ) -> Result<HttpResponseOk<DhcpLease>, HttpError>;

    /// Operator-driven release: remove the lease record. The
    /// underlying IP is freed for re-allocation; sticky-by-MAC for
    /// this MAC is broken until the operator re-creates the
    /// reservation.
    #[endpoint {
        method = DELETE,
        path = "/v2/tenants/{tenant_id}/projects/{project_id}/vpcs/{vpc_id}/dhcp/leases/{mac}",
        tags = ["dhcp"],
    }]
    async fn delete_vpc_dhcp_lease(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcDhcpMacPath>,
    ) -> Result<HttpResponseDeleted, HttpError>;

    /// List NAT gateways inside a VPC.
    #[endpoint {
        method = GET,
        path = "/v2/tenants/{tenant_id}/projects/{project_id}/vpcs/{vpc_id}/nat-gateways",
        tags = ["nat-gateways"],
    }]
    async fn list_vpc_nat_gateways(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcPath>,
    ) -> Result<HttpResponseOk<Vec<NatGateway>>, HttpError>;

    /// Create a NAT gateway in a VPC. Returns 409 if the name is
    /// already in use within the VPC. The returned record includes
    /// the reserved public source address.
    #[endpoint {
        method = POST,
        path = "/v2/tenants/{tenant_id}/projects/{project_id}/vpcs/{vpc_id}/nat-gateways",
        tags = ["nat-gateways"],
    }]
    async fn create_vpc_nat_gateway(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcPath>,
        body: TypedBody<NewNatGateway>,
    ) -> Result<HttpResponseCreated<NatGateway>, HttpError>;

    /// Read a single NAT gateway.
    #[endpoint {
        method = GET,
        path = "/v2/tenants/{tenant_id}/projects/{project_id}/vpcs/{vpc_id}/nat-gateways/{nat_gateway_id}",
        tags = ["nat-gateways"],
    }]
    async fn get_vpc_nat_gateway(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcNatGatewayPath>,
    ) -> Result<HttpResponseOk<NatGateway>, HttpError>;

    /// Delete a NAT gateway and release its public address.
    #[endpoint {
        method = DELETE,
        path = "/v2/tenants/{tenant_id}/projects/{project_id}/vpcs/{vpc_id}/nat-gateways/{nat_gateway_id}",
        tags = ["nat-gateways"],
    }]
    async fn delete_vpc_nat_gateway(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcNatGatewayPath>,
    ) -> Result<HttpResponseDeleted, HttpError>;

    /// List Public SSH keys. Anonymous-accessible — Public means
    /// public, so unauthenticated probes get the catalog.
    #[endpoint {
        method = GET,
        path = "/v2/ssh-keys",
        tags = ["ssh-keys"],
    }]
    async fn list_public_ssh_keys(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseOk<Vec<SshKey>>, HttpError>;

    /// Create a `Public` SSH key. Root-only via Cedar.
    /// Returns 400 if the key cannot be parsed as openssh,
    /// 409 if the name or fingerprint is already in use among
    /// Public keys.
    #[endpoint {
        method = POST,
        path = "/v2/ssh-keys",
        tags = ["ssh-keys"],
    }]
    async fn create_public_ssh_key(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<NewSshKey>,
    ) -> Result<HttpResponseCreated<SshKey>, HttpError>;

    /// List the SSH keys whose scope is exactly `Silo { silo_id }`
    /// (does NOT include Public — use `/v2/tenants/{tenant_id}/ssh-keys`
    /// for the unioned tenant view).
    #[endpoint {
        method = GET,
        path = "/v2/silos/{silo_id}/ssh-keys",
        tags = ["ssh-keys"],
    }]
    async fn list_silo_ssh_keys(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloPath>,
    ) -> Result<HttpResponseOk<Vec<SshKey>>, HttpError>;

    /// Register a `Silo`-scoped SSH key. The server parses
    /// `public_key` as openssh format and computes the SHA-256
    /// fingerprint. Returns 400 if the key cannot be parsed, 409
    /// if the name or fingerprint is already in use within the silo.
    #[endpoint {
        method = POST,
        path = "/v2/silos/{silo_id}/ssh-keys",
        tags = ["ssh-keys"],
    }]
    async fn create_silo_ssh_key(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloPath>,
        body: TypedBody<NewSshKey>,
    ) -> Result<HttpResponseCreated<SshKey>, HttpError>;

    /// List SSH keys visible to the tenant: Public + Silo (of
    /// tenant's silo) + Tenant.
    #[endpoint {
        method = GET,
        path = "/v2/tenants/{tenant_id}/ssh-keys",
        tags = ["ssh-keys"],
    }]
    async fn list_tenant_ssh_keys(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantPath>,
    ) -> Result<HttpResponseOk<Vec<SshKey>>, HttpError>;

    /// Register a `Tenant`-scoped SSH key.
    #[endpoint {
        method = POST,
        path = "/v2/tenants/{tenant_id}/ssh-keys",
        tags = ["ssh-keys"],
    }]
    async fn create_tenant_ssh_key(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantPath>,
        body: TypedBody<NewSshKey>,
    ) -> Result<HttpResponseCreated<SshKey>, HttpError>;

    /// List SSH keys visible to the project: Public + Silo (of
    /// project's silo) + Tenant (of project's tenant) + Project.
    #[endpoint {
        method = GET,
        path = "/v2/tenants/{tenant_id}/projects/{project_id}/ssh-keys",
        tags = ["ssh-keys"],
    }]
    async fn list_project_ssh_keys(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectPath>,
    ) -> Result<HttpResponseOk<Vec<SshKey>>, HttpError>;

    /// Register a `Project`-scoped SSH key.
    #[endpoint {
        method = POST,
        path = "/v2/tenants/{tenant_id}/projects/{project_id}/ssh-keys",
        tags = ["ssh-keys"],
    }]
    async fn create_project_ssh_key(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectPath>,
        body: TypedBody<NewSshKey>,
    ) -> Result<HttpResponseCreated<SshKey>, HttpError>;

    /// List the calling user's `User`-scoped SSH keys. Returns
    /// only the caller's own keys; the bound user_id is resolved
    /// from the authenticated principal.
    #[endpoint {
        method = GET,
        path = "/v2/auth/ssh-keys",
        tags = ["ssh-keys"],
    }]
    async fn list_my_ssh_keys(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseOk<Vec<SshKey>>, HttpError>;

    /// Register a `User`-scoped SSH key owned by the caller.
    #[endpoint {
        method = POST,
        path = "/v2/auth/ssh-keys",
        tags = ["ssh-keys"],
    }]
    async fn create_my_ssh_key(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<NewSshKey>,
    ) -> Result<HttpResponseCreated<SshKey>, HttpError>;

    /// Read a single SSH key by id. Returns 404 when the key
    /// does not exist OR when the principal cannot see it
    /// (cross-scope visibility deny).
    #[endpoint {
        method = GET,
        path = "/v2/ssh-keys/{key_id}",
        tags = ["ssh-keys"],
    }]
    async fn get_ssh_key(
        rqctx: RequestContext<Self::Context>,
        path: Path<SshKeyPath>,
    ) -> Result<HttpResponseOk<SshKey>, HttpError>;

    /// Delete an SSH key by id. Returns 404 when the key does
    /// not exist OR the principal lacks ownership for the key's
    /// scope:
    /// * `Public` — root only.
    /// * `Silo` / `Tenant` / `Project` — any tenant member of
    ///   the resolved tenant (Phase 0 = same-tenant access).
    /// * `User` — only the owning user (or root).
    #[endpoint {
        method = DELETE,
        path = "/v2/ssh-keys/{key_id}",
        tags = ["ssh-keys"],
    }]
    async fn delete_ssh_key(
        rqctx: RequestContext<Self::Context>,
        path: Path<SshKeyPath>,
    ) -> Result<HttpResponseDeleted, HttpError>;

    /// List Public images. Anonymous-accessible — Public means
    /// public, so unauthenticated probes get the catalog.
    #[endpoint {
        method = GET,
        path = "/v2/images",
        tags = ["images"],
    }]
    async fn list_public_images(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseOk<Vec<Image>>, HttpError>;

    /// Create a `Public` image. Root-only via Cedar.
    /// Returns 400 if `sha256` is not 64 lowercase hex chars or
    /// if `size_bytes` is zero. Returns 409 if the name is
    /// already in use among Public images.
    #[endpoint {
        method = POST,
        path = "/v2/images",
        tags = ["images"],
    }]
    async fn create_public_image(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<NewImage>,
    ) -> Result<HttpResponseCreated<Image>, HttpError>;

    /// List the images whose scope is exactly `Silo { silo_id }`
    /// (does NOT include Public — use `/v2/tenants/{tenant_id}/images`
    /// for the unioned tenant view).
    #[endpoint {
        method = GET,
        path = "/v2/silos/{silo_id}/images",
        tags = ["images"],
    }]
    async fn list_silo_images(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloPath>,
    ) -> Result<HttpResponseOk<Vec<Image>>, HttpError>;

    /// Register a `Silo`-scoped image.
    #[endpoint {
        method = POST,
        path = "/v2/silos/{silo_id}/images",
        tags = ["images"],
    }]
    async fn create_silo_image(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloPath>,
        body: TypedBody<NewImage>,
    ) -> Result<HttpResponseCreated<Image>, HttpError>;

    /// Register an image from a tritond image bundle (silo-scoped
    /// only). tritond fetches the bundle once at registration,
    /// validates the manifest, re-hashes the content, and
    /// populates every Image-record field from the manifest. The
    /// returned `Image` carries `compatibility = Some(...)` so
    /// the per-CN agent enforces brand + min_smartos_platform
    /// gates at provision time. Returns 400 on a malformed
    /// bundle or sha256 mismatch, 502 if `bundle_url` is
    /// unreachable, 409 on a name or content collision within
    /// the silo.
    ///
    /// The path is `/v2/silos/{silo_id}/image-bundles` rather
    /// than `/v2/silos/{silo_id}/images/from-bundle` because
    /// Dropshot's router cannot disambiguate a literal
    /// `from-bundle` segment from a sibling `{image_id}`
    /// parameter at the same level.
    #[endpoint {
        method = POST,
        path = "/v2/silos/{silo_id}/image-bundles",
        tags = ["images"],
    }]
    async fn create_silo_image_from_bundle(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloPath>,
        body: TypedBody<NewImageFromBundle>,
    ) -> Result<HttpResponseCreated<Image>, HttpError>;

    /// Register a `Silo`-scoped image from an IMGAPI v2
    /// manifest. The operator (or `tcadm image fetch-nocloud`)
    /// uploads the blob to Manta first, then POSTs the manifest
    /// + the public Manta URL + the SHA-256 of the bytes here.
    /// tritond derives every Image-record field from the
    /// manifest and persists `manta_url` as `source_url`. The
    /// per-CN agent then uses the existing source_url / sha256
    /// fetch + verify path at provision time.
    ///
    /// Returns 400 on a manifest validation failure, sha256
    /// shape error, or `files[]` count mismatch; 409 on a uuid
    /// or content collision within the silo.
    ///
    /// Path is `/imgapi-images` (sibling resource) to mirror
    /// the `image-bundles` precedent and sidestep any Dropshot
    /// literal-vs-`{image_id}` ambiguity at
    /// `/v2/silos/{silo_id}/images/...`.
    #[endpoint {
        method = POST,
        path = "/v2/silos/{silo_id}/imgapi-images",
        tags = ["images"],
    }]
    async fn create_silo_image_from_imgapi(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloPath>,
        body: TypedBody<NewImageFromImgapi>,
    ) -> Result<HttpResponseCreated<Image>, HttpError>;

    /// List images visible to the tenant: Public + Silo (of
    /// tenant's silo) + Tenant.
    #[endpoint {
        method = GET,
        path = "/v2/tenants/{tenant_id}/images",
        tags = ["images"],
    }]
    async fn list_tenant_images(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantPath>,
    ) -> Result<HttpResponseOk<Vec<Image>>, HttpError>;

    /// Register a `Tenant`-scoped image.
    #[endpoint {
        method = POST,
        path = "/v2/tenants/{tenant_id}/images",
        tags = ["images"],
    }]
    async fn create_tenant_image(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantPath>,
        body: TypedBody<NewImage>,
    ) -> Result<HttpResponseCreated<Image>, HttpError>;

    /// List images visible to the project: Public + Silo (of
    /// project's silo) + Tenant (of project's tenant) + Project.
    /// This is the practical "what can a project member launch
    /// from?" query.
    #[endpoint {
        method = GET,
        path = "/v2/tenants/{tenant_id}/projects/{project_id}/images",
        tags = ["images"],
    }]
    async fn list_project_images(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectPath>,
    ) -> Result<HttpResponseOk<Vec<Image>>, HttpError>;

    /// Register a `Project`-scoped image.
    #[endpoint {
        method = POST,
        path = "/v2/tenants/{tenant_id}/projects/{project_id}/images",
        tags = ["images"],
    }]
    async fn create_project_image(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectPath>,
        body: TypedBody<NewImage>,
    ) -> Result<HttpResponseCreated<Image>, HttpError>;

    /// List the calling user's `User`-scoped images. Returns
    /// only the caller's own images; the bound user_id is
    /// resolved from the authenticated principal.
    #[endpoint {
        method = GET,
        path = "/v2/auth/images",
        tags = ["images"],
    }]
    async fn list_my_images(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseOk<Vec<Image>>, HttpError>;

    /// Register a `User`-scoped image owned by the caller.
    #[endpoint {
        method = POST,
        path = "/v2/auth/images",
        tags = ["images"],
    }]
    async fn create_my_image(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<NewImage>,
    ) -> Result<HttpResponseCreated<Image>, HttpError>;

    /// Read a single image by id. Returns 404 when the image
    /// does not exist OR when the principal cannot see it
    /// (cross-scope visibility deny).
    #[endpoint {
        method = GET,
        path = "/v2/images/{image_id}",
        tags = ["images"],
    }]
    async fn get_image(
        rqctx: RequestContext<Self::Context>,
        path: Path<ImagePath>,
    ) -> Result<HttpResponseOk<Image>, HttpError>;

    /// Delete an image by id. Returns 404 when the image does
    /// not exist OR the principal lacks ownership for the
    /// image's scope:
    /// * `Public` — root only.
    /// * `Silo` / `Tenant` / `Project` — any tenant member of
    ///   the resolved tenant (Phase 0 = same-tenant access).
    /// * `User` — only the owning user (or root).
    #[endpoint {
        method = DELETE,
        path = "/v2/images/{image_id}",
        tags = ["images"],
    }]
    async fn delete_image(
        rqctx: RequestContext<Self::Context>,
        path: Path<ImagePath>,
    ) -> Result<HttpResponseDeleted, HttpError>;

    /// Set (or replace) the resource quota on a project. Returns
    /// 404 when the project does not exist or belongs to a
    /// different tenant. The server assigns `updated_at`. Quotas
    /// are not enforced in Phase 0; the record is stored for the
    /// eventual instance-create flow to consult.
    #[endpoint {
        method = PUT,
        path = "/v2/tenants/{tenant_id}/projects/{project_id}/quota",
        tags = ["quotas"],
    }]
    async fn put_project_quota(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectPath>,
        body: TypedBody<NewQuota>,
    ) -> Result<HttpResponseOk<Quota>, HttpError>;

    /// Read a project's quota. Returns 404 when the project does
    /// not exist, lives in a different tenant, or has no quota set
    /// (no record means "unlimited").
    #[endpoint {
        method = GET,
        path = "/v2/tenants/{tenant_id}/projects/{project_id}/quota",
        tags = ["quotas"],
    }]
    async fn get_project_quota(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectPath>,
    ) -> Result<HttpResponseOk<Quota>, HttpError>;

    /// Remove a project's quota (project becomes unlimited).
    /// Returns 404 when the project does not exist, lives in a
    /// different tenant, or had no quota set.
    #[endpoint {
        method = DELETE,
        path = "/v2/tenants/{tenant_id}/projects/{project_id}/quota",
        tags = ["quotas"],
    }]
    async fn delete_project_quota(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectPath>,
    ) -> Result<HttpResponseDeleted, HttpError>;

    /// List instances in a project.
    #[endpoint {
        method = GET,
        path = "/v2/tenants/{tenant_id}/projects/{project_id}/instances",
        tags = ["instances"],
    }]
    async fn list_project_instances(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectPath>,
    ) -> Result<HttpResponseOk<Vec<Instance>>, HttpError>;

    /// RFD 00007 `PUT /v1/system/users/{user_id}/capabilities/{capability}`.
    /// Grant a capability to a user. Capability gate: `SystemOperate`.
    /// Idempotent: granting an already-present capability is a no-op.
    /// Returns the updated UserView on success.
    #[endpoint {
        method = PUT,
        path = "/v1/system/users/{user_id}/capabilities/{capability}",
        tags = ["system"],
    }]
    async fn grant_user_capability_v1(
        rqctx: RequestContext<Self::Context>,
        path: Path<crate::v1::SystemUserCapabilityPath>,
    ) -> Result<HttpResponseOk<crate::types::UserView>, HttpError>;

    /// RFD 00007 `DELETE /v1/system/users/{user_id}/capabilities/{capability}`.
    /// Revoke a capability from a user. Capability gate: `SystemOperate`.
    /// Idempotent: revoking an absent capability is a no-op.
    /// Refuses to revoke from root operators with 400.
    #[endpoint {
        method = DELETE,
        path = "/v1/system/users/{user_id}/capabilities/{capability}",
        tags = ["system"],
    }]
    async fn revoke_user_capability_v1(
        rqctx: RequestContext<Self::Context>,
        path: Path<crate::v1::SystemUserCapabilityPath>,
    ) -> Result<HttpResponseDeleted, HttpError>;

    /// RFD 00007 `GET /v1/system/networking/nics?ip=&subnet=&instance=`.
    /// Fleet-wide NIC search ("who owns 10.x.x.x?"). Capability:
    /// `SystemRead`. Backed by the AP-1c IP and subnet indexes.
    #[endpoint {
        method = GET,
        path = "/v1/system/networking/nics",
        tags = ["system"],
    }]
    async fn list_system_nics_v1(
        rqctx: RequestContext<Self::Context>,
        query: Query<crate::v1::NicQuery>,
    ) -> Result<HttpResponseOk<crate::v1::ResultsPage<Nic>>, HttpError>;

    /// RFD 00007 `GET /v1/system/cns/{cn_id}/instances`. Fixed-axis
    /// "what is running on this CN?" view. Capability: `SystemRead`.
    #[endpoint {
        method = GET,
        path = "/v1/system/cns/{cn_id}/instances",
        tags = ["system"],
    }]
    async fn list_system_cn_instances_v1(
        rqctx: RequestContext<Self::Context>,
        path: Path<crate::v1::SystemCnPath>,
    ) -> Result<HttpResponseOk<crate::v1::ResultsPage<Instance>>, HttpError>;

    /// RFD 00007 `GET /v1/system/images/{image_id}/instances`.
    /// Fixed-axis "what's using this image?" view (the answer to the
    /// question that opened RFD 00007). Capability: `SystemRead`.
    #[endpoint {
        method = GET,
        path = "/v1/system/images/{image_id}/instances",
        tags = ["system"],
    }]
    async fn list_system_image_instances_v1(
        rqctx: RequestContext<Self::Context>,
        path: Path<crate::v1::SystemImagePath>,
    ) -> Result<HttpResponseOk<crate::v1::ResultsPage<Instance>>, HttpError>;

    /// RFD 00007 `GET /v1/system/instances?image=&cn=&silo=&tenant=&project=&state=`.
    ///
    /// Fleet-wide instance search - the answer to "which VMs are
    /// using image X?" in one HTTP call. Capability-gated:
    /// requires `SystemRead`. Callers without it get 404 NotFound.
    ///
    /// Indexed dispatch: `?image=` -> `idx/image/...`, `?cn=` ->
    /// `instance/in_host_cn/...`. Both narrow before per-row scope
    /// + state filtering.
    #[endpoint {
        method = GET,
        path = "/v1/system/instances",
        tags = ["system"],
    }]
    async fn list_system_instances_v1(
        rqctx: RequestContext<Self::Context>,
        query: Query<crate::v1::InstanceQuery>,
    ) -> Result<HttpResponseOk<crate::v1::ResultsPage<Instance>>, HttpError>;

    /// RFD 00007 `GET /v1/instances?tenant=&project=&image=&cn=&state=`.
    ///
    /// Flat customer-facing instance list with scope and reference
    /// selectors per RFD 00007 D-Ap-1. Dispatches on the selector
    /// set: `image=` and `cn=` are indexed (AP-1c); `tenant=&project=`
    /// uses the existing project membership index. AP-2b accepts
    /// UUID-only selectors; name resolution (`NameOrId::Name`) lands
    /// in AP-3a via `handlers::selectors::resolve_name_or_id`.
    ///
    /// Returns `400 ScopeNotAccepted` if `silo=` is set (that
    /// selector is reserved for `/v1/system/instances`). Returns
    /// `400 MissingScope` if no indexed selector or project scope
    /// is set (cross-project scans on the customer surface are not
    /// supported in AP-2b; the operator surface at `/v1/system/`
    /// will accept them).
    #[endpoint {
        method = GET,
        path = "/v1/instances",
        tags = ["instances"],
    }]
    async fn list_instances_v1(
        rqctx: RequestContext<Self::Context>,
        query: Query<crate::v1::InstanceQuery>,
    ) -> Result<HttpResponseOk<crate::v1::ResultsPage<Instance>>, HttpError>;

    /// Create an instance in a project.
    ///
    /// Returns 400 if `cpu == 0` or `memory_bytes == 0`. Returns 404
    /// if any referenced resource (image, subnet, ssh-keys) does
    /// not exist or lives outside this tenant/project (or, for
    /// image/ssh-key which remain silo-scoped in E-3, outside the
    /// tenant's silo). Returns 409 if the instance name is already
    /// taken in the project.
    ///
    /// Phase 0 ships synchronous lifecycle: the create handler
    /// transitions the new instance through Pending → Running
    /// before responding. A future slice introduces an async
    /// provisioning queue + stub executor; the API surface stays
    /// the same but tests will observe Pending-then-Running
    /// transitions instead of an instant Running.
    #[endpoint {
        method = POST,
        path = "/v2/tenants/{tenant_id}/projects/{project_id}/instances",
        tags = ["instances"],
    }]
    async fn create_project_instance(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectPath>,
        body: TypedBody<NewInstance>,
    ) -> Result<HttpResponseCreated<Instance>, HttpError>;

    /// Read a single instance.
    #[endpoint {
        method = GET,
        path = "/v2/tenants/{tenant_id}/projects/{project_id}/instances/{instance_id}",
        tags = ["instances"],
    }]
    async fn get_project_instance(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectInstancePath>,
    ) -> Result<HttpResponseOk<Instance>, HttpError>;

    /// RFD 00007 `GET /v1/instances/{instance_id}`. Flat single-
    /// instance read by UUID. The handler reads the instance row
    /// from the store and checks the principal's tenant against
    /// `Instance.tenant_id` for the cross-tenant-probe invariant
    /// (404 on mismatch). Name resolution lands in AP-3a.
    #[endpoint {
        method = GET,
        path = "/v1/instances/{instance_id}",
        tags = ["instances"],
    }]
    async fn get_instance_v1(
        rqctx: RequestContext<Self::Context>,
        path: Path<crate::v1::InstancePath>,
    ) -> Result<HttpResponseOk<Instance>, HttpError>;

    /// RFD 00007 `POST /v1/instances?tenant=&project=`. Create an
    /// instance in the named tenant + project. Equivalent semantics
    /// to the v2 `POST /v2/tenants/{t}/projects/{p}/instances`; the
    /// only difference is the URL shape (scope as selectors, not
    /// path segments). The handler still validates that the
    /// resolved tenant / project exist and live in the principal's
    /// silo, surfacing cross-tenant 404 as before.
    ///
    /// `tenant` and `project` are required selectors at AP-2d.
    /// AP-3a swaps to a `NameOrId` newtype.
    #[endpoint {
        method = POST,
        path = "/v1/instances",
        tags = ["instances"],
    }]
    async fn create_instance_v1(
        rqctx: RequestContext<Self::Context>,
        query: Query<crate::v1::ScopeSelectors>,
        body: TypedBody<NewInstance>,
    ) -> Result<HttpResponseCreated<Instance>, HttpError>;

    /// RFD 00007 `DELETE /v1/instances/{instance_id}`.
    #[endpoint {
        method = DELETE,
        path = "/v1/instances/{instance_id}",
        tags = ["instances"],
    }]
    async fn delete_instance_v1(
        rqctx: RequestContext<Self::Context>,
        path: Path<crate::v1::InstancePath>,
        query: Query<InstanceDeleteQuery>,
    ) -> Result<HttpResponseDeleted, HttpError>;

    /// RFD 00007 `POST /v1/instances/{instance_id}/start`.
    #[endpoint {
        method = POST,
        path = "/v1/instances/{instance_id}/start",
        tags = ["instances"],
    }]
    async fn start_instance_v1(
        rqctx: RequestContext<Self::Context>,
        path: Path<crate::v1::InstancePath>,
    ) -> Result<HttpResponseOk<Instance>, HttpError>;

    /// RFD 00007 `POST /v1/instances/{instance_id}/stop`.
    #[endpoint {
        method = POST,
        path = "/v1/instances/{instance_id}/stop",
        tags = ["instances"],
    }]
    async fn stop_instance_v1(
        rqctx: RequestContext<Self::Context>,
        path: Path<crate::v1::InstancePath>,
    ) -> Result<HttpResponseOk<Instance>, HttpError>;

    /// RFD 00007 `POST /v1/instances/{instance_id}/restart`.
    #[endpoint {
        method = POST,
        path = "/v1/instances/{instance_id}/restart",
        tags = ["instances"],
    }]
    async fn restart_instance_v1(
        rqctx: RequestContext<Self::Context>,
        path: Path<crate::v1::InstancePath>,
    ) -> Result<HttpResponseOk<Instance>, HttpError>;

    /// Delete an instance. Returns 409 if the instance is not in
    /// a deletable state (must be Stopped or Failed); pass
    /// `?force=true` to override and delete from any state.
    /// Returns 404 if the instance does not exist or belongs to
    /// a different tenant or project. The tritond record is cleared
    /// synchronously; the agent vmadm-deletes the SmartOS zone
    /// asynchronously via a `JobKind::Delete` job.
    #[endpoint {
        method = DELETE,
        path = "/v2/tenants/{tenant_id}/projects/{project_id}/instances/{instance_id}",
        tags = ["instances"],
    }]
    async fn delete_project_instance(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectInstancePath>,
        query: Query<InstanceDeleteQuery>,
    ) -> Result<HttpResponseDeleted, HttpError>;

    /// Start a stopped instance. Returns 409 if the instance is
    /// not in `Stopped` state.
    #[endpoint {
        method = POST,
        path = "/v2/tenants/{tenant_id}/projects/{project_id}/instances/{instance_id}/start",
        tags = ["instances"],
    }]
    async fn start_project_instance(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectInstancePath>,
    ) -> Result<HttpResponseOk<Instance>, HttpError>;

    /// Stop a running instance. Returns 409 if the instance is
    /// not in `Running` state.
    #[endpoint {
        method = POST,
        path = "/v2/tenants/{tenant_id}/projects/{project_id}/instances/{instance_id}/stop",
        tags = ["instances"],
    }]
    async fn stop_project_instance(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectInstancePath>,
    ) -> Result<HttpResponseOk<Instance>, HttpError>;

    /// Restart a running instance. Returns 409 if the instance is
    /// not in `Running` state.
    #[endpoint {
        method = POST,
        path = "/v2/tenants/{tenant_id}/projects/{project_id}/instances/{instance_id}/restart",
        tags = ["instances"],
    }]
    async fn restart_project_instance(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectInstancePath>,
    ) -> Result<HttpResponseOk<Instance>, HttpError>;

    /// Start a live migration of a running instance (LM-5).
    ///
    /// The body's `action` dispatches between begin / estimate /
    /// pause / switch / abort / rollback / finalize / sync.
    /// LM-5 ships `action=Begin` only; the others return 501
    /// until LM-6 / LM-8 wire the sub-sagas. Begin creates a
    /// `MigrationRecord` (atomic active-key guard against
    /// concurrent migrations of the same VM), kicks off the
    /// `migrate-instance` saga, and returns 202 with the
    /// migration id + the saga id (operation id).
    ///
    /// Tenant-scoped: any project member can migrate their own
    /// instances. Operator-only fields (`target_server_uuid` to
    /// force a specific target CN, cross-tenant target) gate
    /// behind the `root-allows-all` Cedar rule.
    #[endpoint {
        method = POST,
        path = "/v2/tenants/{tenant_id}/projects/{project_id}/instances/{instance_id}/migrate",
        tags = ["instances", "migrations"],
    }]
    async fn migrate_project_instance(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectInstancePath>,
        body: TypedBody<MigrateInstanceBody>,
    ) -> Result<HttpResponseCreated<MigrateInstanceResponse>, HttpError>;

    /// Browser-facing serial / VNC console for a managed instance.
    /// Authorises via the `instance_console` Cedar action; only valid
    /// while the instance is Running. `?kind=vnc` is refused for
    /// brands without a framebuffer (anything but `bhyve` / `kvm`).
    /// Not covered by the generated client (`#[channel]` endpoints are
    /// consumed by the hand-rolled admin-backend proxy).
    #[channel {
        protocol = WEBSOCKETS,
        path = "/v2/tenants/{tenant_id}/projects/{project_id}/instances/{instance_id}/console",
        tags = ["instances"],
    }]
    async fn instance_console(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectInstancePath>,
        query: Query<ConsoleQuery>,
        upgraded: WebsocketConnection,
    ) -> WebsocketChannelResult;

    /// List the NICs attached to an instance. Phase 0 produces
    /// exactly one (the auto-created `"primary"`); a future slice
    /// adds NIC attach/detach.
    #[endpoint {
        method = GET,
        path = "/v2/tenants/{tenant_id}/projects/{project_id}/instances/{instance_id}/nics",
        tags = ["nics"],
    }]
    async fn list_instance_nics(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectInstancePath>,
    ) -> Result<HttpResponseOk<Vec<Nic>>, HttpError>;

    /// Read a single NIC. Returns 404 if the NIC does not exist or
    /// belongs to a different tenant, project, or instance.
    #[endpoint {
        method = GET,
        path = "/v2/tenants/{tenant_id}/projects/{project_id}/instances/{instance_id}/nics/{nic_id}",
        tags = ["nics"],
    }]
    async fn get_instance_nic(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectInstanceNicPath>,
    ) -> Result<HttpResponseOk<Nic>, HttpError>;

    /// List the Disks attached to an instance. Phase 0 produces
    /// exactly one (the auto-created `"boot"`); future multi-disk
    /// attach lands as a follow-on slice.
    #[endpoint {
        method = GET,
        path = "/v2/tenants/{tenant_id}/projects/{project_id}/instances/{instance_id}/disks",
        tags = ["disks"],
    }]
    async fn list_instance_disks(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectInstancePath>,
    ) -> Result<HttpResponseOk<Vec<Disk>>, HttpError>;

    /// Read a single Disk. Returns 404 if the Disk does not exist
    /// or belongs to a different tenant, project, or instance.
    #[endpoint {
        method = GET,
        path = "/v2/tenants/{tenant_id}/projects/{project_id}/instances/{instance_id}/disks/{disk_id}",
        tags = ["disks"],
    }]
    async fn get_instance_disk(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectInstanceDiskPath>,
    ) -> Result<HttpResponseOk<Disk>, HttpError>;

    /// RFD 00007 `GET /v1/disks?tenant=&project=&instance=`. Flat
    /// disk list. AP-2e requires `?instance=<uuid>`; cross-project
    /// disk searches arrive when the customer surface needs them.
    #[endpoint {
        method = GET,
        path = "/v1/disks",
        tags = ["disks"],
    }]
    async fn list_disks_v1(
        rqctx: RequestContext<Self::Context>,
        query: Query<crate::v1::DiskQuery>,
    ) -> Result<HttpResponseOk<crate::v1::ResultsPage<Disk>>, HttpError>;

    /// RFD 00007 `GET /v1/nics?tenant=&project=&instance=&subnet=&ip=`.
    /// Backed by the AP-1c secondary indexes (subnet, ip,
    /// instance-membership). Returns a single-row page when `ip=` is
    /// set (IP -> NIC is unique by invariant).
    #[endpoint {
        method = GET,
        path = "/v1/nics",
        tags = ["nics"],
    }]
    async fn list_nics_v1(
        rqctx: RequestContext<Self::Context>,
        query: Query<crate::v1::NicQuery>,
    ) -> Result<HttpResponseOk<crate::v1::ResultsPage<Nic>>, HttpError>;

    /// RFD 00007 `GET /v1/nics/{nic_id}`. Flat single-NIC read.
    #[endpoint {
        method = GET,
        path = "/v1/nics/{nic_id}",
        tags = ["nics"],
    }]
    async fn get_nic_v1(
        rqctx: RequestContext<Self::Context>,
        path: Path<crate::v1::NicPath>,
    ) -> Result<HttpResponseOk<Nic>, HttpError>;

    /// RFD 00007 `GET /v1/disks/{disk_id}`. Flat single-disk read.
    #[endpoint {
        method = GET,
        path = "/v1/disks/{disk_id}",
        tags = ["disks"],
    }]
    async fn get_disk_v1(
        rqctx: RequestContext<Self::Context>,
        path: Path<crate::v1::DiskPath>,
    ) -> Result<HttpResponseOk<Disk>, HttpError>;

    /// List FloatingIps owned by a project.
    #[endpoint {
        method = GET,
        path = "/v2/tenants/{tenant_id}/projects/{project_id}/floating-ips",
        tags = ["floating-ips"],
    }]
    async fn list_project_floating_ips(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectPath>,
    ) -> Result<HttpResponseOk<Vec<FloatingIp>>, HttpError>;

    /// Allocate a FloatingIp from the requested family's pool.
    /// Returns 409 if the name is already in use within the
    /// project. Returns 404 if the project does not exist or
    /// belongs to a different tenant. The returned FloatingIp
    /// starts unattached.
    #[endpoint {
        method = POST,
        path = "/v2/tenants/{tenant_id}/projects/{project_id}/floating-ips",
        tags = ["floating-ips"],
    }]
    async fn create_project_floating_ip(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectPath>,
        body: TypedBody<NewFloatingIp>,
    ) -> Result<HttpResponseCreated<FloatingIp>, HttpError>;

    /// Read a single FloatingIp. Returns 404 if the FloatingIp
    /// does not exist or belongs to a different tenant or project.
    #[endpoint {
        method = GET,
        path = "/v2/tenants/{tenant_id}/projects/{project_id}/floating-ips/{floating_ip_id}",
        tags = ["floating-ips"],
    }]
    async fn get_project_floating_ip(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectFloatingIpPath>,
    ) -> Result<HttpResponseOk<FloatingIp>, HttpError>;

    /// Release a FloatingIp back to its pool. Returns 409 if the
    /// FloatingIp is currently attached (operator must detach
    /// first).
    #[endpoint {
        method = DELETE,
        path = "/v2/tenants/{tenant_id}/projects/{project_id}/floating-ips/{floating_ip_id}",
        tags = ["floating-ips"],
    }]
    async fn delete_project_floating_ip(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectFloatingIpPath>,
    ) -> Result<HttpResponseDeleted, HttpError>;

    /// Atomically attach a FloatingIp to a NIC, replacing any
    /// existing attachment. The target NIC must live in the same
    /// tenant + project as the FloatingIp; mismatch surfaces as 404.
    #[endpoint {
        method = POST,
        path = "/v2/tenants/{tenant_id}/projects/{project_id}/floating-ips/{floating_ip_id}/attach",
        tags = ["floating-ips"],
    }]
    async fn attach_project_floating_ip(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectFloatingIpPath>,
        body: TypedBody<AttachFloatingIpRequest>,
    ) -> Result<HttpResponseOk<FloatingIp>, HttpError>;

    /// Detach a FloatingIp from its current NIC. Idempotent — a
    /// detach on an already-detached FloatingIp is a no-op that
    /// returns the current record.
    #[endpoint {
        method = POST,
        path = "/v2/tenants/{tenant_id}/projects/{project_id}/floating-ips/{floating_ip_id}/detach",
        tags = ["floating-ips"],
    }]
    async fn detach_project_floating_ip(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectFloatingIpPath>,
    ) -> Result<HttpResponseOk<FloatingIp>, HttpError>;

    // ----- Legacy admin (fleet-scoped) -----

    /// List CNs with their managed-vs-legacy zone counts. Fleet-admin
    /// only. Supports the operator workflow of "show me which CNs
    /// still have legacy zones I haven't adopted yet".
    #[endpoint {
        method = GET,
        path = "/v2/admin/legacy/cns",
        tags = ["legacy-admin"],
    }]
    async fn list_legacy_cns(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseOk<Vec<LegacyCnSummary>>, HttpError>;

    /// List legacy VMs across the fleet, optionally filtered by host
    /// CN. Fleet-admin only.
    #[endpoint {
        method = GET,
        path = "/v2/admin/legacy/vms",
        tags = ["legacy-admin"],
    }]
    async fn list_legacy_vms(
        rqctx: RequestContext<Self::Context>,
        query: Query<LegacyVmListQuery>,
    ) -> Result<HttpResponseOk<Vec<LegacyVm>>, HttpError>;

    /// Read a single legacy VM by SmartOS zone uuid, including full
    /// NIC inventory. Fleet-admin only.
    #[endpoint {
        method = GET,
        path = "/v2/admin/legacy/vms/{smartos_uuid}",
        tags = ["legacy-admin"],
    }]
    async fn get_legacy_vm(
        rqctx: RequestContext<Self::Context>,
        path: Path<LegacyVmPath>,
    ) -> Result<HttpResponseOk<LegacyVm>, HttpError>;

    /// Operator console for a discovered (non-tritond-managed) zone,
    /// addressed by SmartOS zone uuid. Fleet-admin only — no tenant
    /// scoping. Serial only in practice for native zones; `kind=vnc`
    /// is honoured for bhyve / kvm zones. Not covered by the
    /// generated client.
    #[channel {
        protocol = WEBSOCKETS,
        path = "/v2/admin/legacy/vms/{smartos_uuid}/console",
        tags = ["legacy-admin"],
    }]
    async fn legacy_vm_console(
        rqctx: RequestContext<Self::Context>,
        path: Path<LegacyVmPath>,
        query: Query<ConsoleQuery>,
        upgraded: WebsocketConnection,
    ) -> WebsocketChannelResult;

    // ----- Storage clusters (operator-only) -----

    /// List every registered manta-storage cluster, sorted by name.
    /// Operator surface (root-only via Cedar root-allows-all).
    #[endpoint {
        method = GET,
        path = "/v2/storage/clusters",
        tags = ["storage-clusters"],
    }]
    async fn list_storage_clusters(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseOk<Vec<StorageClusterView>>, HttpError>;

    /// Register a new storage cluster (mantad / mantafs / manta-block).
    /// The bearer token submitted in the body is held server-side
    /// and never returned by any GET — see [`StorageClusterView`]
    /// for the redacted wire shape.
    #[endpoint {
        method = POST,
        path = "/v2/storage/clusters",
        tags = ["storage-clusters"],
    }]
    async fn create_storage_cluster(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<NewStorageCluster>,
    ) -> Result<HttpResponseCreated<StorageClusterView>, HttpError>;

    /// Read a single registered storage cluster by id.
    #[endpoint {
        method = GET,
        path = "/v2/storage/clusters/{id}",
        tags = ["storage-clusters"],
    }]
    async fn get_storage_cluster(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterPath>,
    ) -> Result<HttpResponseOk<StorageClusterView>, HttpError>;

    /// Drop a storage cluster registration. Idempotent.
    #[endpoint {
        method = DELETE,
        path = "/v2/storage/clusters/{id}",
        tags = ["storage-clusters"],
    }]
    async fn delete_storage_cluster(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterPath>,
    ) -> Result<HttpResponseDeleted, HttpError>;

    /// Trigger an out-of-band health probe against the cluster's
    /// `/admin/v1/cluster` summary, persist the observed status to
    /// `last_observed_at`, and return the refreshed view. POST
    /// because it actively mutates state.
    #[endpoint {
        method = POST,
        path = "/v2/storage/clusters/{id}/health",
        tags = ["storage-clusters"],
    }]
    async fn probe_storage_cluster_health(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterPath>,
    ) -> Result<HttpResponseOk<StorageClusterView>, HttpError>;

    // ----- Storage cluster forwarders (mantad /admin/v1/* proxy) -----
    //
    // Every forwarder below looks up the StorageCluster by id, builds
    // a `mantad_client::MantadClient` with the stored bearer token,
    // calls the typed method, and returns the response converted via
    // `From` impls into the mirror types defined above. admin-backend
    // and tcadm reach mantad's admin surface only through these
    // endpoints; they never see the raw bearer token.

    /// `GET /admin/v1/cluster` — cluster summary.
    #[endpoint {
        method = GET,
        path = "/v2/storage/clusters/{id}/cluster",
        tags = ["storage-clusters"],
    }]
    async fn get_storage_cluster_summary(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterPath>,
    ) -> Result<HttpResponseOk<StorageClusterSummary>, HttpError>;

    /// `GET /admin/v1/nodes` — list all nodes.
    #[endpoint {
        method = GET,
        path = "/v2/storage/clusters/{id}/nodes",
        tags = ["storage-clusters"],
    }]
    async fn list_storage_cluster_nodes(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterPath>,
    ) -> Result<HttpResponseOk<Vec<StorageNode>>, HttpError>;

    /// `GET /admin/v1/nodes/{node_id}` — single-node detail.
    #[endpoint {
        method = GET,
        path = "/v2/storage/clusters/{id}/nodes/{node_id}",
        tags = ["storage-clusters"],
    }]
    async fn get_storage_cluster_node(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterNodePath>,
    ) -> Result<HttpResponseOk<StorageNode>, HttpError>;

    /// `POST /admin/v1/nodes` — register a peer.
    #[endpoint {
        method = POST,
        path = "/v2/storage/clusters/{id}/nodes",
        tags = ["storage-clusters"],
    }]
    async fn add_storage_cluster_node(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterPath>,
        body: TypedBody<StorageAddNodeRequest>,
    ) -> Result<HttpResponseOk<StorageMembership>, HttpError>;

    /// `DELETE /admin/v1/nodes/{node_id}` — drop a peer.
    #[endpoint {
        method = DELETE,
        path = "/v2/storage/clusters/{id}/nodes/{node_id}",
        tags = ["storage-clusters"],
    }]
    async fn remove_storage_cluster_node(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterNodePath>,
    ) -> Result<HttpResponseOk<StorageMembership>, HttpError>;

    /// `POST /admin/v1/nodes/{node_id}/drain`.
    #[endpoint {
        method = POST,
        path = "/v2/storage/clusters/{id}/nodes/{node_id}/drain",
        tags = ["storage-clusters"],
    }]
    async fn drain_storage_cluster_node(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterNodePath>,
    ) -> Result<HttpResponseOk<StorageMembership>, HttpError>;

    /// `POST /admin/v1/nodes/{node_id}/undrain`.
    #[endpoint {
        method = POST,
        path = "/v2/storage/clusters/{id}/nodes/{node_id}/undrain",
        tags = ["storage-clusters"],
    }]
    async fn undrain_storage_cluster_node(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterNodePath>,
    ) -> Result<HttpResponseOk<StorageMembership>, HttpError>;

    /// `POST /admin/v1/nodes/{node_id}/reweight`.
    #[endpoint {
        method = POST,
        path = "/v2/storage/clusters/{id}/nodes/{node_id}/reweight",
        tags = ["storage-clusters"],
    }]
    async fn reweight_storage_cluster_node(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterNodePath>,
        body: TypedBody<StorageReweightRequest>,
    ) -> Result<HttpResponseOk<StorageMembership>, HttpError>;

    /// `GET /admin/v1/membership` — current membership view.
    #[endpoint {
        method = GET,
        path = "/v2/storage/clusters/{id}/membership",
        tags = ["storage-clusters"],
    }]
    async fn get_storage_cluster_membership(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterPath>,
    ) -> Result<HttpResponseOk<StorageMembership>, HttpError>;

    /// `GET /admin/v1/buckets` — all buckets in the cluster.
    /// `?stats=true` forwards `?stats=1` to mantad and includes
    /// per-bucket object counts + total bytes (a more expensive scan).
    #[endpoint {
        method = GET,
        path = "/v2/storage/clusters/{id}/buckets",
        tags = ["storage-clusters"],
    }]
    async fn list_storage_cluster_buckets(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterPath>,
        query: Query<StorageBucketListQuery>,
    ) -> Result<HttpResponseOk<Vec<StorageBucket>>, HttpError>;

    /// `GET /admin/v1/buckets/{bucket}` — bucket detail.
    #[endpoint {
        method = GET,
        path = "/v2/storage/clusters/{id}/buckets/{bucket}",
        tags = ["storage-clusters"],
    }]
    async fn get_storage_cluster_bucket(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterBucketPath>,
    ) -> Result<HttpResponseOk<StorageBucket>, HttpError>;

    /// `POST /admin/v1/buckets` — create a bucket.
    #[endpoint {
        method = POST,
        path = "/v2/storage/clusters/{id}/buckets",
        tags = ["storage-clusters"],
    }]
    async fn create_storage_cluster_bucket(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterPath>,
        body: TypedBody<StorageCreateBucketRequest>,
    ) -> Result<HttpResponseCreated<StorageBucket>, HttpError>;

    /// `DELETE /admin/v1/buckets/{bucket}` — delete an empty bucket.
    #[endpoint {
        method = DELETE,
        path = "/v2/storage/clusters/{id}/buckets/{bucket}",
        tags = ["storage-clusters"],
    }]
    async fn delete_storage_cluster_bucket(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterBucketPath>,
    ) -> Result<HttpResponseDeleted, HttpError>;

    /// `GET /admin/v1/buckets/{bucket}/objects` — paged object listing.
    #[endpoint {
        method = GET,
        path = "/v2/storage/clusters/{id}/buckets/{bucket}/objects",
        tags = ["storage-clusters"],
    }]
    async fn list_storage_cluster_objects(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterBucketPath>,
        query: Query<StorageObjectsQuery>,
    ) -> Result<HttpResponseOk<StorageObjectsPage>, HttpError>;

    /// `GET /admin/v1/users`.
    #[endpoint {
        method = GET,
        path = "/v2/storage/clusters/{id}/users",
        tags = ["storage-clusters"],
    }]
    async fn list_storage_cluster_users(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterPath>,
    ) -> Result<HttpResponseOk<Vec<StorageUser>>, HttpError>;

    /// `POST /admin/v1/users`.
    #[endpoint {
        method = POST,
        path = "/v2/storage/clusters/{id}/users",
        tags = ["storage-clusters"],
    }]
    async fn create_storage_cluster_user(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterPath>,
        body: TypedBody<StorageCreateUserRequest>,
    ) -> Result<HttpResponseCreated<StorageUser>, HttpError>;

    /// `GET /admin/v1/users/{user}`.
    #[endpoint {
        method = GET,
        path = "/v2/storage/clusters/{id}/users/{user}",
        tags = ["storage-clusters"],
    }]
    async fn get_storage_cluster_user(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterUserPath>,
    ) -> Result<HttpResponseOk<StorageUser>, HttpError>;

    /// `DELETE /admin/v1/users/{user}`.
    #[endpoint {
        method = DELETE,
        path = "/v2/storage/clusters/{id}/users/{user}",
        tags = ["storage-clusters"],
    }]
    async fn delete_storage_cluster_user(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterUserPath>,
    ) -> Result<HttpResponseDeleted, HttpError>;

    /// `GET /admin/v1/users/{user}/access-keys`.
    #[endpoint {
        method = GET,
        path = "/v2/storage/clusters/{id}/users/{user}/access-keys",
        tags = ["storage-clusters"],
    }]
    async fn list_storage_cluster_access_keys(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterUserPath>,
    ) -> Result<HttpResponseOk<Vec<StorageAccessKey>>, HttpError>;

    /// `POST /admin/v1/users/{user}/access-keys` — secret returned
    /// once on the response. Caller must capture it; mantad does not
    /// retain the cleartext.
    #[endpoint {
        method = POST,
        path = "/v2/storage/clusters/{id}/users/{user}/access-keys",
        tags = ["storage-clusters"],
    }]
    async fn create_storage_cluster_access_key(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterUserPath>,
    ) -> Result<HttpResponseCreated<StorageAccessKey>, HttpError>;

    /// `DELETE /admin/v1/access-keys/{access_key_id}`. Note that the
    /// AKID alone identifies the key — there's no per-user nesting
    /// here, mirroring mantad's surface.
    #[endpoint {
        method = DELETE,
        path = "/v2/storage/clusters/{id}/access-keys/{access_key_id}",
        tags = ["storage-clusters"],
    }]
    async fn delete_storage_cluster_access_key(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterAccessKeyPath>,
    ) -> Result<HttpResponseDeleted, HttpError>;

    /// `GET /admin/v1/users/{user}/policies` — list attached policy
    /// names. Returns a Vec<String> verbatim from mantad.
    #[endpoint {
        method = GET,
        path = "/v2/storage/clusters/{id}/users/{user}/policies",
        tags = ["storage-clusters"],
    }]
    async fn list_storage_cluster_user_policies(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterUserPath>,
    ) -> Result<HttpResponseOk<Vec<String>>, HttpError>;

    /// `GET /admin/v1/users/{user}/policies/{policy}` — raw policy JSON.
    #[endpoint {
        method = GET,
        path = "/v2/storage/clusters/{id}/users/{user}/policies/{policy}",
        tags = ["storage-clusters"],
    }]
    async fn get_storage_cluster_user_policy(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterUserPolicyPath>,
    ) -> Result<HttpResponseOk<serde_json::Value>, HttpError>;

    /// `PUT /admin/v1/users/{user}/policies/{policy}` — upsert a JSON
    /// policy doc. Body is the raw policy document; mantad does not
    /// validate schema beyond "is JSON".
    #[endpoint {
        method = PUT,
        path = "/v2/storage/clusters/{id}/users/{user}/policies/{policy}",
        tags = ["storage-clusters"],
    }]
    async fn put_storage_cluster_user_policy(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterUserPolicyPath>,
        body: TypedBody<serde_json::Value>,
    ) -> Result<HttpResponseUpdatedNoContent, HttpError>;

    /// `DELETE /admin/v1/users/{user}/policies/{policy}`.
    #[endpoint {
        method = DELETE,
        path = "/v2/storage/clusters/{id}/users/{user}/policies/{policy}",
        tags = ["storage-clusters"],
    }]
    async fn delete_storage_cluster_user_policy(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterUserPolicyPath>,
    ) -> Result<HttpResponseDeleted, HttpError>;

    // ----- Presign + multipart (Stage 6a) -----
    //
    // tritond holds the per-cluster IAM presigner credential; the
    // handlers below use it to sign URLs the browser PUTs/GETs
    // bytes against directly. Bytes never proxy through tritond or
    // admin-backend — the URL is the entire authorization token.

    /// Configure (or rotate) the per-cluster presigner identity.
    /// `s3_endpoint` updates the cluster's data-plane URL; pass
    /// `None` to leave it unchanged. Empty `access_key_id` +
    /// `secret_access_key` clear the presigner. Mismatched
    /// (one empty, one set) → 409.
    #[endpoint {
        method = POST,
        path = "/v2/storage/clusters/{id}/presigner",
        tags = ["storage-clusters"],
    }]
    async fn set_storage_cluster_presigner(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterPath>,
        body: TypedBody<SetPresignerRequest>,
    ) -> Result<HttpResponseOk<StorageClusterView>, HttpError>;

    /// Mint a single-shot presigned PUT URL (uploads < 5 MB).
    /// Returns 409 when the cluster has no presigner configured.
    #[endpoint {
        method = POST,
        path = "/v2/storage/clusters/{id}/s3/presign/put",
        tags = ["storage-clusters"],
    }]
    async fn presign_storage_cluster_object_put(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterPath>,
        body: TypedBody<PresignPutRequest>,
    ) -> Result<HttpResponseOk<PresignResponse>, HttpError>;

    /// Mint a single-shot presigned GET URL (downloads or shareable
    /// links).
    #[endpoint {
        method = POST,
        path = "/v2/storage/clusters/{id}/s3/presign/get",
        tags = ["storage-clusters"],
    }]
    async fn presign_storage_cluster_object_get(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterPath>,
        body: TypedBody<PresignGetRequest>,
    ) -> Result<HttpResponseOk<PresignResponse>, HttpError>;

    // Multipart upload endpoints (initiate / parts / complete / abort)
    // are deferred — single-shot PUT supports files up to 5 GB, which
    // covers ~all admin-UI upload cases. Multipart adds full SigV4
    // request signing + XML parsing complexity that's out of scope for
    // v1. The `Action::StorageObjectMultipart*` Cedar variants and
    // the `Multipart*` wire types are kept in place so the policy +
    // contract don't churn when the follow-up lands.

    // ----- Metrics (Slice 1: per-zone CPU) -----
    //
    // The V5 dashboard renders six multi-series charts; the data
    // pipeline fans out into:
    //   * tritonagent reads kstats and POSTs Samples here.
    //   * tritond writes Samples to a `MetricsStore` (in-memory ring
    //     buffer in dev, ClickHouse in production).
    //   * The admin UI calls `instance_metrics_range` for a typed
    //     `RangeResult` shaped for SVG line charts.
    //
    // Per-tenant + admin Prometheus text exposition lives on a
    // separate metrics-only HTTP listener in tritond's main process
    // (different port, no JWT) and is intentionally NOT part of this
    // OpenAPI surface — Prometheus scrapers don't consume OpenAPI
    // schemas, and the text exposition's content-type would force a
    // raw-body return type that doesn't round-trip cleanly through
    // Progenitor.

    /// Ingest a batch of metric samples from a registered agent.
    /// Auth: requires an API key with
    /// [`tritond_store::ApiKeyScope::Agent`]. Batches larger than
    /// [`tritond_metrics::SampleBatch::MAX_SAMPLES`] are rejected
    /// with `413 Payload Too Large`.
    #[endpoint {
        method = POST,
        path = "/v2/agent/metrics",
        tags = ["agent"],
    }]
    async fn agent_metrics_ingest(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<tritond_metrics::SampleBatch>,
    ) -> Result<HttpResponseUpdatedNoContent, HttpError>;

    /// Range query for the per-instance metrics chart on the VM
    /// detail view. Returns one numeric series per distinct `series`
    /// identity field encountered in the requested window. Counter
    /// datums are emitted as per-bucket deltas; gauges as last-value
    /// per bucket.
    ///
    /// Auth: requires a tenant-scoped credential with read access
    /// to the named instance.
    #[endpoint {
        method = GET,
        path = "/v2/tenants/{tenant_id}/projects/{project_id}/instances/{instance_id}/metrics",
        tags = ["instances"],
    }]
    async fn instance_metrics_range(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectInstancePath>,
        query: Query<MetricsRangeQuery>,
    ) -> Result<HttpResponseOk<tritond_metrics::RangeResult>, HttpError>;

    /// Range query for the per-CN metrics dashboard (NodeDetail
    /// page's Metrics tab). Returns one series per CPU mode for the
    /// requested CN's global zone -- host services + kernel time
    /// not charged to a tenant zone. Mirrors the legacy cmon
    /// `/v1/discover` -> per-CN scrape pattern, just typed.
    ///
    /// Auth: requires fleet-read access to the CN (same scope as
    /// `get_cn`). No tenant filter -- per-CN samples are operator
    /// surface.
    #[endpoint {
        method = GET,
        path = "/v2/cns/{server_uuid}/metrics",
        tags = ["cns"],
    }]
    async fn cn_metrics_range(
        rqctx: RequestContext<Self::Context>,
        path: Path<CnPath>,
        query: Query<MetricsRangeQuery>,
    ) -> Result<HttpResponseOk<tritond_metrics::RangeResult>, HttpError>;

    // ----- Logs (zone console.log + platform.log) -----
    //
    // Same control-plane shape as metrics: agent posts batches; the
    // tail endpoint serves the most recent N lines from a per-stream
    // ring buffer. Console + platform are kept as distinct sources
    // because the line semantics differ (raw text vs structured
    // bunyan) and operators reason about them separately.

    /// Ingest a batch of log lines from a registered agent.
    /// Auth: requires an API key with
    /// [`tritond_store::ApiKeyScope::Agent`]. Batches larger than
    /// [`tritond_logs::LogBatch::MAX_LINES`] are rejected with
    /// `413 Payload Too Large`.
    #[endpoint {
        method = POST,
        path = "/v2/agent/logs",
        tags = ["agent"],
    }]
    async fn agent_logs_ingest(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<tritond_logs::LogBatch>,
    ) -> Result<HttpResponseUpdatedNoContent, HttpError>;

    /// Tail the most recent log lines for one source on one VM.
    /// Pagination via `before_seq` -- pass the smallest `seq` from
    /// the previous response to fetch older lines.
    ///
    /// Auth: requires a tenant-scoped credential with read access
    /// to the named instance (same envelope as `get_project_instance`).
    #[endpoint {
        method = GET,
        path = "/v2/tenants/{tenant_id}/projects/{project_id}/instances/{instance_id}/logs/{source}",
        tags = ["instances"],
    }]
    async fn instance_logs_tail(
        rqctx: RequestContext<Self::Context>,
        path: Path<InstanceLogsPath>,
        query: Query<LogTailQuery>,
    ) -> Result<HttpResponseOk<tritond_logs::LogTailResult>, HttpError>;

    // ---- Layered instance metadata (IMDS) — IMDS_DESIGN.md §4.1 ----

    /// List every metadata entry stored at one scope. RBAC: silo-member
    /// for `scope=silo`; tenant-member (of the scope's owning tenant)
    /// for `scope=tenant|project|instance`.
    #[endpoint {
        method = GET,
        path = "/v2/meta/{scope}/{scope_id}",
        tags = ["meta"],
    }]
    async fn list_meta(
        rqctx: RequestContext<Self::Context>,
        path: Path<MetaScopePath>,
    ) -> Result<HttpResponseOk<Vec<MetaEntry>>, HttpError>;

    /// Read one metadata entry by key (query parameter). The key may
    /// contain `/` (`config/ntp-servers`, `state/active-color`, …),
    /// hence the query-string placement.
    #[endpoint {
        method = GET,
        path = "/v2/meta/{scope}/{scope_id}/entry",
        tags = ["meta"],
    }]
    async fn get_meta(
        rqctx: RequestContext<Self::Context>,
        path: Path<MetaScopePath>,
        query: Query<MetaKeyQuery>,
    ) -> Result<HttpResponseOk<MetaEntry>, HttpError>;

    /// Upsert one metadata entry. Returns the stored entry plus the
    /// scope's new generation counter (the realized-view cache key).
    /// Validation per `tritond_store::validate_meta_entry`: namespace
    /// + scope + value-type + byte-cap rules; `guest_writable=true` is
    /// only accepted on `guest/*` keys at instance scope.
    #[endpoint {
        method = PUT,
        path = "/v2/meta/{scope}/{scope_id}/entry",
        tags = ["meta"],
    }]
    async fn set_meta(
        rqctx: RequestContext<Self::Context>,
        path: Path<MetaScopePath>,
        query: Query<MetaKeyQuery>,
        body: TypedBody<SetMetaRequest>,
    ) -> Result<HttpResponseOk<SetMetaResponse>, HttpError>;

    /// Delete one metadata entry. 404 if the key is absent (no
    /// generation bump in that case).
    #[endpoint {
        method = DELETE,
        path = "/v2/meta/{scope}/{scope_id}/entry",
        tags = ["meta"],
    }]
    async fn delete_meta(
        rqctx: RequestContext<Self::Context>,
        path: Path<MetaScopePath>,
        query: Query<MetaKeyQuery>,
    ) -> Result<HttpResponseDeleted, HttpError>;

    /// The full realized metadata view for one instance: the
    /// precedence merge of the four stored scopes (Silo < Tenant <
    /// Project < Instance) with the computed system keys
    /// (`meta-data/*`, `triton/system/*`) layered on top, each leaf
    /// tagged with the scope it came from. See `IMDS_DESIGN.md` §1.5.
    /// RBAC: tenant-member of the instance's owning tenant.
    #[endpoint {
        method = GET,
        path = "/v2/instances/{instance_id}/realized-meta",
        tags = ["meta"],
    }]
    async fn get_instance_realized_meta(
        rqctx: RequestContext<Self::Context>,
        path: Path<InstanceRealizedMetaPath>,
    ) -> Result<HttpResponseOk<Vec<RealizedMetaEntry>>, HttpError>;

    /// The affected-instances reverse-index for one (scope, key) pair.
    /// Walks every instance under the request scope and partitions
    /// them by whether the request scope's value wins for that
    /// instance or a narrower scope's value shadows it. RBAC: same
    /// as [`Self::list_meta`] at the request scope.
    ///
    /// Used by the operator console's IMDS authoring surface to
    /// answer "if I edit here, what changes?" *before* the operator
    /// commits — see `IMDS_DESIGN.md` §1.5 and the admin v3 design
    /// chat.
    #[endpoint {
        method = GET,
        path = "/v2/meta/{scope}/{scope_id}/affected",
        tags = ["meta"],
    }]
    async fn get_affected_instances(
        rqctx: RequestContext<Self::Context>,
        path: Path<MetaScopePath>,
        query: Query<MetaKeyQuery>,
    ) -> Result<HttpResponseOk<AffectedInstancesResponse>, HttpError>;
}
