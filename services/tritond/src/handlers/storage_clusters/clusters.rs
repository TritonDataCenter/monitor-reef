// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `storage_clusters::clusters` HTTP handlers (delegated to from the `TritondApi` impl).

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

pub(crate) async fn list_storage_clusters(
    rqctx: RequestContext<ApiContext>,
) -> Result<HttpResponseOk<Vec<StorageClusterView>>, HttpError> {
    let ctx = rqctx.context();
    authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::StorageClusterList,
    )
    .await?;
    let clusters = ctx
        .store
        .list_storage_clusters()
        .await
        .map_err(store_error_to_http)?;
    let views: Vec<StorageClusterView> = clusters.into_iter().map(Into::into).collect();
    Ok(HttpResponseOk(views))
}

pub(crate) async fn create_storage_cluster(
    rqctx: RequestContext<ApiContext>,
    body: TypedBody<NewStorageCluster>,
) -> Result<HttpResponseCreated<StorageClusterView>, HttpError> {
    let ctx = rqctx.context();
    let principal = authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::StorageClusterCreate,
    )
    .await?;
    let request_id = parse_request_id(&rqctx);
    let req = body.into_inner();
    // Audit payload deliberately captures only non-secret
    // identification fields. The bearer token is *never*
    // written to the audit chain — the storage layer holds
    // it in plaintext but the audit log must remain freely
    // readable by anyone with AuditOnly scope.
    let audit_payload = serde_json::json!({
        "name": req.name,
        "surface": req.surface,
        "endpoint": req.endpoint,
        "default_region": req.default_region,
    });
    match ctx.store.create_storage_cluster(req).await {
        Ok(cluster) => {
            let view: StorageClusterView = cluster.into();
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::StorageClusterCreate,
                    request_id,
                    Some(format!("StorageCluster::\"{}\"", view.id)),
                    AuditOutcome::Success {
                        resource: Some(format!("StorageCluster::\"{}\"", view.id)),
                    },
                    audit_payload,
                )
                .await;
            Ok(HttpResponseCreated(view))
        }
        Err(e) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::StorageClusterCreate,
                    request_id,
                    None,
                    store_error_to_audit_outcome(&e),
                    audit_payload,
                )
                .await;
            Err(store_error_to_http(e))
        }
    }
}

pub(crate) async fn get_storage_cluster(
    rqctx: RequestContext<ApiContext>,
    path: Path<StorageClusterPath>,
) -> Result<HttpResponseOk<StorageClusterView>, HttpError> {
    let ctx = rqctx.context();
    authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::StorageClusterGet,
    )
    .await?;
    let id = path.into_inner().id;
    let cluster = ctx
        .store
        .get_storage_cluster(id)
        .await
        .map_err(store_error_to_http)?;
    Ok(HttpResponseOk(cluster.into()))
}

pub(crate) async fn delete_storage_cluster(
    rqctx: RequestContext<ApiContext>,
    path: Path<StorageClusterPath>,
) -> Result<HttpResponseDeleted, HttpError> {
    let ctx = rqctx.context();
    let principal = authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::StorageClusterDelete,
    )
    .await?;
    let request_id = parse_request_id(&rqctx);
    let id = path.into_inner().id;
    match ctx.store.delete_storage_cluster(id).await {
        Ok(()) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::StorageClusterDelete,
                    request_id,
                    Some(format!("StorageCluster::\"{id}\"")),
                    AuditOutcome::Success {
                        resource: Some(format!("StorageCluster::\"{id}\"")),
                    },
                    serde_json::Value::Null,
                )
                .await;
            Ok(HttpResponseDeleted())
        }
        Err(e) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::StorageClusterDelete,
                    request_id,
                    Some(format!("StorageCluster::\"{id}\"")),
                    store_error_to_audit_outcome(&e),
                    serde_json::Value::Null,
                )
                .await;
            Err(store_error_to_http(e))
        }
    }
}

pub(crate) async fn probe_storage_cluster_health(
    rqctx: RequestContext<ApiContext>,
    path: Path<StorageClusterPath>,
) -> Result<HttpResponseOk<StorageClusterView>, HttpError> {
    let ctx = rqctx.context();
    let principal = authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::StorageClusterHealthProbe,
    )
    .await?;
    let request_id = parse_request_id(&rqctx);
    let id = path.into_inner().id;
    let (_, client) = crate::storage::client_for(&ctx.store, id).await?;
    let observed_at = chrono::Utc::now();
    let new_status = match client.cluster_summary().await {
        Ok(summary) => {
            if summary.nodes_alive == summary.nodes_total {
                tritond_store::StorageClusterStatus::Healthy
            } else if summary.nodes_alive == 0 {
                tritond_store::StorageClusterStatus::Unreachable
            } else {
                tritond_store::StorageClusterStatus::Degraded
            }
        }
        Err(_) => tritond_store::StorageClusterStatus::Unreachable,
    };
    let updated = ctx
        .store
        .update_storage_cluster_status(id, new_status, observed_at)
        .await
        .map_err(store_error_to_http)?;
    let view: StorageClusterView = updated.into();
    ctx.audit
        .record_mutation(
            &principal,
            Action::StorageClusterHealthProbe,
            request_id,
            Some(format!("StorageCluster::\"{id}\"")),
            AuditOutcome::Success {
                resource: Some(format!("StorageCluster::\"{id}\"")),
            },
            serde_json::json!({ "observed_status": view.status }),
        )
        .await;
    Ok(HttpResponseOk(view))
}

pub(crate) async fn get_storage_cluster_summary(
    rqctx: RequestContext<ApiContext>,
    path: Path<StorageClusterPath>,
) -> Result<HttpResponseOk<StorageClusterSummary>, HttpError> {
    let ctx = rqctx.context();
    authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::StorageClusterSummary,
    )
    .await?;
    let id = path.into_inner().id;
    let (_, client) = crate::storage::client_for(&ctx.store, id).await?;
    let summary = client
        .cluster_summary()
        .await
        .map_err(crate::storage::mantad_error_to_http)?;
    Ok(HttpResponseOk(crate::storage::cluster_summary_from(
        summary,
    )))
}
