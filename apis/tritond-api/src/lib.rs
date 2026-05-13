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
    IdpConfigView, Image, Instance, JobKind, JobOutcome, LegacyVm, ManagedIdentity, NatGateway,
    NetworkResourceId, NewDhcpPool, NewDhcpReservation, NewFirewallRule, NewFloatingIp, NewImage,
    NewInstance, NewNatGateway, NewProject, NewQuota, NewRoute, NewRouteTable, NewSilo, NewSshKey,
    NewStorageCluster, NewSubnet, NewTenant, NewVpc, Nic, Project, ProvisioningJob, Quota,
    RealizationStatus, RealizerId, Route, RouteTable, Silo, SshKey, StorageClusterView, Subnet,
    Tenant, Vpc,
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
}

/// Path parameters for endpoints that operate on a single audit event.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct AuditEventPath {
    pub seq: u64,
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
}

/// Path parameter for endpoints that operate on a single CN.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct CnPath {
    pub server_uuid: Uuid,
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
}
