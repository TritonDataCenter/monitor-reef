// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Authentication and authorization for tritond.
//!
//! This module pulls together three things at request time:
//!
//! 1. **Authentication.** The `Authorization: Bearer …` header is
//!    inspected. Tokens beginning with [`tritond_auth::API_KEY_PREFIX`]
//!    are looked up against bcrypt-hashed records in the store; other
//!    tokens are validated as HS256 JWTs against the cluster's
//!    operator signing key.
//! 2. **Principal construction.** Authenticated requests yield an
//!    `Operator` entity carrying an `is_root` attribute drawn from the
//!    user record; unauthenticated requests yield an `Anonymous`
//!    entity.
//! 3. **Authorization.** A Cedar `PolicySet` evaluates the request.
//!    Phase 0e ships a deliberately small embedded bundle: anonymous
//!    callers can hit health, login, and refresh; root operators can
//!    do anything; everything else is denied.
//!
//! When per-silo OIDC and finer-grained policies arrive, the entity
//! model expands but the call shape (`AuthService::authenticate` →
//! `AuthService::authorize`) stays the same.

use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::sync::Arc;

use anyhow::{Context, Result};
use cedar_policy::{
    Authorizer, Context as CedarContext, Decision as CedarDecision, Entities, Entity, EntityUid,
    PolicySet, Request, RestrictedExpression,
};
use chrono::Utc;
use dropshot::{ClientErrorStatusCode, HttpError, RequestContext};
use tracing::warn;
use tritond_audit::Decision as AuditDecision;
use tritond_auth::{
    JwtKey, OidcConfig, OidcVerifier, TokenKind, parse_api_key, peek_issuer, verify,
    verify_api_key_secret,
};
use tritond_store::{ApiKeyScope, Federation, Store, StoreError, User};
use uuid::Uuid;

use crate::audit::AuditService;

/// Cedar policy bundle. `Action::StorageCluster*` are deliberately
/// root-only: a tenant operator who could register a storage cluster
/// could point tritond at a malicious mantad and proxy admin calls
/// back across our trust boundary. Everything not matched here falls
/// through to Cedar's default deny.
const POLICY_BUNDLE: &str = r#"
@id("anonymous-public-actions")
permit(
    principal,
    action in [
        Action::"health",
        Action::"login",
        Action::"refresh",
        Action::"agent_register",
        Action::"agent_register_status",
        Action::"image_list_public",
        Action::"image_get",
        Action::"ssh_key_list_public",
        Action::"ssh_key_get"
    ],
    resource
);

@id("root-operator-allows-all")
permit(
    principal,
    action,
    resource
) when {
    principal has is_root && principal.is_root == true
};

@id("silo-member-allows-silo-scoped-actions")
permit(
    principal,
    action in [
        Action::"ssh_key_list",
        Action::"ssh_key_create",
        Action::"ssh_key_get",
        Action::"ssh_key_delete",
        Action::"image_list",
        Action::"image_create",
        Action::"image_get",
        Action::"image_delete",
        Action::"tenant_list",
        Action::"tenant_create",
        Action::"tenant_get",
        Action::"tenant_delete",
        Action::"meta_set",
        Action::"meta_get",
        Action::"meta_list",
        Action::"meta_delete"
    ],
    resource
) when {
    principal has silo_id &&
    resource has silo_id &&
    principal.silo_id == resource.silo_id
};

@id("tenant-member-allows-tenant-scoped-actions")
permit(
    principal,
    action in [
        Action::"project_list",
        Action::"project_create",
        Action::"project_get",
        Action::"project_delete",
        Action::"vpc_list",
        Action::"vpc_create",
        Action::"vpc_get",
        Action::"vpc_delete",
        Action::"subnet_list",
        Action::"subnet_create",
        Action::"subnet_get",
        Action::"subnet_delete",
        Action::"route_table_list",
        Action::"route_table_create",
        Action::"route_table_get",
        Action::"route_table_delete",
        Action::"route_list",
        Action::"route_create",
        Action::"route_get",
        Action::"route_delete",
        Action::"nat_gateway_list",
        Action::"nat_gateway_create",
        Action::"nat_gateway_get",
        Action::"nat_gateway_delete",
        Action::"quota_set",
        Action::"quota_get",
        Action::"quota_delete",
        Action::"instance_list",
        Action::"instance_create",
        Action::"instance_get",
        Action::"instance_delete",
        Action::"instance_start",
        Action::"instance_stop",
        Action::"instance_restart",
        Action::"instance_console",
        Action::"instance_migrate",
        Action::"nic_list",
        Action::"nic_get",
        Action::"disk_list",
        Action::"disk_get",
        Action::"floating_ip_list",
        Action::"floating_ip_create",
        Action::"floating_ip_get",
        Action::"floating_ip_delete",
        Action::"floating_ip_attach",
        Action::"floating_ip_detach",
        Action::"image_list",
        Action::"image_create",
        Action::"image_get",
        Action::"image_delete",
        Action::"ssh_key_list",
        Action::"ssh_key_create",
        Action::"ssh_key_get",
        Action::"ssh_key_delete",
        Action::"meta_set",
        Action::"meta_get",
        Action::"meta_list",
        Action::"meta_delete"
    ],
    resource
) when {
    principal has tenant_id &&
    resource has tenant_id &&
    principal.tenant_id == resource.tenant_id
};

@id("authenticated-image-global-actions")
permit(
    principal,
    action in [
        Action::"image_list",
        Action::"image_create",
        Action::"image_get",
        Action::"image_delete",
        Action::"ssh_key_list",
        Action::"ssh_key_create",
        Action::"ssh_key_get",
        Action::"ssh_key_delete"
    ],
    resource
) when {
    principal has user_id &&
    !(resource has silo_id) &&
    !(resource has tenant_id)
};

@id("fleet-admin-allows-fleet-actions")
permit(
    principal,
    action in [
        Action::"legacy_cn_list",
        Action::"legacy_cn_get",
        Action::"legacy_vm_list",
        Action::"legacy_vm_get",
        Action::"legacy_alarm_list",
        Action::"config_list",
        Action::"config_get",
        Action::"config_set",
        Action::"config_reset"
    ],
    resource
) when {
    principal has fleet_admin && principal.fleet_admin == true
};
"#;

/// Result of authenticating an inbound request.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum Principal {
    Operator {
        user_id: Uuid,
        /// Bootstrap / cluster-wide root. Bypasses capability checks.
        is_root: bool,
        fleet_admin: bool,
        /// `is_root` skips this check; for non-root callers
        /// `/v1/system/*` endpoints require the matching capability.
        capabilities: std::collections::BTreeSet<tritond_store::Capability>,
        tenant_id: Option<Uuid>,
        /// Cached derivation from `tenant_id` so silo-gated Cedar
        /// rules don't re-fetch the tenant on every request. `None`
        /// here is treated as "no silo membership."
        silo_id: Option<Uuid>,
        scope: Option<ApiKeyScope>,
        /// CN binding from the per-CN API key minted at registration
        /// approval. `/v2/agent/*` handlers must verify this matches
        /// the CN identity the request claims; mismatch is a 403.
        bound_cn: Option<Uuid>,
    },
    Anonymous,
}

/// Errors that can come out of [`AuthService::authenticate`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AuthError {
    /// Backend failure (e.g. FoundationDB unreachable). Surfaced as
    /// 503, not downgraded to anonymous — a partial outage should
    /// not silently de-authenticate every caller.
    #[error("auth backend unavailable: {0}")]
    Backend(StoreError),
}

impl From<AuthError> for HttpError {
    fn from(err: AuthError) -> Self {
        match err {
            AuthError::Backend(inner) => {
                HttpError::for_internal_error(format!("auth backend unavailable: {inner}"))
            }
        }
    }
}

impl Principal {
    /// Cedar entity uid for this principal, e.g. `Operator::"<uuid>"`.
    fn entity_uid(&self) -> Result<EntityUid> {
        let raw = match self {
            Principal::Operator { user_id, .. } => format!("Operator::\"{user_id}\""),
            Principal::Anonymous => "Anonymous::\"anon\"".to_string(),
        };
        EntityUid::from_str(&raw).context("constructing principal entity uid")
    }

    /// Cedar entity carrying the principal's attributes (`is_root`
    /// for bootstrap-style accounts; `silo_id` and `tenant_id` for
    /// scoped ones). Both `silo_id` and `tenant_id` are emitted
    /// when present so the silo-gating rules carried over from
    /// E-1/E-2 keep firing while future tenant-scoped rules can
    /// also read `principal.tenant_id`.
    fn entity(&self) -> Result<Entity> {
        let uid = self.entity_uid()?;
        let mut attrs: HashMap<String, RestrictedExpression> = HashMap::new();
        if let Principal::Operator {
            user_id,
            is_root,
            fleet_admin,
            silo_id,
            tenant_id,
            ..
        } = self
        {
            attrs.insert(
                "is_root".to_string(),
                RestrictedExpression::new_bool(*is_root),
            );
            attrs.insert(
                "fleet_admin".to_string(),
                RestrictedExpression::new_bool(*fleet_admin),
            );
            // user_id is always present on an authenticated
            // operator; emitting it as an attribute lets Cedar
            // gate user-scoped actions (e.g. /v2/auth/images)
            // on `principal has user_id`.
            attrs.insert(
                "user_id".to_string(),
                RestrictedExpression::new_string(user_id.to_string()),
            );
            if let Some(silo_id) = silo_id {
                attrs.insert(
                    "silo_id".to_string(),
                    RestrictedExpression::new_string(silo_id.to_string()),
                );
            }
            if let Some(tenant_id) = tenant_id {
                attrs.insert(
                    "tenant_id".to_string(),
                    RestrictedExpression::new_string(tenant_id.to_string()),
                );
            }
        }
        Entity::new(uid, attrs, HashSet::new()).context("constructing principal entity")
    }
}

