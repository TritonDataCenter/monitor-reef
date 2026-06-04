// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `storage_clusters::buckets` HTTP handlers (delegated to from the `TritondApi` impl).

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

pub(crate) async fn list_storage_cluster_buckets(
    rqctx: RequestContext<ApiContext>,
    path: Path<StorageClusterPath>,
    query: Query<tritond_api::StorageBucketListQuery>,
) -> Result<HttpResponseOk<Vec<StorageBucket>>, HttpError> {
    let ctx = rqctx.context();
    let principal = authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::StorageBucketList,
    )
    .await?;
    let scope = crate::storage::resolve_workspace_scope(&ctx.auth, &ctx.store, &principal).await?;
    let id = path.into_inner().id;
    let with_stats = query.into_inner().stats.unwrap_or(false);
    let (_, client) = crate::storage::client_for(&ctx.store, id).await?;
    let buckets = client
        .list_buckets(with_stats, scope.workspace_name())
        .await
        .map_err(crate::storage::mantad_error_to_http)?;
    Ok(HttpResponseOk(
        buckets
            .into_iter()
            .map(crate::storage::bucket_from)
            .collect(),
    ))
}

pub(crate) async fn get_storage_cluster_bucket(
    rqctx: RequestContext<ApiContext>,
    path: Path<StorageClusterBucketPath>,
) -> Result<HttpResponseOk<StorageBucket>, HttpError> {
    let ctx = rqctx.context();
    let principal = authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::StorageBucketGet,
    )
    .await?;
    let scope = crate::storage::resolve_workspace_scope(&ctx.auth, &ctx.store, &principal).await?;
    let p = path.into_inner();
    let (_, client) = crate::storage::client_for(&ctx.store, p.id).await?;
    let b = client
        .get_bucket(&p.bucket, scope.workspace_name())
        .await
        .map_err(crate::storage::mantad_error_to_http)?;
    Ok(HttpResponseOk(crate::storage::bucket_from(b)))
}

