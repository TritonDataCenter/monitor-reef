// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `ssh_keys` HTTP handlers (delegated to from the `TritondApi` impl).

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
use crate::service_impl::{
    audit_ssh_key_create_failure, audit_ssh_key_create_success, parse_and_audit_ssh_key,
    ssh_key_deletable_by, ssh_key_visible_to,
};

pub(crate) async fn list_public_ssh_keys(
    rqctx: RequestContext<ApiContext>,
) -> Result<HttpResponseOk<Vec<SshKey>>, HttpError> {
    let ctx = rqctx.context();
    // Anonymous probes get through via the
    // anonymous-public-actions Cedar rule on
    // `ssh_key_list_public`. The silo / tenant / project
    // lists use `ssh_key_list` instead so unauthenticated
    // callers can't poke at scoped catalogs.
    authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::SshKeyListPublic,
    )
    .await?;
    let keys = ctx
        .store
        .list_ssh_keys_public()
        .await
        .map_err(store_error_to_http)?;
    Ok(HttpResponseOk(keys))
}

pub(crate) async fn create_public_ssh_key(
    rqctx: RequestContext<ApiContext>,
    body: TypedBody<NewSshKey>,
) -> Result<HttpResponseCreated<SshKey>, HttpError> {
    let ctx = rqctx.context();
    // Cedar's authenticated-image-global-actions rule (which
    // also covers ssh-key) lets any authenticated principal
    // pass ssh_key_create at the global resource so the
    // per-URL handlers can dispatch. The Public scope is
    // operator turf, so we add an explicit root check here —
    // the audit event still records the deny.
    let principal = authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::SshKeyCreate,
    )
    .await?;
    let request_id = parse_request_id(&rqctx);
    if !matches!(principal, Principal::Operator { is_root: true, .. }) {
        ctx.audit
            .record_mutation(
                &principal,
                Action::SshKeyCreate,
                request_id,
                None,
                AuditOutcome::ClientError {
                    code: 403,
                    message: "public ssh key creation is root-only".to_string(),
                },
                serde_json::json!({ "scope": "public" }),
            )
            .await;
        return Err(HttpError::for_client_error(
            Some("Forbidden".to_string()),
            ClientErrorStatusCode::FORBIDDEN,
            "public ssh key creation is root-only".to_string(),
        ));
    }
    let req = body.into_inner();
    let fingerprint = match parse_and_audit_ssh_key(
        ctx,
        &principal,
        request_id,
        &req,
        serde_json::json!({ "scope": "public" }),
    )
    .await
    {
        Ok(fp) => fp,
        Err(err) => return Err(err),
    };
    match ctx.store.create_ssh_key_public(req, fingerprint).await {
        Ok(key) => {
            audit_ssh_key_create_success(
                ctx,
                &principal,
                request_id,
                &key,
                serde_json::json!({ "scope": "public" }),
            )
            .await;
            Ok(HttpResponseCreated(key))
        }
        Err(e) => {
            audit_ssh_key_create_failure(ctx, &principal, request_id, &e).await;
            Err(store_error_to_http(e))
        }
    }
}

pub(crate) async fn list_silo_ssh_keys(
    rqctx: RequestContext<ApiContext>,
    path: Path<SiloPath>,
) -> Result<HttpResponseOk<Vec<SshKey>>, HttpError> {
    let ctx = rqctx.context();
    let silo_id = path.into_inner().silo_id;
    authenticate_and_authorize_in_silo(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::SshKeyList,
        silo_id,
    )
    .await?;
    let keys = ctx
        .store
        .list_ssh_keys_in_silo(silo_id)
        .await
        .map_err(store_error_to_http)?;
    Ok(HttpResponseOk(keys))
}

pub(crate) async fn create_silo_ssh_key(
    rqctx: RequestContext<ApiContext>,
    path: Path<SiloPath>,
    body: TypedBody<NewSshKey>,
) -> Result<HttpResponseCreated<SshKey>, HttpError> {
    let ctx = rqctx.context();
    let silo_id = path.into_inner().silo_id;
    let principal = authenticate_and_authorize_in_silo(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::SshKeyCreate,
        silo_id,
    )
    .await?;
    let request_id = parse_request_id(&rqctx);
    let req = body.into_inner();
    let fingerprint = match parse_and_audit_ssh_key(
        ctx,
        &principal,
        request_id,
        &req,
        serde_json::json!({ "scope": "silo", "silo_id": silo_id }),
    )
    .await
    {
        Ok(fp) => fp,
        Err(err) => return Err(err),
    };
    match ctx
        .store
        .create_ssh_key_silo(silo_id, req, fingerprint)
        .await
    {
        Ok(key) => {
            audit_ssh_key_create_success(
                ctx,
                &principal,
                request_id,
                &key,
                serde_json::json!({ "scope": "silo", "silo_id": silo_id }),
            )
            .await;
            Ok(HttpResponseCreated(key))
        }
        Err(e) => {
            audit_ssh_key_create_failure(ctx, &principal, request_id, &e).await;
            Err(store_error_to_http(e))
        }
    }
}

