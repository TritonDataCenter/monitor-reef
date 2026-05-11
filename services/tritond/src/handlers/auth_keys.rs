// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `auth_keys` HTTP handlers (delegated to from the `TritondApi` impl).

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
use crate::service_impl::mint_token_pair;

pub(crate) async fn login(
    rqctx: RequestContext<ApiContext>,
    body: TypedBody<LoginRequest>,
) -> Result<HttpResponseOk<TokenResponse>, HttpError> {
    let ctx = rqctx.context();
    let request_id = parse_request_id(&rqctx);
    // Per-source-IP throttle. Runs before Cedar and well before
    // bcrypt so an automated guesser can't burn server CPU on
    // password verification. We use the TCP peer address; X-
    // Forwarded-For is intentionally ignored — see crate::rate_limit.
    let source_ip = rqctx.request.remote_addr().ip();
    if let Err(retry_after) = ctx.login_rate_limiter.check(source_ip) {
        ctx.audit
            .record_auth_event(
                Action::Login,
                "",
                request_id,
                AuditActor::Anonymous,
                AuditOutcome::ClientError {
                    code: 429,
                    message: format!("rate limited from {source_ip}"),
                },
            )
            .await;
        return Err(too_many_requests(retry_after));
    }
    // Cedar still gates login (the public-actions rule), partly so
    // the policy bundle is the single source of truth for what an
    // unauth'd caller can do.
    authenticate_and_authorize(&rqctx, &ctx.auth, &ctx.audit, &ctx.store, Action::Login).await?;
    let req = body.into_inner();

    let username = req.username.clone();
    let user = match ctx.store.get_user_by_username(&username).await {
        Ok(user) => user,
        Err(StoreError::NotFound) => {
            ctx.audit
                .record_auth_event(
                    Action::Login,
                    &username,
                    request_id,
                    AuditActor::Anonymous,
                    AuditOutcome::Unauthenticated {
                        reason: "unknown user".to_string(),
                    },
                )
                .await;
            return Err(invalid_credentials());
        }
        Err(e) => return Err(store_error_to_http(e)),
    };
    let password_ok = verify_password(&req.password, &user.password_hash)
        .await
        .map_err(|e| HttpError::for_internal_error(format!("verify password: {e}")))?;
    if !password_ok {
        ctx.audit
            .record_auth_event(
                Action::Login,
                &username,
                request_id,
                AuditActor::Anonymous,
                AuditOutcome::Unauthenticated {
                    reason: "bad password".to_string(),
                },
            )
            .await;
        return Err(invalid_credentials());
    }

    let response = mint_token_pair(&ctx.auth, user.id)?;
    ctx.audit
        .record_auth_event(
            Action::Login,
            &username,
            request_id,
            AuditActor::Operator {
                user_id: user.id,
                is_root: user.is_root,
            },
            AuditOutcome::Success {
                resource: Some(format!("User::\"{}\"", user.id)),
            },
        )
        .await;
    Ok(HttpResponseOk(response))
}

pub(crate) async fn refresh(
    rqctx: RequestContext<ApiContext>,
    body: TypedBody<RefreshRequest>,
) -> Result<HttpResponseOk<TokenResponse>, HttpError> {
    let ctx = rqctx.context();
    authenticate_and_authorize(&rqctx, &ctx.auth, &ctx.audit, &ctx.store, Action::Refresh).await?;
    let request_id = parse_request_id(&rqctx);
    let req = body.into_inner();

    let claims =
        verify(ctx.auth.jwt_key(), &req.refresh_token, TokenKind::Refresh).map_err(|_| {
            // We don't have a username here; record the rejection
            // with an empty username. Operators learn from the chain
            // that someone presented a bad refresh.
            let audit = ctx.audit.clone();
            let req_id = request_id;
            tokio::spawn(async move {
                audit
                    .record_auth_event(
                        Action::Refresh,
                        "",
                        req_id,
                        AuditActor::Anonymous,
                        AuditOutcome::Unauthenticated {
                            reason: "invalid refresh token".to_string(),
                        },
                    )
                    .await;
            });
            invalid_credentials()
        })?;
    // Confirm the user still exists; deactivated users can't
    // silently extend their session via stored refresh tokens.
    let user = ctx
        .store
        .get_user_by_id(claims.sub)
        .await
        .map_err(|_| invalid_credentials())?;

    let response = mint_token_pair(&ctx.auth, claims.sub)?;
    ctx.audit
        .record_auth_event(
            Action::Refresh,
            &user.username,
            request_id,
            AuditActor::Operator {
                user_id: user.id,
                is_root: user.is_root,
            },
            AuditOutcome::Success {
                resource: Some(format!("User::\"{}\"", user.id)),
            },
        )
        .await;
    Ok(HttpResponseOk(response))
}