/// Stable identifier for a Cedar action — one entry per endpoint.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub enum Action {
    Health,
    Login,
    Refresh,
    CreateSilo,
    GetSilo,
    CreateApiKey,
    ListApiKeys,
    DeleteApiKey,
    AuditList,
    AuditFetch,
    AuditVerify,
    /// Operator-initiated saga abandon (forces unwind at the current
    /// node). Operator-only by the root-allows-all Cedar rule.
    OperationsAbandon,
    /// Read-only migrations surface. Operator-only at the
    /// fleet-wide list endpoint; per-instance views are gated by
    /// the existing instance-read permission elsewhere.
    MigrationList,
    MigrationGet,
    /// LM-5 — start a live migration via
    /// `POST /v2/instances/{id}/actions/migrate`. Tenant-scoped
    /// (any project member can migrate their own instances);
    /// operator-only sub-actions like `target_server_uuid` force
    /// override gate behind the root-allows-all Cedar rule
    /// because they cross tenant boundaries.
    InstanceMigrate,
    TenantIdpSet,
    TenantIdpGet,
    TenantIdpDelete,
    TenantList,
    TenantCreate,
    TenantGet,
    TenantDelete,
    ProjectList,
    ProjectCreate,
    ProjectGet,
    ProjectDelete,
    VpcList,
    VpcCreate,
    VpcGet,
    VpcDelete,
    SubnetList,
    SubnetCreate,
    SubnetGet,
    SubnetDelete,
    RouteTableList,
    RouteTableCreate,
    RouteTableGet,
    RouteTableDelete,
    RouteList,
    RouteCreate,
    RouteGet,
    RouteDelete,
    NatGatewayList,
    NatGatewayCreate,
    NatGatewayGet,
    NatGatewayDelete,
    FirewallRuleList,
    FirewallRuleCreate,
    FirewallRuleDelete,
    /// List every silo. Used by the admin UI's silo picker.
    SiloList,
    /// γ.1 — DHCP / IPAM read+write actions, scoped per VPC.
    DhcpPoolGet,
    DhcpPoolSet,
    DhcpPoolClear,
    DhcpReservationList,
    DhcpReservationCreate,
    DhcpReservationGet,
    DhcpReservationDelete,
    DhcpLeaseList,
    DhcpLeaseGet,
    DhcpLeaseDelete,
    /// Scope-aware ssh-key list (silo/tenant/project/user URLs).
    /// Gated by per-scope Cedar rules.
    SshKeyList,
    /// Anonymous-allowed Public-ssh-key list (only `/v2/ssh-keys`).
    /// Separate so the anonymous-public-actions Cedar rule
    /// doesn't accidentally permit `/v2/silos/.../ssh-keys` to
    /// unauthenticated probes.
    SshKeyListPublic,
    SshKeyCreate,
    SshKeyGet,
    SshKeyDelete,
    /// Scope-aware image list (silo/tenant/project/user URLs).
    /// Gated by per-scope Cedar rules.
    ImageList,
    /// Anonymous-allowed Public-image list (only `/v2/images`).
    /// Separate so the anonymous-public-actions Cedar rule
    /// doesn't accidentally permit `/v2/silos/.../images` to
    /// unauthenticated probes.
    ImageListPublic,
    ImageCreate,
    ImageGet,
    ImageDelete,
    QuotaSet,
    QuotaGet,
    QuotaDelete,
    InstanceList,
    InstanceCreate,
    InstanceGet,
    InstanceDelete,
    InstanceStart,
    InstanceStop,
    InstanceRestart,
    /// Open a browser-facing serial / VNC console to a managed
    /// instance. Granted to the same principals that can
    /// start/stop the instance — opening a console is an
    /// interactive state-touching action, not a passive read.
    InstanceConsole,
    NicList,
    NicGet,
    DiskList,
    DiskGet,
    FloatingIpList,
    FloatingIpCreate,
    FloatingIpGet,
    FloatingIpDelete,
    FloatingIpAttach,
    FloatingIpDetach,
    /// Pull the next Pending [`ProvisioningJob`] from the queue.
    /// Fleet-scoped (no silo); the agent identifies itself as
    /// `claimed_by` so concurrent agents can be told apart in the
    /// audit log.
    AgentClaim,
    /// Mark a previously-claimed [`ProvisioningJob`] as terminal.
    /// Cedar gates the action; the store layer verifies the
    /// outcome's transitions are legal.
    AgentComplete,
    /// Read the materialised blueprint for a claimed job —
    /// instance + image + NICs + disks + ssh public keys, in one
    /// response. The Agent scope is the *only* path to this
    /// data: it does not require silo-scoped tenant reads.
    AgentBlueprint,
    /// Lightweight liveness ping from a bound agent. Bumps
    /// `Cn.last_seen`. Body-less.
    AgentHeartbeat,
    /// Full agent status sample (vms / zpools / meminfo / etc.).
    /// Replaces `Cn.last_status` + bumps `last_seen`.
    AgentStatus,
    /// Per-resource network realization row reported by a bound
    /// agent after it accepts, applies, or fails a dataplane
    /// generation.
    NetworkRealizationReport,
    /// Batch of DHCP request activity observed by the kmod, forwarded
    /// by a bound agent. Refreshes the lease record's renewal clock.
    DhcpLeaseActivityReport,
    /// Anonymous self-registration of a compute node. Gated by
    /// the per-source-IP rate limiter, not by Cedar credentials —
    /// the agent has no key at this point in its lifecycle.
    AgentRegister,
    /// Anonymous long-poll for the per-CN API key after
    /// registration. Authenticated only by holding the
    /// `poll_token` returned at registration; rate-limited per IP.
    AgentRegisterStatus,
    /// List CN registrations. Operator surface (root-only today;
    /// no silo dimension).
    CnList,
    /// Read a single CN record.
    CnGet,
    /// Approve a Pending CN by claim code. Mints the per-CN bound
    /// API key.
    CnApprove,
    /// Disable a CN; revokes the bound key.
    CnDisable,
    /// Set a CN's operator-controlled placement role.
    CnSetRole,
    /// Read the current auto-approve window state.
    AutoApproveGet,
    /// Open (or replace) the auto-approve window.
    AutoApproveSet,
    /// Close the auto-approve window early.
    AutoApproveClear,
    /// Cluster-wide configuration surface (`/v2/config`). Gated by
    /// the fleet-admin Cedar rule (and root-allows-all). `*List`/
    /// `*Get` are reads; `*Set`/`*Reset` are writes (so a ReadOnly
    /// API key can inspect but not change cluster settings).
    ConfigList,
    ConfigGet,
    ConfigSet,
    ConfigReset,
    /// Fleet-scoped legacy-discovery surface
    /// (`/v2/admin/legacy/*`). Gated by the fleet-admin Cedar rule.
    LegacyCnList,
    LegacyCnGet,
    LegacyVmList,
    LegacyVmGet,
    LegacyAlarmList,
    /// Operator-only storage-cluster registration surface
    /// (`/v2/storage/clusters`). Gated by the root-allows-all rule —
    /// see the `POLICY_BUNDLE` doc-comment for why these never get
    /// a per-silo or fleet-admin variant. Per-forwarder action
    /// variants (StorageClusterSummary, StorageNodes, etc.) ship
    /// in Stage 3.5 once the proxy endpoints land.
    StorageClusterList,
    StorageClusterCreate,
    StorageClusterGet,
    StorageClusterDelete,
    /// Trigger an out-of-band health probe against a registered
    /// cluster's `/admin/v1/cluster` summary. Mutates `status` and
    /// `last_observed_at` on the StorageCluster record, so it's
    /// classified as a write for ReadOnly-scope purposes.
    StorageClusterHealthProbe,
    // ----- Storage cluster forwarders (mantad /admin/v1/* proxy) -----
    //
    // Each forwarder gets its own Action so the audit log can answer
    // "who created bucket X / deleted access key Y" without parsing
    // request bodies. All gated by the existing root-allows-all
    // Cedar rule — see the doc-comment on `POLICY_BUNDLE` for why
    // these never get a per-silo or fleet-admin variant.
    StorageClusterSummary,
    StorageNodeList,
    StorageNodeGet,
    StorageNodeAdd,
    StorageNodeRemove,
    StorageNodeDrain,
    StorageNodeUndrain,
    StorageNodeReweight,
    StorageMembershipGet,
    StorageBucketList,
    StorageBucketGet,
    StorageBucketCreate,
    StorageBucketDelete,
    StorageObjectList,
    StorageUserList,
    StorageUserGet,
    StorageUserCreate,
    StorageUserDelete,
    StorageAccessKeyList,
    StorageAccessKeyCreate,
    StorageAccessKeyDelete,
    StorageUserPolicyList,
    StorageUserPolicyGet,
    StorageUserPolicyPut,
    StorageUserPolicyDelete,
    /// Configure (or rotate) the per-cluster IAM presigner credential
    /// tritond signs presigned S3 URLs with. Distinct from the bucket
    /// `Create*`/`Delete*` actions because rotating the signer
    /// credential is a registration-level concern.
    StorageClusterSetPresigner,
    /// Mint a single-shot presigned PUT URL for a bucket/key. The URL
    /// grants the holder the ability to write that object, so this
    /// is classified as a *write* (a ReadOnly tritond key cannot
    /// mint URLs that would mutate cluster state).
    StorageObjectPresignPut,
    /// Mint a single-shot presigned GET URL for a bucket/key. The URL
    /// grants read access, so this is a *read* (audit-friendly: a
    /// shared download link gets logged at mint time).
    StorageObjectPresignGet,
    /// Initiate a multipart upload (`POST ?uploads`). Returns the
    /// upload id needed by every subsequent presigned-part URL.
    StorageObjectMultipartInit,
    /// Mint per-part presigned PUT URLs against an active upload id.
    StorageObjectMultipartParts,
    /// Complete a multipart upload (`POST ?uploadId=...`); commits
    /// the assembled object atomically.
    StorageObjectMultipartComplete,
    /// Abort a multipart upload (`DELETE ?uploadId=...`). mantad will
    /// garbage-collect the parts.
    StorageObjectMultipartAbort,
    // ---- Layered instance metadata (IMDS) ----
    /// `set_meta` -- upsert one metadata entry at any of the four scopes.
    MetaSet,
    /// `get_meta` -- read one metadata entry.
    MetaGet,
    /// `list_meta` -- list a scope's whole `key`->`MetaValue` map.
    MetaList,
    /// `delete_meta` -- remove one metadata entry.
    MetaDelete,
}