pub(crate) async fn list_tenant_ssh_keys(
    rqctx: RequestContext<ApiContext>,
    path: Path<TenantPath>,
) -> Result<HttpResponseOk<Vec<SshKey>>, HttpError> {
    let ctx = rqctx.context();
    let tenant_id = path.into_inner().tenant_id;
    authenticate_and_authorize_in_tenant(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::SshKeyList,
        tenant_id,
    )
    .await?;
    let keys = ctx
        .store
        .list_visible_ssh_keys_in_tenant(tenant_id)
        .await
        .map_err(store_error_to_http)?;
    Ok(HttpResponseOk(keys))
}

pub(crate) async fn create_tenant_ssh_key(
    rqctx: RequestContext<ApiContext>,
    path: Path<TenantPath>,
    body: TypedBody<NewSshKey>,
) -> Result<HttpResponseCreated<SshKey>, HttpError> {
    let ctx = rqctx.context();
    let tenant_id = path.into_inner().tenant_id;
    let principal = authenticate_and_authorize_in_tenant(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::SshKeyCreate,
        tenant_id,
    )
    .await?;
    let request_id = parse_request_id(&rqctx);
    let req = body.into_inner();
    let fingerprint = match parse_and_audit_ssh_key(
        ctx,
        &principal,
        request_id,
        &req,
        serde_json::json!({ "scope": "tenant", "tenant_id": tenant_id }),
    )
    .await
    {
        Ok(fp) => fp,
        Err(err) => return Err(err),
    };
    match ctx
        .store
        .create_ssh_key_tenant(tenant_id, req, fingerprint)
        .await
    {
        Ok(key) => {
            audit_ssh_key_create_success(
                ctx,
                &principal,
                request_id,
                &key,
                serde_json::json!({ "scope": "tenant", "tenant_id": tenant_id }),
            )
            .await;
            Ok(HttpResponseCreated(key))
        }
        Err(e) => {
            audit_ssh_key_create_failure(ctx, &principal, request_id, &e).await;
            Err(store_error_to_http(e))
        }
    }
}

pub(crate) async fn list_project_ssh_keys(
    rqctx: RequestContext<ApiContext>,
    path: Path<TenantProjectPath>,
) -> Result<HttpResponseOk<Vec<SshKey>>, HttpError> {
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
        Action::SshKeyList,
        tenant_id,
    )
    .await?;
    // Project must exist and live in this tenant.
    let project = ctx
        .store
        .get_project(project_id)
        .await
        .map_err(store_error_to_http)?;
    if project.tenant_id != tenant_id {
        return Err(not_found());
    }
    let keys = ctx
        .store
        .list_visible_ssh_keys_in_project(project_id)
        .await
        .map_err(store_error_to_http)?;
    Ok(HttpResponseOk(keys))
}

pub(crate) async fn create_project_ssh_key(
    rqctx: RequestContext<ApiContext>,
    path: Path<TenantProjectPath>,
    body: TypedBody<NewSshKey>,
) -> Result<HttpResponseCreated<SshKey>, HttpError> {
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
        Action::SshKeyCreate,
        tenant_id,
    )
    .await?;
    let request_id = parse_request_id(&rqctx);
    // Verify the project belongs to the tenant before the
    // store call (defence in depth; cross-tenant probe
    // surfaces as 404).
    let project = ctx
        .store
        .get_project(project_id)
        .await
        .map_err(store_error_to_http)?;
    if project.tenant_id != tenant_id {
        return Err(not_found());
    }
    let req = body.into_inner();
    let fingerprint = match parse_and_audit_ssh_key(
        ctx,
        &principal,
        request_id,
        &req,
        serde_json::json!({
            "scope": "project",
            "tenant_id": tenant_id,
            "project_id": project_id,
        }),
    )
    .await
    {
        Ok(fp) => fp,
        Err(err) => return Err(err),
    };
    match ctx
        .store
        .create_ssh_key_project(project_id, req, fingerprint)
        .await
    {
        Ok(key) => {
            audit_ssh_key_create_success(
                ctx,
                &principal,
                request_id,
                &key,
                serde_json::json!({
                    "scope": "project",
                    "tenant_id": tenant_id,
                    "project_id": project_id,
                }),
            )
            .await;
            Ok(HttpResponseCreated(key))
        }
        Err(e) => {
            audit_ssh_key_create_failure(ctx, &principal, request_id, &e).await;
            Err(store_error_to_http(e))
        }
    }
}