pub(crate) async fn create_storage_cluster_bucket(
    rqctx: RequestContext<ApiContext>,
    path: Path<StorageClusterPath>,
    body: TypedBody<tritond_api::StorageCreateBucketRequest>,
) -> Result<HttpResponseCreated<StorageBucket>, HttpError> {
    let ctx = rqctx.context();
    let principal = authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::StorageBucketCreate,
    )
    .await?;
    let scope = crate::storage::resolve_workspace_scope(&ctx.auth, &ctx.store, &principal).await?;
    let request_id = parse_request_id(&rqctx);
    let id = path.into_inner().id;
    let req = body.into_inner();
    let payload = serde_json::json!({
        "name": req.name,
        "owner": req.owner,
    });
    let (_, client) = crate::storage::client_for(&ctx.store, id).await?;
    let mantad_req = crate::storage::create_bucket_request_to(req);
    match client
        .create_bucket(&mantad_req, scope.workspace_name())
        .await
    {
        Ok(b) => {
            let view = crate::storage::bucket_from(b);
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::StorageBucketCreate,
                    request_id,
                    Some(format!("StorageBucket::\"{}\"", view.name)),
                    AuditOutcome::Success {
                        resource: Some(format!("StorageBucket::\"{}\"", view.name)),
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
                    Action::StorageBucketCreate,
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

pub(crate) async fn delete_storage_cluster_bucket(
    rqctx: RequestContext<ApiContext>,
    path: Path<StorageClusterBucketPath>,
) -> Result<HttpResponseDeleted, HttpError> {
    let ctx = rqctx.context();
    let principal = authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::StorageBucketDelete,
    )
    .await?;
    let scope = crate::storage::resolve_workspace_scope(&ctx.auth, &ctx.store, &principal).await?;
    let request_id = parse_request_id(&rqctx);
    let p = path.into_inner();
    let (_, client) = crate::storage::client_for(&ctx.store, p.id).await?;
    match client
        .delete_bucket(&p.bucket, scope.workspace_name())
        .await
    {
        Ok(()) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::StorageBucketDelete,
                    request_id,
                    Some(format!("StorageBucket::\"{}\"", p.bucket)),
                    AuditOutcome::Success {
                        resource: Some(format!("StorageBucket::\"{}\"", p.bucket)),
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
                    Action::StorageBucketDelete,
                    request_id,
                    Some(format!("StorageBucket::\"{}\"", p.bucket)),
                    audit_outcome,
                    serde_json::Value::Null,
                )
                .await;
            Err(http_err)
        }
    }
}

/// `GET /v1/silos/{silo_id}/tenants/{tenant_id}/storage/buckets` — list
/// the buckets owned by a single tenant's storage workspace.
///
/// Mirrors `list_storage_cluster_buckets` but resolves the
/// `(cluster_id, workspace_name)` pair from the tenant binding on the
/// URL, so the caller can never enumerate buckets owned by a sibling
/// tenant on the same cluster — regardless of caller principal. This
/// is the surface the operator-ui's "filter by silo+tenant" view
/// consumes; the cluster-flat endpoint above stays operator-flat by
/// design.
///
/// Authz is silo-scoped (`Action::TenantGet`); cross-silo probes are
/// neutralised by a 404 on tenant lookup. The tenant must have a
/// storage binding (412 `TenantStorageUnbound` otherwise — same
/// failure shape as `drop_silo_tenant_storage` so operators see the
/// same precondition vocabulary across the binding lifecycle).
pub(crate) async fn list_silo_tenant_storage_buckets(
    rqctx: RequestContext<ApiContext>,
    path: Path<tritond_api::SiloTenantPath>,
    query: Query<tritond_api::StorageBucketListQuery>,
) -> Result<HttpResponseOk<Vec<StorageBucket>>, HttpError> {
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

    // Cross-silo defence: a tenant in silo B reached via silo A's URL
    // must look "missing" not "forbidden".
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
    let with_stats = query.into_inner().stats.unwrap_or(false);
    let (_, client) = crate::storage::client_for(&ctx.store, cluster_id).await?;
    let buckets = client
        .list_buckets(with_stats, Some(workspace_name.as_str()))
        .await
        .map_err(crate::storage::mantad_error_to_http)?;
    Ok(HttpResponseOk(
        buckets
            .into_iter()
            .map(crate::storage::bucket_from)
            .collect(),
    ))
}

pub(crate) async fn list_storage_cluster_objects(
    rqctx: RequestContext<ApiContext>,
    path: Path<StorageClusterBucketPath>,
    query: Query<tritond_api::StorageObjectsQuery>,
) -> Result<HttpResponseOk<StorageObjectsPage>, HttpError> {
    let ctx = rqctx.context();
    let principal = authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::StorageObjectList,
    )
    .await?;
    let _scope = crate::storage::resolve_workspace_scope(&ctx.auth, &ctx.store, &principal).await?;
    let p = path.into_inner();
    let q = crate::storage::objects_query_to(query.into_inner());
    let (_, client) = crate::storage::client_for(&ctx.store, p.id).await?;
    let page = client
        .list_objects(&p.bucket, &q)
        .await
        .map_err(crate::storage::mantad_error_to_http)?;
    Ok(HttpResponseOk(crate::storage::objects_page_from(page)))
}