impl Action {
    /// Stable string identifier used in Cedar policies and audit
    /// events. Public so the audit emitter can name the action it
    /// just gated on without redoing the match.
    #[must_use]
    pub fn cedar_id(self) -> &'static str {
        match self {
            Action::Health => "health",
            Action::Login => "login",
            Action::Refresh => "refresh",
            Action::CreateSilo => "create_silo",
            Action::GetSilo => "get_silo",
            Action::CreateApiKey => "create_api_key",
            Action::ListApiKeys => "list_api_keys",
            Action::DeleteApiKey => "delete_api_key",
            Action::AuditList => "audit_list",
            Action::AuditFetch => "audit_fetch",
            Action::AuditVerify => "audit_verify",
            Action::OperationsAbandon => "operations_abandon",
            Action::MigrationList => "migration_list",
            Action::MigrationGet => "migration_get",
            Action::InstanceMigrate => "instance_migrate",
            Action::TenantIdpSet => "tenant_idp_set",
            Action::TenantIdpGet => "tenant_idp_get",
            Action::TenantIdpDelete => "tenant_idp_delete",
            Action::TenantList => "tenant_list",
            Action::TenantCreate => "tenant_create",
            Action::TenantGet => "tenant_get",
            Action::TenantDelete => "tenant_delete",
            Action::ProjectList => "project_list",
            Action::ProjectCreate => "project_create",
            Action::ProjectGet => "project_get",
            Action::ProjectDelete => "project_delete",
            Action::VpcList => "vpc_list",
            Action::VpcCreate => "vpc_create",
            Action::VpcGet => "vpc_get",
            Action::VpcDelete => "vpc_delete",
            Action::SubnetList => "subnet_list",
            Action::SubnetCreate => "subnet_create",
            Action::SubnetGet => "subnet_get",
            Action::SubnetDelete => "subnet_delete",
            Action::RouteTableList => "route_table_list",
            Action::RouteTableCreate => "route_table_create",
            Action::RouteTableGet => "route_table_get",
            Action::RouteTableDelete => "route_table_delete",
            Action::RouteList => "route_list",
            Action::RouteCreate => "route_create",
            Action::RouteGet => "route_get",
            Action::RouteDelete => "route_delete",
            Action::NatGatewayList => "nat_gateway_list",
            Action::NatGatewayCreate => "nat_gateway_create",
            Action::NatGatewayGet => "nat_gateway_get",
            Action::NatGatewayDelete => "nat_gateway_delete",
            Action::FirewallRuleList => "firewall_rule_list",
            Action::FirewallRuleCreate => "firewall_rule_create",
            Action::FirewallRuleDelete => "firewall_rule_delete",
            Action::SiloList => "silo_list",
            Action::DhcpPoolGet => "dhcp_pool_get",
            Action::DhcpPoolSet => "dhcp_pool_set",
            Action::DhcpPoolClear => "dhcp_pool_clear",
            Action::DhcpReservationList => "dhcp_reservation_list",
            Action::DhcpReservationCreate => "dhcp_reservation_create",
            Action::DhcpReservationGet => "dhcp_reservation_get",
            Action::DhcpReservationDelete => "dhcp_reservation_delete",
            Action::DhcpLeaseList => "dhcp_lease_list",
            Action::DhcpLeaseGet => "dhcp_lease_get",
            Action::DhcpLeaseDelete => "dhcp_lease_delete",
            Action::SshKeyList => "ssh_key_list",
            Action::SshKeyListPublic => "ssh_key_list_public",
            Action::SshKeyCreate => "ssh_key_create",
            Action::SshKeyGet => "ssh_key_get",
            Action::SshKeyDelete => "ssh_key_delete",
            Action::ImageList => "image_list",
            Action::ImageListPublic => "image_list_public",
            Action::ImageCreate => "image_create",
            Action::ImageGet => "image_get",
            Action::ImageDelete => "image_delete",
            Action::QuotaSet => "quota_set",
            Action::QuotaGet => "quota_get",
            Action::QuotaDelete => "quota_delete",
            Action::InstanceList => "instance_list",
            Action::InstanceCreate => "instance_create",
            Action::InstanceGet => "instance_get",
            Action::InstanceDelete => "instance_delete",
            Action::InstanceStart => "instance_start",
            Action::InstanceStop => "instance_stop",
            Action::InstanceRestart => "instance_restart",
            Action::InstanceConsole => "instance_console",
            Action::NicList => "nic_list",
            Action::NicGet => "nic_get",
            Action::DiskList => "disk_list",
            Action::DiskGet => "disk_get",
            Action::FloatingIpList => "floating_ip_list",
            Action::FloatingIpCreate => "floating_ip_create",
            Action::FloatingIpGet => "floating_ip_get",
            Action::FloatingIpDelete => "floating_ip_delete",
            Action::FloatingIpAttach => "floating_ip_attach",
            Action::FloatingIpDetach => "floating_ip_detach",
            Action::AgentClaim => "agent_claim",
            Action::AgentComplete => "agent_complete",
            Action::AgentBlueprint => "agent_blueprint",
            Action::AgentHeartbeat => "agent_heartbeat",
            Action::AgentStatus => "agent_status",
            Action::NetworkRealizationReport => "network_realization_report",
            Action::DhcpLeaseActivityReport => "dhcp_lease_activity_report",
            Action::AgentRegister => "agent_register",
            Action::AgentRegisterStatus => "agent_register_status",
            Action::CnList => "cn_list",
            Action::CnGet => "cn_get",
            Action::CnApprove => "cn_approve",
            Action::CnDisable => "cn_disable",
            Action::CnSetRole => "cn_set_role",
            Action::AutoApproveGet => "auto_approve_get",
            Action::AutoApproveSet => "auto_approve_set",
            Action::AutoApproveClear => "auto_approve_clear",
            Action::ConfigList => "config_list",
            Action::ConfigGet => "config_get",
            Action::ConfigSet => "config_set",
            Action::ConfigReset => "config_reset",
            Action::LegacyCnList => "legacy_cn_list",
            Action::LegacyCnGet => "legacy_cn_get",
            Action::LegacyVmList => "legacy_vm_list",
            Action::LegacyVmGet => "legacy_vm_get",
            Action::LegacyAlarmList => "legacy_alarm_list",
            Action::StorageClusterList => "storage_cluster_list",
            Action::StorageClusterCreate => "storage_cluster_create",
            Action::StorageClusterGet => "storage_cluster_get",
            Action::StorageClusterDelete => "storage_cluster_delete",
            Action::StorageClusterHealthProbe => "storage_cluster_health_probe",
            Action::StorageClusterSummary => "storage_cluster_summary",
            Action::StorageNodeList => "storage_node_list",
            Action::StorageNodeGet => "storage_node_get",
            Action::StorageNodeAdd => "storage_node_add",
            Action::StorageNodeRemove => "storage_node_remove",
            Action::StorageNodeDrain => "storage_node_drain",
            Action::StorageNodeUndrain => "storage_node_undrain",
            Action::StorageNodeReweight => "storage_node_reweight",
            Action::StorageMembershipGet => "storage_membership_get",
            Action::StorageBucketList => "storage_bucket_list",
            Action::StorageBucketGet => "storage_bucket_get",
            Action::StorageBucketCreate => "storage_bucket_create",
            Action::StorageBucketDelete => "storage_bucket_delete",
            Action::StorageObjectList => "storage_object_list",
            Action::StorageUserList => "storage_user_list",
            Action::StorageUserGet => "storage_user_get",
            Action::StorageUserCreate => "storage_user_create",
            Action::StorageUserDelete => "storage_user_delete",
            Action::StorageAccessKeyList => "storage_access_key_list",
            Action::StorageAccessKeyCreate => "storage_access_key_create",
            Action::StorageAccessKeyDelete => "storage_access_key_delete",
            Action::StorageUserPolicyList => "storage_user_policy_list",
            Action::StorageUserPolicyGet => "storage_user_policy_get",
            Action::StorageUserPolicyPut => "storage_user_policy_put",
            Action::StorageUserPolicyDelete => "storage_user_policy_delete",
            Action::StorageClusterSetPresigner => "storage_cluster_set_presigner",
            Action::StorageObjectPresignPut => "storage_object_presign_put",
            Action::StorageObjectPresignGet => "storage_object_presign_get",
            Action::StorageObjectMultipartInit => "storage_object_multipart_init",
            Action::StorageObjectMultipartParts => "storage_object_multipart_parts",
            Action::StorageObjectMultipartComplete => "storage_object_multipart_complete",
            Action::StorageObjectMultipartAbort => "storage_object_multipart_abort",
            Action::MetaSet => "meta_set",
            Action::MetaGet => "meta_get",
            Action::MetaList => "meta_list",
            Action::MetaDelete => "meta_delete",
        }
    }

    fn entity_uid(self) -> Result<EntityUid> {
        EntityUid::from_str(&format!("Action::\"{}\"", self.cedar_id()))
            .context("constructing action entity uid")
    }
}

