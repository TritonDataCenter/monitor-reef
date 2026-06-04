// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `storage_clusters::access_keys` HTTP handlers (delegated to from the `TritondApi` impl).

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

pub(crate) async fn list_storage_cluster_access_keys(
    rqctx: RequestContext<ApiContext>,
    path: Path<StorageClusterUserPath>,
) -> Result<HttpResponseOk<Vec<StorageAccessKey>>, HttpError> {
    let ctx = rqctx.context();
    let principal = authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::StorageAccessKeyList,
    )
    .await?;
    let scope = crate::storage::resolve_workspace_scope(&ctx.auth, &ctx.store, &principal).await?;
    let p = path.into_inner();
    let (_, client) = crate::storage::client_for(&ctx.store, p.id).await?;
    let keys = client
        .list_access_keys(&p.user, scope.workspace_name())
        .await
        .map_err(crate::storage::mantad_error_to_http)?;
    Ok(HttpResponseOk(
        keys.into_iter()
            .map(crate::storage::access_key_from)
            .collect(),
    ))
}

/// `GET /v1/silos/{silo_id}/tenants/{tenant_id}/storage/users/{user}/access-keys`
/// — tenant-scoped sibling of `list_storage_cluster_access_keys`.
/// Pre-resolves the `(cluster_id, workspace_name)` pair from the
/// tenant binding so the operator-ui never enumerates keys owned by
/// a sibling tenant's user, even when login names collide.
/// (monitor-reef-8imp)
pub(crate) async fn list_silo_tenant_storage_user_access_keys(
    rqctx: RequestContext<ApiContext>,
    path: Path<tritond_api::SiloTenantUserPath>,
) -> Result<HttpResponseOk<Vec<StorageAccessKey>>, HttpError> {
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
    let keys = client
        .list_access_keys(&user, Some(workspace_name.as_str()))
        .await
        .map_err(crate::storage::mantad_error_to_http)?;
    Ok(HttpResponseOk(
        keys.into_iter()
            .map(crate::storage::access_key_from)
            .collect(),
    ))
}

pub(crate) async fn create_storage_cluster_access_key(
    rqctx: RequestContext<ApiContext>,
    path: Path<StorageClusterUserPath>,
) -> Result<HttpResponseCreated<StorageAccessKey>, HttpError> {
    let ctx = rqctx.context();
    let principal = authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::StorageAccessKeyCreate,
    )
    .await?;
    let scope = crate::storage::resolve_workspace_scope(&ctx.auth, &ctx.store, &principal).await?;
    let request_id = parse_request_id(&rqctx);
    let p = path.into_inner();
    let payload = serde_json::json!({ "user": p.user });
    let (_, client) = crate::storage::client_for(&ctx.store, p.id).await?;
    match client
        .create_access_key(&p.user, scope.workspace_name())
        .await
    {
        Ok(k) => {
            let view = crate::storage::access_key_from(k);
            // Audit captures only the AKID — the cleartext
            // secret is in the response and must not enter the
            // audit chain.
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::StorageAccessKeyCreate,
                    request_id,
                    Some(format!("StorageAccessKey::\"{}\"", view.access_key_id)),
                    AuditOutcome::Success {
                        resource: Some(format!("StorageAccessKey::\"{}\"", view.access_key_id)),
                    },
                    serde_json::json!({
                        "user": view.user,
                        "access_key_id": view.access_key_id,
                    }),
                )
                .await;
            Ok(HttpResponseCreated(view))
        }
        Err(e) => {
            let (http_err, audit_outcome) = crate::storage::mantad_error_to_http_audit(e);
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::StorageAccessKeyCreate,
                    request_id,
                    None,
                    audit_outcome,
                    payload,
                )
                .await;
            Err(http_err)
        }
    }
}

pub(crate) async fn delete_storage_cluster_access_key(
    rqctx: RequestContext<ApiContext>,
    path: Path<StorageClusterAccessKeyPath>,
) -> Result<HttpResponseDeleted, HttpError> {
    let ctx = rqctx.context();
    let principal = authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::StorageAccessKeyDelete,
    )
    .await?;
    let scope = crate::storage::resolve_workspace_scope(&ctx.auth, &ctx.store, &principal).await?;
    let request_id = parse_request_id(&rqctx);
    let p = path.into_inner();
    let (_, client) = crate::storage::client_for(&ctx.store, p.id).await?;
    match client
        .delete_access_key(&p.access_key_id, scope.workspace_name())
        .await
    {
        Ok(()) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::StorageAccessKeyDelete,
                    request_id,
                    Some(format!("StorageAccessKey::\"{}\"", p.access_key_id)),
                    AuditOutcome::Success {
                        resource: Some(format!("StorageAccessKey::\"{}\"", p.access_key_id)),
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
                    Action::StorageAccessKeyDelete,
                    request_id,
                    Some(format!("StorageAccessKey::\"{}\"", p.access_key_id)),
                    audit_outcome,
                    serde_json::Value::Null,
                )
                .await;
            Err(http_err)
        }
    }
}

