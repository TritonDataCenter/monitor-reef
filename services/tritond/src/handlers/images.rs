// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `images` HTTP handlers (delegated to from the `TritondApi` impl).

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

use crate::context::ApiContext;
use crate::scope::{image_deletable_by, image_visible_to};
use crate::service_impl::{
    audit_image_create_failure, audit_image_create_success, validate_image_request,
};

pub(crate) async fn list_public_images(
    rqctx: RequestContext<ApiContext>,
) -> Result<HttpResponseOk<Vec<Image>>, HttpError> {
    let ctx = rqctx.context();
    // Anonymous probes get through via the
    // anonymous-public-actions Cedar rule on
    // `image_list_public`. The silo / tenant / project
    // lists use `image_list` instead so unauthenticated
    // callers can't poke at scoped catalogs.
    authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::ImageListPublic,
    )
    .await?;
    let images = ctx
        .store
        .list_images_public()
        .await
        .map_err(store_error_to_http)?;
    Ok(HttpResponseOk(images))
}

pub(crate) async fn create_public_image(
    rqctx: RequestContext<ApiContext>,
    body: TypedBody<NewImage>,
) -> Result<HttpResponseCreated<Image>, HttpError> {
    let ctx = rqctx.context();
    // Cedar's authenticated-image-actions rule lets any
    // authenticated principal pass image_create at the
    // global resource so the per-URL handlers can dispatch.
    // The Public scope is operator turf, so we add an
    // explicit root check here — the audit event still
    // records the deny.
    let principal = authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::ImageCreate,
    )
    .await?;
    let request_id = parse_request_id(&rqctx);
    if !matches!(principal, Principal::Operator { is_root: true, .. }) {
        ctx.audit
            .record_mutation(
                &principal,
                Action::ImageCreate,
                request_id,
                None,
                AuditOutcome::ClientError {
                    code: 403,
                    message: "public image creation is root-only".to_string(),
                },
                serde_json::json!({ "scope": "public" }),
            )
            .await;
        return Err(HttpError::for_client_error(
            Some("Forbidden".to_string()),
            ClientErrorStatusCode::FORBIDDEN,
            "public image creation is root-only".to_string(),
        ));
    }
    let req = body.into_inner();
    if let Some(err) = validate_image_request(
        &req,
        ctx,
        &principal,
        request_id,
        serde_json::json!({ "scope": "public" }),
    )
    .await
    {
        return Err(err);
    }
    match ctx.store.create_image_public(req).await {
        Ok(image) => {
            audit_image_create_success(
                ctx,
                &principal,
                request_id,
                &image,
                serde_json::json!({ "scope": "public" }),
            )
            .await;
            Ok(HttpResponseCreated(image))
        }
        Err(e) => {
            audit_image_create_failure(ctx, &principal, request_id, &e).await;
            Err(store_error_to_http(e))
        }
    }
}

pub(crate) async fn list_silo_images(
    rqctx: RequestContext<ApiContext>,
    path: Path<SiloPath>,
) -> Result<HttpResponseOk<Vec<Image>>, HttpError> {
    let ctx = rqctx.context();
    let silo_id = path.into_inner().silo_id;
    authenticate_and_authorize_in_silo(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::ImageList,
        silo_id,
    )
    .await?;
    let images = ctx
        .store
        .list_images_in_silo(silo_id)
        .await
        .map_err(store_error_to_http)?;
    Ok(HttpResponseOk(images))
}

pub(crate) async fn create_silo_image(
    rqctx: RequestContext<ApiContext>,
    path: Path<SiloPath>,
    body: TypedBody<NewImage>,
) -> Result<HttpResponseCreated<Image>, HttpError> {
    let ctx = rqctx.context();
    let silo_id = path.into_inner().silo_id;
    let principal = authenticate_and_authorize_in_silo(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::ImageCreate,
        silo_id,
    )
    .await?;
    let request_id = parse_request_id(&rqctx);
    let req = body.into_inner();
    if let Some(err) = validate_image_request(
        &req,
        ctx,
        &principal,
        request_id,
        serde_json::json!({ "scope": "silo", "silo_id": silo_id }),
    )
    .await
    {
        return Err(err);
    }
    match ctx.store.create_image_silo(silo_id, req).await {
        Ok(image) => {
            // RFD 00004 SG-6: record the import in the saga catalog
            // so the operation surface and per-image / per-silo
            // saga views pick it up. Marker-only today.
            let imp_params = crate::sagas::image_import::ImageImportParams {
                image_id: image.id,
                silo_id: Some(silo_id),
                tenant_id: None,
                project_id: None,
            };
            if let Ok(saga_dag) = crate::sagas::image_import::build_dag(&imp_params) {
                let saga_refs = crate::sagas::image_import::build_references(&imp_params);
                let saga_id = tritond_saga::SagaId(uuid::Uuid::new_v4());
                let _ = ctx
                    .saga
                    .saga_execute(
                        saga_id,
                        crate::sagas::image_import::SAGA_NAME,
                        crate::sagas::image_import::SAGA_VERSION,
                        saga_dag,
                        &saga_refs,
                    )
                    .await;
            }
            audit_image_create_success(
                ctx,
                &principal,
                request_id,
                &image,
                serde_json::json!({ "scope": "silo", "silo_id": silo_id }),
            )
            .await;
            Ok(HttpResponseCreated(image))
        }
        Err(e) => {
            audit_image_create_failure(ctx, &principal, request_id, &e).await;
            Err(store_error_to_http(e))
        }
    }
}

