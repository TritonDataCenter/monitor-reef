// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `projects` HTTP handlers (delegated to from the `TritondApi` impl).

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

pub(crate) async fn get_tenant_project(
    rqctx: RequestContext<ApiContext>,
    path: Path<TenantProjectPath>,
) -> Result<HttpResponseOk<Project>, HttpError> {
    let ctx = rqctx.context();
    let TenantProjectPath {
        tenant_id,
        project_id,
    } = path.into_inner();
    authenticate_and_authorize_in_tenant(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::ProjectGet,
        tenant_id,
    )
    .await?;
    let project = ctx
        .store
        .get_project(project_id)
        .await
        .map_err(store_error_to_http)?;
    // Project found globally — confirm it actually belongs to the
    // path's tenant. Cross-tenant lookups (would-be probes) get
    // the same 404 as a missing project.
    if project.tenant_id != tenant_id {
        return Err(HttpError::for_client_error(
            Some("NotFound".to_string()),
            ClientErrorStatusCode::NOT_FOUND,
            "not found".to_string(),
        ));
    }
    Ok(HttpResponseOk(project))
}

pub(crate) async fn delete_tenant_project(
    rqctx: RequestContext<ApiContext>,
    path: Path<TenantProjectPath>,
) -> Result<HttpResponseDeleted, HttpError> {
    let ctx = rqctx.context();
    let TenantProjectPath {
        tenant_id,
        project_id,
    } = path.into_inner();
    let principal = authenticate_and_authorize_in_tenant(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::ProjectDelete,
        tenant_id,
    )
    .await?;
    let request_id = parse_request_id(&rqctx);

    // Confirm tenant membership before deleting; cross-tenant
    // deletes get a 404 like cross-tenant gets.
    let project = ctx
        .store
        .get_project(project_id)
        .await
        .map_err(store_error_to_http)?;
    if project.tenant_id != tenant_id {
        return Err(HttpError::for_client_error(
            Some("NotFound".to_string()),
            ClientErrorStatusCode::NOT_FOUND,
            "not found".to_string(),
        ));
    }
    ctx.store
        .delete_project(project_id)
        .await
        .map_err(store_error_to_http)?;
    ctx.audit
        .record_mutation(
            &principal,
            Action::ProjectDelete,
            request_id,
            Some(format!("Project::\"{project_id}\"")),
            AuditOutcome::Success {
                resource: Some(format!("Project::\"{project_id}\"")),
            },
            serde_json::json!({ "tenant_id": tenant_id }),
        )
        .await;
    Ok(HttpResponseDeleted())
}

pub(crate) async fn put_project_quota(
    rqctx: RequestContext<ApiContext>,
    path: Path<TenantProjectPath>,
    body: TypedBody<NewQuota>,
) -> Result<HttpResponseOk<Quota>, HttpError> {
    let ctx = rqctx.context();
    let TenantProjectPath {
        tenant_id,
        project_id,
    } = path.into_inner();
    let principal = authenticate_and_authorize_in_tenant(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::QuotaSet,
        tenant_id,
    )
    .await?;
    let request_id = parse_request_id(&rqctx);
    let req = body.into_inner();

    match ctx.store.put_quota(tenant_id, project_id, req).await {
        Ok(quota) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::QuotaSet,
                    request_id,
                    Some(format!("Quota::\"{project_id}\"")),
                    AuditOutcome::Success {
                        resource: Some(format!("Quota::\"{project_id}\"")),
                    },
                    serde_json::json!({
                        "tenant_id": tenant_id,
                        "project_id": project_id,
                        "cpu_limit": quota.cpu_limit,
                        "memory_bytes": quota.memory_bytes,
                        "disk_bytes": quota.disk_bytes,
                        "instance_limit": quota.instance_limit,
                    }),
                )
                .await;
            Ok(HttpResponseOk(quota))
        }
        Err(e) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::QuotaSet,
                    request_id,
                    None,
                    store_error_to_audit_outcome(&e),
                    serde_json::Value::Null,
                )
                .await;
            Err(store_error_to_http(e))
        }
    }
}

pub(crate) async fn get_project_quota(
    rqctx: RequestContext<ApiContext>,
    path: Path<TenantProjectPath>,
) -> Result<HttpResponseOk<Quota>, HttpError> {
    let ctx = rqctx.context();
    let TenantProjectPath {
        tenant_id,
        project_id,
    } = path.into_inner();
    authenticate_and_authorize_in_tenant(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::QuotaGet,
        tenant_id,
    )
    .await?;
    let quota = ctx
        .store
        .get_quota(tenant_id, project_id)
        .await
        .map_err(store_error_to_http)?;
    Ok(HttpResponseOk(quota))
}

pub(crate) async fn delete_project_quota(
    rqctx: RequestContext<ApiContext>,
    path: Path<TenantProjectPath>,
) -> Result<HttpResponseDeleted, HttpError> {
    let ctx = rqctx.context();
    let TenantProjectPath {
        tenant_id,
        project_id,
    } = path.into_inner();
    let principal = authenticate_and_authorize_in_tenant(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::QuotaDelete,
        tenant_id,
    )
    .await?;
    let request_id = parse_request_id(&rqctx);

    match ctx.store.delete_quota(tenant_id, project_id).await {
        Ok(()) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::QuotaDelete,
                    request_id,
                    Some(format!("Quota::\"{project_id}\"")),
                    AuditOutcome::Success {
                        resource: Some(format!("Quota::\"{project_id}\"")),
                    },
                    serde_json::json!({
                        "tenant_id": tenant_id,
                        "project_id": project_id,
                    }),
                )
                .await;
            Ok(HttpResponseDeleted())
        }
        Err(e) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::QuotaDelete,
                    request_id,
                    None,
                    store_error_to_audit_outcome(&e),
                    serde_json::Value::Null,
                )
                .await;
            Err(store_error_to_http(e))
        }
    }
}
