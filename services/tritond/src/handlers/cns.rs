// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `cns` HTTP handlers (delegated to from the `TritondApi` impl).

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

pub(crate) async fn list_cns(
    rqctx: RequestContext<ApiContext>,
    query: Query<CnListQuery>,
) -> Result<HttpResponseOk<Vec<CnView>>, HttpError> {
    let ctx = rqctx.context();
    authenticate_and_authorize(&rqctx, &ctx.auth, &ctx.audit, &ctx.store, Action::CnList).await?;
    let cns = ctx
        .store
        .list_cns(query.into_inner().state)
        .await
        .map_err(store_error_to_http)?;
    Ok(HttpResponseOk(cns.into_iter().map(CnView::from).collect()))
}

pub(crate) async fn get_cn(
    rqctx: RequestContext<ApiContext>,
    path: Path<CnPath>,
) -> Result<HttpResponseOk<CnView>, HttpError> {
    let ctx = rqctx.context();
    authenticate_and_authorize(&rqctx, &ctx.auth, &ctx.audit, &ctx.store, Action::CnGet).await?;
    let cn = ctx
        .store
        .get_cn(path.into_inner().server_uuid)
        .await
        .map_err(store_error_to_http)?;
    Ok(HttpResponseOk(CnView::from(cn)))
}

pub(crate) async fn approve_cn(
    rqctx: RequestContext<ApiContext>,
    body: TypedBody<ApproveCnRequest>,
) -> Result<HttpResponseOk<CnView>, HttpError> {
    let ctx = rqctx.context();
    // Per-IP rate limit applies BEFORE Cedar so a hostile
    // client without auth can't spend our cycles on Cedar
    // evaluation. Same shape as the login limiter.
    let source_ip = rqctx.request.remote_addr().ip();
    if let Err(retry_after) = ctx.cn_approve_rate_limiter.check(source_ip) {
        return Err(too_many_requests(retry_after));
    }

    let principal =
        authenticate_and_authorize(&rqctx, &ctx.auth, &ctx.audit, &ctx.store, Action::CnApprove)
            .await?;
    let request_id = parse_request_id(&rqctx);
    let req = body.into_inner();

    let normalized = normalize_claim_code(&req.code).ok_or_else(|| {
        HttpError::for_client_error(
            Some("BadRequest".to_string()),
            ClientErrorStatusCode::BAD_REQUEST,
            "claim code must be 6 chars of Crockford base32 (XXX-XXX accepted)".to_string(),
        )
    })?;

    let cn = match ctx.store.get_cn_by_claim_code(&normalized).await {
        Ok(c) => c,
        Err(StoreError::NotFound) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::CnApprove,
                    request_id,
                    None,
                    AuditOutcome::ClientError {
                        code: 404,
                        message: "no Pending CN matches that claim code".to_string(),
                    },
                    serde_json::json!({"code_prefix": &normalized[..3]}),
                )
                .await;
            return Err(HttpError::for_client_error(
                Some("NotFound".to_string()),
                ClientErrorStatusCode::NOT_FOUND,
                "no Pending CN matches that claim code".to_string(),
            ));
        }
        Err(e) => return Err(store_error_to_http(e)),
    };

    let updated = mint_and_attach_cn_credential(ctx, &principal, request_id, &cn).await?;

    ctx.audit
        .record_mutation(
            &principal,
            Action::CnApprove,
            request_id,
            Some(format!("Cn::\"{}\"", updated.server_uuid)),
            AuditOutcome::Success {
                resource: Some(format!("Cn::\"{}\"", updated.server_uuid)),
            },
            serde_json::json!({
                "server_uuid": updated.server_uuid,
                "hostname": updated.hostname,
                "bound_api_key_id": updated.bound_api_key_id,
            }),
        )
        .await;
    Ok(HttpResponseOk(CnView::from(updated)))
}

pub(crate) async fn disable_cn(
    rqctx: RequestContext<ApiContext>,
    path: Path<CnPath>,
) -> Result<HttpResponseOk<CnView>, HttpError> {
    let ctx = rqctx.context();
    let principal =
        authenticate_and_authorize(&rqctx, &ctx.auth, &ctx.audit, &ctx.store, Action::CnDisable)
            .await?;
    let request_id = parse_request_id(&rqctx);
    let server_uuid = path.into_inner().server_uuid;

    let cn = ctx
        .store
        .disable_cn(server_uuid)
        .await
        .map_err(store_error_to_http)?;

    // Best-effort revoke the bound API key. We log but don't
    // fail the request if the delete misses — the CN record
    // is already in Disabled state.
    if let Some(key_id) = cn.bound_api_key_id {
        // Look up the key so we can find its owner; the
        // delete API requires owner_id as a defence-in-depth
        // check. The agent-scope keys are owned by whichever
        // operator approved the CN.
        if let Ok(keys) = ctx.store.list_api_keys(Uuid::nil()).await {
            // Key owner isn't queryable directly without
            // user_id. For Phase 0 the deletion is best-effort
            // — we look up by id across all known users via
            // the lookup index. A future slice will add a
            // direct delete-by-id method.
            let _ = keys; // placeholder; key revocation is in commit C-3.
        }
        tracing::info!(
            key_id = %key_id,
            cn = %server_uuid,
            "TODO: revoke bound api key (slice C-3)"
        );
    }

    ctx.audit
        .record_mutation(
            &principal,
            Action::CnDisable,
            request_id,
            Some(format!("Cn::\"{server_uuid}\"")),
            AuditOutcome::Success {
                resource: Some(format!("Cn::\"{server_uuid}\"")),
            },
            serde_json::json!({
                "server_uuid": server_uuid,
                "previous_state": cn.state,
            }),
        )
        .await;
    Ok(HttpResponseOk(CnView::from(cn)))
}