pub(crate) async fn list_my_ssh_keys(
    rqctx: RequestContext<ApiContext>,
) -> Result<HttpResponseOk<Vec<SshKey>>, HttpError> {
    let ctx = rqctx.context();
    let principal = authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::SshKeyList,
    )
    .await?;
    // /v2/auth/* requires an authenticated principal — Cedar
    // would otherwise let an Anonymous probe reach this list.
    let (user_id, _) = require_authenticated(principal)?;
    let keys = ctx
        .store
        .list_ssh_keys_for_user(user_id)
        .await
        .map_err(store_error_to_http)?;
    Ok(HttpResponseOk(keys))
}

pub(crate) async fn create_my_ssh_key(
    rqctx: RequestContext<ApiContext>,
    body: TypedBody<NewSshKey>,
) -> Result<HttpResponseCreated<SshKey>, HttpError> {
    let ctx = rqctx.context();
    let principal = authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::SshKeyCreate,
    )
    .await?;
    let (user_id, _) = require_authenticated(principal.clone())?;
    let request_id = parse_request_id(&rqctx);
    let req = body.into_inner();
    let fingerprint = match parse_and_audit_ssh_key(
        ctx,
        &principal,
        request_id,
        &req,
        serde_json::json!({ "scope": "user", "user_id": user_id }),
    )
    .await
    {
        Ok(fp) => fp,
        Err(err) => return Err(err),
    };
    match ctx
        .store
        .create_ssh_key_user(user_id, req, fingerprint)
        .await
    {
        Ok(key) => {
            audit_ssh_key_create_success(
                ctx,
                &principal,
                request_id,
                &key,
                serde_json::json!({ "scope": "user", "user_id": user_id }),
            )
            .await;
            Ok(HttpResponseCreated(key))
        }
        Err(e) => {
            audit_ssh_key_create_failure(ctx, &principal, request_id, &e).await;
            Err(store_error_to_http(e))
        }
    }
}

pub(crate) async fn get_ssh_key(
    rqctx: RequestContext<ApiContext>,
    path: Path<SshKeyPath>,
) -> Result<HttpResponseOk<SshKey>, HttpError> {
    let ctx = rqctx.context();
    let key_id = path.into_inner().key_id;
    // Anonymous principals can hit Public ssh keys via the
    // anonymous-public-actions Cedar rule + the visibility
    // check below; authenticated callers go through scope
    // gating in ssh_key_visible_to.
    let principal =
        authenticate_and_authorize(&rqctx, &ctx.auth, &ctx.audit, &ctx.store, Action::SshKeyGet)
            .await?;
    let key = ctx
        .store
        .get_ssh_key(key_id)
        .await
        .map_err(store_error_to_http)?;
    if !ssh_key_visible_to(&key, &principal, ctx.store.as_ref())
        .await
        .map_err(store_error_to_http)?
    {
        return Err(not_found());
    }
    Ok(HttpResponseOk(key))
}

pub(crate) async fn delete_ssh_key(
    rqctx: RequestContext<ApiContext>,
    path: Path<SshKeyPath>,
) -> Result<HttpResponseDeleted, HttpError> {
    let ctx = rqctx.context();
    let key_id = path.into_inner().key_id;
    let principal = authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::SshKeyDelete,
    )
    .await?;
    let request_id = parse_request_id(&rqctx);
    let key = ctx
        .store
        .get_ssh_key(key_id)
        .await
        .map_err(store_error_to_http)?;
    // Ownership gate — stricter than visibility.
    if !ssh_key_deletable_by(&key, &principal, ctx.store.as_ref())
        .await
        .map_err(store_error_to_http)?
    {
        return Err(not_found());
    }
    ctx.store
        .delete_ssh_key(key_id)
        .await
        .map_err(store_error_to_http)?;
    ctx.audit
        .record_mutation(
            &principal,
            Action::SshKeyDelete,
            request_id,
            Some(format!("SshKey::\"{key_id}\"")),
            AuditOutcome::Success {
                resource: Some(format!("SshKey::\"{key_id}\"")),
            },
            serde_json::json!({ "scope": key.scope }),
        )
        .await;
    Ok(HttpResponseDeleted())
}