pub(crate) async fn create_silo_image_from_bundle(
    rqctx: RequestContext<ApiContext>,
    path: Path<SiloPath>,
    body: TypedBody<NewImageFromBundle>,
) -> Result<HttpResponseCreated<Image>, HttpError> {
    let ctx = rqctx.context();
    let silo_id = path.into_inner().silo_id;
    let principal = authenticate_and_authorize_in_silo(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::ImageCreate,
        silo_id,
    )
    .await?;
    let request_id = parse_request_id(&rqctx);
    let req = body.into_inner();

    // Fetch + parse the bundle. Audit the failure paths so
    // operators can correlate "bundle URL was bad" against
    // their request_id.
    let new_image = match ingest_bundle(&req.bundle_url).await {
        Ok(n) => n,
        Err(e) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::ImageCreate,
                    request_id,
                    None,
                    AuditOutcome::ClientError {
                        code: 502,
                        message: format!("ingest bundle: {e:#}"),
                    },
                    serde_json::json!({
                        "silo_id": silo_id,
                        "bundle_url": req.bundle_url,
                    }),
                )
                .await;
            return Err(HttpError::for_client_error(
                Some("BadGateway".to_string()),
                ClientErrorStatusCode::BAD_REQUEST,
                format!("ingest bundle: {e:#}"),
            ));
        }
    };

    match ctx.store.create_image_silo(silo_id, new_image).await {
        Ok(image) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::ImageCreate,
                    request_id,
                    Some(format!("Image::\"{}\"", image.id)),
                    AuditOutcome::Success {
                        resource: Some(format!("Image::\"{}\"", image.id)),
                    },
                    serde_json::json!({
                        "silo_id": silo_id,
                        "name": image.name,
                        "sha256": image.sha256,
                        "bundle_url": req.bundle_url,
                    }),
                )
                .await;
            Ok(HttpResponseCreated(image))
        }
        Err(e) => {
            audit_image_create_failure(ctx, &principal, request_id, &e).await;
            Err(store_error_to_http(e))
        }
    }
}

pub(crate) async fn list_tenant_images(
    rqctx: RequestContext<ApiContext>,
    path: Path<TenantPath>,
) -> Result<HttpResponseOk<Vec<Image>>, HttpError> {
    let ctx = rqctx.context();
    let tenant_id = path.into_inner().tenant_id;
    authenticate_and_authorize_in_tenant(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::ImageList,
        tenant_id,
    )
    .await?;
    let images = ctx
        .store
        .list_visible_images_in_tenant(tenant_id)
        .await
        .map_err(store_error_to_http)?;
    Ok(HttpResponseOk(images))
}

pub(crate) async fn create_tenant_image(
    rqctx: RequestContext<ApiContext>,
    path: Path<TenantPath>,
    body: TypedBody<NewImage>,
) -> Result<HttpResponseCreated<Image>, HttpError> {
    let ctx = rqctx.context();
    let tenant_id = path.into_inner().tenant_id;
    let principal = authenticate_and_authorize_in_tenant(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::ImageCreate,
        tenant_id,
    )
    .await?;
    let request_id = parse_request_id(&rqctx);
    let req = body.into_inner();
    if let Some(err) = validate_image_request(
        &req,
        ctx,
        &principal,
        request_id,
        serde_json::json!({ "scope": "tenant", "tenant_id": tenant_id }),
    )
    .await
    {
        return Err(err);
    }
    match ctx.store.create_image_tenant(tenant_id, req).await {
        Ok(image) => {
            audit_image_create_success(
                ctx,
                &principal,
                request_id,
                &image,
                serde_json::json!({ "scope": "tenant", "tenant_id": tenant_id }),
            )
            .await;
            Ok(HttpResponseCreated(image))
        }
        Err(e) => {
            audit_image_create_failure(ctx, &principal, request_id, &e).await;
            Err(store_error_to_http(e))
        }
    }
}

pub(crate) async fn list_project_images(
    rqctx: RequestContext<ApiContext>,
    path: Path<TenantProjectPath>,
) -> Result<HttpResponseOk<Vec<Image>>, HttpError> {
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
        Action::ImageList,
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
    let images = ctx
        .store
        .list_visible_images_in_project(project_id)
        .await
        .map_err(store_error_to_http)?;
    Ok(HttpResponseOk(images))
}