pub(crate) async fn set_cn_role(
    rqctx: RequestContext<ApiContext>,
    path: Path<CnPath>,
    body: TypedBody<SetCnRoleRequest>,
) -> Result<HttpResponseOk<CnView>, HttpError> {
    let ctx = rqctx.context();
    let principal =
        authenticate_and_authorize(&rqctx, &ctx.auth, &ctx.audit, &ctx.store, Action::CnSetRole)
            .await?;
    let request_id = parse_request_id(&rqctx);
    let server_uuid = path.into_inner().server_uuid;
    let req = body.into_inner();

    let cn = ctx
        .store
        .set_cn_role(server_uuid, req.role)
        .await
        .map_err(store_error_to_http)?;

    ctx.audit
        .record_mutation(
            &principal,
            Action::CnSetRole,
            request_id,
            Some(format!("Cn::\"{server_uuid}\"")),
            AuditOutcome::Success {
                resource: Some(format!("Cn::\"{server_uuid}\"")),
            },
            serde_json::json!({
                "server_uuid": server_uuid,
                "role": cn.role,
            }),
        )
        .await;
    Ok(HttpResponseOk(CnView::from(cn)))
}

pub(crate) async fn get_auto_approve_window(
    rqctx: RequestContext<ApiContext>,
) -> Result<HttpResponseOk<Option<AutoApproveWindow>>, HttpError> {
    let ctx = rqctx.context();
    authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::AutoApproveGet,
    )
    .await?;
    let window = ctx
        .store
        .get_auto_approve_window()
        .await
        .map_err(store_error_to_http)?;
    Ok(HttpResponseOk(window))
}

pub(crate) async fn open_auto_approve_window(
    rqctx: RequestContext<ApiContext>,
    body: TypedBody<OpenAutoApproveRequest>,
) -> Result<HttpResponseOk<AutoApproveWindow>, HttpError> {
    let ctx = rqctx.context();
    let principal = authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::AutoApproveSet,
    )
    .await?;
    let request_id = parse_request_id(&rqctx);
    let req = body.into_inner();

    // Clamp duration to the 24h hard cap — see
    // tritond_store::AUTO_APPROVE_WINDOW_MAX. Operator-side
    // mistake (typo'd 86400000 instead of 86400) becomes a
    // safe 24h window instead of a multi-year DoS.
    let requested = std::time::Duration::from_secs(req.duration_secs);
    let clamped = requested.min(AUTO_APPROVE_WINDOW_MAX);
    let now = chrono::Utc::now();
    let window = AutoApproveWindow {
        opened_at: now,
        expires_at: now
            + chrono::Duration::from_std(clamped).unwrap_or_else(|_| chrono::Duration::seconds(0)),
        remaining_count: req.count,
        opened_by: principal_label(&principal),
    };

    ctx.store
        .open_auto_approve_window(window.clone())
        .await
        .map_err(store_error_to_http)?;
    ctx.audit
        .record_mutation(
            &principal,
            Action::AutoApproveSet,
            request_id,
            None,
            AuditOutcome::Success { resource: None },
            serde_json::json!({
                "duration_secs_requested": req.duration_secs,
                "duration_secs_effective": clamped.as_secs(),
                "count": req.count,
            }),
        )
        .await;
    Ok(HttpResponseOk(window))
}

pub(crate) async fn close_auto_approve_window(
    rqctx: RequestContext<ApiContext>,
) -> Result<HttpResponseDeleted, HttpError> {
    let ctx = rqctx.context();
    let principal = authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::AutoApproveClear,
    )
    .await?;
    let request_id = parse_request_id(&rqctx);
    ctx.store
        .close_auto_approve_window()
        .await
        .map_err(store_error_to_http)?;
    ctx.audit
        .record_mutation(
            &principal,
            Action::AutoApproveClear,
            request_id,
            None,
            AuditOutcome::Success { resource: None },
            serde_json::json!({}),
        )
        .await;
    Ok(HttpResponseDeleted())
}