pub(crate) async fn create_api_key(
    rqctx: RequestContext<ApiContext>,
    body: TypedBody<NewApiKey>,
) -> Result<HttpResponseCreated<ApiKeyCreated>, HttpError> {
    let ctx = rqctx.context();
    let principal = authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::CreateApiKey,
    )
    .await?;
    let (user_id, _) = require_authenticated(principal.clone())?;
    let request_id = parse_request_id(&rqctx);
    let req = body.into_inner();

    let material = generate_api_key()
        .await
        .map_err(|e| HttpError::for_internal_error(format!("generate api key: {e}")))?;
    let record = ApiKey {
        id: Uuid::new_v4(),
        user_id,
        description: req.description,
        lookup_id: material.lookup_id,
        hash: material.hash,
        scope: req.scope,
        bound_to_cn: None,
        created_at: chrono::Utc::now(),
    };
    let saved = ctx
        .store
        .create_api_key(record)
        .await
        .map_err(store_error_to_http)?;
    ctx.audit
        .record_mutation(
            &principal,
            Action::CreateApiKey,
            request_id,
            Some(format!("ApiKey::\"{}\"", saved.id)),
            AuditOutcome::Success {
                resource: Some(format!("ApiKey::\"{}\"", saved.id)),
            },
            serde_json::json!({
                "description": saved.description,
                "lookup_id": saved.lookup_id,
                "scope": saved.scope,
            }),
        )
        .await;
    let view: ApiKeyView = saved.into();
    Ok(HttpResponseCreated(ApiKeyCreated {
        key: view,
        secret: material.plaintext,
    }))
}

pub(crate) async fn list_api_keys(
    rqctx: RequestContext<ApiContext>,
) -> Result<HttpResponseOk<Vec<ApiKeyView>>, HttpError> {
    let ctx = rqctx.context();
    let principal = authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::ListApiKeys,
    )
    .await?;
    let (user_id, _) = require_authenticated(principal)?;
    let keys = ctx
        .store
        .list_api_keys(user_id)
        .await
        .map_err(store_error_to_http)?;
    Ok(HttpResponseOk(keys.into_iter().map(Into::into).collect()))
}

pub(crate) async fn delete_api_key(
    rqctx: RequestContext<ApiContext>,
    path: Path<ApiKeyPath>,
) -> Result<HttpResponseDeleted, HttpError> {
    let ctx = rqctx.context();
    let principal = authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::DeleteApiKey,
    )
    .await?;
    let (user_id, _) = require_authenticated(principal.clone())?;
    let request_id = parse_request_id(&rqctx);
    let key_id = path.into_inner().api_key_id;
    ctx.store
        .delete_api_key(user_id, key_id)
        .await
        .map_err(store_error_to_http)?;
    ctx.audit
        .record_mutation(
            &principal,
            Action::DeleteApiKey,
            request_id,
            Some(format!("ApiKey::\"{key_id}\"")),
            AuditOutcome::Success {
                resource: Some(format!("ApiKey::\"{key_id}\"")),
            },
            serde_json::Value::Null,
        )
        .await;
    Ok(HttpResponseDeleted())
}

pub(crate) async fn list_audit_events(
    rqctx: RequestContext<ApiContext>,
    query: Query<AuditListQuery>,
) -> Result<HttpResponseOk<AuditEventList>, HttpError> {
    let ctx = rqctx.context();
    authenticate_and_authorize(&rqctx, &ctx.auth, &ctx.audit, &ctx.store, Action::AuditList)
        .await?;
    let q = query.into_inner();
    let after_seq = q.after_seq.unwrap_or(0);
    let limit = q.limit.unwrap_or(100).min(1000) as usize;

    let chain = ctx.audit.chain();
    let events = chain
        .list(after_seq, limit)
        .await
        .map_err(audit_error_to_http)?;
    let head = chain.head().await.map_err(audit_error_to_http)?;
    Ok(HttpResponseOk(AuditEventList { events, head }))
}

pub(crate) async fn get_audit_event(
    rqctx: RequestContext<ApiContext>,
    path: Path<AuditEventPath>,
) -> Result<HttpResponseOk<AuditEvent>, HttpError> {
    let ctx = rqctx.context();
    authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::AuditFetch,
    )
    .await?;
    let seq = path.into_inner().seq;
    let event = ctx
        .audit
        .chain()
        .get(seq)
        .await
        .map_err(audit_error_to_http)?;
    Ok(HttpResponseOk(event))
}

pub(crate) async fn verify_audit_chain(
    rqctx: RequestContext<ApiContext>,
    query: Query<AuditVerifyQuery>,
) -> Result<HttpResponseOk<AuditVerifyResponse>, HttpError> {
    let ctx = rqctx.context();
    authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::AuditVerify,
    )
    .await?;
    let q = query.into_inner();
    let chain = ctx.audit.chain();
    let head = chain.head().await.map_err(audit_error_to_http)?;
    let from = q.from.unwrap_or(0);
    let to = q.to.unwrap_or_else(|| head.as_ref().map_or(0, |h| h.seq));
    let outcome = chain.verify(from, to).await.map_err(audit_error_to_http)?;
    Ok(HttpResponseOk(AuditVerifyResponse { outcome, head }))
}
