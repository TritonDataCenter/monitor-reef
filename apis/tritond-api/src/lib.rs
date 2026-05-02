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
    ApiKeyScope, ApiKeyView, AuditChainHead, AuditEvent, AuditVerifyOutcome, Disk, FloatingIp,
    IdpConfigView, Image, Instance, JobKind, JobOutcome, NewFloatingIp, NewImage, NewInstance,
    NewProject, NewQuota, NewSilo, NewSshKey, NewSubnet, NewVpc, Nic, Project, ProvisioningJob,
    Quota, Silo, SshKey, Subnet, Vpc,
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

/// Path parameters for endpoints that operate on a single project
/// inside a silo.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SiloProjectPath {
    pub silo_id: Uuid,
    pub project_id: Uuid,
}

/// Path parameters for endpoints that operate on a single VPC inside a
/// project inside a silo.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SiloProjectVpcPath {
    pub silo_id: Uuid,
    pub project_id: Uuid,
    pub vpc_id: Uuid,
}

/// Path parameters for endpoints that operate on a single subnet
/// inside a VPC inside a project inside a silo.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SiloProjectVpcSubnetPath {
    pub silo_id: Uuid,
    pub project_id: Uuid,
    pub vpc_id: Uuid,
    pub subnet_id: Uuid,
}

/// Path parameters for endpoints that operate on a single SSH key
/// inside a silo.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SiloSshKeyPath {
    pub silo_id: Uuid,
    pub ssh_key_id: Uuid,
}

/// Path parameters for endpoints that operate on a single image
/// inside a silo.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SiloImagePath {
    pub silo_id: Uuid,
    pub image_id: Uuid,
}

/// Path parameters for endpoints that operate on a single instance
/// inside a project inside a silo.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SiloProjectInstancePath {
    pub silo_id: Uuid,
    pub project_id: Uuid,
    pub instance_id: Uuid,
}

/// Path parameters for endpoints that operate on a single NIC
/// belonging to an instance.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SiloProjectInstanceNicPath {
    pub silo_id: Uuid,
    pub project_id: Uuid,
    pub instance_id: Uuid,
    pub nic_id: Uuid,
}

/// Path parameters for endpoints that operate on a single Disk
/// belonging to an instance.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SiloProjectInstanceDiskPath {
    pub silo_id: Uuid,
    pub project_id: Uuid,
    pub instance_id: Uuid,
    pub disk_id: Uuid,
}

/// Path parameters for endpoints that operate on a single
/// FloatingIp inside a project.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SiloProjectFloatingIpPath {
    pub silo_id: Uuid,
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

