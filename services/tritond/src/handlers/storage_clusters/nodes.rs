// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `storage_clusters::nodes` HTTP handlers (delegated to from the `TritondApi` impl).

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

pub(crate) async fn list_storage_cluster_nodes(
    rqctx: RequestContext<ApiContext>,
    path: Path<StorageClusterPath>,
) -> Result<HttpResponseOk<Vec<StorageNode>>, HttpError> {
    let ctx = rqctx.context();
    authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::StorageNodeList,
    )
    .await?;
    let id = path.into_inner().id;
    let (_, client) = crate::storage::client_for_with_context(ctx, id).await?;
    let nodes = client
        .list_nodes()
        .await
        .map_err(crate::storage::mantad_error_to_http)?;
    Ok(HttpResponseOk(
        nodes.into_iter().map(crate::storage::node_from).collect(),
    ))
}

pub(crate) async fn get_storage_cluster_node(
    rqctx: RequestContext<ApiContext>,
    path: Path<StorageClusterNodePath>,
) -> Result<HttpResponseOk<StorageNode>, HttpError> {
    let ctx = rqctx.context();
    authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::StorageNodeGet,
    )
    .await?;
    let p = path.into_inner();
    let (_, client) = crate::storage::client_for_with_context(ctx, p.id).await?;
    let node = client
        .get_node(p.node_id)
        .await
        .map_err(crate::storage::mantad_error_to_http)?;
    Ok(HttpResponseOk(crate::storage::node_from(node)))
}

pub(crate) async fn add_storage_cluster_node(
    rqctx: RequestContext<ApiContext>,
    path: Path<StorageClusterPath>,
    body: TypedBody<tritond_api::StorageAddNodeRequest>,
) -> Result<HttpResponseOk<StorageMembership>, HttpError> {
    let ctx = rqctx.context();
    let principal = authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::StorageNodeAdd,
    )
    .await?;
    let request_id = parse_request_id(&rqctx);
    let id = path.into_inner().id;
    let req = body.into_inner();
    let audit_payload = serde_json::json!({
        "node_id": req.id,
        "rack": req.rack,
        "internal_url": req.internal_url,
    });
    let (_, client) = crate::storage::client_for_with_context(ctx, id).await?;
    let mantad_req = crate::storage::add_node_request_to(req);
    match client.add_node(&mantad_req).await {
        Ok(m) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::StorageNodeAdd,
                    request_id,
                    Some(format!("StorageCluster::\"{id}\"")),
                    AuditOutcome::Success {
                        resource: Some(format!("StorageCluster::\"{id}\"")),
                    },
                    audit_payload,
                )
                .await;
            Ok(HttpResponseOk(crate::storage::membership_from(m)))
        }
        Err(e) => {
            let (http_err, audit_outcome) = crate::storage::mantad_error_to_http_audit(e);
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::StorageNodeAdd,
                    request_id,
                    Some(format!("StorageCluster::\"{id}\"")),
                    audit_outcome,
                    audit_payload,
                )
                .await;
            Err(http_err)
        }
    }
}

pub(crate) async fn remove_storage_cluster_node(
    rqctx: RequestContext<ApiContext>,
    path: Path<StorageClusterNodePath>,
) -> Result<HttpResponseOk<StorageMembership>, HttpError> {
    let ctx = rqctx.context();
    let principal = authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::StorageNodeRemove,
    )
    .await?;
    let request_id = parse_request_id(&rqctx);
    let p = path.into_inner();
    let (_, client) = crate::storage::client_for_with_context(ctx, p.id).await?;
    let payload = serde_json::json!({ "node_id": p.node_id });
    match client.remove_node(p.node_id).await {
        Ok(m) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::StorageNodeRemove,
                    request_id,
                    Some(format!("StorageCluster::\"{}\"", p.id)),
                    AuditOutcome::Success {
                        resource: Some(format!("StorageCluster::\"{}\"", p.id)),
                    },
                    payload,
                )
                .await;
            Ok(HttpResponseOk(crate::storage::membership_from(m)))
        }
        Err(e) => {
            let (http_err, audit_outcome) = crate::storage::mantad_error_to_http_audit(e);
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::StorageNodeRemove,
                    request_id,
                    Some(format!("StorageCluster::\"{}\"", p.id)),
                    audit_outcome,
                    payload,
                )
                .await;
            Err(http_err)
        }
    }
}

pub(crate) async fn drain_storage_cluster_node(
    rqctx: RequestContext<ApiContext>,
    path: Path<StorageClusterNodePath>,
) -> Result<HttpResponseOk<StorageMembership>, HttpError> {
    forward_node_membership_op(
        &rqctx,
        path,
        Action::StorageNodeDrain,
        |client, node_id| async move { client.drain_node(node_id).await },
    )
    .await
}

pub(crate) async fn undrain_storage_cluster_node(
    rqctx: RequestContext<ApiContext>,
    path: Path<StorageClusterNodePath>,
) -> Result<HttpResponseOk<StorageMembership>, HttpError> {
    forward_node_membership_op(
        &rqctx,
        path,
        Action::StorageNodeUndrain,
        |client, node_id| async move { client.undrain_node(node_id).await },
    )
    .await
}

pub(crate) async fn reweight_storage_cluster_node(
    rqctx: RequestContext<ApiContext>,
    path: Path<StorageClusterNodePath>,
    body: TypedBody<tritond_api::StorageReweightRequest>,
) -> Result<HttpResponseOk<StorageMembership>, HttpError> {
    let ctx = rqctx.context();
    let principal = authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::StorageNodeReweight,
    )
    .await?;
    let request_id = parse_request_id(&rqctx);
    let p = path.into_inner();
    let req = body.into_inner();
    let payload = serde_json::json!({
        "node_id": p.node_id,
        "factor": req.factor,
    });
    let (_, client) = crate::storage::client_for_with_context(ctx, p.id).await?;
    let mantad_req = crate::storage::reweight_request_to(req);
    match client.reweight_node(p.node_id, &mantad_req).await {
        Ok(m) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::StorageNodeReweight,
                    request_id,
                    Some(format!("StorageCluster::\"{}\"", p.id)),
                    AuditOutcome::Success {
                        resource: Some(format!("StorageCluster::\"{}\"", p.id)),
                    },
                    payload,
                )
                .await;
            Ok(HttpResponseOk(crate::storage::membership_from(m)))
        }
        Err(e) => {
            let (http_err, audit_outcome) = crate::storage::mantad_error_to_http_audit(e);
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::StorageNodeReweight,
                    request_id,
                    Some(format!("StorageCluster::\"{}\"", p.id)),
                    audit_outcome,
                    payload,
                )
                .await;
            Err(http_err)
        }
    }
}

pub(crate) async fn get_storage_cluster_membership(
    rqctx: RequestContext<ApiContext>,
    path: Path<StorageClusterPath>,
) -> Result<HttpResponseOk<StorageMembership>, HttpError> {
    let ctx = rqctx.context();
    authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::StorageMembershipGet,
    )
    .await?;
    let id = path.into_inner().id;
    let (_, client) = crate::storage::client_for_with_context(ctx, id).await?;
    let m = client
        .membership()
        .await
        .map_err(crate::storage::mantad_error_to_http)?;
    Ok(HttpResponseOk(crate::storage::membership_from(m)))
}
