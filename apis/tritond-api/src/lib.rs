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
    HttpError, HttpResponseCreated, HttpResponseDeleted, HttpResponseOk, Path, Query,
    RequestContext, TypedBody,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tritond_auth::RedactedString;
use uuid::Uuid;

use crate::types::{
    ApiKeyScope, ApiKeyView, AuditChainHead, AuditEvent, AuditVerifyOutcome, AutoApproveWindow,
    CnRole, CnState, CnView, Disk, FloatingIp, IdpConfigView, Image, Instance, JobKind, JobOutcome,
    NatGateway, NetworkResourceId, NewFloatingIp, NewImage, NewInstance, NewNatGateway, NewProject,
    NewQuota, NewRoute, NewRouteTable, NewSilo, NewSshKey, NewSubnet, NewTenant, NewVpc, Nic,
    Project, ProvisioningJob, Quota, RealizationStatus, RealizerId, Route, RouteTable, Silo,
    SshKey, Subnet, Tenant, Vpc,
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
/// nics + disks + ssh public keys, all in one response. That lets
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
///   any `ssh_public_keys` are populated. The agent has
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
    /// All disks attached to the instance. Empty for non-Provision
    /// jobs.
    #[serde(default)]
    pub disks: Vec<Disk>,
    /// Raw openssh-form public keys to inject via the SmartOS
    /// `root_authorized_keys` metadata at zone-create time.
    /// Resolved from `Instance::ssh_key_ids`.
    #[serde(default)]
    pub ssh_public_keys: Vec<String>,
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
}