/// `POST /v1/silos/{silo_id}/tenants/{tenant_id}/storage/buckets` —
/// create a bucket owned by a single tenant's storage workspace.
///
/// Mirrors `create_storage_cluster_bucket` but resolves the
/// `(cluster_id, workspace_name)` pair from the tenant binding on the
/// URL, so the caller can never create buckets under a sibling
/// tenant's workspace on the same cluster — regardless of caller
/// principal. The cluster-flat endpoint above stays operator-flat by
/// design.
///
/// Authz is silo-scoped (`Action::StorageBucketCreateInTenant`);
/// cross-silo probes are neutralised by a 404 on tenant lookup. The
/// tenant must have a storage binding (412 `TenantStorageUnbound`
/// otherwise — same failure shape as `list_silo_tenant_storage_buckets`
/// so operators see the same precondition vocabulary across the
/// binding lifecycle).
pub(crate) async fn create_silo_tenant_storage_bucket(
    rqctx: RequestContext<ApiContext>,
    path: Path<tritond_api::SiloTenantPath>,
    body: TypedBody<tritond_api::StorageCreateBucketRequest>,
) -> Result<HttpResponseCreated<StorageBucket>, HttpError> {
    let ctx = rqctx.context();
    let tritond_api::SiloTenantPath { silo_id, tenant_id } = path.into_inner();
    let principal = authenticate_and_authorize_in_silo(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::StorageBucketCreateInTenant,
        silo_id,
    )
    .await?;

    // Cross-silo defence: a tenant in silo B reached via silo A's URL
    // must look "missing" not "forbidden".
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
        "silo_id": silo_id,
        "tenant_id": tenant_id,
        "name": req.name,
        "owner": req.owner,
    });
    let (_, client) = crate::storage::client_for(&ctx.store, cluster_id).await?;
    let mantad_req = crate::storage::create_bucket_request_to(req);
    match client
        .create_bucket(&mantad_req, Some(workspace_name.as_str()))
        .await
    {
        Ok(b) => {
            let view = crate::storage::bucket_from(b);
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::StorageBucketCreateInTenant,
                    request_id,
                    Some(format!("StorageBucket::\"{}\"", view.name)),
                    AuditOutcome::Success {
                        resource: Some(format!("StorageBucket::\"{}\"", view.name)),
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
                    Action::StorageBucketCreateInTenant,
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

/// `DELETE /v1/silos/{silo_id}/tenants/{tenant_id}/storage/buckets/{bucket}` —
/// delete a bucket owned by a single tenant's storage workspace.
///
/// Mirrors `delete_storage_cluster_bucket` but resolves the
/// `(cluster_id, workspace_name)` pair from the tenant binding on the
/// URL, so the caller can never delete buckets under a sibling
/// tenant's workspace on the same cluster — regardless of caller
/// principal. The cluster-flat endpoint above stays operator-flat by
/// design.
///
/// Authz is silo-scoped (`Action::StorageBucketDeleteInTenant`);
/// cross-silo probes are neutralised by a 404 on tenant lookup. The
/// tenant must have a storage binding (412 `TenantStorageUnbound`
/// otherwise).
pub(crate) async fn delete_silo_tenant_storage_bucket(
    rqctx: RequestContext<ApiContext>,
    path: Path<tritond_api::SiloTenantBucketPath>,
) -> Result<HttpResponseDeleted, HttpError> {
    let ctx = rqctx.context();
    let tritond_api::SiloTenantBucketPath {
        silo_id,
        tenant_id,
        bucket,
    } = path.into_inner();
    let principal = authenticate_and_authorize_in_silo(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::StorageBucketDeleteInTenant,
        silo_id,
    )
    .await?;

    // Cross-silo defence: a tenant in silo B reached via silo A's URL
    // must look "missing" not "forbidden".
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
    let payload = serde_json::json!({
        "silo_id": silo_id,
        "tenant_id": tenant_id,
        "bucket": bucket,
    });
    let (_, client) = crate::storage::client_for(&ctx.store, cluster_id).await?;
    match client
        .delete_bucket(&bucket, Some(workspace_name.as_str()))
        .await
    {
        Ok(()) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::StorageBucketDeleteInTenant,
                    request_id,
                    Some(format!("StorageBucket::\"{bucket}\"")),
                    AuditOutcome::Success {
                        resource: Some(format!("StorageBucket::\"{bucket}\"")),
                    },
                    payload,
                )
                .await;
            Ok(HttpResponseDeleted())
        }
        Err(e) => {
            let (http_err, audit_outcome) = crate::storage::mantad_error_to_http_audit(e);
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::StorageBucketDeleteInTenant,
                    request_id,
                    Some(format!("StorageBucket::\"{bucket}\"")),
                    audit_outcome,
                    payload,
                )
                .await;
            Err(http_err)
        }
    }
}