/// Per-cluster auth service: holds the JWT signing key, the parsed
/// Cedar policy set, the Cedar `Authorizer`, and the OIDC verifier
/// shared across silos (cheap to reuse across requests).
pub struct AuthService {
    jwt_key: JwtKey,
    policy_set: PolicySet,
    authorizer: Authorizer,
    oidc: OidcVerifier,
}

impl AuthService {
    pub fn new(jwt_key: JwtKey) -> Result<Self> {
        let policy_set = PolicySet::from_str(POLICY_BUNDLE)
            .map_err(|e| anyhow::anyhow!("parse Cedar policy bundle: {e}"))?;
        Ok(Self {
            jwt_key,
            policy_set,
            authorizer: Authorizer::new(),
            oidc: OidcVerifier::new(),
        })
    }

    pub fn jwt_key(&self) -> &JwtKey {
        &self.jwt_key
    }

    pub fn oidc(&self) -> &OidcVerifier {
        &self.oidc
    }

    /// Authenticate the inbound request.
    ///
    /// Returns:
    /// * [`Principal::Operator`] on a valid credential.
    /// * [`Principal::Anonymous`] on missing, malformed, expired, or
    ///   unknown credentials — anything that points at the user
    ///   rather than the system.
    /// * [`AuthError::Backend`] when the store itself fails. The
    ///   caller maps this to a 5xx so a half-broken cluster does not
    ///   silently deauthenticate every caller as 403.
    pub async fn authenticate(
        &self,
        store: &dyn Store,
        bearer: Option<&str>,
    ) -> Result<Principal, AuthError> {
        let Some(token) = bearer else {
            return Ok(Principal::Anonymous);
        };

        if token.starts_with(tritond_auth::API_KEY_PREFIX) {
            self.authenticate_api_key(store, token).await
        } else {
            self.authenticate_jwt(store, token).await
        }
    }

    async fn authenticate_jwt(
        &self,
        store: &dyn Store,
        token: &str,
    ) -> Result<Principal, AuthError> {
        // Operator tokens (HS256, our signing key) come first; if the
        // token isn't one of ours, fall through to OIDC against
        // configured silo IdPs.
        match verify(&self.jwt_key, token, TokenKind::Access) {
            Ok(claims) => match store.get_user_by_id(claims.sub).await {
                Ok(user) => {
                    let silo_id = derive_silo_id(store, &user).await?;
                    Ok(Principal::Operator {
                        user_id: user.id,
                        is_root: user.is_root,
                        fleet_admin: user.fleet_admin,
                        capabilities: user.capabilities.clone(),
                        tenant_id: user.tenant_id,
                        silo_id,
                        // JWT-authenticated principals carry the user's
                        // full permissions; scope only applies to API keys.
                        scope: None,
                        bound_cn: None,
                    })
                }
                Err(StoreError::NotFound) => Ok(Principal::Anonymous),
                Err(e) => {
                    warn!(error = %e, "store failure while resolving JWT principal");
                    Err(AuthError::Backend(e))
                }
            },
            Err(_) => self.authenticate_oidc(store, token).await,
        }
    }

    async fn authenticate_oidc(
        &self,
        store: &dyn Store,
        token: &str,
    ) -> Result<Principal, AuthError> {
        // Cheaply peek at the `iss` claim so we can route the
        // token to its owning tenant via the IdP's `issuer→tenant`
        // index. A token without a parseable `iss` is just
        // anonymous; same goes for one whose issuer doesn't match
        // any configured tenant.
        let Some(issuer) = peek_issuer(token) else {
            return Ok(Principal::Anonymous);
        };
        let (tenant_id, idp) = match store.get_idp_config_by_issuer(&issuer).await {
            Ok(pair) => pair,
            Err(StoreError::NotFound) => return Ok(Principal::Anonymous),
            Err(e) => {
                warn!(error = %e, "store failure resolving idp by issuer");
                return Err(AuthError::Backend(e));
            }
        };

        let oidc_cfg = OidcConfig {
            issuer_url: idp.issuer_url,
            client_id: idp.client_id,
            client_secret: idp.client_secret,
            audience: idp.audience,
        };
        // The OIDC verifier caches discovery + JWKS per cache key;
        // tenant_id is the right granularity now that IdPs are
        // tenant-scoped.
        let cache_key = tenant_id.to_string();
        let claims = match self.oidc.verify(&cache_key, &oidc_cfg, token).await {
            Ok(c) => c,
            Err(e) => {
                tracing::debug!(error = %e, %tenant_id, "rejecting oidc token as anonymous");
                return Ok(Principal::Anonymous);
            }
        };

        // JIT user lookup or create for this (tenant, issuer,
        // subject). Federated users land in the tenant whose IdP
        // authenticated them — no more silo-default-tenant
        // routing. The tenant must exist (we just looked up its
        // IdP), so we don't re-read it here.
        let user = match store
            .get_user_by_federation(tenant_id, &claims.issuer, &claims.subject)
            .await
        {
            Ok(user) => user,
            Err(StoreError::NotFound) => {
                let new_user = User {
                    id: Uuid::new_v4(),
                    // Disambiguate username across tenants so a
                    // user with the same email in two tenants
                    // doesn't collide on the global username
                    // uniqueness key.
                    username: format!("{}@{tenant_id}", claims.username),
                    password_hash: String::new(),
                    is_root: false,
                    fleet_admin: false,
                    created_at: Utc::now(),
                    tenant_id: Some(tenant_id),
                    federation: Some(Federation {
                        issuer: claims.issuer.clone(),
                        subject: claims.subject.clone(),
                    }),
                    // Federated users land with no operator
                    // capabilities by default. The customer surface
                    // does not require any capabilities; the operator
                    // surface (`/v1/system/`) is unreachable until an
                    // operator explicitly grants them. Per RFD 00007
                    // D-Ap-13.
                    capabilities: Default::default(),
                };
                match store.create_user(new_user).await {
                    Ok(u) => u,
                    Err(StoreError::Conflict(_)) => {
                        // A concurrent first login won the race. Re-read.
                        store
                            .get_user_by_federation(tenant_id, &claims.issuer, &claims.subject)
                            .await
                            .map_err(|e| {
                                warn!(error = %e, "post-conflict refetch failed");
                                AuthError::Backend(e)
                            })?
                    }
                    Err(e) => {
                        warn!(error = %e, "JIT create_user failed");
                        return Err(AuthError::Backend(e));
                    }
                }
            }
            Err(e) => {
                warn!(error = %e, "store failure resolving federated user");
                return Err(AuthError::Backend(e));
            }
        };

        let derived_silo_id = derive_silo_id(store, &user).await?;
        Ok(Principal::Operator {
            user_id: user.id,
            is_root: user.is_root,
            fleet_admin: user.fleet_admin,
            capabilities: user.capabilities.clone(),
            tenant_id: user.tenant_id,
            silo_id: derived_silo_id,
            // OIDC-authenticated principals carry the user's full
            // permissions; scope only applies to API keys.
            scope: None,
            bound_cn: None,
        })
    }

    async fn authenticate_api_key(
        &self,
        store: &dyn Store,
        token: &str,
    ) -> Result<Principal, AuthError> {
        let Some((lookup_id, secret)) = parse_api_key(token) else {
            return Ok(Principal::Anonymous);
        };
        let record = match store.get_api_key_by_lookup_id(lookup_id).await {
            Ok(record) => record,
            Err(StoreError::NotFound) => return Ok(Principal::Anonymous),
            Err(e) => {
                warn!(error = %e, "store failure while resolving api key by lookup id");
                return Err(AuthError::Backend(e));
            }
        };
        let verified = match verify_api_key_secret(secret, &record.hash).await {
            Ok(b) => b,
            Err(e) => {
                warn!(error = %e, "bcrypt failure while verifying api key");
                return Ok(Principal::Anonymous);
            }
        };
        if !verified {
            return Ok(Principal::Anonymous);
        }
        match store.get_user_by_id(record.user_id).await {
            Ok(user) => {
                let silo_id = derive_silo_id(store, &user).await?;
                Ok(Principal::Operator {
                    user_id: user.id,
                    is_root: user.is_root,
                    fleet_admin: user.fleet_admin,
                    capabilities: user.capabilities.clone(),
                    tenant_id: user.tenant_id,
                    silo_id,
                    // The API key's scope rides along on the principal so
                    // [`AuthService::authorize`] can gate per-action without
                    // a second store round-trip. `Full` falls through as
                    // "no extra restriction" — see [`scope_allows_action`].
                    scope: Some(record.scope),
                    // Per-CN binding (set by the registration approval
                    // flow). Handlers under `/v2/agent/*` enforce that
                    // the request's CN identity matches this value.
                    bound_cn: record.bound_to_cn,
                })
            }
            Err(StoreError::NotFound) => Ok(Principal::Anonymous),
            Err(e) => {
                warn!(error = %e, "store failure while resolving api-key user");
                Err(AuthError::Backend(e))
            }
        }
    }

