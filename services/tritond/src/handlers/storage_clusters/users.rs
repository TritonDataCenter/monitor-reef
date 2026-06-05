// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `storage_clusters::users` HTTP handlers (delegated to from the `TritondApi` impl).

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

pub(crate) async fn list_storage_cluster_users(
    rqctx: RequestContext<ApiContext>,
    path: Path<StorageClusterPath>,
) -> Result<HttpResponseOk<Vec<StorageUser>>, HttpError> {
    let ctx = rqctx.context();
    let principal = authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::StorageUserList,
    )
    .await?;
    let scope = crate::storage::resolve_workspace_scope(&ctx.auth, &ctx.store, &principal).await?;
    let id = path.into_inner().id;
    let (_, client) = crate::storage::client_for_with_context(ctx, id).await?;
    let users = client
        .list_users(scope.workspace_name())
        .await
        .map_err(crate::storage::mantad_error_to_http)?;
    Ok(HttpResponseOk(
        users.into_iter().map(crate::storage::user_from).collect(),
    ))
}

/// `GET /v1/silos/{silo_id}/tenants/{tenant_id}/storage/users` —
/// list the IAM users owned by a single tenant's storage workspace.
///
/// Mirrors `list_storage_cluster_users` but resolves the
/// `(cluster_id, workspace_name)` pair from the tenant binding on
/// the URL, so an operator-ui drill-down can never enumerate users
/// owned by a sibling tenant on the same cluster. The cluster-flat
/// endpoint above stays operator-flat by design. (monitor-reef-nbdp)
pub(crate) async fn list_silo_tenant_storage_users(
    rqctx: RequestContext<ApiContext>,
    path: Path<tritond_api::SiloTenantPath>,
) -> Result<HttpResponseOk<Vec<StorageUser>>, HttpError> {
    let ctx = rqctx.context();
    let tritond_api::SiloTenantPath { silo_id, tenant_id } = path.into_inner();
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
    let (_, client) = crate::storage::client_for_with_context(ctx, cluster_id).await?;
    let users = client
        .list_users(Some(workspace_name.as_str()))
        .await
        .map_err(crate::storage::mantad_error_to_http)?;
    Ok(HttpResponseOk(
        users.into_iter().map(crate::storage::user_from).collect(),
    ))
}

pub(crate) async fn create_storage_cluster_user(
    rqctx: RequestContext<ApiContext>,
    path: Path<StorageClusterPath>,
    body: TypedBody<tritond_api::StorageCreateUserRequest>,
) -> Result<HttpResponseCreated<StorageUser>, HttpError> {
    let ctx = rqctx.context();
    let principal = authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::StorageUserCreate,
    )
    .await?;
    let scope = crate::storage::resolve_workspace_scope(&ctx.auth, &ctx.store, &principal).await?;
    let request_id = parse_request_id(&rqctx);
    let id = path.into_inner().id;
    let req = body.into_inner();
    let payload = serde_json::json!({ "name": req.name });
    let (_, client) = crate::storage::client_for_with_context(ctx, id).await?;
    let mantad_req = crate::storage::create_user_request_to(req);
    match client
        .create_user(&mantad_req, scope.workspace_name())
        .await
    {
        Ok(u) => {
            let view = crate::storage::user_from(u);
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::StorageUserCreate,
                    request_id,
                    Some(format!("StorageUser::\"{}\"", view.name)),
                    AuditOutcome::Success {
                        resource: Some(format!("StorageUser::\"{}\"", view.name)),
                    },
                    payload,
                )
                .await;
            Ok(HttpResponseCreated(view))
        }
        Err(e) => {
            let (http_err, audit_outcome) = crate::storage::mantad_error_to_http_audit(e);
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::StorageUserCreate,
                    request_id,
                    Some(format!("StorageCluster::\"{id}\"")),
                    audit_outcome,
                    payload,
                )
                .await;
            Err(http_err)
        }
    }
}

pub(crate) async fn get_storage_cluster_user(
    rqctx: RequestContext<ApiContext>,
    path: Path<StorageClusterUserPath>,
) -> Result<HttpResponseOk<StorageUser>, HttpError> {
    let ctx = rqctx.context();
    let principal = authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::StorageUserGet,
    )
    .await?;
    let scope = crate::storage::resolve_workspace_scope(&ctx.auth, &ctx.store, &principal).await?;
    let p = path.into_inner();
    let (_, client) = crate::storage::client_for_with_context(ctx, p.id).await?;
    let u = client
        .get_user(&p.user, scope.workspace_name())
        .await
        .map_err(crate::storage::mantad_error_to_http)?;
    Ok(HttpResponseOk(crate::storage::user_from(u)))
}