/// `POST /v1/silos/{silo_id}/tenants/{tenant_id}/storage/users/{user}/access-keys`
/// — tenant-scoped sibling of `create_storage_cluster_access_key`.
/// Mints a new access key for the tenant's user, resolving the
/// `(cluster_id, workspace_name)` pair from the tenant binding so
/// the operator-ui can never create a key on a sibling tenant's
/// workspace, even when login names collide. (monitor-reef-5fek)
pub(crate) async fn create_silo_tenant_storage_user_access_key(
    rqctx: RequestContext<ApiContext>,
    path: Path<tritond_api::SiloTenantUserPath>,
) -> Result<HttpResponseCreated<StorageAccessKey>, HttpError> {
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
        Action::StorageAccessKeyCreateInTenant,
        silo_id,
    )
    .await?;
    let request_id = parse_request_id(&rqctx);

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
    let payload = serde_json::json!({
        "user": user,
        "silo_id": silo_id,
        "tenant_id": tenant_id,
    });
    let (_, client) = crate::storage::client_for(&ctx.store, cluster_id).await?;
    match client
        .create_access_key(&user, Some(workspace_name.as_str()))
        .await
    {
        Ok(k) => {
            let view = crate::storage::access_key_from(k);
            // Audit captures only the AKID — the cleartext
            // secret is in the response and must not enter the
            // audit chain.
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::StorageAccessKeyCreateInTenant,
                    request_id,
                    Some(format!("StorageAccessKey::\"{}\"", view.access_key_id)),
                    AuditOutcome::Success {
                        resource: Some(format!("StorageAccessKey::\"{}\"", view.access_key_id)),
                    },
                    serde_json::json!({
                        "user": user,
                        "silo_id": silo_id,
                        "tenant_id": tenant_id,
                        "access_key_id": view.access_key_id,
                    }),
                )
                .await;
            Ok(HttpResponseCreated(view))
        }
        Err(e) => {
            let (http_err, audit_outcome) = crate::storage::mantad_error_to_http_audit(e);
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::StorageAccessKeyCreateInTenant,
                    request_id,
                    None,
                    audit_outcome,
                    payload,
                )
                .await;
            Err(http_err)
        }
    }
}

/// `DELETE /v1/silos/{silo_id}/tenants/{tenant_id}/storage/users/{user}/access-keys/{access_key_id}`
/// — tenant-scoped sibling of `delete_storage_cluster_access_key`.
/// Resolves the `(cluster_id, workspace_name)` pair from the tenant
/// binding so cross-tenant deletes return 404 by design. The `user`
/// segment is part of the URL hierarchy but mantad's delete call
/// only requires the access_key_id (matching the flat handler).
/// (monitor-reef-5fek)
pub(crate) async fn delete_silo_tenant_storage_user_access_key(
    rqctx: RequestContext<ApiContext>,
    path: Path<tritond_api::SiloTenantUserAccessKeyPath>,
) -> Result<HttpResponseDeleted, HttpError> {
    let ctx = rqctx.context();
    let tritond_api::SiloTenantUserAccessKeyPath {
        silo_id,
        tenant_id,
        user,
        access_key_id,
    } = path.into_inner();
    let principal = authenticate_and_authorize_in_silo(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::StorageAccessKeyDeleteInTenant,
        silo_id,
    )
    .await?;
    let request_id = parse_request_id(&rqctx);

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
    match client
        .delete_access_key(&access_key_id, Some(workspace_name.as_str()))
        .await
    {
        Ok(()) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::StorageAccessKeyDeleteInTenant,
                    request_id,
                    Some(format!("StorageAccessKey::\"{}\"", access_key_id)),
                    AuditOutcome::Success {
                        resource: Some(format!("StorageAccessKey::\"{}\"", access_key_id)),
                    },
                    serde_json::json!({
                        "user": user,
                        "silo_id": silo_id,
                        "tenant_id": tenant_id,
                        "access_key_id": access_key_id,
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
                    Action::StorageAccessKeyDeleteInTenant,
                    request_id,
                    Some(format!("StorageAccessKey::\"{}\"", access_key_id)),
                    audit_outcome,
                    serde_json::json!({
                        "user": user,
                        "silo_id": silo_id,
                        "tenant_id": tenant_id,
                        "access_key_id": access_key_id,
                    }),
                )
                .await;
            Err(http_err)
        }
    }
}