    /// Evaluate the embedded Cedar policy against `System::"global"`.
    /// Returns `Ok(())` on permit, `Err(403)` on deny. Used for
    /// fleet-scoped actions (no silo in the URL path).
    ///
    /// API-key scope is checked *before* Cedar so a scoped key that
    /// can never authorise the requested action is rejected without
    /// loading the resource graph. The error shape matches Cedar
    /// deny for the same action so callers can't distinguish
    /// scope-deny from policy-deny via timing or status code.
    pub fn authorize(&self, principal: &Principal, action: Action) -> Result<(), HttpError> {
        if !principal_scope_allows(principal, action) {
            return Err(forbidden_for(action));
        }
        let resource_uid = EntityUid::from_str("System::\"global\"")
            .map_err(|e| HttpError::for_internal_error(format!("resource uid: {e}")))?;
        let resource_entity = Entity::new(resource_uid.clone(), HashMap::new(), HashSet::new())
            .map_err(|e| HttpError::for_internal_error(format!("resource entity: {e}")))?;
        match self.evaluate(principal, action, resource_uid, resource_entity)? {
            CedarDecision::Allow => Ok(()),
            CedarDecision::Deny => Err(forbidden_for(action)),
        }
    }

    /// Evaluate the policy against a `Silo::"<silo_id>"` resource
    /// carrying a `silo_id` attribute, so the silo-membership rule
    /// can fire. Returns `Ok(())` on permit; on deny, returns **404
    /// Not Found** rather than 403 — a federated user from silo A
    /// hitting silo B's resources should not be able to learn that
    /// silo B exists.
    ///
    /// API-key scope is checked first; scope-deny on a silo-scoped
    /// action returns 404 (matching cross-tenant deny) so a scoped
    /// key can't be used to enumerate silos by attempting actions.
    pub fn authorize_in_silo(
        &self,
        principal: &Principal,
        action: Action,
        silo_id: Uuid,
    ) -> Result<(), HttpError> {
        if !principal_scope_allows(principal, action) {
            return Err(not_found_in_silo());
        }
        let resource_uid = EntityUid::from_str(&format!("Silo::\"{silo_id}\""))
            .map_err(|e| HttpError::for_internal_error(format!("silo resource uid: {e}")))?;
        let mut attrs = HashMap::new();
        attrs.insert(
            "silo_id".to_string(),
            RestrictedExpression::new_string(silo_id.to_string()),
        );
        let resource_entity = Entity::new(resource_uid.clone(), attrs, HashSet::new())
            .map_err(|e| HttpError::for_internal_error(format!("silo resource entity: {e}")))?;
        match self.evaluate(principal, action, resource_uid, resource_entity)? {
            CedarDecision::Allow => Ok(()),
            CedarDecision::Deny => Err(not_found_in_silo()),
        }
    }

    /// Evaluate the policy against a `Tenant::"<tenant_id>"`
    /// resource carrying a `tenant_id` attribute, so the
    /// tenant-membership rule can fire. Returns `Ok(())` on
    /// permit; on deny, returns **404 Not Found** rather than 403
    /// — a tenant member hitting another tenant's resources
    /// should not learn that the other tenant exists.
    ///
    /// The cross-tenant 404 invariant is strictly stricter than
    /// the cross-silo invariant: a request gated here will refuse
    /// to confirm the target tenant's existence even when the
    /// caller and the target live in the same silo.
    pub fn authorize_in_tenant(
        &self,
        principal: &Principal,
        action: Action,
        tenant_id: Uuid,
    ) -> Result<(), HttpError> {
        if !principal_scope_allows(principal, action) {
            return Err(not_found_in_tenant());
        }
        let resource_uid = EntityUid::from_str(&format!("Tenant::\"{tenant_id}\""))
            .map_err(|e| HttpError::for_internal_error(format!("tenant resource uid: {e}")))?;
        let mut attrs = HashMap::new();
        attrs.insert(
            "tenant_id".to_string(),
            RestrictedExpression::new_string(tenant_id.to_string()),
        );
        let resource_entity = Entity::new(resource_uid.clone(), attrs, HashSet::new())
            .map_err(|e| HttpError::for_internal_error(format!("tenant resource entity: {e}")))?;
        match self.evaluate(principal, action, resource_uid, resource_entity)? {
            CedarDecision::Allow => Ok(()),
            CedarDecision::Deny => Err(not_found_in_tenant()),
        }
    }

    fn evaluate(
        &self,
        principal: &Principal,
        action: Action,
        resource_uid: EntityUid,
        resource_entity: Entity,
    ) -> Result<CedarDecision, HttpError> {
        let principal_uid = principal
            .entity_uid()
            .map_err(|e| HttpError::for_internal_error(e.to_string()))?;
        let action_uid = action
            .entity_uid()
            .map_err(|e| HttpError::for_internal_error(e.to_string()))?;
        let principal_entity = principal
            .entity()
            .map_err(|e| HttpError::for_internal_error(e.to_string()))?;
        let entities = Entities::from_entities([principal_entity, resource_entity], None)
            .map_err(|e| HttpError::for_internal_error(format!("entities: {e}")))?;
        let request = Request::new(
            principal_uid,
            action_uid,
            resource_uid,
            CedarContext::empty(),
            None,
        )
        .map_err(|e| HttpError::for_internal_error(format!("cedar request: {e}")))?;
        let response = self
            .authorizer
            .is_authorized(&request, &self.policy_set, &entities);
        Ok(response.decision())
    }
}

/// Resolve the owning silo for a user by looking up their tenant.
///
/// * Returns `Ok(None)` for cluster-wide accounts (root operator) —
///   `user.tenant_id` is `None`, so there is no silo to derive.
/// * Returns `Ok(Some(silo_id))` for tenant-scoped users when the
///   tenant lookup succeeds.
/// * Returns `Ok(None)` (with a logged warning) if `user.tenant_id`
///   is `Some` but the tenant is missing — defensive against an
///   orphaned user row that can't actually happen during normal
///   operation. Treating it as "no silo membership" denies any
///   silo-scoped action, which is the safe default.
/// * Returns [`AuthError::Backend`] on any other store failure so
///   the caller surfaces a 5xx instead of silently downgrading.
async fn derive_silo_id(store: &dyn Store, user: &User) -> Result<Option<Uuid>, AuthError> {
    let Some(tenant_id) = user.tenant_id else {
        return Ok(None);
    };
    match store.get_tenant(tenant_id).await {
        Ok(tenant) => Ok(Some(tenant.silo_id)),
        Err(StoreError::NotFound) => {
            warn!(
                user_id = %user.id,
                %tenant_id,
                "user references missing tenant; silo membership cannot be derived"
            );
            Ok(None)
        }
        Err(e) => {
            warn!(error = %e, "store failure while resolving user tenant");
            Err(AuthError::Backend(e))
        }
    }
}

/// `true` if the principal's API-key scope (if any) permits this
/// action. Anonymous principals and JWT/OIDC operators (no scope)
/// always pass — the rest of the gate falls to Cedar.
fn principal_scope_allows(principal: &Principal, action: Action) -> bool {
    match principal {
        Principal::Operator {
            scope: Some(scope), ..
        } => scope_allows_action(*scope, action),
        Principal::Operator { scope: None, .. } | Principal::Anonymous => true,
    }
}

/// Map an [`ApiKeyScope`] to the set of actions it permits.
///
/// The match on `Action` is deliberately exhaustive (no `_` arm)
/// so adding a new action elsewhere in the codebase is a compile
/// error here until someone classifies it as read or write. That
/// fail-loud behaviour is the point: a scoped key must never
/// silently inherit permissions for a freshly-added action.
///
/// `ApiKeyScope` itself is `#[non_exhaustive]` (defined in the
/// `tritond-store` crate), so we fall through with `_ => false` —
/// any scope variant we haven't classified here denies all actions
/// until it's explicitly handled. Fail-safe by default.
fn scope_allows_action(scope: ApiKeyScope, action: Action) -> bool {
    match scope {
        ApiKeyScope::Full => true,
        ApiKeyScope::ReadOnly => is_read_action(action),
        ApiKeyScope::AuditOnly => matches!(
            action,
            Action::Health
                | Action::Login
                | Action::Refresh
                | Action::AuditList
                | Action::AuditFetch
                | Action::AuditVerify
        ),
        ApiKeyScope::Agent => matches!(
            action,
            Action::Health
                | Action::AgentClaim
                | Action::AgentComplete
                | Action::AgentBlueprint
                | Action::AgentHeartbeat
                | Action::AgentStatus
                | Action::NetworkRealizationReport
                | Action::DhcpLeaseActivityReport
        ),
        _ => false,
    }
}