pub(crate) async fn delete_storage_cluster_user(
    rqctx: RequestContext<ApiContext>,
    path: Path<StorageClusterUserPath>,
) -> Result<HttpResponseDeleted, HttpError> {
    let ctx = rqctx.context();
    let principal = authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::StorageUserDelete,
    )
    .await?;
    let scope = crate::storage::resolve_workspace_scope(&ctx.auth, &ctx.store, &principal).await?;
    let request_id = parse_request_id(&rqctx);
    let p = path.into_inner();
    let (_, client) = crate::storage::client_for_with_context(ctx, p.id).await?;
    match client.delete_user(&p.user, scope.workspace_name()).await {
        Ok(()) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::StorageUserDelete,
                    request_id,
                    Some(format!("StorageUser::\"{}\"", p.user)),
                    AuditOutcome::Success {
                        resource: Some(format!("StorageUser::\"{}\"", p.user)),
                    },
                    serde_json::Value::Null,
                )
                .await;
            Ok(HttpResponseDeleted())
        }
        Err(e) => {
            let (http_err, audit_outcome) = crate::storage::mantad_error_to_http_audit(e);
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::StorageUserDelete,
                    request_id,
                    Some(format!("StorageUser::\"{}\"", p.user)),
                    audit_outcome,
                    serde_json::Value::Null,
                )
                .await;
            Err(http_err)
        }
    }
}

/// `POST /v1/silos/{silo_id}/tenants/{tenant_id}/storage/users` —
/// create an IAM user inside a single tenant's storage workspace.
///
/// Mirrors `create_storage_cluster_user` but resolves the
/// `(cluster_id, workspace_name)` pair from the tenant binding on
/// the URL so an operator-ui drill-down cannot create a user inside
/// a sibling tenant on the same cluster. The cluster-flat endpoint
/// stays operator-flat by design. (monitor-reef-5fek)
pub(crate) async fn create_silo_tenant_storage_user(
    rqctx: RequestContext<ApiContext>,
    path: Path<tritond_api::SiloTenantPath>,
    body: TypedBody<tritond_api::StorageCreateUserRequest>,
) -> Result<HttpResponseCreated<StorageUser>, HttpError> {
    let ctx = rqctx.context();
    let tritond_api::SiloTenantPath { silo_id, tenant_id } = path.into_inner();
    let principal = authenticate_and_authorize_in_silo(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::StorageUserCreateInTenant,
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
    let req = body.into_inner();
    let payload = serde_json::json!({
        "name": req.name,
        "silo_id": silo_id,
        "tenant_id": tenant_id,
    });
    let (_, client) = crate::storage::client_for_with_context(ctx, cluster_id).await?;
    let mantad_req = crate::storage::create_user_request_to(req);
    match client
        .create_user(&mantad_req, Some(workspace_name.as_str()))
        .await
    {
        Ok(u) => {
            let view = crate::storage::user_from(u);
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::StorageUserCreateInTenant,
                    request_id,
                    Some(format!("StorageUser::\"{}\"", view.name)),
                    AuditOutcome::Success {
                        resource: Some(format!("StorageUser::\"{}\"", view.name)),
                    },
                    payload,
                )
                .await;
            Ok(HttpResponseCreated(view))
        }
        Err(e) => {
            let (http_err, audit_outcome) = crate::storage::mantad_error_to_http_audit(e);
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::StorageUserCreateInTenant,
                    request_id,
                    Some(format!("StorageCluster::\"{cluster_id}\"")),
                    audit_outcome,
                    payload,
                )
                .await;
            Err(http_err)
        }
    }
}

/// `DELETE /v1/silos/{silo_id}/tenants/{tenant_id}/storage/users/{user}`
/// — delete an IAM user owned by a single tenant's storage workspace.
///
/// Mirrors `delete_storage_cluster_user` but resolves the
/// `(cluster_id, workspace_name)` pair from the tenant binding on
/// the URL so an operator-ui drill-down cannot delete a user owned
/// by a sibling tenant on the same cluster, even when login names
/// collide. (monitor-reef-5fek)
pub(crate) async fn delete_silo_tenant_storage_user(
    rqctx: RequestContext<ApiContext>,
    path: Path<tritond_api::SiloTenantUserPath>,
) -> Result<HttpResponseDeleted, HttpError> {
    let ctx = rqctx.context();
    let tritond_api::SiloTenantUserPath {
        silo_id,
        tenant_id,
        user,
    } = path.into_inner();
    let principal = authenticate_and_authorize_in_silo(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::StorageUserDeleteInTenant,
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
    let (_, client) = crate::storage::client_for_with_context(ctx, cluster_id).await?;
    match client
        .delete_user(&user, Some(workspace_name.as_str()))
        .await
    {
        Ok(()) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::StorageUserDeleteInTenant,
                    request_id,
                    Some(format!("StorageUser::\"{user}\"")),
                    AuditOutcome::Success {
                        resource: Some(format!("StorageUser::\"{user}\"")),
                    },
                    serde_json::json!({
                        "user": user,
                        "silo_id": silo_id,
                        "tenant_id": tenant_id,
                    }),
                )
                .await;
            Ok(HttpResponseDeleted())
        }
        Err(e) => {
            let (http_err, audit_outcome) = crate::storage::mantad_error_to_http_audit(e);
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::StorageUserDeleteInTenant,
                    request_id,
                    Some(format!("StorageUser::\"{user}\"")),
                    audit_outcome,
                    serde_json::json!({
                        "user": user,
                        "silo_id": silo_id,
                        "tenant_id": tenant_id,
                    }),
                )
                .await;
            Err(http_err)
        }
    }
}