pub(crate) async fn create_project_image(
    rqctx: RequestContext<ApiContext>,
    path: Path<TenantProjectPath>,
    body: TypedBody<NewImage>,
) -> Result<HttpResponseCreated<Image>, HttpError> {
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
        Action::ImageCreate,
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
    if let Some(err) = validate_image_request(
        &req,
        ctx,
        &principal,
        request_id,
        serde_json::json!({
            "scope": "project",
            "tenant_id": tenant_id,
            "project_id": project_id,
        }),
    )
    .await
    {
        return Err(err);
    }
    match ctx.store.create_image_project(project_id, req).await {
        Ok(image) => {
            audit_image_create_success(
                ctx,
                &principal,
                request_id,
                &image,
                serde_json::json!({
                    "scope": "project",
                    "tenant_id": tenant_id,
                    "project_id": project_id,
                }),
            )
            .await;
            Ok(HttpResponseCreated(image))
        }
        Err(e) => {
            audit_image_create_failure(ctx, &principal, request_id, &e).await;
            Err(store_error_to_http(e))
        }
    }
}

pub(crate) async fn list_my_images(
    rqctx: RequestContext<ApiContext>,
) -> Result<HttpResponseOk<Vec<Image>>, HttpError> {
    let ctx = rqctx.context();
    let principal =
        authenticate_and_authorize(&rqctx, &ctx.auth, &ctx.audit, &ctx.store, Action::ImageList)
            .await?;
    // /v2/auth/* requires an authenticated principal — Cedar
    // would otherwise let an Anonymous probe reach this list.
    let (user_id, _) = require_authenticated(principal)?;
    let images = ctx
        .store
        .list_images_for_user(user_id)
        .await
        .map_err(store_error_to_http)?;
    Ok(HttpResponseOk(images))
}

pub(crate) async fn create_my_image(
    rqctx: RequestContext<ApiContext>,
    body: TypedBody<NewImage>,
) -> Result<HttpResponseCreated<Image>, HttpError> {
    let ctx = rqctx.context();
    let principal = authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::ImageCreate,
    )
    .await?;
    let (user_id, _) = require_authenticated(principal.clone())?;
    let request_id = parse_request_id(&rqctx);
    let req = body.into_inner();
    if let Some(err) = validate_image_request(
        &req,
        ctx,
        &principal,
        request_id,
        serde_json::json!({ "scope": "user", "user_id": user_id }),
    )
    .await
    {
        return Err(err);
    }
    match ctx.store.create_image_user(user_id, req).await {
        Ok(image) => {
            audit_image_create_success(
                ctx,
                &principal,
                request_id,
                &image,
                serde_json::json!({ "scope": "user", "user_id": user_id }),
            )
            .await;
            Ok(HttpResponseCreated(image))
        }
        Err(e) => {
            audit_image_create_failure(ctx, &principal, request_id, &e).await;
            Err(store_error_to_http(e))
        }
    }
}

pub(crate) async fn get_image(
    rqctx: RequestContext<ApiContext>,
    path: Path<ImagePath>,
) -> Result<HttpResponseOk<Image>, HttpError> {
    let ctx = rqctx.context();
    let image_id = path.into_inner().image_id;
    // Anonymous principals can hit Public images via the
    // anonymous-public-actions Cedar rule + the visibility
    // check below; authenticated callers go through scope
    // gating in image_visible_to.
    let principal =
        authenticate_and_authorize(&rqctx, &ctx.auth, &ctx.audit, &ctx.store, Action::ImageGet)
            .await?;
    let image = ctx
        .store
        .get_image(image_id)
        .await
        .map_err(store_error_to_http)?;
    if !image_visible_to(&image, &principal, ctx.store.as_ref())
        .await
        .map_err(store_error_to_http)?
    {
        return Err(not_found());
    }
    Ok(HttpResponseOk(image))
}

pub(crate) async fn delete_image(
    rqctx: RequestContext<ApiContext>,
    path: Path<ImagePath>,
) -> Result<HttpResponseDeleted, HttpError> {
    let ctx = rqctx.context();
    let image_id = path.into_inner().image_id;
    let principal = authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::ImageDelete,
    )
    .await?;
    let request_id = parse_request_id(&rqctx);
    let image = ctx
        .store
        .get_image(image_id)
        .await
        .map_err(store_error_to_http)?;
    // Ownership gate — stricter than visibility.
    if !image_deletable_by(&image, &principal, ctx.store.as_ref())
        .await
        .map_err(store_error_to_http)?
    {
        return Err(not_found());
    }
    ctx.store
        .delete_image(image_id)
        .await
        .map_err(store_error_to_http)?;
    ctx.audit
        .record_mutation(
            &principal,
            Action::ImageDelete,
            request_id,
            Some(format!("Image::\"{image_id}\"")),
            AuditOutcome::Success {
                resource: Some(format!("Image::\"{image_id}\"")),
            },
            serde_json::json!({ "scope": image.scope }),
        )
        .await;
    Ok(HttpResponseDeleted())
}