/// Classify an [`Action`] as read or write. Read = list/get +
/// the audit chain reads + the public-flow actions (login,
/// refresh, health). Anything else is a write.
fn is_read_action(action: Action) -> bool {
    match action {
        // Public / always-allowed at auth layer.
        Action::Health | Action::Login | Action::Refresh => true,
        // Read-only fleet & per-silo metadata.
        Action::GetSilo
        | Action::SiloList
        | Action::ListApiKeys
        | Action::AuditList
        | Action::AuditFetch
        | Action::AuditVerify
        | Action::TenantIdpGet
        | Action::TenantList
        | Action::TenantGet => true,
        // Read-only project & workload resources.
        Action::ProjectList
        | Action::ProjectGet
        | Action::VpcList
        | Action::VpcGet
        | Action::SubnetList
        | Action::SubnetGet
        | Action::RouteTableList
        | Action::RouteTableGet
        | Action::RouteList
        | Action::RouteGet
        | Action::NatGatewayList
        | Action::NatGatewayGet
        | Action::FirewallRuleList
        | Action::DhcpPoolGet
        | Action::DhcpReservationList
        | Action::DhcpReservationGet
        | Action::DhcpLeaseList
        | Action::DhcpLeaseGet
        | Action::SshKeyList
        | Action::SshKeyListPublic
        | Action::SshKeyGet
        | Action::ImageList
        | Action::ImageListPublic
        | Action::ImageGet
        | Action::QuotaGet
        | Action::InstanceList
        | Action::InstanceGet
        | Action::NicList
        | Action::NicGet
        | Action::DiskList
        | Action::DiskGet
        | Action::FloatingIpList
        | Action::FloatingIpGet
        | Action::MigrationList
        | Action::MigrationGet => true,
        // Writes / state changes / admin: explicitly denied.
        Action::CreateSilo
        | Action::CreateApiKey
        | Action::DeleteApiKey
        | Action::TenantIdpSet
        | Action::TenantIdpDelete
        | Action::TenantCreate
        | Action::TenantDelete
        | Action::ProjectCreate
        | Action::ProjectDelete
        | Action::VpcCreate
        | Action::VpcDelete
        | Action::SubnetCreate
        | Action::SubnetDelete
        | Action::RouteTableCreate
        | Action::RouteTableDelete
        | Action::RouteCreate
        | Action::RouteDelete
        | Action::NatGatewayCreate
        | Action::NatGatewayDelete
        | Action::FirewallRuleCreate
        | Action::FirewallRuleDelete
        | Action::DhcpPoolSet
        | Action::DhcpPoolClear
        | Action::DhcpReservationCreate
        | Action::DhcpReservationDelete
        | Action::DhcpLeaseDelete
        | Action::SshKeyCreate
        | Action::SshKeyDelete
        | Action::ImageCreate
        | Action::ImageDelete
        | Action::QuotaSet
        | Action::QuotaDelete
        | Action::InstanceCreate
        | Action::InstanceDelete
        | Action::InstanceStart
        | Action::InstanceStop
        | Action::InstanceRestart
        // Opening a console is an interactive action that touches
        // the guest; a ReadOnly key must not reach it.
        | Action::InstanceConsole
        // Starting a migration is a multi-host write that
        // mutates instance ownership; ReadOnly keys must not
        // reach it.
        | Action::InstanceMigrate
        | Action::FloatingIpCreate
        | Action::FloatingIpDelete
        | Action::FloatingIpAttach
        | Action::FloatingIpDetach
        // Agent actions are queue mutations, agent-only data
        // reads, or per-CN inventory writes; classified as
        // writes so a ReadOnly key can't peek at jobs / blueprints
        // / status. The Agent scope is the only way to authorise
        // them — see `scope_allows_action`.
        | Action::AgentClaim
        | Action::AgentComplete
        | Action::AgentBlueprint
        | Action::AgentHeartbeat
        | Action::AgentStatus
        | Action::NetworkRealizationReport
        | Action::DhcpLeaseActivityReport
        // Agent registration is anonymous (no key), but if a key
        // is somehow attached the scope check should reject it
        // outright — these aren't read actions, they create a CN
        // record / consume a credential.
        | Action::AgentRegister
        | Action::AgentRegisterStatus
        // CN management: writes change CN state; the read fns are
        // operator-only via Cedar.
        | Action::CnApprove
        | Action::CnDisable
        | Action::CnSetRole
        | Action::AutoApproveSet
        | Action::AutoApproveClear
        // Cluster config mutations.
        | Action::ConfigSet
        | Action::ConfigReset
        // Storage cluster mutations: registration writes, deletion,
        // and the health probe (which writes status + last_observed_at).
        | Action::StorageClusterCreate
        | Action::StorageClusterDelete
        | Action::StorageClusterHealthProbe
        // Storage forwarders that mutate cluster-side state. The
        // forwarders proxy to mantad's /admin/v1/* but the *intent*
        // is a write, so a ReadOnly tritond key cannot reach them
        // even though tritond itself only mutates its registry on
        // the health probe.
        | Action::StorageNodeAdd
        | Action::StorageNodeRemove
        | Action::StorageNodeDrain
        | Action::StorageNodeUndrain
        | Action::StorageNodeReweight
        | Action::StorageBucketCreate
        | Action::StorageBucketDelete
        | Action::StorageUserCreate
        | Action::StorageUserDelete
        | Action::StorageAccessKeyCreate
        | Action::StorageAccessKeyDelete
        | Action::StorageUserPolicyPut
        | Action::StorageUserPolicyDelete
        // Presigner config + object mutations: rotating the signing
        // credential, minting a PUT URL, and the multipart lifecycle
        // (init/parts/complete/abort) all change cluster-side state
        // and are off-limits to a ReadOnly tritond key.
        | Action::StorageClusterSetPresigner
        | Action::StorageObjectPresignPut
        | Action::StorageObjectMultipartInit
        | Action::StorageObjectMultipartParts
        | Action::StorageObjectMultipartComplete
        | Action::StorageObjectMultipartAbort => false,
        // CN reads.
        Action::CnList | Action::CnGet | Action::AutoApproveGet => true,
        // Cluster config reads.
        Action::ConfigList | Action::ConfigGet => true,
        // Legacy admin reads.
        Action::LegacyCnList
        | Action::LegacyCnGet
        | Action::LegacyVmList
        | Action::LegacyVmGet
        | Action::LegacyAlarmList => true,
        // Storage cluster registry + read-only forwarders.
        Action::StorageClusterList
        | Action::StorageClusterGet
        | Action::StorageClusterSummary
        | Action::StorageNodeList
        | Action::StorageNodeGet
        | Action::StorageMembershipGet
        | Action::StorageBucketList
        | Action::StorageBucketGet
        | Action::StorageObjectList
        | Action::StorageUserList
        | Action::StorageUserGet
        | Action::StorageAccessKeyList
        | Action::StorageUserPolicyList
        | Action::StorageUserPolicyGet
        // Presigned GET grants read access only, so it's a read
        // even though it's auditable as a separate Action.
        | Action::StorageObjectPresignGet => true,
        // Layered instance metadata (IMDS).
        Action::MetaGet | Action::MetaList => true,
        Action::MetaSet | Action::MetaDelete => false,
        // Saga abandon is operator-only by Cedar; a ReadOnly key
        // must never reach it.
        Action::OperationsAbandon => false,
    }
}

/// Helper: pull a `Bearer <token>` value out of the request's
/// `Authorization` header, if present.
fn extract_bearer<C>(rqctx: &RequestContext<C>) -> Option<String>
where
    C: dropshot::ServerContext,
{
    let raw = rqctx
        .request
        .headers()
        .get(http::header::AUTHORIZATION)?
        .to_str()
        .ok()?;
    let token = raw.strip_prefix("Bearer ")?;
    Some(token.trim().to_string())
}

/// Authenticate then authorize a request in one shot. Returns the
/// principal so handlers that care about identity (e.g. `create_api_key`,
/// `list_api_keys`) can use it without a second round trip.
///
/// Emits exactly one audit event per call:
/// - Cedar **Allow** on any principal → logs Allow.
/// - Cedar **Deny** on an authenticated principal → logs Deny.
/// - Cedar **Deny** on an anonymous principal → does not log
///   (probe noise; see [`crate::audit::AuditService::record_decision`]).
pub async fn authenticate_and_authorize<C>(
    rqctx: &RequestContext<C>,
    auth: &AuthService,
    audit: &AuditService,
    store: &Arc<dyn Store>,
    action: Action,
) -> Result<Principal, HttpError>
where
    C: dropshot::ServerContext,
{
    let bearer = extract_bearer(rqctx);
    let principal = auth.authenticate(store.as_ref(), bearer.as_deref()).await?;
    let request_id = Uuid::parse_str(&rqctx.request_id).ok();
    match auth.authorize(&principal, action) {
        Ok(()) => {
            audit
                .record_decision(&principal, action, request_id, AuditDecision::Allow)
                .await;
            Ok(principal)
        }
        Err(http_err) => {
            audit
                .record_decision(&principal, action, request_id, AuditDecision::Deny)
                .await;
            Err(http_err)
        }
    }
}

