// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `storage_clusters::policies` HTTP handlers (delegated to from the `TritondApi` impl).

#![allow(unused_imports)]

use crate::blueprint::*;
use crate::bundle::*;
use crate::cn_credential::*;
use crate::error::*;
use crate::lifecycle::*;
use crate::principal::*;
use crate::validate::*;

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use dropshot::{
    ApiDescription, ClientErrorStatusCode, ConfigDropshot, ConfigLogging, ConfigLoggingLevel,
    HttpError, HttpResponseCreated, HttpResponseDeleted, HttpResponseOk,
    HttpResponseUpdatedNoContent, HttpServer, HttpServerStarter, Path, Query, RequestContext,
    TypedBody,
};
use tritond_api::{
    AgentJobPath, AgentPortBlueprint, AgentPortBlueprintPath, AgentStatusRequest, ApiKeyCreated,
    ApiKeyPath, ApproveCnRequest, AttachFloatingIpRequest, AuditEventList, AuditEventPath,
    AuditListQuery, AuditVerifyQuery, AuditVerifyResponse, ClaimJobRequest, ClaimJobResponse,
    CnListQuery, CnPath, CompleteJobRequest, ConfigEntry, ConfigKeyPath, HealthResponse, ImagePath,
    InstanceDeleteQuery, InstanceLogsPath, LegacyCnSummary, LegacyVmListQuery, LegacyVmPath,
    LogTailQuery, LoginRequest, MetricsRangeQuery, NetworkRealizationRequest, NewApiKey,
    NewIdpConfig, NewImageFromBundle, OpenAutoApproveRequest, ProvisioningBlueprint,
    RefreshRequest, RegisterCnRequest, RegisterCnResponse, RegisterStatusQuery,
    RegisterStatusResponse, SetCnRoleRequest, SetConfigRequest, SiloPath, SiloTenantPath,
    SshKeyPath, StorageClusterAccessKeyPath, StorageClusterBucketPath, StorageClusterNodePath,
    StorageClusterPath, StorageClusterUserPath, StorageClusterUserPolicyPath, TenantIdpPath,
    TenantPath, TenantProjectFloatingIpPath, TenantProjectInstanceDiskPath,
    TenantProjectInstanceNicPath, TenantProjectInstancePath, TenantProjectPath,
    TenantProjectVpcDhcpMacPath, TenantProjectVpcFirewallRulePath, TenantProjectVpcNatGatewayPath,
    TenantProjectVpcPath, TenantProjectVpcRouteTablePath, TenantProjectVpcRouteTableRoutePath,
    TenantProjectVpcSubnetPath, TokenResponse, TritondApi,
    types::{
        ApiKeyView, AuditEvent, AutoApproveWindow, CnView, DhcpLease, DhcpPool, DhcpReservation,
        Disk, FirewallRule, FloatingIp, IdpConfigView, Image, ImageScope, Instance, JobKind,
        JobOutcome, LegacyVm, LifecycleState, LifecycleStateKind, NatGateway, NewDhcpPool,
        NewDhcpReservation, NewFirewallRule, NewFloatingIp, NewImage, NewInstance, NewJob,
        NewNatGateway, NewProject, NewQuota, NewRoute, NewRouteTable, NewSilo, NewSshKey,
        NewStorageCluster, NewSubnet, NewTenant, NewVpc, Nic, PresignGetRequest, PresignPutRequest,
        PresignResponse, Project, ProvisioningJob, Quota, Route, RouteTable, RouteTarget,
        SetPresignerRequest, Silo, SshKey, SshKeyScope, StorageAccessKey, StorageBucket,
        StorageClusterSummary, StorageClusterView, StorageMembership, StorageNode,
        StorageObjectsPage, StorageUser, Subnet, Tenant, Vpc,
    },
};
use tritond_audit::{Actor as AuditActor, Outcome as AuditOutcome};
use tritond_auth::OidcConfig;
use tritond_auth::{
    TokenKind, generate_api_key, mint_access, mint_refresh, verify, verify_password,
};
use tritond_store::{
    AUTO_APPROVE_WINDOW_MAX, ApiKey, CnState, ConfigError, ConfigKey, IdpConfig, Store, StoreError,
    normalize_claim_code,
};
use uuid::Uuid;