/// Request body for `POST /v2/silos/{silo_id}/idp`. tritond
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

    /// Configure the OIDC identity provider for a silo. Returns 502
    /// if the discovery document cannot be fetched, 404 if the silo
    /// does not exist, otherwise 201 with the redacted view of what
    /// was persisted.
    #[endpoint {
        method = POST,
        path = "/v2/silos/{silo_id}/idp",
        tags = ["silos", "auth"],
    }]
    async fn put_silo_idp(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloPath>,
        body: TypedBody<NewIdpConfig>,
    ) -> Result<HttpResponseCreated<IdpConfigView>, HttpError>;

    /// Read the OIDC IdP config for a silo. The client secret is
    /// never returned. 404 when no IdP is configured.
    #[endpoint {
        method = GET,
        path = "/v2/silos/{silo_id}/idp",
        tags = ["silos", "auth"],
    }]
    async fn get_silo_idp(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloPath>,
    ) -> Result<HttpResponseOk<IdpConfigView>, HttpError>;

    /// Remove the OIDC IdP config for a silo. Federated users in
    /// that silo will fail to authenticate until a new config is
    /// posted.
    #[endpoint {
        method = DELETE,
        path = "/v2/silos/{silo_id}/idp",
        tags = ["silos", "auth"],
    }]
    async fn delete_silo_idp(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloPath>,
    ) -> Result<HttpResponseDeleted, HttpError>;

    /// List the projects inside a silo.
    #[endpoint {
        method = GET,
        path = "/v2/silos/{silo_id}/projects",
        tags = ["projects"],
    }]
    async fn list_silo_projects(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloPath>,
    ) -> Result<HttpResponseOk<Vec<Project>>, HttpError>;

    /// Create a project in a silo. Returns 409 if the project name
    /// is already in use within that silo.
    #[endpoint {
        method = POST,
        path = "/v2/silos/{silo_id}/projects",
        tags = ["projects"],
    }]
    async fn create_silo_project(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloPath>,
        body: TypedBody<NewProject>,
    ) -> Result<HttpResponseCreated<Project>, HttpError>;

    /// Read a single project. Returns 404 when the project does not
    /// exist or belongs to a different silo (cross-silo probes do not
    /// learn that other silos exist).
    #[endpoint {
        method = GET,
        path = "/v2/silos/{silo_id}/projects/{project_id}",
        tags = ["projects"],
    }]
    async fn get_silo_project(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloProjectPath>,
    ) -> Result<HttpResponseOk<Project>, HttpError>;

    /// Delete a project. Returns 404 when the project does not exist
    /// or belongs to a different silo.
    #[endpoint {
        method = DELETE,
        path = "/v2/silos/{silo_id}/projects/{project_id}",
        tags = ["projects"],
    }]
    async fn delete_silo_project(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloProjectPath>,
    ) -> Result<HttpResponseDeleted, HttpError>;

    /// List the VPCs inside a project. Returns 404 when the silo or
    /// project does not exist (or the project belongs to a different
    /// silo).
    #[endpoint {
        method = GET,
        path = "/v2/silos/{silo_id}/projects/{project_id}/vpcs",
        tags = ["vpcs"],
    }]
    async fn list_project_vpcs(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloProjectPath>,
    ) -> Result<HttpResponseOk<Vec<Vpc>>, HttpError>;

    /// Create a VPC in a project. Returns 400 if neither `ipv4_block`
    /// nor `ipv6_block` is provided. Returns 409 if a VPC with the
    /// same name already exists in the project. The server assigns
    /// `id`, `vni` (random in `[4096, 2^24)`, unique rack-wide), and
    /// `created_at`.
    #[endpoint {
        method = POST,
        path = "/v2/silos/{silo_id}/projects/{project_id}/vpcs",
        tags = ["vpcs"],
    }]
    async fn create_project_vpc(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloProjectPath>,
        body: TypedBody<NewVpc>,
    ) -> Result<HttpResponseCreated<Vpc>, HttpError>;

    /// Read a single VPC. Returns 404 when the VPC does not exist or
    /// belongs to a different silo or project (cross-tenant probes
    /// do not learn that the resource exists).
    #[endpoint {
        method = GET,
        path = "/v2/silos/{silo_id}/projects/{project_id}/vpcs/{vpc_id}",
        tags = ["vpcs"],
    }]
    async fn get_project_vpc(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloProjectVpcPath>,
    ) -> Result<HttpResponseOk<Vpc>, HttpError>;

    /// Delete a VPC. Returns 404 when the VPC does not exist or
    /// belongs to a different silo or project. Returns 409 if the
    /// VPC still has subnets attached — operators must clear subnets
    /// before deleting the parent VPC (Phase 0 has no cascade).
    #[endpoint {
        method = DELETE,
        path = "/v2/silos/{silo_id}/projects/{project_id}/vpcs/{vpc_id}",
        tags = ["vpcs"],
    }]
    async fn delete_project_vpc(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloProjectVpcPath>,
    ) -> Result<HttpResponseDeleted, HttpError>;

    /// List the subnets inside a VPC. Returns 404 when the silo,
    /// project, or VPC does not exist (or is in the wrong parent).
    #[endpoint {
        method = GET,
        path = "/v2/silos/{silo_id}/projects/{project_id}/vpcs/{vpc_id}/subnets",
        tags = ["subnets"],
    }]
    async fn list_vpc_subnets(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloProjectVpcPath>,
    ) -> Result<HttpResponseOk<Vec<Subnet>>, HttpError>;

    /// Create a subnet in a VPC. Returns 400 if neither
    /// `ipv4_block` nor `ipv6_block` is provided. Returns 409 if a
    /// subnet with the same name already exists in the VPC, if a
    /// CIDR is not contained in the parent VPC's matching-family
    /// CIDR, or if a CIDR overlaps an existing subnet's CIDR. The
    /// server assigns `id` and `created_at`.
    #[endpoint {
        method = POST,
        path = "/v2/silos/{silo_id}/projects/{project_id}/vpcs/{vpc_id}/subnets",
        tags = ["subnets"],
    }]
    async fn create_vpc_subnet(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloProjectVpcPath>,
        body: TypedBody<NewSubnet>,
    ) -> Result<HttpResponseCreated<Subnet>, HttpError>;

    /// Read a single subnet. Returns 404 when the subnet does not
    /// exist or belongs to a different silo, project, or VPC.
    #[endpoint {
        method = GET,
        path = "/v2/silos/{silo_id}/projects/{project_id}/vpcs/{vpc_id}/subnets/{subnet_id}",
        tags = ["subnets"],
    }]
    async fn get_vpc_subnet(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloProjectVpcSubnetPath>,
    ) -> Result<HttpResponseOk<Subnet>, HttpError>;

    /// Delete a subnet. Returns 404 when the subnet does not exist
    /// or belongs to a different silo, project, or VPC.
    #[endpoint {
        method = DELETE,
        path = "/v2/silos/{silo_id}/projects/{project_id}/vpcs/{vpc_id}/subnets/{subnet_id}",
        tags = ["subnets"],
    }]
    async fn delete_vpc_subnet(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloProjectVpcSubnetPath>,
    ) -> Result<HttpResponseDeleted, HttpError>;

    /// List the SSH keys registered in a silo's catalog.
    #[endpoint {
        method = GET,
        path = "/v2/silos/{silo_id}/ssh-keys",
        tags = ["ssh-keys"],
    }]
    async fn list_silo_ssh_keys(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloPath>,
    ) -> Result<HttpResponseOk<Vec<SshKey>>, HttpError>;

    /// Register an SSH key in a silo's catalog. The server parses
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

    /// Read a single SSH key. Returns 404 when the key does not
    /// exist or belongs to a different silo.
    #[endpoint {
        method = GET,
        path = "/v2/silos/{silo_id}/ssh-keys/{ssh_key_id}",
        tags = ["ssh-keys"],
    }]
    async fn get_silo_ssh_key(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloSshKeyPath>,
    ) -> Result<HttpResponseOk<SshKey>, HttpError>;

    /// Delete an SSH key. Returns 404 when the key does not exist
    /// or belongs to a different silo.
    #[endpoint {
        method = DELETE,
        path = "/v2/silos/{silo_id}/ssh-keys/{ssh_key_id}",
        tags = ["ssh-keys"],
    }]
    async fn delete_silo_ssh_key(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloSshKeyPath>,
    ) -> Result<HttpResponseDeleted, HttpError>;

    /// List the images registered in a silo's catalog.
    #[endpoint {
        method = GET,
        path = "/v2/silos/{silo_id}/images",
        tags = ["images"],
    }]
    async fn list_silo_images(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloPath>,
    ) -> Result<HttpResponseOk<Vec<Image>>, HttpError>;

    /// Register an image in a silo's catalog. Returns 400 if
    /// `sha256` is not 64 lowercase hex chars or if `size_bytes`
    /// is zero. Returns 409 if the name is already in use within
    /// the silo.
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

    /// Read a single image. Returns 404 when the image does not
    /// exist or belongs to a different silo.
    #[endpoint {
        method = GET,
        path = "/v2/silos/{silo_id}/images/{image_id}",
        tags = ["images"],
    }]
    async fn get_silo_image(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloImagePath>,
    ) -> Result<HttpResponseOk<Image>, HttpError>;

    /// Delete an image. Returns 404 when the image does not exist
    /// or belongs to a different silo.
    #[endpoint {
        method = DELETE,
        path = "/v2/silos/{silo_id}/images/{image_id}",
        tags = ["images"],
    }]
    async fn delete_silo_image(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloImagePath>,
    ) -> Result<HttpResponseDeleted, HttpError>;

    /// Set (or replace) the resource quota on a project. Returns
    /// 404 when the project does not exist or belongs to a
    /// different silo. The server assigns `updated_at`. Quotas
    /// are not enforced in Phase 0; the record is stored for the
    /// eventual instance-create flow to consult.
    #[endpoint {
        method = PUT,
        path = "/v2/silos/{silo_id}/projects/{project_id}/quota",
        tags = ["quotas"],
    }]
    async fn put_project_quota(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloProjectPath>,
        body: TypedBody<NewQuota>,
    ) -> Result<HttpResponseOk<Quota>, HttpError>;

    /// Read a project's quota. Returns 404 when the project does
    /// not exist, lives in a different silo, or has no quota set
    /// (no record means "unlimited").
    #[endpoint {
        method = GET,
        path = "/v2/silos/{silo_id}/projects/{project_id}/quota",
        tags = ["quotas"],
    }]
    async fn get_project_quota(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloProjectPath>,
    ) -> Result<HttpResponseOk<Quota>, HttpError>;

    /// Remove a project's quota (project becomes unlimited).
    /// Returns 404 when the project does not exist, lives in a
    /// different silo, or had no quota set.
    #[endpoint {
        method = DELETE,
        path = "/v2/silos/{silo_id}/projects/{project_id}/quota",
        tags = ["quotas"],
    }]
    async fn delete_project_quota(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloProjectPath>,
    ) -> Result<HttpResponseDeleted, HttpError>;

    /// List instances in a project.
    #[endpoint {
        method = GET,
        path = "/v2/silos/{silo_id}/projects/{project_id}/instances",
        tags = ["instances"],
    }]
    async fn list_project_instances(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloProjectPath>,
    ) -> Result<HttpResponseOk<Vec<Instance>>, HttpError>;

    /// Create an instance in a project.
    ///
    /// Returns 400 if `cpu == 0` or `memory_bytes == 0`. Returns 404
    /// if any referenced resource (image, subnet, ssh-keys) does
    /// not exist or lives outside this silo/project. Returns 409
    /// if the instance name is already taken in the project.
    ///
    /// Phase 0 ships synchronous lifecycle: the create handler
    /// transitions the new instance through Pending → Running
    /// before responding. A future slice introduces an async
    /// provisioning queue + stub executor; the API surface stays
    /// the same but tests will observe Pending-then-Running
    /// transitions instead of an instant Running.
    #[endpoint {
        method = POST,
        path = "/v2/silos/{silo_id}/projects/{project_id}/instances",
        tags = ["instances"],
    }]
    async fn create_project_instance(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloProjectPath>,
        body: TypedBody<NewInstance>,
    ) -> Result<HttpResponseCreated<Instance>, HttpError>;

    /// Read a single instance.
    #[endpoint {
        method = GET,
        path = "/v2/silos/{silo_id}/projects/{project_id}/instances/{instance_id}",
        tags = ["instances"],
    }]
    async fn get_project_instance(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloProjectInstancePath>,
    ) -> Result<HttpResponseOk<Instance>, HttpError>;

    /// Delete an instance. Returns 409 if the instance is not in
    /// a deletable state (must be Stopped or Failed). Returns 404
    /// if the instance does not exist or belongs to a different
    /// silo or project.
    #[endpoint {
        method = DELETE,
        path = "/v2/silos/{silo_id}/projects/{project_id}/instances/{instance_id}",
        tags = ["instances"],
    }]
    async fn delete_project_instance(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloProjectInstancePath>,
    ) -> Result<HttpResponseDeleted, HttpError>;

    /// Start a stopped instance. Returns 409 if the instance is
    /// not in `Stopped` state.
    #[endpoint {
        method = POST,
        path = "/v2/silos/{silo_id}/projects/{project_id}/instances/{instance_id}/start",
        tags = ["instances"],
    }]
    async fn start_project_instance(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloProjectInstancePath>,
    ) -> Result<HttpResponseOk<Instance>, HttpError>;

    /// Stop a running instance. Returns 409 if the instance is
    /// not in `Running` state.
    #[endpoint {
        method = POST,
        path = "/v2/silos/{silo_id}/projects/{project_id}/instances/{instance_id}/stop",
        tags = ["instances"],
    }]
    async fn stop_project_instance(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloProjectInstancePath>,
    ) -> Result<HttpResponseOk<Instance>, HttpError>;

    /// Restart a running instance. Returns 409 if the instance is
    /// not in `Running` state.
    #[endpoint {
        method = POST,
        path = "/v2/silos/{silo_id}/projects/{project_id}/instances/{instance_id}/restart",
        tags = ["instances"],
    }]
    async fn restart_project_instance(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloProjectInstancePath>,
    ) -> Result<HttpResponseOk<Instance>, HttpError>;

    /// List the NICs attached to an instance. Phase 0 produces
    /// exactly one (the auto-created `"primary"`); a future slice
    /// adds NIC attach/detach.
    #[endpoint {
        method = GET,
        path = "/v2/silos/{silo_id}/projects/{project_id}/instances/{instance_id}/nics",
        tags = ["nics"],
    }]
    async fn list_instance_nics(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloProjectInstancePath>,
    ) -> Result<HttpResponseOk<Vec<Nic>>, HttpError>;

    /// Read a single NIC. Returns 404 if the NIC does not exist or
    /// belongs to a different silo, project, or instance.
    #[endpoint {
        method = GET,
        path = "/v2/silos/{silo_id}/projects/{project_id}/instances/{instance_id}/nics/{nic_id}",
        tags = ["nics"],
    }]
    async fn get_instance_nic(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloProjectInstanceNicPath>,
    ) -> Result<HttpResponseOk<Nic>, HttpError>;

    /// List the Disks attached to an instance. Phase 0 produces
    /// exactly one (the auto-created `"boot"`); future multi-disk
    /// attach lands as a follow-on slice.
    #[endpoint {
        method = GET,
        path = "/v2/silos/{silo_id}/projects/{project_id}/instances/{instance_id}/disks",
        tags = ["disks"],
    }]
    async fn list_instance_disks(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloProjectInstancePath>,
    ) -> Result<HttpResponseOk<Vec<Disk>>, HttpError>;

    /// Read a single Disk. Returns 404 if the Disk does not exist
    /// or belongs to a different silo, project, or instance.
    #[endpoint {
        method = GET,
        path = "/v2/silos/{silo_id}/projects/{project_id}/instances/{instance_id}/disks/{disk_id}",
        tags = ["disks"],
    }]
    async fn get_instance_disk(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloProjectInstanceDiskPath>,
    ) -> Result<HttpResponseOk<Disk>, HttpError>;

    /// List FloatingIps owned by a project.
    #[endpoint {
        method = GET,
        path = "/v2/silos/{silo_id}/projects/{project_id}/floating-ips",
        tags = ["floating-ips"],
    }]
    async fn list_project_floating_ips(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloProjectPath>,
    ) -> Result<HttpResponseOk<Vec<FloatingIp>>, HttpError>;

    /// Allocate a FloatingIp from the requested family's pool.
    /// Returns 409 if the name is already in use within the
    /// project. Returns 404 if the project does not exist or
    /// belongs to a different silo. The returned FloatingIp
    /// starts unattached.
    #[endpoint {
        method = POST,
        path = "/v2/silos/{silo_id}/projects/{project_id}/floating-ips",
        tags = ["floating-ips"],
    }]
    async fn create_project_floating_ip(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloProjectPath>,
        body: TypedBody<NewFloatingIp>,
    ) -> Result<HttpResponseCreated<FloatingIp>, HttpError>;

    /// Read a single FloatingIp. Returns 404 if the FloatingIp
    /// does not exist or belongs to a different silo or project.
    #[endpoint {
        method = GET,
        path = "/v2/silos/{silo_id}/projects/{project_id}/floating-ips/{floating_ip_id}",
        tags = ["floating-ips"],
    }]
    async fn get_project_floating_ip(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloProjectFloatingIpPath>,
    ) -> Result<HttpResponseOk<FloatingIp>, HttpError>;

    /// Release a FloatingIp back to its pool. Returns 409 if the
    /// FloatingIp is currently attached (operator must detach
    /// first).
    #[endpoint {
        method = DELETE,
        path = "/v2/silos/{silo_id}/projects/{project_id}/floating-ips/{floating_ip_id}",
        tags = ["floating-ips"],
    }]
    async fn delete_project_floating_ip(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloProjectFloatingIpPath>,
    ) -> Result<HttpResponseDeleted, HttpError>;

    /// Atomically attach a FloatingIp to a NIC, replacing any
    /// existing attachment. The target NIC must live in the same
    /// silo + project as the FloatingIp; mismatch surfaces as 404.
    #[endpoint {
        method = POST,
        path = "/v2/silos/{silo_id}/projects/{project_id}/floating-ips/{floating_ip_id}/attach",
        tags = ["floating-ips"],
    }]
    async fn attach_project_floating_ip(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloProjectFloatingIpPath>,
        body: TypedBody<AttachFloatingIpRequest>,
    ) -> Result<HttpResponseOk<FloatingIp>, HttpError>;

    /// Detach a FloatingIp from its current NIC. Idempotent — a
    /// detach on an already-detached FloatingIp is a no-op that
    /// returns the current record.
    #[endpoint {
        method = POST,
        path = "/v2/silos/{silo_id}/projects/{project_id}/floating-ips/{floating_ip_id}/detach",
        tags = ["floating-ips"],
    }]
    async fn detach_project_floating_ip(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloProjectFloatingIpPath>,
    ) -> Result<HttpResponseOk<FloatingIp>, HttpError>;
}