/// authenticate the caller WITHOUT running Cedar.
/// Used by the `/v1/system/` handlers which gate on
/// [`require_capability`] instead of Cedar - the capability check
/// is the operator-surface gate. Returns the
/// resolved [`Principal`] (which may be `Anonymous`); the caller
/// must immediately follow with `require_capability` to reject
/// anonymous / under-capabilitied callers with the standard
/// cross-scope-deny 404 shape.
pub async fn authenticate_only<C>(
    rqctx: &RequestContext<C>,
    auth: &AuthService,
    store: &Arc<dyn Store>,
) -> Result<Principal, HttpError>
where
    C: dropshot::ServerContext,
{
    let bearer = extract_bearer(rqctx);
    let principal = auth.authenticate(store.as_ref(), bearer.as_deref()).await?;
    Ok(principal)
}

/// Returns 404 (not 403) on miss so an attacker can't distinguish
/// "no resource", "no capability", and "bad token". `is_root` users
/// always pass.
pub fn require_capability(
    principal: &Principal,
    capability: tritond_store::Capability,
) -> Result<(), HttpError> {
    if let Principal::Operator { is_root: true, .. } = principal {
        return Ok(());
    }
    if let Principal::Operator { capabilities, .. } = principal
        && capabilities.contains(&capability)
    {
        return Ok(());
    }
    Err(HttpError::for_client_error(
        Some("NotFound".to_string()),
        ClientErrorStatusCode::NOT_FOUND,
        "not found".to_string(),
    ))
}

/// 401 helper — used by handlers that need an *authenticated*
/// principal even if Cedar would let an anonymous request through
/// (e.g. /v2/auth/api-keys must run as somebody).
pub fn require_authenticated(principal: Principal) -> Result<(Uuid, bool), HttpError> {
    match principal {
        Principal::Operator {
            user_id, is_root, ..
        } => Ok((user_id, is_root)),
        Principal::Anonymous => Err(HttpError::for_client_error(
            Some("Unauthenticated".to_string()),
            ClientErrorStatusCode::UNAUTHORIZED,
            "authentication required".to_string(),
        )),
    }
}

/// Returns the per-CN binding from the principal's API key, if
/// any. Used by `/v2/agent/*` handlers to enforce that a key
/// minted for CN-A cannot drive work as CN-B.
#[must_use]
pub fn principal_bound_cn(principal: &Principal) -> Option<Uuid> {
    match principal {
        Principal::Operator { bound_cn, .. } => *bound_cn,
        Principal::Anonymous => None,
    }
}

/// 403 helper for the per-CN binding check. Caller passes the
/// bound CN (from `principal_bound_cn`) and the CN identity the
/// request claims (e.g. `claimed_by` parsed as a UUID, or the
/// job's `claimed_by`); returns `Ok(())` when they match (or the
/// principal is unbound), `Err(403)` otherwise.
pub fn enforce_cn_binding(bound_cn: Option<Uuid>, claimed_cn: Uuid) -> Result<(), HttpError> {
    match bound_cn {
        None => Ok(()), // Unbound key (operator-minted): no binding to check.
        Some(b) if b == claimed_cn => Ok(()),
        Some(_) => Err(HttpError::for_client_error(
            Some("Forbidden".to_string()),
            ClientErrorStatusCode::FORBIDDEN,
            "api key is bound to a different cn than the one this request names".to_string(),
        )),
    }
}

fn forbidden_for(action: Action) -> HttpError {
    HttpError::for_client_error(
        Some("Forbidden".to_string()),
        ClientErrorStatusCode::FORBIDDEN,
        format!("not authorised for {}", action.cedar_id()),
    )
}

/// Cross-silo deny: return 404 so cross-tenant probes can't enumerate
/// silos. The shape matches resource-not-found errors from
/// `store_error_to_http`, which is intentional.
fn not_found_in_silo() -> HttpError {
    HttpError::for_client_error(
        Some("NotFound".to_string()),
        ClientErrorStatusCode::NOT_FOUND,
        "not found".to_string(),
    )
}

/// Cross-tenant deny: return 404 so cross-tenant probes can't
/// enumerate tenants. Strictly stricter than [`not_found_in_silo`]:
/// even two tenants in the same silo cannot see each other.
fn not_found_in_tenant() -> HttpError {
    HttpError::for_client_error(
        Some("NotFound".to_string()),
        ClientErrorStatusCode::NOT_FOUND,
        "not found".to_string(),
    )
}

/// Silo-scoped variant of [`authenticate_and_authorize`]. The Cedar
/// resource is `Silo::"<silo_id>"` so the silo-member rule can fire;
/// deny returns **404** rather than 403 so cross-silo probes can't
/// enumerate silos. Used for resources that remain silo-scoped after
/// E-3 (SSH keys, images, IdP config).
pub async fn authenticate_and_authorize_in_silo<C>(
    rqctx: &RequestContext<C>,
    auth: &AuthService,
    audit: &AuditService,
    store: &Arc<dyn Store>,
    action: Action,
    silo_id: Uuid,
) -> Result<Principal, HttpError>
where
    C: dropshot::ServerContext,
{
    let bearer = extract_bearer(rqctx);
    let principal = auth.authenticate(store.as_ref(), bearer.as_deref()).await?;
    let request_id = Uuid::parse_str(&rqctx.request_id).ok();
    match auth.authorize_in_silo(&principal, action, silo_id) {
        Ok(()) => {
            audit
                .record_decision(&principal, action, request_id, AuditDecision::Allow)
                .await;
            Ok(principal)
        }
        Err(http_err) => {
            audit
                .record_decision(&principal, action, request_id, AuditDecision::Deny)
                .await;
            Err(http_err)
        }
    }
}