use crate::auth::{
    Action, AuthService, Principal, authenticate_and_authorize, authenticate_and_authorize_in_silo,
    authenticate_and_authorize_in_tenant, require_authenticated,
};

use crate::VERSION;

/// Concrete implementor of [`TritondApi`].
use crate::context::ApiContext;

pub(crate) async fn list_storage_cluster_user_policies(
    rqctx: RequestContext<ApiContext>,
    path: Path<StorageClusterUserPath>,
) -> Result<HttpResponseOk<Vec<String>>, HttpError> {
    let ctx = rqctx.context();
    let principal = authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::StorageUserPolicyList,
    )
    .await?;
    let scope = crate::storage::resolve_workspace_scope(&ctx.auth, &ctx.store, &principal).await?;
    let p = path.into_inner();
    let (_, client) = crate::storage::client_for(&ctx.store, p.id).await?;
    let policies = client
        .list_user_policies(&p.user, scope.workspace_name())
        .await
        .map_err(crate::storage::mantad_error_to_http)?;
    Ok(HttpResponseOk(policies))
}

pub(crate) async fn get_storage_cluster_user_policy(
    rqctx: RequestContext<ApiContext>,
    path: Path<StorageClusterUserPolicyPath>,
) -> Result<HttpResponseOk<serde_json::Value>, HttpError> {
    let ctx = rqctx.context();
    let principal = authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::StorageUserPolicyGet,
    )
    .await?;
    let scope = crate::storage::resolve_workspace_scope(&ctx.auth, &ctx.store, &principal).await?;
    let p = path.into_inner();
    let (_, client) = crate::storage::client_for(&ctx.store, p.id).await?;
    let doc = client
        .get_user_policy(&p.user, &p.policy, scope.workspace_name())
        .await
        .map_err(crate::storage::mantad_error_to_http)?;
    Ok(HttpResponseOk(doc))
}

/// `GET /v1/silos/{silo_id}/tenants/{tenant_id}/storage/users/{user}/policies`
/// — tenant-scoped sibling of `list_storage_cluster_user_policies`.
/// Pre-resolves the `(cluster_id, workspace_name)` pair from the
/// tenant binding so the operator-ui never enumerates policy names
/// attached to a sibling tenant's user. Policy names can themselves
/// be sensitive (often hint at intended actions). (monitor-reef-fydj)
pub(crate) async fn list_silo_tenant_storage_user_policies(
    rqctx: RequestContext<ApiContext>,
    path: Path<tritond_api::SiloTenantUserPath>,
) -> Result<HttpResponseOk<Vec<String>>, HttpError> {
    let ctx = rqctx.context();
    let tritond_api::SiloTenantUserPath {
        silo_id,
        tenant_id,
        user,
    } = path.into_inner();
    let _principal = authenticate_and_authorize_in_silo(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::TenantGet,
        silo_id,
    )
    .await?;

    let tenant = ctx
        .store
        .get_tenant(tenant_id)
        .await
        .map_err(store_error_to_http)?;
    if tenant.silo_id != silo_id {
        return Err(HttpError::for_not_found(
            Some("NotFound".to_string()),
            format!("tenant {tenant_id} not found in silo {silo_id}"),
        ));
    }

    let (workspace_uuid, cluster_id) =
        match (tenant.storage_workspace_id, tenant.storage_cluster_id) {
            (Some(w), Some(c)) => (w, c),
            _ => {
                return Err(HttpError::for_client_error(
                    Some("TenantStorageUnbound".to_string()),
                    ClientErrorStatusCode::PRECONDITION_FAILED,
                    format!(
                        "tenant {tenant_id} has no storage binding; run \
                         `POST /v1/silos/{silo_id}/tenants/{tenant_id}/init-storage` first"
                    ),
                ));
            }
        };

    let workspace_name = format!("t-{}", workspace_uuid.simple());
    let (_, client) = crate::storage::client_for(&ctx.store, cluster_id).await?;
    let policies = client
        .list_user_policies(&user, Some(workspace_name.as_str()))
        .await
        .map_err(crate::storage::mantad_error_to_http)?;
    Ok(HttpResponseOk(policies))
}

