// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `tenants` HTTP handlers (delegated to from the `TritondApi` impl).

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

pub(crate) async fn put_tenant_idp(
    rqctx: RequestContext<ApiContext>,
    path: Path<TenantIdpPath>,
    body: TypedBody<NewIdpConfig>,
) -> Result<HttpResponseCreated<IdpConfigView>, HttpError> {
    let ctx = rqctx.context();
    authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::TenantIdpSet,
    )
    .await?;
    let tenant_id = path.into_inner().tenant_id;
    // Confirm the tenant exists; reject 404 cleanly rather
    // than dangling an IdP config off a non-existent tenant.
    ctx.store
        .get_tenant(tenant_id)
        .await
        .map_err(store_error_to_http)?;

    let req = body.into_inner();
    let config = IdpConfig {
        issuer_url: req.issuer_url,
        client_id: req.client_id,
        client_secret: req.client_secret.expose().to_string(),
        audience: req.audience,
    };

    // Eager discovery: populate the verifier cache (and prove the
    // IdP is reachable + speaks OIDC) before persisting. A failed
    // discovery returns 4xx with the upstream error.
    let oidc_cfg = OidcConfig {
        issuer_url: config.issuer_url.clone(),
        client_id: config.client_id.clone(),
        client_secret: config.client_secret.clone(),
        audience: config.audience.clone(),
    };
    ctx.auth
        .oidc()
        .discover(&tenant_id.to_string(), &oidc_cfg)
        .await
        .map_err(|e| {
            HttpError::for_client_error(
                Some("IdpUnreachable".to_string()),
                ClientErrorStatusCode::BAD_REQUEST,
                format!("idp discovery failed: {e}"),
            )
        })?;

    let saved = ctx
        .store
        .put_idp_config(tenant_id, config)
        .await
        .map_err(store_error_to_http)?;
    Ok(HttpResponseCreated(saved.into()))
}

pub(crate) async fn get_tenant_idp(
    rqctx: RequestContext<ApiContext>,
    path: Path<TenantIdpPath>,
) -> Result<HttpResponseOk<IdpConfigView>, HttpError> {
    let ctx = rqctx.context();
    authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::TenantIdpGet,
    )
    .await?;
    let tenant_id = path.into_inner().tenant_id;
    let config = ctx
        .store
        .get_idp_config(tenant_id)
        .await
        .map_err(store_error_to_http)?;
    Ok(HttpResponseOk(config.into()))
}

pub(crate) async fn delete_tenant_idp(
    rqctx: RequestContext<ApiContext>,
    path: Path<TenantIdpPath>,
) -> Result<HttpResponseDeleted, HttpError> {
    let ctx = rqctx.context();
    authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::TenantIdpDelete,
    )
    .await?;
    let tenant_id = path.into_inner().tenant_id;
    ctx.store
        .delete_idp_config(tenant_id)
        .await
        .map_err(store_error_to_http)?;
    ctx.auth.oidc().invalidate(&tenant_id.to_string()).await;
    Ok(HttpResponseDeleted())
}

pub(crate) async fn list_silo_tenants(
    rqctx: RequestContext<ApiContext>,
    path: Path<SiloPath>,
) -> Result<HttpResponseOk<Vec<Tenant>>, HttpError> {
    let ctx = rqctx.context();
    let silo_id = path.into_inner().silo_id;
    authenticate_and_authorize_in_silo(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::TenantList,
        silo_id,
    )
    .await?;
    let tenants = ctx
        .store
        .list_tenants_in_silo(silo_id)
        .await
        .map_err(store_error_to_http)?;
    Ok(HttpResponseOk(tenants))
}