/// Tenant-scoped variant of [`authenticate_and_authorize`]. The
/// Cedar resource is `Tenant::"<tenant_id>"` so the tenant-member
/// rule can fire; deny returns **404** rather than 403 so
/// cross-tenant probes can't enumerate tenants. Used for the
/// project-rooted workload graph (project, VPC, subnet, instance,
/// NIC, disk, floating IP, quota).
pub async fn authenticate_and_authorize_in_tenant<C>(
    rqctx: &RequestContext<C>,
    auth: &AuthService,
    audit: &AuditService,
    store: &Arc<dyn Store>,
    action: Action,
    tenant_id: Uuid,
) -> Result<Principal, HttpError>
where
    C: dropshot::ServerContext,
{
    let bearer = extract_bearer(rqctx);
    let principal = auth.authenticate(store.as_ref(), bearer.as_deref()).await?;
    let request_id = Uuid::parse_str(&rqctx.request_id).ok();
    match auth.authorize_in_tenant(&principal, action, tenant_id) {
        Ok(()) => {
            audit
                .record_decision(&principal, action, request_id, AuditDecision::Allow)
                .await;
            Ok(principal)
        }
        Err(http_err) => {
            audit
                .record_decision(&principal, action, request_id, AuditDecision::Deny)
                .await;
            Err(http_err)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tritond_auth::{JwtKey, mint_access};
    use tritond_store::{MemStore, User};

    fn fresh_service() -> AuthService {
        AuthService::new(JwtKey::generate()).unwrap()
    }

    #[tokio::test]
    async fn anonymous_can_hit_public_actions() {
        let auth = fresh_service();
        for action in [Action::Health, Action::Login, Action::Refresh] {
            assert!(auth.authorize(&Principal::Anonymous, action).is_ok());
        }
    }

    #[tokio::test]
    async fn anonymous_cannot_create_silo() {
        let auth = fresh_service();
        let err = auth
            .authorize(&Principal::Anonymous, Action::CreateSilo)
            .expect_err("anonymous should be denied");
        assert_eq!(err.status_code.as_status().as_u16(), 403);
    }

    #[tokio::test]
    async fn root_operator_can_do_anything() {
        let auth = fresh_service();
        let p = Principal::Operator {
            user_id: Uuid::new_v4(),
            is_root: true,
            fleet_admin: true,
            capabilities: Default::default(),
            tenant_id: None,
            silo_id: None,
            scope: None,
            bound_cn: None,
        };
        for action in [
            Action::CreateSilo,
            Action::GetSilo,
            Action::CreateApiKey,
            Action::ListApiKeys,
            Action::DeleteApiKey,
        ] {
            assert!(auth.authorize(&p, action).is_ok(), "denied {action:?}");
        }
    }

    #[tokio::test]
    async fn non_root_operator_is_denied_outside_public_actions() {
        let auth = fresh_service();
        let p = Principal::Operator {
            user_id: Uuid::new_v4(),
            is_root: false,
            fleet_admin: false,
            capabilities: Default::default(),
            tenant_id: None,
            silo_id: None,
            scope: None,
            bound_cn: None,
        };
        assert!(auth.authorize(&p, Action::Health).is_ok());
        let err = auth
            .authorize(&p, Action::CreateSilo)
            .expect_err("non-root should be denied");
        assert_eq!(err.status_code.as_status().as_u16(), 403);
    }

    #[tokio::test]
    async fn read_only_scope_blocks_writes_even_for_root() {
        let auth = fresh_service();
        let p = Principal::Operator {
            user_id: Uuid::new_v4(),
            is_root: true,
            fleet_admin: true,
            capabilities: Default::default(),
            tenant_id: None,
            silo_id: None,
            scope: Some(ApiKeyScope::ReadOnly),
            bound_cn: None,
        };
        // Reads pass.
        assert!(auth.authorize(&p, Action::ListApiKeys).is_ok());
        assert!(auth.authorize(&p, Action::GetSilo).is_ok());
        // Writes are denied even though the underlying user is root.
        let err = auth
            .authorize(&p, Action::CreateSilo)
            .expect_err("read-only scope must deny CreateSilo");
        assert_eq!(err.status_code.as_status().as_u16(), 403);
        let err = auth
            .authorize(&p, Action::CreateApiKey)
            .expect_err("read-only scope must deny CreateApiKey");
        assert_eq!(err.status_code.as_status().as_u16(), 403);
    }

    #[tokio::test]
    async fn fleet_admin_can_hit_legacy_admin_actions() {
        // Decision #2 sixth rule: a non-root operator with
        // `fleet_admin == true` can list legacy CNs / VMs / alarms.
        let auth = fresh_service();
        let p = Principal::Operator {
            user_id: Uuid::new_v4(),
            is_root: false,
            fleet_admin: true,
            capabilities: Default::default(),
            tenant_id: None,
            silo_id: None,
            scope: None,
            bound_cn: None,
        };
        for action in [
            Action::LegacyCnList,
            Action::LegacyCnGet,
            Action::LegacyVmList,
            Action::LegacyVmGet,
            Action::LegacyAlarmList,
        ] {
            assert!(
                auth.authorize(&p, action).is_ok(),
                "fleet-admin denied {action:?}",
            );
        }
        // Tenant-scoped writes still denied — fleet_admin is not
        // is_root.
        let err = auth
            .authorize(&p, Action::CreateSilo)
            .expect_err("fleet-admin should not be able to create silos");
        assert_eq!(err.status_code.as_status().as_u16(), 403);
    }

    #[tokio::test]
    async fn non_fleet_admin_is_denied_legacy_admin_actions() {
        let auth = fresh_service();
        let p = Principal::Operator {
            user_id: Uuid::new_v4(),
            is_root: false,
            fleet_admin: false,
            capabilities: Default::default(),
            tenant_id: None,
            silo_id: None,
            scope: None,
            bound_cn: None,
        };
        for action in [
            Action::LegacyCnList,
            Action::LegacyVmList,
            Action::LegacyAlarmList,
        ] {
            let err = auth
                .authorize(&p, action)
                .expect_err("non-fleet-admin should be denied");
            assert_eq!(err.status_code.as_status().as_u16(), 403);
        }
    }

    /// Every Action variant in the StorageCluster surface — the
    /// registry CRUD + health probe (Stage 3.2/3.3/3.4) plus the
    /// mantad-forwarder family (Stage 3.5). Used by the three
    /// scope-coverage tests below so adding a new forwarder
    /// variant is checked end-to-end without rewriting each test.
    const ALL_STORAGE_CLUSTER_ACTIONS: &[Action] = &[
        Action::StorageClusterList,
        Action::StorageClusterCreate,
        Action::StorageClusterGet,
        Action::StorageClusterDelete,
        Action::StorageClusterHealthProbe,
        Action::StorageClusterSummary,
        Action::StorageNodeList,
        Action::StorageNodeGet,
        Action::StorageNodeAdd,
        Action::StorageNodeRemove,
        Action::StorageNodeDrain,
        Action::StorageNodeUndrain,
        Action::StorageNodeReweight,
        Action::StorageMembershipGet,
        Action::StorageBucketList,
        Action::StorageBucketGet,
        Action::StorageBucketCreate,
        Action::StorageBucketDelete,
        Action::StorageObjectList,
        Action::StorageUserList,
        Action::StorageUserGet,
        Action::StorageUserCreate,
        Action::StorageUserDelete,
        Action::StorageAccessKeyList,
        Action::StorageAccessKeyCreate,
        Action::StorageAccessKeyDelete,
        Action::StorageUserPolicyList,
        Action::StorageUserPolicyGet,
        Action::StorageUserPolicyPut,
        Action::StorageUserPolicyDelete,
        Action::StorageClusterSetPresigner,
        Action::StorageObjectPresignPut,
        Action::StorageObjectPresignGet,
        Action::StorageObjectMultipartInit,
        Action::StorageObjectMultipartParts,
        Action::StorageObjectMultipartComplete,
        Action::StorageObjectMultipartAbort,
    ];

    #[tokio::test]
    async fn root_can_perform_all_storage_cluster_actions() {
        // Cedar root-allows-all covers the entire StorageCluster
        // surface; no per-silo or fleet-admin variant exists.
        let auth = fresh_service();
        let p = Principal::Operator {
            user_id: Uuid::new_v4(),
            is_root: true,
            fleet_admin: true,
            capabilities: Default::default(),
            tenant_id: None,
            silo_id: None,
            scope: None,
            bound_cn: None,
        };
        for action in ALL_STORAGE_CLUSTER_ACTIONS {
            assert!(auth.authorize(&p, *action).is_ok(), "denied {action:?}");
        }
    }

    #[tokio::test]
    async fn fleet_admin_is_denied_storage_cluster_actions() {
        // Fleet-admin grants legacy-discovery reads; it must NOT
        // bleed into the storage surface. Letting a non-root
        // operator register a mantad endpoint or proxy admin
        // calls would point tritond at a potentially malicious
        // admin API and leak its trust boundary.
        let auth = fresh_service();
        let p = Principal::Operator {
            user_id: Uuid::new_v4(),
            is_root: false,
            fleet_admin: true,
            capabilities: Default::default(),
            tenant_id: None,
            silo_id: None,
            scope: None,
            bound_cn: None,
        };
        for action in ALL_STORAGE_CLUSTER_ACTIONS {
            let err = auth
                .authorize(&p, *action)
                .expect_err("fleet-admin should not reach storage-cluster surface");
            assert_eq!(err.status_code.as_status().as_u16(), 403);
        }
    }

    #[tokio::test]
    async fn read_only_scope_blocks_storage_cluster_writes() {
        // ReadOnly key on a root user: every list/get/summary read
        // passes, but every mutating forwarder is rejected by the
        // scope check ahead of Cedar. The split mirrors the
        // is_read_action() classification, so adding a new
        // Storage* Action variant without updating that classifier
        // turns into a compile-time exhaustiveness failure first
        // and a test failure here second.
        let auth = fresh_service();
        let p = Principal::Operator {
            user_id: Uuid::new_v4(),
            is_root: true,
            fleet_admin: true,
            capabilities: Default::default(),
            tenant_id: None,
            silo_id: None,
            scope: Some(ApiKeyScope::ReadOnly),
            bound_cn: None,
        };
        for action in ALL_STORAGE_CLUSTER_ACTIONS {
            let result = auth.authorize(&p, *action);
            if is_read_action(*action) {
                assert!(result.is_ok(), "read action {action:?} was denied");
            } else {
                let err = result.expect_err("write action must be denied for ReadOnly scope");
                assert_eq!(
                    err.status_code.as_status().as_u16(),
                    403,
                    "write action {action:?} returned non-403"
                );
            }
        }
    }

    #[tokio::test]
    async fn audit_only_scope_permits_only_audit_reads() {
        let auth = fresh_service();
        let p = Principal::Operator {
            user_id: Uuid::new_v4(),
            is_root: true,
            fleet_admin: true,
            capabilities: Default::default(),
            tenant_id: None,
            silo_id: None,
            scope: Some(ApiKeyScope::AuditOnly),
            bound_cn: None,
        };
        for action in [Action::AuditList, Action::AuditFetch, Action::AuditVerify] {
            assert!(auth.authorize(&p, action).is_ok(), "denied {action:?}");
        }
        // Even read-only on resources is denied for an audit-only key.
        let err = auth
            .authorize(&p, Action::ListApiKeys)
            .expect_err("audit-only scope must deny ListApiKeys");
        assert_eq!(err.status_code.as_status().as_u16(), 403);
    }

    #[tokio::test]
    async fn jwt_authenticates_to_operator() {
        let auth = fresh_service();
        let store = MemStore::new();
        let user = User {
            id: Uuid::new_v4(),
            username: "root".to_string(),
            password_hash: "$2y$12$dummy".to_string(),
            is_root: true,
            fleet_admin: true,
            created_at: chrono::Utc::now(),
            tenant_id: None,
            federation: None,
            capabilities: Default::default(),
        };
        let user_id = user.id;
        store.create_user(user).await.unwrap();
        let (token, _) = mint_access(auth.jwt_key(), user_id).unwrap();

        let p = auth.authenticate(&store, Some(&token)).await.unwrap();
        match p {
            Principal::Operator {
                user_id: got_id,
                is_root,
                ..
            } => {
                assert_eq!(got_id, user_id);
                assert!(is_root);
            }
            Principal::Anonymous => panic!("expected operator"),
        }
    }

    #[tokio::test]
    async fn jwt_for_unknown_user_yields_anonymous() {
        let auth = fresh_service();
        let store = MemStore::new();
        let (token, _) = mint_access(auth.jwt_key(), Uuid::new_v4()).unwrap();
        let p = auth.authenticate(&store, Some(&token)).await.unwrap();
        assert!(matches!(p, Principal::Anonymous));
    }

    #[tokio::test]
    async fn bogus_jwt_yields_anonymous() {
        let auth = fresh_service();
        let store = MemStore::new();
        let p = auth.authenticate(&store, Some("not.a.jwt")).await.unwrap();
        assert!(matches!(p, Principal::Anonymous));
    }

    #[tokio::test]
    async fn malformed_api_key_token_yields_anonymous() {
        let auth = fresh_service();
        let store = MemStore::new();
        // Right prefix, wrong length: not a real api key.
        let p = auth
            .authenticate(&store, Some("tcadm_short"))
            .await
            .unwrap();
        assert!(matches!(p, Principal::Anonymous));
    }
}