/// `GET /v1/silos/{silo_id}/tenants/{tenant_id}/storage/users/{user}/policies/{policy}`
/// — tenant-scoped sibling of `get_storage_cluster_user_policy`.
/// Pre-resolves the `(cluster_id, workspace_name)` pair from the
/// tenant binding so the operator-ui's policy detail view never
/// reads a sibling tenant's body. Policy documents carry
/// tenant-identifying ARNs and resource paths; this is the
/// highest-sensitivity leg of the family. (monitor-reef-fydj)
pub(crate) async fn get_silo_tenant_storage_user_policy(
    rqctx: RequestContext<ApiContext>,
    path: Path<tritond_api::SiloTenantUserPolicyPath>,
) -> Result<HttpResponseOk<serde_json::Value>, HttpError> {
    let ctx = rqctx.context();
    let tritond_api::SiloTenantUserPolicyPath {
        silo_id,
        tenant_id,
        user,
        policy,
    } = path.into_inner();
    let _principal = authenticate_and_authorize_in_silo(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::TenantGet,
        silo_id,
    )
    .await?;

    let tenant = ctx
        .store
        .get_tenant(tenant_id)
        .await
        .map_err(store_error_to_http)?;
    if tenant.silo_id != silo_id {
        return Err(HttpError::for_not_found(
            Some("NotFound".to_string()),
            format!("tenant {tenant_id} not found in silo {silo_id}"),
        ));
    }

    let (workspace_uuid, cluster_id) =
        match (tenant.storage_workspace_id, tenant.storage_cluster_id) {
            (Some(w), Some(c)) => (w, c),
            _ => {
                return Err(HttpError::for_client_error(
                    Some("TenantStorageUnbound".to_string()),
                    ClientErrorStatusCode::PRECONDITION_FAILED,
                    format!(
                        "tenant {tenant_id} has no storage binding; run \
                         `POST /v1/silos/{silo_id}/tenants/{tenant_id}/init-storage` first"
                    ),
                ));
            }
        };

    let workspace_name = format!("t-{}", workspace_uuid.simple());
    let (_, client) = crate::storage::client_for(&ctx.store, cluster_id).await?;
    let doc = client
        .get_user_policy(&user, &policy, Some(workspace_name.as_str()))
        .await
        .map_err(crate::storage::mantad_error_to_http)?;
    Ok(HttpResponseOk(doc))
}

pub(crate) async fn put_storage_cluster_user_policy(
    rqctx: RequestContext<ApiContext>,
    path: Path<StorageClusterUserPolicyPath>,
    body: TypedBody<serde_json::Value>,
) -> Result<HttpResponseUpdatedNoContent, HttpError> {
    let ctx = rqctx.context();
    let principal = authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::StorageUserPolicyPut,
    )
    .await?;
    let scope = crate::storage::resolve_workspace_scope(&ctx.auth, &ctx.store, &principal).await?;
    let request_id = parse_request_id(&rqctx);
    let p = path.into_inner();
    let doc = body.into_inner();
    let (_, client) = crate::storage::client_for(&ctx.store, p.id).await?;
    let resource = format!("StorageUserPolicy::\"{}/{}\"", p.user, p.policy);
    match client
        .put_user_policy(&p.user, &p.policy, &doc, scope.workspace_name())
        .await
    {
        Ok(()) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::StorageUserPolicyPut,
                    request_id,
                    Some(resource.clone()),
                    AuditOutcome::Success {
                        resource: Some(resource),
                    },
                    serde_json::json!({ "user": p.user, "policy": p.policy }),
                )
                .await;
            Ok(HttpResponseUpdatedNoContent())
        }
        Err(e) => {
            let (http_err, audit_outcome) = crate::storage::mantad_error_to_http_audit(e);
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::StorageUserPolicyPut,
                    request_id,
                    Some(resource),
                    audit_outcome,
                    serde_json::json!({ "user": p.user, "policy": p.policy }),
                )
                .await;
            Err(http_err)
        }
    }
}