pub(crate) async fn create_silo_tenant(
    rqctx: RequestContext<ApiContext>,
    path: Path<SiloPath>,
    body: TypedBody<NewTenant>,
) -> Result<HttpResponseCreated<Tenant>, HttpError> {
    let ctx = rqctx.context();
    let silo_id = path.into_inner().silo_id;
    let principal = authenticate_and_authorize_in_silo(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::TenantCreate,
        silo_id,
    )
    .await?;
    let request_id = parse_request_id(&rqctx);
    let req = body.into_inner();
    match ctx.store.create_tenant(silo_id, req).await {
        Ok(tenant) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::TenantCreate,
                    request_id,
                    Some(format!("Tenant::\"{}\"", tenant.id)),
                    AuditOutcome::Success {
                        resource: Some(format!("Tenant::\"{}\"", tenant.id)),
                    },
                    serde_json::json!({
                        "silo_id": silo_id,
                        "name": tenant.name,
                    }),
                )
                .await;
            Ok(HttpResponseCreated(tenant))
        }
        Err(e) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::TenantCreate,
                    request_id,
                    None,
                    store_error_to_audit_outcome(&e),
                    serde_json::json!({ "silo_id": silo_id }),
                )
                .await;
            Err(store_error_to_http(e))
        }
    }
}

pub(crate) async fn get_silo_tenant(
    rqctx: RequestContext<ApiContext>,
    path: Path<SiloTenantPath>,
) -> Result<HttpResponseOk<Tenant>, HttpError> {
    let ctx = rqctx.context();
    let SiloTenantPath { silo_id, tenant_id } = path.into_inner();
    authenticate_and_authorize_in_silo(
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
    // Defence-in-depth: a tenant from another silo must surface as
    // 404, not as a successful read of a sibling silo's resource.
    if tenant.silo_id != silo_id {
        return Err(not_found());
    }
    Ok(HttpResponseOk(tenant))
}

pub(crate) async fn delete_silo_tenant(
    rqctx: RequestContext<ApiContext>,
    path: Path<SiloTenantPath>,
) -> Result<HttpResponseDeleted, HttpError> {
    let ctx = rqctx.context();
    let SiloTenantPath { silo_id, tenant_id } = path.into_inner();
    let principal = authenticate_and_authorize_in_silo(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::TenantDelete,
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
        return Err(not_found());
    }
    // TODO: today's `Store::delete_tenant` is permissive — it
    // does not block the delete when child projects (or other
    // descendant resources) still exist. The block-on-children
    // guard belongs in a future cleanup so a careless operator
    // can't orphan a project graph by deleting its tenant.
    ctx.store
        .delete_tenant(tenant_id)
        .await
        .map_err(store_error_to_http)?;
    ctx.audit
        .record_mutation(
            &principal,
            Action::TenantDelete,
            request_id,
            Some(format!("Tenant::\"{tenant_id}\"")),
            AuditOutcome::Success {
                resource: Some(format!("Tenant::\"{tenant_id}\"")),
            },
            serde_json::json!({ "silo_id": silo_id }),
        )
        .await;
    Ok(HttpResponseDeleted())
}

pub(crate) async fn list_tenant_projects(
    rqctx: RequestContext<ApiContext>,
    path: Path<TenantPath>,
) -> Result<HttpResponseOk<Vec<Project>>, HttpError> {
    let ctx = rqctx.context();
    let tenant_id = path.into_inner().tenant_id;
    authenticate_and_authorize_in_tenant(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::ProjectList,
        tenant_id,
    )
    .await?;
    let projects = ctx
        .store
        .list_projects_in_tenant(tenant_id)
        .await
        .map_err(store_error_to_http)?;
    Ok(HttpResponseOk(projects))
}

pub(crate) async fn create_tenant_project(
    rqctx: RequestContext<ApiContext>,
    path: Path<TenantPath>,
    body: TypedBody<NewProject>,
) -> Result<HttpResponseCreated<Project>, HttpError> {
    let ctx = rqctx.context();
    let tenant_id = path.into_inner().tenant_id;
    let principal = authenticate_and_authorize_in_tenant(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::ProjectCreate,
        tenant_id,
    )
    .await?;
    let request_id = parse_request_id(&rqctx);
    let req = body.into_inner();
    match ctx.store.create_project(tenant_id, req).await {
        Ok(project) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::ProjectCreate,
                    request_id,
                    Some(format!("Project::\"{}\"", project.id)),
                    AuditOutcome::Success {
                        resource: Some(format!("Project::\"{}\"", project.id)),
                    },
                    serde_json::json!({
                        "tenant_id": tenant_id,
                        "name": project.name,
                    }),
                )
                .await;
            Ok(HttpResponseCreated(project))
        }
        Err(e) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::ProjectCreate,
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
