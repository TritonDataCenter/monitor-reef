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
    let _scope = crate::storage::resolve_workspace_scope(&ctx.store, &principal).await?;
    let p = path.into_inner();
    let (_, client) = crate::storage::client_for(&ctx.store, p.id).await?;
    let policies = client
        .list_user_policies(&p.user)
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
    let _scope = crate::storage::resolve_workspace_scope(&ctx.store, &principal).await?;
    let p = path.into_inner();
    let (_, client) = crate::storage::client_for(&ctx.store, p.id).await?;
    let doc = client
        .get_user_policy(&p.user, &p.policy)
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
    let _scope = crate::storage::resolve_workspace_scope(&ctx.store, &principal).await?;
    let request_id = parse_request_id(&rqctx);
    let p = path.into_inner();
    let doc = body.into_inner();
    let (_, client) = crate::storage::client_for(&ctx.store, p.id).await?;
    let resource = format!("StorageUserPolicy::\"{}/{}\"", p.user, p.policy);
    match client.put_user_policy(&p.user, &p.policy, &doc).await {
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
    let _scope = crate::storage::resolve_workspace_scope(&ctx.store, &principal).await?;
    let request_id = parse_request_id(&rqctx);
    let p = path.into_inner();
    let (_, client) = crate::storage::client_for(&ctx.store, p.id).await?;
    let resource = format!("StorageUserPolicy::\"{}/{}\"", p.user, p.policy);
    match client.delete_user_policy(&p.user, &p.policy).await {
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