pub(crate) async fn delete_storage_cluster_user_policy(
    rqctx: RequestContext<ApiContext>,
    path: Path<StorageClusterUserPolicyPath>,
) -> Result<HttpResponseDeleted, HttpError> {
    let ctx = rqctx.context();
    let principal = authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::StorageUserPolicyDelete,
    )
    .await?;
    let scope = crate::storage::resolve_workspace_scope(&ctx.auth, &ctx.store, &principal).await?;
    let request_id = parse_request_id(&rqctx);
    let p = path.into_inner();
    let (_, client) = crate::storage::client_for(&ctx.store, p.id).await?;
    let resource = format!("StorageUserPolicy::\"{}/{}\"", p.user, p.policy);
    match client
        .delete_user_policy(&p.user, &p.policy, scope.workspace_name())
        .await
    {
        Ok(()) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::StorageUserPolicyDelete,
                    request_id,
                    Some(resource.clone()),
                    AuditOutcome::Success {
                        resource: Some(resource),
                    },
                    serde_json::json!({ "user": p.user, "policy": p.policy }),
                )
                .await;
            Ok(HttpResponseDeleted())
        }
        Err(e) => {
            let (http_err, audit_outcome) = crate::storage::mantad_error_to_http_audit(e);
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::StorageUserPolicyDelete,
                    request_id,
                    Some(resource),
                    audit_outcome,
                    serde_json::json!({ "user": p.user, "policy": p.policy }),
                )
                .await;
            Err(http_err)
        }
    }
}

/// `PUT /v1/silos/{silo_id}/tenants/{tenant_id}/storage/users/{user}/policies/{policy}`
/// — tenant-scoped sibling of `put_storage_cluster_user_policy`.
/// Pre-resolves the `(cluster_id, workspace_name)` pair from the
/// tenant binding so an operator-ui PUT cannot install a policy
/// document onto a sibling tenant's user. Policy bodies carry
/// tenant-identifying ARNs and resource paths; writing the wrong
/// body to the wrong workspace would silently widen access.
/// (monitor-reef-5fek)
pub(crate) async fn put_silo_tenant_storage_user_policy(
    rqctx: RequestContext<ApiContext>,
    path: Path<tritond_api::SiloTenantUserPolicyPath>,
    body: TypedBody<serde_json::Value>,
) -> Result<HttpResponseUpdatedNoContent, HttpError> {
    let ctx = rqctx.context();
    let tritond_api::SiloTenantUserPolicyPath {
        silo_id,
        tenant_id,
        user,
        policy,
    } = path.into_inner();
    let principal = authenticate_and_authorize_in_silo(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::StorageUserPolicyPutInTenant,
        silo_id,
    )
    .await?;

    let tenant = ctx
        .store
        .get_tenant(tenant_id)
        .await
        .map_err(store_error_to_http)?;
    if tenant.silo_id != silo_id {
        return Err(HttpError::for_not_found(
            Some("NotFound".to_string()),
            format!("tenant {tenant_id} not found in silo {silo_id}"),
        ));
    }

    let (workspace_uuid, cluster_id) =
        match (tenant.storage_workspace_id, tenant.storage_cluster_id) {
            (Some(w), Some(c)) => (w, c),
            _ => {
                return Err(HttpError::for_client_error(
                    Some("TenantStorageUnbound".to_string()),
                    ClientErrorStatusCode::PRECONDITION_FAILED,
                    format!(
                        "tenant {tenant_id} has no storage binding; run \
                         `POST /v1/silos/{silo_id}/tenants/{tenant_id}/init-storage` first"
                    ),
                ));
            }
        };

    let workspace_name = format!("t-{}", workspace_uuid.simple());
    let request_id = parse_request_id(&rqctx);
    let doc = body.into_inner();
    let (_, client) = crate::storage::client_for(&ctx.store, cluster_id).await?;
    let resource = format!("StorageUserPolicy::\"{}/{}\"", user, policy);
    match client
        .put_user_policy(&user, &policy, &doc, Some(workspace_name.as_str()))
        .await
    {
        Ok(()) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::StorageUserPolicyPutInTenant,
                    request_id,
                    Some(resource.clone()),
                    AuditOutcome::Success {
                        resource: Some(resource),
                    },
                    serde_json::json!({
                        "user": user,
                        "policy": policy,
                        "silo_id": silo_id,
                        "tenant_id": tenant_id,
                    }),
                )
                .await;
            Ok(HttpResponseUpdatedNoContent())
        }
        Err(e) => {
            let (http_err, audit_outcome) = crate::storage::mantad_error_to_http_audit(e);
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::StorageUserPolicyPutInTenant,
                    request_id,
                    Some(resource),
                    audit_outcome,
                    serde_json::json!({
                        "user": user,
                        "policy": policy,
                        "silo_id": silo_id,
                        "tenant_id": tenant_id,
                    }),
                )
                .await;
            Err(http_err)
        }
    }
}
