// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! The `TritondApi` trait implementation: every HTTP handler, the
//! cross-scope visibility helpers they share, the `ApiDescription`
//! builder, and the Dropshot server bootstrap. Helpers that don't
//! belong to a single handler family live in the sibling modules
//! (`error`, `principal`, `validate`, `lifecycle`, `cn_credential`,
//! `blueprint`, `edge_cluster`, `bundle`).

use crate::blueprint::*;
use crate::bundle::*;
use crate::cn_credential::*;
use crate::error::*;
use crate::lifecycle::*;
use crate::principal::*;
use crate::validate::*;
use crate::{dhcp_reconciler, provisioner, sweeper};

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

/// Concrete implementor of [`TritondApi`].
pub enum TritondServiceImpl {}

impl TritondApi for TritondServiceImpl {
    type Context = ApiContext;

    async fn health(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseOk<HealthResponse>, HttpError> {
        let ctx = rqctx.context();
        authenticate_and_authorize(&rqctx, &ctx.auth, &ctx.audit, &ctx.store, Action::Health)
            .await?;
        Ok(HttpResponseOk(HealthResponse {
            status: "ok".to_string(),
            version: VERSION.to_string(),
        }))
    }

    async fn create_silo(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<NewSilo>,
    ) -> Result<HttpResponseCreated<Silo>, HttpError> {
        let ctx = rqctx.context();
        let principal = authenticate_and_authorize(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::CreateSilo,
        )
        .await?;
        let request_id = parse_request_id(&rqctx);
        let req = body.into_inner();
        match ctx.store.create_silo(req).await {
            Ok(silo) => {
                ctx.audit
                    .record_mutation(
                        &principal,
                        Action::CreateSilo,
                        request_id,
                        Some(format!("Silo::\"{}\"", silo.id)),
                        AuditOutcome::Success {
                            resource: Some(format!("Silo::\"{}\"", silo.id)),
                        },
                        serde_json::json!({ "name": silo.name }),
                    )
                    .await;
                Ok(HttpResponseCreated(silo))
            }
            Err(e) => {
                ctx.audit
                    .record_mutation(
                        &principal,
                        Action::CreateSilo,
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

    async fn list_silos(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseOk<Vec<Silo>>, HttpError> {
        let ctx = rqctx.context();
        authenticate_and_authorize(&rqctx, &ctx.auth, &ctx.audit, &ctx.store, Action::SiloList)
            .await?;
        let silos = ctx.store.list_silos().await.map_err(store_error_to_http)?;
        Ok(HttpResponseOk(silos))
    }

    async fn get_silo(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloPath>,
    ) -> Result<HttpResponseOk<Silo>, HttpError> {
        let ctx = rqctx.context();
        authenticate_and_authorize(&rqctx, &ctx.auth, &ctx.audit, &ctx.store, Action::GetSilo)
            .await?;
        let silo_id = path.into_inner().silo_id;
        let silo = ctx
            .store
            .get_silo(silo_id)
            .await
            .map_err(store_error_to_http)?;
        Ok(HttpResponseOk(silo))
    }

    async fn login(
        rqctx: RequestContext<Self::Context>,
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
        authenticate_and_authorize(&rqctx, &ctx.auth, &ctx.audit, &ctx.store, Action::Login)
            .await?;
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

    async fn refresh(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<RefreshRequest>,
    ) -> Result<HttpResponseOk<TokenResponse>, HttpError> {
        let ctx = rqctx.context();
        authenticate_and_authorize(&rqctx, &ctx.auth, &ctx.audit, &ctx.store, Action::Refresh)
            .await?;
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

    async fn create_api_key(
        rqctx: RequestContext<Self::Context>,
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

    async fn list_api_keys(
        rqctx: RequestContext<Self::Context>,
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

    async fn delete_api_key(
        rqctx: RequestContext<Self::Context>,
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

    async fn list_audit_events(
        rqctx: RequestContext<Self::Context>,
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

    async fn get_audit_event(
        rqctx: RequestContext<Self::Context>,
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

    async fn verify_audit_chain(
        rqctx: RequestContext<Self::Context>,
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

    async fn agent_claim_job(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<ClaimJobRequest>,
    ) -> Result<HttpResponseOk<ClaimJobResponse>, HttpError> {
        let ctx = rqctx.context();
        let principal = authenticate_and_authorize(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::AgentClaim,
        )
        .await?;
        let request_id = parse_request_id(&rqctx);
        let req = body.into_inner();
        // Per-CN binding: a key minted for CN-A cannot claim as
        // CN-B. The string `claimed_by` must parse as the bound
        // server_uuid. Unbound keys (operator-minted) skip the
        // check; their `claimed_by` stays free-text.
        if let Some(bound) = crate::auth::principal_bound_cn(&principal) {
            let claimed_uuid = Uuid::parse_str(&req.claimed_by).map_err(|_| {
                HttpError::for_client_error(
                    Some("Forbidden".to_string()),
                    ClientErrorStatusCode::FORBIDDEN,
                    "bound api key requires claimed_by to be a uuid".to_string(),
                )
            })?;
            crate::auth::enforce_cn_binding(Some(bound), claimed_uuid)?;
        }
        // The store returns NotFound when the queue is empty; we
        // turn that into the wire-level "no work" signal so the
        // agent can poll on a timer without 404 noise.
        // Pass the bound CN through as the claimer identity.
        // Unbound claimers (the in-process stub or a legacy
        // operator-minted Agent key) get only unrouted jobs.
        let claimer_cn = crate::auth::principal_bound_cn(&principal);
        let job = match ctx.store.claim_next_job(&req.claimed_by, claimer_cn).await {
            Ok(job) => Some(job),
            Err(StoreError::NotFound) => None,
            Err(e) => return Err(store_error_to_http(e)),
        };
        // Audit only successful claims — empty-queue polls are noise.
        if let Some(j) = &job {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::AgentClaim,
                    request_id,
                    Some(format!("ProvisioningJob::\"{}\"", j.id)),
                    AuditOutcome::Success {
                        resource: Some(format!("ProvisioningJob::\"{}\"", j.id)),
                    },
                    serde_json::json!({
                        "job_id": j.id,
                        "claimed_by": req.claimed_by,
                        "kind": j.kind,
                    }),
                )
                .await;
            // Drive the instance lifecycle forward. For a Provision
            // job this advances Pending → Provisioning so operators
            // see the in-flight state. Stop / Restart already moved
            // the instance to Stopping in the operator-facing
            // handler, so claim has nothing to advance there. CAS
            // failures (instance gone, lifecycle drift) are logged
            // but don't fail the claim — the agent has the job and
            // will fail at vmadm time if the instance really is
            // gone, surfacing a clean Failed back to the operator.
            drive_lifecycle_for_claim(ctx.store.as_ref(), j).await;
        }
        Ok(HttpResponseOk(ClaimJobResponse { job }))
    }

    async fn agent_job_blueprint(
        rqctx: RequestContext<Self::Context>,
        path: Path<AgentJobPath>,
    ) -> Result<HttpResponseOk<ProvisioningBlueprint>, HttpError> {
        let ctx = rqctx.context();
        let principal = authenticate_and_authorize(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::AgentBlueprint,
        )
        .await?;
        let job_id = path.into_inner().job_id;
        let job = ctx
            .store
            .get_job(job_id)
            .await
            .map_err(store_error_to_http)?;
        // Per-CN binding: a bound key may only fetch blueprints
        // for jobs it itself claimed. Unbound keys see anything.
        if let Some(bound) = crate::auth::principal_bound_cn(&principal) {
            enforce_job_belongs_to_bound_cn(&job, bound)?;
        }
        let blueprint = build_blueprint(ctx.store.as_ref(), &ctx.identity_hmac_key, &job).await?;
        Ok(HttpResponseOk(blueprint))
    }

    async fn agent_port_blueprint(
        rqctx: RequestContext<Self::Context>,
        path: Path<AgentPortBlueprintPath>,
    ) -> Result<HttpResponseOk<AgentPortBlueprint>, HttpError> {
        let ctx = rqctx.context();
        let principal = authenticate_and_authorize(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::AgentBlueprint,
        )
        .await?;
        let bound_cn = require_bound_cn(&principal)?;
        let port_id = path.into_inner().port_id;
        let blueprint = build_port_blueprint(ctx.store.as_ref(), port_id, bound_cn).await?;
        Ok(HttpResponseOk(blueprint))
    }

    async fn agent_complete_job(
        rqctx: RequestContext<Self::Context>,
        path: Path<AgentJobPath>,
        body: TypedBody<CompleteJobRequest>,
    ) -> Result<HttpResponseOk<ProvisioningJob>, HttpError> {
        let ctx = rqctx.context();
        let principal = authenticate_and_authorize(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::AgentComplete,
        )
        .await?;
        let request_id = parse_request_id(&rqctx);
        let job_id = path.into_inner().job_id;
        let req = body.into_inner();
        // Per-CN binding: a bound key may only complete jobs it
        // itself claimed. We look up the job, check the binding,
        // and only then issue the terminal write.
        if let Some(bound) = crate::auth::principal_bound_cn(&principal) {
            let job = ctx
                .store
                .get_job(job_id)
                .await
                .map_err(store_error_to_http)?;
            enforce_job_belongs_to_bound_cn(&job, bound)?;
        }
        let outcome_label = match &req.outcome {
            JobOutcome::Completed => "completed",
            JobOutcome::Failed { .. } => "failed",
            _ => "unknown",
        };
        match ctx.store.complete_job(job_id, req.outcome.clone()).await {
            Ok(updated) => {
                ctx.audit
                    .record_mutation(
                        &principal,
                        Action::AgentComplete,
                        request_id,
                        Some(format!("ProvisioningJob::\"{job_id}\"")),
                        AuditOutcome::Success {
                            resource: Some(format!("ProvisioningJob::\"{job_id}\"")),
                        },
                        serde_json::json!({
                            "job_id": job_id,
                            "outcome": outcome_label,
                        }),
                    )
                    .await;
                // Drive the instance lifecycle to its terminal
                // state for this job. Provisioning → Running on
                // success; Stopping → Stopped (or Running for
                // Restart); any → Failed{reason} on failure. The
                // job is already terminal regardless of whether
                // the lifecycle CAS succeeds, so a stale or
                // missing instance just gets logged.
                drive_lifecycle_for_complete(ctx.store.as_ref(), &updated, &req.outcome).await;
                Ok(HttpResponseOk(updated))
            }
            Err(e) => {
                ctx.audit
                    .record_mutation(
                        &principal,
                        Action::AgentComplete,
                        request_id,
                        None,
                        store_error_to_audit_outcome(&e),
                        serde_json::json!({
                            "job_id": job_id,
                            "outcome": outcome_label,
                        }),
                    )
                    .await;
                Err(store_error_to_http(e))
            }
        }
    }

    async fn put_tenant_idp(
        rqctx: RequestContext<Self::Context>,
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

    async fn get_tenant_idp(
        rqctx: RequestContext<Self::Context>,
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

    async fn delete_tenant_idp(
        rqctx: RequestContext<Self::Context>,
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

    async fn list_silo_tenants(
        rqctx: RequestContext<Self::Context>,
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

    async fn create_silo_tenant(
        rqctx: RequestContext<Self::Context>,
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

    async fn get_silo_tenant(
        rqctx: RequestContext<Self::Context>,
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

    async fn delete_silo_tenant(
        rqctx: RequestContext<Self::Context>,
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

    async fn list_tenant_projects(
        rqctx: RequestContext<Self::Context>,
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

    async fn create_tenant_project(
        rqctx: RequestContext<Self::Context>,
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

    async fn get_tenant_project(
        rqctx: RequestContext<Self::Context>,
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

    async fn delete_tenant_project(
        rqctx: RequestContext<Self::Context>,
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

    async fn list_project_vpcs(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectPath>,
    ) -> Result<HttpResponseOk<Vec<Vpc>>, HttpError> {
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
            Action::VpcList,
            tenant_id,
        )
        .await?;

        // Verify the project actually lives in the path's tenant. A
        // project_id that names some other tenant's project is treated
        // as not-found; this stops cross-tenant enumeration via the
        // VPC list endpoint.
        let project = ctx
            .store
            .get_project(project_id)
            .await
            .map_err(store_error_to_http)?;
        if project.tenant_id != tenant_id {
            return Err(not_found());
        }
        let vpcs = ctx
            .store
            .list_vpcs_in_project(project_id)
            .await
            .map_err(store_error_to_http)?;
        Ok(HttpResponseOk(vpcs))
    }

    async fn create_project_vpc(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectPath>,
        body: TypedBody<NewVpc>,
    ) -> Result<HttpResponseCreated<Vpc>, HttpError> {
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
            Action::VpcCreate,
            tenant_id,
        )
        .await?;
        let request_id = parse_request_id(&rqctx);
        let req = body.into_inner();

        // At least one IP family is required (matches OPTE's IpCfg
        // enum: Ipv4, Ipv6, or DualStack — never neither). Reject at
        // the API edge so the store doesn't have to re-validate.
        if req.ipv4_block.is_none() && req.ipv6_block.is_none() {
            let outcome = AuditOutcome::ClientError {
                code: 400,
                message: "vpc must specify ipv4_block, ipv6_block, or both".to_string(),
            };
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::VpcCreate,
                    request_id,
                    None,
                    outcome,
                    serde_json::json!({ "tenant_id": tenant_id, "project_id": project_id }),
                )
                .await;
            return Err(HttpError::for_bad_request(
                Some("BadRequest".to_string()),
                "vpc must specify ipv4_block, ipv6_block, or both".to_string(),
            ));
        }

        match ctx.store.create_vpc(tenant_id, project_id, req).await {
            Ok(vpc) => {
                ctx.audit
                    .record_mutation(
                        &principal,
                        Action::VpcCreate,
                        request_id,
                        Some(format!("Vpc::\"{}\"", vpc.id)),
                        AuditOutcome::Success {
                            resource: Some(format!("Vpc::\"{}\"", vpc.id)),
                        },
                        serde_json::json!({
                            "tenant_id": tenant_id,
                            "project_id": project_id,
                            "name": vpc.name,
                            "vni": vpc.vni,
                        }),
                    )
                    .await;
                Ok(HttpResponseCreated(vpc))
            }
            Err(e) => {
                ctx.audit
                    .record_mutation(
                        &principal,
                        Action::VpcCreate,
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

    async fn get_project_vpc(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcPath>,
    ) -> Result<HttpResponseOk<Vpc>, HttpError> {
        let ctx = rqctx.context();
        let TenantProjectVpcPath {
            tenant_id,
            project_id,
            vpc_id,
        } = path.into_inner();
        authenticate_and_authorize_in_tenant(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::VpcGet,
            tenant_id,
        )
        .await?;
        let vpc = ctx
            .store
            .get_vpc(vpc_id)
            .await
            .map_err(store_error_to_http)?;
        // Defence-in-depth: the VPC must live in *both* the path's
        // silo and the path's project. Mismatch on either dimension is
        // a 404 so cross-tenant probes don't learn the resource exists
        // somewhere else.
        if vpc.tenant_id != tenant_id || vpc.project_id != project_id {
            return Err(not_found());
        }
        Ok(HttpResponseOk(vpc))
    }

    async fn delete_project_vpc(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcPath>,
    ) -> Result<HttpResponseDeleted, HttpError> {
        let ctx = rqctx.context();
        let TenantProjectVpcPath {
            tenant_id,
            project_id,
            vpc_id,
        } = path.into_inner();
        let principal = authenticate_and_authorize_in_tenant(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::VpcDelete,
            tenant_id,
        )
        .await?;
        let request_id = parse_request_id(&rqctx);

        // Same defence-in-depth shape as get_project_vpc.
        let vpc = ctx
            .store
            .get_vpc(vpc_id)
            .await
            .map_err(store_error_to_http)?;
        if vpc.tenant_id != tenant_id || vpc.project_id != project_id {
            return Err(not_found());
        }
        match ctx.store.delete_vpc(vpc_id).await {
            Ok(()) => {
                ctx.audit
                    .record_mutation(
                        &principal,
                        Action::VpcDelete,
                        request_id,
                        Some(format!("Vpc::\"{vpc_id}\"")),
                        AuditOutcome::Success {
                            resource: Some(format!("Vpc::\"{vpc_id}\"")),
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
                        Action::VpcDelete,
                        request_id,
                        Some(format!("Vpc::\"{vpc_id}\"")),
                        store_error_to_audit_outcome(&e),
                        serde_json::Value::Null,
                    )
                    .await;
                Err(store_error_to_http(e))
            }
        }
    }

    async fn list_vpc_subnets(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcPath>,
    ) -> Result<HttpResponseOk<Vec<Subnet>>, HttpError> {
        let ctx = rqctx.context();
        let TenantProjectVpcPath {
            tenant_id,
            project_id,
            vpc_id,
        } = path.into_inner();
        authenticate_and_authorize_in_tenant(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::SubnetList,
            tenant_id,
        )
        .await?;

        // Verify the parent VPC actually lives under the path's
        // silo+project. Cross-silo or cross-project list paths must
        // 404 — the cross-tenant enumeration invariant extends to
        // VPCs the way it does for projects in `list_project_vpcs`.
        let vpc = ctx
            .store
            .get_vpc(vpc_id)
            .await
            .map_err(store_error_to_http)?;
        if vpc.tenant_id != tenant_id || vpc.project_id != project_id {
            return Err(not_found());
        }
        let subnets = ctx
            .store
            .list_subnets_in_vpc(vpc_id)
            .await
            .map_err(store_error_to_http)?;
        Ok(HttpResponseOk(subnets))
    }

    async fn create_vpc_subnet(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcPath>,
        body: TypedBody<NewSubnet>,
    ) -> Result<HttpResponseCreated<Subnet>, HttpError> {
        let ctx = rqctx.context();
        let TenantProjectVpcPath {
            tenant_id,
            project_id,
            vpc_id,
        } = path.into_inner();
        let principal = authenticate_and_authorize_in_tenant(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::SubnetCreate,
            tenant_id,
        )
        .await?;
        let request_id = parse_request_id(&rqctx);
        let req = body.into_inner();

        // At least one IP family is required, mirroring the VPC
        // create-time invariant. Same OPTE rationale: an `IpCfg`
        // must be Ipv4, Ipv6, or DualStack.
        if req.ipv4_block.is_none() && req.ipv6_block.is_none() {
            let outcome = AuditOutcome::ClientError {
                code: 400,
                message: "subnet must specify ipv4_block, ipv6_block, or both".to_string(),
            };
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::SubnetCreate,
                    request_id,
                    None,
                    outcome,
                    serde_json::json!({
                        "tenant_id": tenant_id,
                        "project_id": project_id,
                        "vpc_id": vpc_id,
                    }),
                )
                .await;
            return Err(HttpError::for_bad_request(
                Some("BadRequest".to_string()),
                "subnet must specify ipv4_block, ipv6_block, or both".to_string(),
            ));
        }

        match ctx
            .store
            .create_subnet(tenant_id, project_id, vpc_id, req)
            .await
        {
            Ok(subnet) => {
                ctx.audit
                    .record_mutation(
                        &principal,
                        Action::SubnetCreate,
                        request_id,
                        Some(format!("Subnet::\"{}\"", subnet.id)),
                        AuditOutcome::Success {
                            resource: Some(format!("Subnet::\"{}\"", subnet.id)),
                        },
                        serde_json::json!({
                            "tenant_id": tenant_id,
                            "project_id": project_id,
                            "vpc_id": vpc_id,
                            "name": subnet.name,
                        }),
                    )
                    .await;
                Ok(HttpResponseCreated(subnet))
            }
            Err(e) => {
                ctx.audit
                    .record_mutation(
                        &principal,
                        Action::SubnetCreate,
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

    async fn get_vpc_subnet(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcSubnetPath>,
    ) -> Result<HttpResponseOk<Subnet>, HttpError> {
        let ctx = rqctx.context();
        let TenantProjectVpcSubnetPath {
            tenant_id,
            project_id,
            vpc_id,
            subnet_id,
        } = path.into_inner();
        authenticate_and_authorize_in_tenant(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::SubnetGet,
            tenant_id,
        )
        .await?;
        let subnet = ctx
            .store
            .get_subnet(subnet_id)
            .await
            .map_err(store_error_to_http)?;
        // Defence-in-depth: subnet must live in path silo + project + vpc.
        if subnet.tenant_id != tenant_id
            || subnet.project_id != project_id
            || subnet.vpc_id != vpc_id
        {
            return Err(not_found());
        }
        Ok(HttpResponseOk(subnet))
    }

    async fn delete_vpc_subnet(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcSubnetPath>,
    ) -> Result<HttpResponseDeleted, HttpError> {
        let ctx = rqctx.context();
        let TenantProjectVpcSubnetPath {
            tenant_id,
            project_id,
            vpc_id,
            subnet_id,
        } = path.into_inner();
        let principal = authenticate_and_authorize_in_tenant(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::SubnetDelete,
            tenant_id,
        )
        .await?;
        let request_id = parse_request_id(&rqctx);

        let subnet = ctx
            .store
            .get_subnet(subnet_id)
            .await
            .map_err(store_error_to_http)?;
        if subnet.tenant_id != tenant_id
            || subnet.project_id != project_id
            || subnet.vpc_id != vpc_id
        {
            return Err(not_found());
        }
        ctx.store
            .delete_subnet(subnet_id)
            .await
            .map_err(store_error_to_http)?;
        ctx.audit
            .record_mutation(
                &principal,
                Action::SubnetDelete,
                request_id,
                Some(format!("Subnet::\"{subnet_id}\"")),
                AuditOutcome::Success {
                    resource: Some(format!("Subnet::\"{subnet_id}\"")),
                },
                serde_json::json!({
                    "tenant_id": tenant_id,
                    "project_id": project_id,
                    "vpc_id": vpc_id,
                }),
            )
            .await;
        Ok(HttpResponseDeleted())
    }

    async fn list_vpc_route_tables(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcPath>,
    ) -> Result<HttpResponseOk<Vec<RouteTable>>, HttpError> {
        let ctx = rqctx.context();
        let TenantProjectVpcPath {
            tenant_id,
            project_id,
            vpc_id,
        } = path.into_inner();
        authenticate_and_authorize_in_tenant(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::RouteTableList,
            tenant_id,
        )
        .await?;

        let vpc = ctx
            .store
            .get_vpc(vpc_id)
            .await
            .map_err(store_error_to_http)?;
        if vpc.tenant_id != tenant_id || vpc.project_id != project_id {
            return Err(not_found());
        }
        let route_tables = ctx
            .store
            .list_route_tables_in_vpc(vpc_id)
            .await
            .map_err(store_error_to_http)?;
        Ok(HttpResponseOk(route_tables))
    }

    async fn create_vpc_route_table(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcPath>,
        body: TypedBody<NewRouteTable>,
    ) -> Result<HttpResponseCreated<RouteTable>, HttpError> {
        let ctx = rqctx.context();
        let TenantProjectVpcPath {
            tenant_id,
            project_id,
            vpc_id,
        } = path.into_inner();
        let principal = authenticate_and_authorize_in_tenant(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::RouteTableCreate,
            tenant_id,
        )
        .await?;
        let request_id = parse_request_id(&rqctx);
        let req = body.into_inner();

        match ctx
            .store
            .create_route_table(tenant_id, project_id, vpc_id, req)
            .await
        {
            Ok(route_table) => {
                ctx.audit
                    .record_mutation(
                        &principal,
                        Action::RouteTableCreate,
                        request_id,
                        Some(format!("RouteTable::\"{}\"", route_table.id)),
                        AuditOutcome::Success {
                            resource: Some(format!("RouteTable::\"{}\"", route_table.id)),
                        },
                        serde_json::json!({
                            "tenant_id": tenant_id,
                            "project_id": project_id,
                            "vpc_id": vpc_id,
                            "name": route_table.name,
                        }),
                    )
                    .await;
                Ok(HttpResponseCreated(route_table))
            }
            Err(e) => {
                ctx.audit
                    .record_mutation(
                        &principal,
                        Action::RouteTableCreate,
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

    async fn get_vpc_route_table(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcRouteTablePath>,
    ) -> Result<HttpResponseOk<RouteTable>, HttpError> {
        let ctx = rqctx.context();
        let TenantProjectVpcRouteTablePath {
            tenant_id,
            project_id,
            vpc_id,
            route_table_id,
        } = path.into_inner();
        authenticate_and_authorize_in_tenant(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::RouteTableGet,
            tenant_id,
        )
        .await?;
        let route_table = ctx
            .store
            .get_route_table(route_table_id)
            .await
            .map_err(store_error_to_http)?;
        if route_table.tenant_id != tenant_id
            || route_table.project_id != project_id
            || route_table.vpc_id != vpc_id
        {
            return Err(not_found());
        }
        Ok(HttpResponseOk(route_table))
    }

    async fn delete_vpc_route_table(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcRouteTablePath>,
    ) -> Result<HttpResponseDeleted, HttpError> {
        let ctx = rqctx.context();
        let TenantProjectVpcRouteTablePath {
            tenant_id,
            project_id,
            vpc_id,
            route_table_id,
        } = path.into_inner();
        let principal = authenticate_and_authorize_in_tenant(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::RouteTableDelete,
            tenant_id,
        )
        .await?;
        let request_id = parse_request_id(&rqctx);

        let route_table = ctx
            .store
            .get_route_table(route_table_id)
            .await
            .map_err(store_error_to_http)?;
        if route_table.tenant_id != tenant_id
            || route_table.project_id != project_id
            || route_table.vpc_id != vpc_id
        {
            return Err(not_found());
        }
        match ctx.store.delete_route_table(route_table_id).await {
            Ok(()) => {
                ctx.audit
                    .record_mutation(
                        &principal,
                        Action::RouteTableDelete,
                        request_id,
                        Some(format!("RouteTable::\"{route_table_id}\"")),
                        AuditOutcome::Success {
                            resource: Some(format!("RouteTable::\"{route_table_id}\"")),
                        },
                        serde_json::json!({
                            "tenant_id": tenant_id,
                            "project_id": project_id,
                            "vpc_id": vpc_id,
                        }),
                    )
                    .await;
                Ok(HttpResponseDeleted())
            }
            Err(e) => {
                ctx.audit
                    .record_mutation(
                        &principal,
                        Action::RouteTableDelete,
                        request_id,
                        Some(format!("RouteTable::\"{route_table_id}\"")),
                        store_error_to_audit_outcome(&e),
                        serde_json::Value::Null,
                    )
                    .await;
                Err(store_error_to_http(e))
            }
        }
    }

    async fn list_vpc_route_table_routes(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcRouteTablePath>,
    ) -> Result<HttpResponseOk<Vec<Route>>, HttpError> {
        let ctx = rqctx.context();
        let TenantProjectVpcRouteTablePath {
            tenant_id,
            project_id,
            vpc_id,
            route_table_id,
        } = path.into_inner();
        authenticate_and_authorize_in_tenant(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::RouteList,
            tenant_id,
        )
        .await?;

        let route_table = ctx
            .store
            .get_route_table(route_table_id)
            .await
            .map_err(store_error_to_http)?;
        if route_table.tenant_id != tenant_id
            || route_table.project_id != project_id
            || route_table.vpc_id != vpc_id
        {
            return Err(not_found());
        }
        let routes = ctx
            .store
            .list_routes_in_table(route_table_id)
            .await
            .map_err(store_error_to_http)?;
        Ok(HttpResponseOk(routes))
    }

    async fn create_vpc_route_table_route(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcRouteTablePath>,
        body: TypedBody<NewRoute>,
    ) -> Result<HttpResponseCreated<Route>, HttpError> {
        let ctx = rqctx.context();
        let TenantProjectVpcRouteTablePath {
            tenant_id,
            project_id,
            vpc_id,
            route_table_id,
        } = path.into_inner();
        let principal = authenticate_and_authorize_in_tenant(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::RouteCreate,
            tenant_id,
        )
        .await?;
        let request_id = parse_request_id(&rqctx);
        let req = body.into_inner();

        if matches!(req.target, RouteTarget::FloatingIp { .. }) {
            let message = "floating ip route targets are system-installed only in v1".to_string();
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::RouteCreate,
                    request_id,
                    None,
                    AuditOutcome::ClientError {
                        code: 400,
                        message: message.clone(),
                    },
                    serde_json::json!({
                        "tenant_id": tenant_id,
                        "project_id": project_id,
                        "vpc_id": vpc_id,
                        "route_table_id": route_table_id,
                    }),
                )
                .await;
            return Err(bad_request(message));
        }

        if let RouteTarget::NatGateway { nat_gateway_id } = &req.target {
            let nat_gateway = match ctx.store.get_nat_gateway(*nat_gateway_id).await {
                Ok(nat_gateway) => nat_gateway,
                Err(e) => {
                    ctx.audit
                        .record_mutation(
                            &principal,
                            Action::RouteCreate,
                            request_id,
                            None,
                            store_error_to_audit_outcome(&e),
                            serde_json::Value::Null,
                        )
                        .await;
                    return Err(store_error_to_http(e));
                }
            };
            if nat_gateway.tenant_id != tenant_id || nat_gateway.project_id != project_id {
                ctx.audit
                    .record_mutation(
                        &principal,
                        Action::RouteCreate,
                        request_id,
                        None,
                        AuditOutcome::ClientError {
                            code: 404,
                            message: "not found".to_string(),
                        },
                        serde_json::Value::Null,
                    )
                    .await;
                return Err(not_found());
            }
            if nat_gateway.vpc_id != vpc_id {
                let message = format!("nat gateway {nat_gateway_id} is not in vpc {vpc_id}");
                ctx.audit
                    .record_mutation(
                        &principal,
                        Action::RouteCreate,
                        request_id,
                        None,
                        AuditOutcome::ClientError {
                            code: 400,
                            message: message.clone(),
                        },
                        serde_json::json!({
                            "tenant_id": tenant_id,
                            "project_id": project_id,
                            "vpc_id": vpc_id,
                            "route_table_id": route_table_id,
                            "nat_gateway_id": nat_gateway_id,
                        }),
                    )
                    .await;
                return Err(bad_request(message));
            }
        }

        match ctx
            .store
            .create_route(tenant_id, project_id, vpc_id, route_table_id, req)
            .await
        {
            Ok(route) => {
                ctx.audit
                    .record_mutation(
                        &principal,
                        Action::RouteCreate,
                        request_id,
                        Some(format!("Route::\"{}\"", route.id)),
                        AuditOutcome::Success {
                            resource: Some(format!("Route::\"{}\"", route.id)),
                        },
                        serde_json::json!({
                            "tenant_id": tenant_id,
                            "project_id": project_id,
                            "vpc_id": vpc_id,
                            "route_table_id": route_table_id,
                            "destination": route.destination.to_string(),
                        }),
                    )
                    .await;
                Ok(HttpResponseCreated(route))
            }
            Err(e) => {
                ctx.audit
                    .record_mutation(
                        &principal,
                        Action::RouteCreate,
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

    async fn get_vpc_route_table_route(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcRouteTableRoutePath>,
    ) -> Result<HttpResponseOk<Route>, HttpError> {
        let ctx = rqctx.context();
        let TenantProjectVpcRouteTableRoutePath {
            tenant_id,
            project_id,
            vpc_id,
            route_table_id,
            route_id,
        } = path.into_inner();
        authenticate_and_authorize_in_tenant(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::RouteGet,
            tenant_id,
        )
        .await?;
        let route = ctx
            .store
            .get_route(route_id)
            .await
            .map_err(store_error_to_http)?;
        if route.tenant_id != tenant_id
            || route.project_id != project_id
            || route.vpc_id != vpc_id
            || route.route_table_id != route_table_id
        {
            return Err(not_found());
        }
        Ok(HttpResponseOk(route))
    }

    async fn delete_vpc_route_table_route(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcRouteTableRoutePath>,
    ) -> Result<HttpResponseDeleted, HttpError> {
        let ctx = rqctx.context();
        let TenantProjectVpcRouteTableRoutePath {
            tenant_id,
            project_id,
            vpc_id,
            route_table_id,
            route_id,
        } = path.into_inner();
        let principal = authenticate_and_authorize_in_tenant(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::RouteDelete,
            tenant_id,
        )
        .await?;
        let request_id = parse_request_id(&rqctx);

        let route = ctx
            .store
            .get_route(route_id)
            .await
            .map_err(store_error_to_http)?;
        if route.tenant_id != tenant_id
            || route.project_id != project_id
            || route.vpc_id != vpc_id
            || route.route_table_id != route_table_id
        {
            return Err(not_found());
        }
        match ctx.store.delete_route(route_id).await {
            Ok(()) => {
                ctx.audit
                    .record_mutation(
                        &principal,
                        Action::RouteDelete,
                        request_id,
                        Some(format!("Route::\"{route_id}\"")),
                        AuditOutcome::Success {
                            resource: Some(format!("Route::\"{route_id}\"")),
                        },
                        serde_json::json!({
                            "tenant_id": tenant_id,
                            "project_id": project_id,
                            "vpc_id": vpc_id,
                            "route_table_id": route_table_id,
                        }),
                    )
                    .await;
                Ok(HttpResponseDeleted())
            }
            Err(e) => {
                ctx.audit
                    .record_mutation(
                        &principal,
                        Action::RouteDelete,
                        request_id,
                        Some(format!("Route::\"{route_id}\"")),
                        store_error_to_audit_outcome(&e),
                        serde_json::Value::Null,
                    )
                    .await;
                Err(store_error_to_http(e))
            }
        }
    }

    // ---- Firewall rules (Slice 1: per-VPC flat rule list) ----------

    async fn list_vpc_firewall_rules(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcPath>,
    ) -> Result<HttpResponseOk<Vec<FirewallRule>>, HttpError> {
        let ctx = rqctx.context();
        let TenantProjectVpcPath {
            tenant_id,
            project_id,
            vpc_id,
        } = path.into_inner();
        authenticate_and_authorize_in_tenant(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::FirewallRuleList,
            tenant_id,
        )
        .await?;

        let vpc = ctx
            .store
            .get_vpc(vpc_id)
            .await
            .map_err(store_error_to_http)?;
        if vpc.tenant_id != tenant_id || vpc.project_id != project_id {
            return Err(not_found());
        }
        let rules = ctx
            .store
            .list_firewall_rules_in_vpc(vpc_id)
            .await
            .map_err(store_error_to_http)?;
        Ok(HttpResponseOk(rules))
    }

    async fn create_vpc_firewall_rule(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcPath>,
        body: TypedBody<NewFirewallRule>,
    ) -> Result<HttpResponseCreated<FirewallRule>, HttpError> {
        let ctx = rqctx.context();
        let TenantProjectVpcPath {
            tenant_id,
            project_id,
            vpc_id,
        } = path.into_inner();
        let principal = authenticate_and_authorize_in_tenant(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::FirewallRuleCreate,
            tenant_id,
        )
        .await?;
        let request_id = parse_request_id(&rqctx);
        let req = body.into_inner();

        match ctx
            .store
            .create_firewall_rule(tenant_id, project_id, vpc_id, req)
            .await
        {
            Ok(rule) => {
                ctx.audit
                    .record_mutation(
                        &principal,
                        Action::FirewallRuleCreate,
                        request_id,
                        Some(format!("FirewallRule::\"{}\"", rule.id)),
                        AuditOutcome::Success {
                            resource: Some(format!("FirewallRule::\"{}\"", rule.id)),
                        },
                        serde_json::json!({
                            "tenant_id": tenant_id,
                            "project_id": project_id,
                            "vpc_id": vpc_id,
                            "name": rule.name,
                            "priority": rule.priority,
                            "direction": rule.direction,
                            "action": rule.action,
                            "protocol": rule.protocol,
                        }),
                    )
                    .await;
                Ok(HttpResponseCreated(rule))
            }
            Err(e) => {
                ctx.audit
                    .record_mutation(
                        &principal,
                        Action::FirewallRuleCreate,
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

    async fn delete_vpc_firewall_rule(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcFirewallRulePath>,
    ) -> Result<HttpResponseDeleted, HttpError> {
        let ctx = rqctx.context();
        let TenantProjectVpcFirewallRulePath {
            tenant_id,
            project_id,
            vpc_id,
            firewall_rule_id,
        } = path.into_inner();
        let principal = authenticate_and_authorize_in_tenant(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::FirewallRuleDelete,
            tenant_id,
        )
        .await?;
        let request_id = parse_request_id(&rqctx);

        let rule = ctx
            .store
            .get_firewall_rule(firewall_rule_id)
            .await
            .map_err(store_error_to_http)?;
        if rule.tenant_id != tenant_id || rule.project_id != project_id || rule.vpc_id != vpc_id {
            return Err(not_found());
        }
        match ctx.store.delete_firewall_rule(firewall_rule_id).await {
            Ok(()) => {
                ctx.audit
                    .record_mutation(
                        &principal,
                        Action::FirewallRuleDelete,
                        request_id,
                        Some(format!("FirewallRule::\"{firewall_rule_id}\"")),
                        AuditOutcome::Success {
                            resource: Some(format!("FirewallRule::\"{firewall_rule_id}\"")),
                        },
                        serde_json::json!({
                            "tenant_id": tenant_id,
                            "project_id": project_id,
                            "vpc_id": vpc_id,
                        }),
                    )
                    .await;
                Ok(HttpResponseDeleted())
            }
            Err(e) => {
                ctx.audit
                    .record_mutation(
                        &principal,
                        Action::FirewallRuleDelete,
                        request_id,
                        Some(format!("FirewallRule::\"{firewall_rule_id}\"")),
                        store_error_to_audit_outcome(&e),
                        serde_json::Value::Null,
                    )
                    .await;
                Err(store_error_to_http(e))
            }
        }
    }

    // ---- DHCP / IPAM (γ.1 + γ.4) -----------------------------------

    async fn get_vpc_dhcp_pool(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcPath>,
    ) -> Result<HttpResponseOk<Option<DhcpPool>>, HttpError> {
        let ctx = rqctx.context();
        let TenantProjectVpcPath {
            tenant_id,
            project_id,
            vpc_id,
        } = path.into_inner();
        authenticate_and_authorize_in_tenant(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::DhcpPoolGet,
            tenant_id,
        )
        .await?;
        check_vpc_parentage(ctx.store.as_ref(), vpc_id, tenant_id, project_id).await?;
        let pool = ctx
            .store
            .get_dhcp_pool(vpc_id)
            .await
            .map_err(store_error_to_http)?;
        Ok(HttpResponseOk(pool))
    }

    async fn set_vpc_dhcp_pool(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcPath>,
        body: TypedBody<NewDhcpPool>,
    ) -> Result<HttpResponseOk<DhcpPool>, HttpError> {
        let ctx = rqctx.context();
        let TenantProjectVpcPath {
            tenant_id,
            project_id,
            vpc_id,
        } = path.into_inner();
        let principal = authenticate_and_authorize_in_tenant(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::DhcpPoolSet,
            tenant_id,
        )
        .await?;
        let request_id = parse_request_id(&rqctx);
        check_vpc_parentage(ctx.store.as_ref(), vpc_id, tenant_id, project_id).await?;
        match ctx.store.set_dhcp_pool(vpc_id, body.into_inner()).await {
            Ok(pool) => {
                ctx.audit
                    .record_mutation(
                        &principal,
                        Action::DhcpPoolSet,
                        request_id,
                        Some(format!("DhcpPool::\"{vpc_id}\"")),
                        AuditOutcome::Success {
                            resource: Some(format!("DhcpPool::\"{vpc_id}\"")),
                        },
                        serde_json::json!({
                            "tenant_id": tenant_id,
                            "project_id": project_id,
                            "vpc_id": vpc_id,
                            "lease_seconds_default": pool.lease_seconds_default,
                        }),
                    )
                    .await;
                Ok(HttpResponseOk(pool))
            }
            Err(e) => {
                ctx.audit
                    .record_mutation(
                        &principal,
                        Action::DhcpPoolSet,
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

    async fn clear_vpc_dhcp_pool(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcPath>,
    ) -> Result<HttpResponseDeleted, HttpError> {
        let ctx = rqctx.context();
        let TenantProjectVpcPath {
            tenant_id,
            project_id,
            vpc_id,
        } = path.into_inner();
        let principal = authenticate_and_authorize_in_tenant(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::DhcpPoolClear,
            tenant_id,
        )
        .await?;
        let request_id = parse_request_id(&rqctx);
        check_vpc_parentage(ctx.store.as_ref(), vpc_id, tenant_id, project_id).await?;
        match ctx.store.clear_dhcp_pool(vpc_id).await {
            Ok(()) => {
                ctx.audit
                    .record_mutation(
                        &principal,
                        Action::DhcpPoolClear,
                        request_id,
                        Some(format!("DhcpPool::\"{vpc_id}\"")),
                        AuditOutcome::Success {
                            resource: Some(format!("DhcpPool::\"{vpc_id}\"")),
                        },
                        serde_json::json!({"vpc_id": vpc_id}),
                    )
                    .await;
                Ok(HttpResponseDeleted())
            }
            Err(e) => {
                ctx.audit
                    .record_mutation(
                        &principal,
                        Action::DhcpPoolClear,
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

    async fn list_vpc_dhcp_reservations(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcPath>,
    ) -> Result<HttpResponseOk<Vec<DhcpReservation>>, HttpError> {
        let ctx = rqctx.context();
        let TenantProjectVpcPath {
            tenant_id,
            project_id,
            vpc_id,
        } = path.into_inner();
        authenticate_and_authorize_in_tenant(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::DhcpReservationList,
            tenant_id,
        )
        .await?;
        check_vpc_parentage(ctx.store.as_ref(), vpc_id, tenant_id, project_id).await?;
        let rs = ctx
            .store
            .list_dhcp_reservations(vpc_id)
            .await
            .map_err(store_error_to_http)?;
        Ok(HttpResponseOk(rs))
    }

    async fn create_vpc_dhcp_reservation(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcPath>,
        body: TypedBody<NewDhcpReservation>,
    ) -> Result<HttpResponseCreated<DhcpReservation>, HttpError> {
        let ctx = rqctx.context();
        let TenantProjectVpcPath {
            tenant_id,
            project_id,
            vpc_id,
        } = path.into_inner();
        let principal = authenticate_and_authorize_in_tenant(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::DhcpReservationCreate,
            tenant_id,
        )
        .await?;
        let request_id = parse_request_id(&rqctx);
        check_vpc_parentage(ctx.store.as_ref(), vpc_id, tenant_id, project_id).await?;
        let req = body.into_inner();
        match ctx.store.create_dhcp_reservation(vpc_id, req).await {
            Ok(r) => {
                ctx.audit
                    .record_mutation(
                        &principal,
                        Action::DhcpReservationCreate,
                        request_id,
                        Some(format!("DhcpReservation::\"{}/{}\"", vpc_id, r.mac)),
                        AuditOutcome::Success {
                            resource: Some(format!("DhcpReservation::\"{}/{}\"", vpc_id, r.mac)),
                        },
                        serde_json::json!({
                            "vpc_id": vpc_id,
                            "mac": r.mac,
                            "ipv4": r.ipv4.to_string(),
                        }),
                    )
                    .await;
                Ok(HttpResponseCreated(r))
            }
            Err(e) => {
                ctx.audit
                    .record_mutation(
                        &principal,
                        Action::DhcpReservationCreate,
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

    async fn get_vpc_dhcp_reservation(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcDhcpMacPath>,
    ) -> Result<HttpResponseOk<DhcpReservation>, HttpError> {
        let ctx = rqctx.context();
        let TenantProjectVpcDhcpMacPath {
            tenant_id,
            project_id,
            vpc_id,
            mac,
        } = path.into_inner();
        authenticate_and_authorize_in_tenant(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::DhcpReservationGet,
            tenant_id,
        )
        .await?;
        check_vpc_parentage(ctx.store.as_ref(), vpc_id, tenant_id, project_id).await?;
        let r = ctx
            .store
            .get_dhcp_reservation(vpc_id, &mac)
            .await
            .map_err(store_error_to_http)?;
        Ok(HttpResponseOk(r))
    }

    async fn delete_vpc_dhcp_reservation(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcDhcpMacPath>,
    ) -> Result<HttpResponseDeleted, HttpError> {
        let ctx = rqctx.context();
        let TenantProjectVpcDhcpMacPath {
            tenant_id,
            project_id,
            vpc_id,
            mac,
        } = path.into_inner();
        let principal = authenticate_and_authorize_in_tenant(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::DhcpReservationDelete,
            tenant_id,
        )
        .await?;
        let request_id = parse_request_id(&rqctx);
        check_vpc_parentage(ctx.store.as_ref(), vpc_id, tenant_id, project_id).await?;
        match ctx.store.delete_dhcp_reservation(vpc_id, &mac).await {
            Ok(()) => {
                ctx.audit
                    .record_mutation(
                        &principal,
                        Action::DhcpReservationDelete,
                        request_id,
                        Some(format!("DhcpReservation::\"{}/{}\"", vpc_id, mac)),
                        AuditOutcome::Success {
                            resource: Some(format!("DhcpReservation::\"{}/{}\"", vpc_id, mac)),
                        },
                        serde_json::json!({"vpc_id": vpc_id, "mac": mac}),
                    )
                    .await;
                Ok(HttpResponseDeleted())
            }
            Err(e) => {
                ctx.audit
                    .record_mutation(
                        &principal,
                        Action::DhcpReservationDelete,
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

    async fn list_vpc_dhcp_leases(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcPath>,
    ) -> Result<HttpResponseOk<Vec<DhcpLease>>, HttpError> {
        let ctx = rqctx.context();
        let TenantProjectVpcPath {
            tenant_id,
            project_id,
            vpc_id,
        } = path.into_inner();
        authenticate_and_authorize_in_tenant(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::DhcpLeaseList,
            tenant_id,
        )
        .await?;
        check_vpc_parentage(ctx.store.as_ref(), vpc_id, tenant_id, project_id).await?;
        let l = ctx
            .store
            .list_dhcp_leases(vpc_id)
            .await
            .map_err(store_error_to_http)?;
        Ok(HttpResponseOk(l))
    }

    async fn get_vpc_dhcp_lease(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcDhcpMacPath>,
    ) -> Result<HttpResponseOk<DhcpLease>, HttpError> {
        let ctx = rqctx.context();
        let TenantProjectVpcDhcpMacPath {
            tenant_id,
            project_id,
            vpc_id,
            mac,
        } = path.into_inner();
        authenticate_and_authorize_in_tenant(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::DhcpLeaseGet,
            tenant_id,
        )
        .await?;
        check_vpc_parentage(ctx.store.as_ref(), vpc_id, tenant_id, project_id).await?;
        let l = ctx
            .store
            .get_dhcp_lease(vpc_id, &mac)
            .await
            .map_err(store_error_to_http)?;
        Ok(HttpResponseOk(l))
    }

    async fn delete_vpc_dhcp_lease(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcDhcpMacPath>,
    ) -> Result<HttpResponseDeleted, HttpError> {
        let ctx = rqctx.context();
        let TenantProjectVpcDhcpMacPath {
            tenant_id,
            project_id,
            vpc_id,
            mac,
        } = path.into_inner();
        let principal = authenticate_and_authorize_in_tenant(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::DhcpLeaseDelete,
            tenant_id,
        )
        .await?;
        let request_id = parse_request_id(&rqctx);
        check_vpc_parentage(ctx.store.as_ref(), vpc_id, tenant_id, project_id).await?;
        match ctx.store.delete_dhcp_lease(vpc_id, &mac).await {
            Ok(()) => {
                ctx.audit
                    .record_mutation(
                        &principal,
                        Action::DhcpLeaseDelete,
                        request_id,
                        Some(format!("DhcpLease::\"{}/{}\"", vpc_id, mac)),
                        AuditOutcome::Success {
                            resource: Some(format!("DhcpLease::\"{}/{}\"", vpc_id, mac)),
                        },
                        serde_json::json!({"vpc_id": vpc_id, "mac": mac}),
                    )
                    .await;
                Ok(HttpResponseDeleted())
            }
            Err(e) => {
                ctx.audit
                    .record_mutation(
                        &principal,
                        Action::DhcpLeaseDelete,
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

    async fn list_vpc_nat_gateways(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcPath>,
    ) -> Result<HttpResponseOk<Vec<NatGateway>>, HttpError> {
        let ctx = rqctx.context();
        let TenantProjectVpcPath {
            tenant_id,
            project_id,
            vpc_id,
        } = path.into_inner();
        authenticate_and_authorize_in_tenant(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::NatGatewayList,
            tenant_id,
        )
        .await?;

        let vpc = ctx
            .store
            .get_vpc(vpc_id)
            .await
            .map_err(store_error_to_http)?;
        if vpc.tenant_id != tenant_id || vpc.project_id != project_id {
            return Err(not_found());
        }
        let nat_gateways = ctx
            .store
            .list_nat_gateways_in_vpc(vpc_id)
            .await
            .map_err(store_error_to_http)?;
        Ok(HttpResponseOk(nat_gateways))
    }

    async fn create_vpc_nat_gateway(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcPath>,
        body: TypedBody<NewNatGateway>,
    ) -> Result<HttpResponseCreated<NatGateway>, HttpError> {
        let ctx = rqctx.context();
        let TenantProjectVpcPath {
            tenant_id,
            project_id,
            vpc_id,
        } = path.into_inner();
        let principal = authenticate_and_authorize_in_tenant(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::NatGatewayCreate,
            tenant_id,
        )
        .await?;
        let request_id = parse_request_id(&rqctx);
        let req = body.into_inner();

        match ctx
            .store
            .create_nat_gateway(tenant_id, project_id, vpc_id, req)
            .await
        {
            Ok(nat_gateway) => {
                ctx.audit
                    .record_mutation(
                        &principal,
                        Action::NatGatewayCreate,
                        request_id,
                        Some(format!("NatGateway::\"{}\"", nat_gateway.id)),
                        AuditOutcome::Success {
                            resource: Some(format!("NatGateway::\"{}\"", nat_gateway.id)),
                        },
                        serde_json::json!({
                            "tenant_id": tenant_id,
                            "project_id": project_id,
                            "vpc_id": vpc_id,
                            "name": nat_gateway.name,
                            "public_address": nat_gateway.public_address.to_string(),
                            "desired_generation": nat_gateway.desired_generation,
                        }),
                    )
                    .await;
                Ok(HttpResponseCreated(nat_gateway))
            }
            Err(e) => {
                ctx.audit
                    .record_mutation(
                        &principal,
                        Action::NatGatewayCreate,
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

    async fn get_vpc_nat_gateway(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcNatGatewayPath>,
    ) -> Result<HttpResponseOk<NatGateway>, HttpError> {
        let ctx = rqctx.context();
        let TenantProjectVpcNatGatewayPath {
            tenant_id,
            project_id,
            vpc_id,
            nat_gateway_id,
        } = path.into_inner();
        authenticate_and_authorize_in_tenant(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::NatGatewayGet,
            tenant_id,
        )
        .await?;
        let nat_gateway = ctx
            .store
            .get_nat_gateway(nat_gateway_id)
            .await
            .map_err(store_error_to_http)?;
        if nat_gateway.tenant_id != tenant_id
            || nat_gateway.project_id != project_id
            || nat_gateway.vpc_id != vpc_id
        {
            return Err(not_found());
        }
        Ok(HttpResponseOk(nat_gateway))
    }

    async fn delete_vpc_nat_gateway(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcNatGatewayPath>,
    ) -> Result<HttpResponseDeleted, HttpError> {
        let ctx = rqctx.context();
        let TenantProjectVpcNatGatewayPath {
            tenant_id,
            project_id,
            vpc_id,
            nat_gateway_id,
        } = path.into_inner();
        let principal = authenticate_and_authorize_in_tenant(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::NatGatewayDelete,
            tenant_id,
        )
        .await?;
        let request_id = parse_request_id(&rqctx);

        let nat_gateway = ctx
            .store
            .get_nat_gateway(nat_gateway_id)
            .await
            .map_err(store_error_to_http)?;
        if nat_gateway.tenant_id != tenant_id
            || nat_gateway.project_id != project_id
            || nat_gateway.vpc_id != vpc_id
        {
            return Err(not_found());
        }
        match ctx.store.delete_nat_gateway(nat_gateway_id).await {
            Ok(()) => {
                ctx.audit
                    .record_mutation(
                        &principal,
                        Action::NatGatewayDelete,
                        request_id,
                        Some(format!("NatGateway::\"{nat_gateway_id}\"")),
                        AuditOutcome::Success {
                            resource: Some(format!("NatGateway::\"{nat_gateway_id}\"")),
                        },
                        serde_json::json!({
                            "tenant_id": tenant_id,
                            "project_id": project_id,
                            "vpc_id": vpc_id,
                        }),
                    )
                    .await;
                Ok(HttpResponseDeleted())
            }
            Err(e) => {
                ctx.audit
                    .record_mutation(
                        &principal,
                        Action::NatGatewayDelete,
                        request_id,
                        Some(format!("NatGateway::\"{nat_gateway_id}\"")),
                        store_error_to_audit_outcome(&e),
                        serde_json::Value::Null,
                    )
                    .await;
                Err(store_error_to_http(e))
            }
        }
    }

    async fn list_public_ssh_keys(
        rqctx: RequestContext<Self::Context>,
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

    async fn create_public_ssh_key(
        rqctx: RequestContext<Self::Context>,
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

    async fn list_silo_ssh_keys(
        rqctx: RequestContext<Self::Context>,
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

    async fn create_silo_ssh_key(
        rqctx: RequestContext<Self::Context>,
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

    async fn list_tenant_ssh_keys(
        rqctx: RequestContext<Self::Context>,
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

    async fn create_tenant_ssh_key(
        rqctx: RequestContext<Self::Context>,
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

    async fn list_project_ssh_keys(
        rqctx: RequestContext<Self::Context>,
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

    async fn create_project_ssh_key(
        rqctx: RequestContext<Self::Context>,
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

    async fn list_my_ssh_keys(
        rqctx: RequestContext<Self::Context>,
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

    async fn create_my_ssh_key(
        rqctx: RequestContext<Self::Context>,
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

    async fn get_ssh_key(
        rqctx: RequestContext<Self::Context>,
        path: Path<SshKeyPath>,
    ) -> Result<HttpResponseOk<SshKey>, HttpError> {
        let ctx = rqctx.context();
        let key_id = path.into_inner().key_id;
        // Anonymous principals can hit Public ssh keys via the
        // anonymous-public-actions Cedar rule + the visibility
        // check below; authenticated callers go through scope
        // gating in ssh_key_visible_to.
        let principal = authenticate_and_authorize(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::SshKeyGet,
        )
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

    async fn delete_ssh_key(
        rqctx: RequestContext<Self::Context>,
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

    async fn list_public_images(
        rqctx: RequestContext<Self::Context>,
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

    async fn create_public_image(
        rqctx: RequestContext<Self::Context>,
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

    async fn list_silo_images(
        rqctx: RequestContext<Self::Context>,
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

    async fn create_silo_image(
        rqctx: RequestContext<Self::Context>,
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

    async fn create_silo_image_from_bundle(
        rqctx: RequestContext<Self::Context>,
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

    async fn list_tenant_images(
        rqctx: RequestContext<Self::Context>,
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

    async fn create_tenant_image(
        rqctx: RequestContext<Self::Context>,
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

    async fn list_project_images(
        rqctx: RequestContext<Self::Context>,
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

    async fn create_project_image(
        rqctx: RequestContext<Self::Context>,
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

    async fn list_my_images(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseOk<Vec<Image>>, HttpError> {
        let ctx = rqctx.context();
        let principal = authenticate_and_authorize(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::ImageList,
        )
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

    async fn create_my_image(
        rqctx: RequestContext<Self::Context>,
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

    async fn get_image(
        rqctx: RequestContext<Self::Context>,
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

    async fn delete_image(
        rqctx: RequestContext<Self::Context>,
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

    async fn put_project_quota(
        rqctx: RequestContext<Self::Context>,
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

    async fn get_project_quota(
        rqctx: RequestContext<Self::Context>,
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

    async fn delete_project_quota(
        rqctx: RequestContext<Self::Context>,
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

    async fn list_project_instances(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectPath>,
    ) -> Result<HttpResponseOk<Vec<Instance>>, HttpError> {
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
            Action::InstanceList,
            tenant_id,
        )
        .await?;
        // Project must exist + be in this silo (matches the
        // list_project_vpcs / list_vpc_subnets pattern).
        let project = ctx
            .store
            .get_project(project_id)
            .await
            .map_err(store_error_to_http)?;
        if project.tenant_id != tenant_id {
            return Err(not_found());
        }
        let instances = ctx
            .store
            .list_instances_in_project(project_id)
            .await
            .map_err(store_error_to_http)?;
        Ok(HttpResponseOk(instances))
    }

    async fn create_project_instance(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectPath>,
        body: TypedBody<NewInstance>,
    ) -> Result<HttpResponseCreated<Instance>, HttpError> {
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
            Action::InstanceCreate,
            tenant_id,
        )
        .await?;
        let request_id = parse_request_id(&rqctx);
        let req = body.into_inner();

        // API-edge size invariants (the store doesn't re-validate).
        if req.cpu == 0 {
            return Err(reject_audit(
                ctx,
                &principal,
                Action::InstanceCreate,
                request_id,
                "cpu must be greater than zero",
                serde_json::json!({ "tenant_id": tenant_id, "project_id": project_id }),
            )
            .await);
        }
        if req.memory_bytes == 0 {
            return Err(reject_audit(
                ctx,
                &principal,
                Action::InstanceCreate,
                request_id,
                "memory_bytes must be greater than zero",
                serde_json::json!({ "tenant_id": tenant_id, "project_id": project_id }),
            )
            .await);
        }

        // Cross-scope visibility check on the referenced image.
        // The store no longer enforces silo membership on images
        // (multi-scope as of slice F); the handler resolves
        // visibility against the principal and surfaces a
        // not-visible image as 404 to preserve the cross-tenant
        // probe invariant.
        match ctx.store.get_image(req.image_id).await {
            Ok(image) => {
                let visible = image_visible_to(&image, &principal, ctx.store.as_ref())
                    .await
                    .map_err(store_error_to_http)?;
                if !visible {
                    ctx.audit
                        .record_mutation(
                            &principal,
                            Action::InstanceCreate,
                            request_id,
                            None,
                            AuditOutcome::ClientError {
                                code: 404,
                                message: "image not visible".to_string(),
                            },
                            serde_json::json!({
                                "tenant_id": tenant_id,
                                "project_id": project_id,
                                "image_id": req.image_id,
                            }),
                        )
                        .await;
                    return Err(not_found());
                }
            }
            Err(StoreError::NotFound) => {
                ctx.audit
                    .record_mutation(
                        &principal,
                        Action::InstanceCreate,
                        request_id,
                        None,
                        AuditOutcome::ClientError {
                            code: 404,
                            message: "image not found".to_string(),
                        },
                        serde_json::json!({
                            "tenant_id": tenant_id,
                            "project_id": project_id,
                            "image_id": req.image_id,
                        }),
                    )
                    .await;
                return Err(not_found());
            }
            Err(e) => return Err(store_error_to_http(e)),
        }

        // Cross-scope visibility check on every referenced SSH
        // key. The store no longer enforces silo membership on
        // SSH keys (multi-scope as of slice G); the handler
        // resolves visibility against the principal and surfaces
        // a not-visible (or not-found) key as 404 to preserve
        // the cross-tenant probe invariant.
        for key_id in &req.ssh_key_ids {
            match ctx.store.get_ssh_key(*key_id).await {
                Ok(key) => {
                    let visible = ssh_key_visible_to(&key, &principal, ctx.store.as_ref())
                        .await
                        .map_err(store_error_to_http)?;
                    if !visible {
                        ctx.audit
                            .record_mutation(
                                &principal,
                                Action::InstanceCreate,
                                request_id,
                                None,
                                AuditOutcome::ClientError {
                                    code: 404,
                                    message: "ssh key not visible".to_string(),
                                },
                                serde_json::json!({
                                    "tenant_id": tenant_id,
                                    "project_id": project_id,
                                    "ssh_key_id": *key_id,
                                }),
                            )
                            .await;
                        return Err(not_found());
                    }
                }
                Err(StoreError::NotFound) => {
                    ctx.audit
                        .record_mutation(
                            &principal,
                            Action::InstanceCreate,
                            request_id,
                            None,
                            AuditOutcome::ClientError {
                                code: 404,
                                message: "ssh key not found".to_string(),
                            },
                            serde_json::json!({
                                "tenant_id": tenant_id,
                                "project_id": project_id,
                                "ssh_key_id": *key_id,
                            }),
                        )
                        .await;
                    return Err(not_found());
                }
                Err(e) => return Err(store_error_to_http(e)),
            }
        }

        let target_cn_uuid = match select_tenant_cn_for_instance(
            ctx.store.as_ref(),
            ctx.spawn_in_process_provisioner,
        )
        .await
        {
            Ok(target) => target,
            Err(e) => {
                ctx.audit
                    .record_mutation(
                        &principal,
                        Action::InstanceCreate,
                        request_id,
                        None,
                        store_error_to_audit_outcome(&e),
                        serde_json::Value::Null,
                    )
                    .await;
                return Err(store_error_to_http(e));
            }
        };

        let mut instance = match ctx.store.create_instance(tenant_id, project_id, req).await {
            Ok(result) => result.instance,
            Err(e) => {
                ctx.audit
                    .record_mutation(
                        &principal,
                        Action::InstanceCreate,
                        request_id,
                        None,
                        store_error_to_audit_outcome(&e),
                        serde_json::Value::Null,
                    )
                    .await;
                return Err(store_error_to_http(e));
            }
        };

        if let Some(host_cn_uuid) = target_cn_uuid {
            instance = match ctx
                .store
                .set_instance_host_cn(instance.id, Some(host_cn_uuid))
                .await
            {
                Ok(updated) => updated,
                Err(e) => {
                    ctx.audit
                        .record_mutation(
                            &principal,
                            Action::InstanceCreate,
                            request_id,
                            Some(format!("Instance::\"{}\"", instance.id)),
                            store_error_to_audit_outcome(&e),
                            serde_json::Value::Null,
                        )
                        .await;
                    return Err(store_error_to_http(e));
                }
            };
        }

        // Enqueue the provisioning job. The stub provisioner (or
        // the selected per-CN agent) will pick it up and drive
        // Pending → Provisioning → Running. The response returns
        // the instance in `Pending` — clients poll the get endpoint
        // to observe the transition.
        if let Err(e) = ctx
            .store
            .enqueue_job(NewJob {
                kind: JobKind::Provision {
                    instance_id: instance.id,
                },
                target_cn_uuid,
            })
            .await
        {
            // Failure to enqueue is operationally bad — the instance
            // record exists but will never provision. Surface as
            // 5xx; operators can retry by re-creating with a new
            // name (Phase 0 doesn't support requeue).
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::InstanceCreate,
                    request_id,
                    Some(format!("Instance::\"{}\"", instance.id)),
                    store_error_to_audit_outcome(&e),
                    serde_json::Value::Null,
                )
                .await;
            return Err(store_error_to_http(e));
        }

        ctx.audit
            .record_mutation(
                &principal,
                Action::InstanceCreate,
                request_id,
                Some(format!("Instance::\"{}\"", instance.id)),
                AuditOutcome::Success {
                    resource: Some(format!("Instance::\"{}\"", instance.id)),
                },
                serde_json::json!({
                    "tenant_id": tenant_id,
                    "project_id": project_id,
                    "name": instance.name,
                    "image_id": instance.image_id,
                    "primary_subnet_id": instance.primary_subnet_id,
                }),
            )
            .await;
        Ok(HttpResponseCreated(instance))
    }

    async fn get_project_instance(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectInstancePath>,
    ) -> Result<HttpResponseOk<Instance>, HttpError> {
        let ctx = rqctx.context();
        let TenantProjectInstancePath {
            tenant_id,
            project_id,
            instance_id,
        } = path.into_inner();
        authenticate_and_authorize_in_tenant(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::InstanceGet,
            tenant_id,
        )
        .await?;
        let instance = ctx
            .store
            .get_instance(instance_id)
            .await
            .map_err(store_error_to_http)?;
        if instance.tenant_id != tenant_id || instance.project_id != project_id {
            return Err(not_found());
        }
        Ok(HttpResponseOk(instance))
    }

    async fn delete_project_instance(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectInstancePath>,
        query: Query<InstanceDeleteQuery>,
    ) -> Result<HttpResponseDeleted, HttpError> {
        let ctx = rqctx.context();
        let TenantProjectInstancePath {
            tenant_id,
            project_id,
            instance_id,
        } = path.into_inner();
        let force = query.into_inner().force;
        let principal = authenticate_and_authorize_in_tenant(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::InstanceDelete,
            tenant_id,
        )
        .await?;
        let request_id = parse_request_id(&rqctx);

        let instance = ctx
            .store
            .get_instance(instance_id)
            .await
            .map_err(store_error_to_http)?;
        if instance.tenant_id != tenant_id || instance.project_id != project_id {
            return Err(not_found());
        }
        let target_cn_uuid = instance.host_cn_uuid;
        match ctx.store.delete_instance(instance_id, force).await {
            Ok(()) => {
                ctx.audit
                    .record_mutation(
                        &principal,
                        Action::InstanceDelete,
                        request_id,
                        Some(format!("Instance::\"{instance_id}\"")),
                        AuditOutcome::Success {
                            resource: Some(format!("Instance::\"{instance_id}\"")),
                        },
                        serde_json::json!({
                            "tenant_id": tenant_id,
                            "project_id": project_id,
                        }),
                    )
                    .await;
                // Best-effort agent cleanup of the SmartOS zone.
                // Failure here is logged but doesn't fail the
                // operator-visible delete — the tritond record
                // is already gone.
                if let Err(e) = ctx
                    .store
                    .enqueue_job(NewJob {
                        kind: JobKind::Delete { instance_id },
                        target_cn_uuid,
                    })
                    .await
                {
                    tracing::warn!(
                        %instance_id,
                        error = %e,
                        "instance delete record cleared, but enqueue of Delete job failed; zone may leak on the host",
                    );
                }
                Ok(HttpResponseDeleted())
            }
            Err(e) => {
                ctx.audit
                    .record_mutation(
                        &principal,
                        Action::InstanceDelete,
                        request_id,
                        Some(format!("Instance::\"{instance_id}\"")),
                        store_error_to_audit_outcome(&e),
                        serde_json::Value::Null,
                    )
                    .await;
                Err(store_error_to_http(e))
            }
        }
    }

    async fn start_project_instance(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectInstancePath>,
    ) -> Result<HttpResponseOk<Instance>, HttpError> {
        // Stopped → Pending; agent then drives Pending → Provisioning
        // → Running. The response shows Pending; clients poll for
        // the final state.
        instance_lifecycle_transition(
            rqctx,
            path,
            Action::InstanceStart,
            &[LifecycleStateKind::Stopped],
            LifecycleState::Pending,
            Some(JobKindTemplate::Provision),
        )
        .await
    }

    async fn stop_project_instance(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectInstancePath>,
    ) -> Result<HttpResponseOk<Instance>, HttpError> {
        // Running → Stopping; agent then drives Stopping → Stopped.
        instance_lifecycle_transition(
            rqctx,
            path,
            Action::InstanceStop,
            &[LifecycleStateKind::Running],
            LifecycleState::Stopping,
            Some(JobKindTemplate::Stop),
        )
        .await
    }

    async fn restart_project_instance(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectInstancePath>,
    ) -> Result<HttpResponseOk<Instance>, HttpError> {
        // Running → Stopping; agent then drives the full restart
        // cycle Stopping → Pending → Provisioning → Running.
        instance_lifecycle_transition(
            rqctx,
            path,
            Action::InstanceRestart,
            &[LifecycleStateKind::Running],
            LifecycleState::Stopping,
            Some(JobKindTemplate::Restart),
        )
        .await
    }

    async fn list_instance_nics(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectInstancePath>,
    ) -> Result<HttpResponseOk<Vec<Nic>>, HttpError> {
        let ctx = rqctx.context();
        let TenantProjectInstancePath {
            tenant_id,
            project_id,
            instance_id,
        } = path.into_inner();
        authenticate_and_authorize_in_tenant(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::NicList,
            tenant_id,
        )
        .await?;
        // Defence-in-depth: instance must live in path's silo+project.
        let instance = ctx
            .store
            .get_instance(instance_id)
            .await
            .map_err(store_error_to_http)?;
        if instance.tenant_id != tenant_id || instance.project_id != project_id {
            return Err(not_found());
        }
        let nics = ctx
            .store
            .list_nics_for_instance(instance_id)
            .await
            .map_err(store_error_to_http)?;
        Ok(HttpResponseOk(nics))
    }

    async fn get_instance_nic(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectInstanceNicPath>,
    ) -> Result<HttpResponseOk<Nic>, HttpError> {
        let ctx = rqctx.context();
        let TenantProjectInstanceNicPath {
            tenant_id,
            project_id,
            instance_id,
            nic_id,
        } = path.into_inner();
        authenticate_and_authorize_in_tenant(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::NicGet,
            tenant_id,
        )
        .await?;
        let nic = ctx
            .store
            .get_nic(nic_id)
            .await
            .map_err(store_error_to_http)?;
        // Defence-in-depth: NIC must live under all three path levels.
        if nic.tenant_id != tenant_id
            || nic.project_id != project_id
            || nic.instance_id != instance_id
        {
            return Err(not_found());
        }
        Ok(HttpResponseOk(nic))
    }

    async fn list_instance_disks(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectInstancePath>,
    ) -> Result<HttpResponseOk<Vec<Disk>>, HttpError> {
        let ctx = rqctx.context();
        let TenantProjectInstancePath {
            tenant_id,
            project_id,
            instance_id,
        } = path.into_inner();
        authenticate_and_authorize_in_tenant(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::DiskList,
            tenant_id,
        )
        .await?;
        // Defence-in-depth: instance must live in path silo+project.
        let instance = ctx
            .store
            .get_instance(instance_id)
            .await
            .map_err(store_error_to_http)?;
        if instance.tenant_id != tenant_id || instance.project_id != project_id {
            return Err(not_found());
        }
        let disks = ctx
            .store
            .list_disks_for_instance(instance_id)
            .await
            .map_err(store_error_to_http)?;
        Ok(HttpResponseOk(disks))
    }

    async fn get_instance_disk(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectInstanceDiskPath>,
    ) -> Result<HttpResponseOk<Disk>, HttpError> {
        let ctx = rqctx.context();
        let TenantProjectInstanceDiskPath {
            tenant_id,
            project_id,
            instance_id,
            disk_id,
        } = path.into_inner();
        authenticate_and_authorize_in_tenant(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::DiskGet,
            tenant_id,
        )
        .await?;
        let disk = ctx
            .store
            .get_disk(disk_id)
            .await
            .map_err(store_error_to_http)?;
        // Defence-in-depth on all three parent ids.
        if disk.tenant_id != tenant_id
            || disk.project_id != project_id
            || disk.instance_id != instance_id
        {
            return Err(not_found());
        }
        Ok(HttpResponseOk(disk))
    }

    async fn list_project_floating_ips(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectPath>,
    ) -> Result<HttpResponseOk<Vec<FloatingIp>>, HttpError> {
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
            Action::FloatingIpList,
            tenant_id,
        )
        .await?;
        // Defence-in-depth: project must live in path's silo.
        let project = ctx
            .store
            .get_project(project_id)
            .await
            .map_err(store_error_to_http)?;
        if project.tenant_id != tenant_id {
            return Err(not_found());
        }
        let fips = ctx
            .store
            .list_floating_ips_in_project(project_id)
            .await
            .map_err(store_error_to_http)?;
        Ok(HttpResponseOk(fips))
    }

    async fn create_project_floating_ip(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectPath>,
        body: TypedBody<NewFloatingIp>,
    ) -> Result<HttpResponseCreated<FloatingIp>, HttpError> {
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
            Action::FloatingIpCreate,
            tenant_id,
        )
        .await?;
        let request_id = parse_request_id(&rqctx);
        let req = body.into_inner();

        match ctx
            .store
            .create_floating_ip(tenant_id, project_id, req)
            .await
        {
            Ok(fip) => {
                ctx.audit
                    .record_mutation(
                        &principal,
                        Action::FloatingIpCreate,
                        request_id,
                        Some(format!("FloatingIp::\"{}\"", fip.id)),
                        AuditOutcome::Success {
                            resource: Some(format!("FloatingIp::\"{}\"", fip.id)),
                        },
                        serde_json::json!({
                            "tenant_id": tenant_id,
                            "project_id": project_id,
                            "name": fip.name,
                            "address": fip.address.to_string(),
                        }),
                    )
                    .await;
                Ok(HttpResponseCreated(fip))
            }
            Err(e) => {
                ctx.audit
                    .record_mutation(
                        &principal,
                        Action::FloatingIpCreate,
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

    async fn get_project_floating_ip(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectFloatingIpPath>,
    ) -> Result<HttpResponseOk<FloatingIp>, HttpError> {
        let ctx = rqctx.context();
        let TenantProjectFloatingIpPath {
            tenant_id,
            project_id,
            floating_ip_id,
        } = path.into_inner();
        authenticate_and_authorize_in_tenant(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::FloatingIpGet,
            tenant_id,
        )
        .await?;
        let fip = ctx
            .store
            .get_floating_ip(floating_ip_id)
            .await
            .map_err(store_error_to_http)?;
        if fip.tenant_id != tenant_id || fip.project_id != project_id {
            return Err(not_found());
        }
        Ok(HttpResponseOk(fip))
    }

    async fn delete_project_floating_ip(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectFloatingIpPath>,
    ) -> Result<HttpResponseDeleted, HttpError> {
        let ctx = rqctx.context();
        let TenantProjectFloatingIpPath {
            tenant_id,
            project_id,
            floating_ip_id,
        } = path.into_inner();
        let principal = authenticate_and_authorize_in_tenant(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::FloatingIpDelete,
            tenant_id,
        )
        .await?;
        let request_id = parse_request_id(&rqctx);

        // Defence-in-depth: confirm the FloatingIp lives under
        // path's silo+project before invoking delete.
        let fip = ctx
            .store
            .get_floating_ip(floating_ip_id)
            .await
            .map_err(store_error_to_http)?;
        if fip.tenant_id != tenant_id || fip.project_id != project_id {
            return Err(not_found());
        }
        match ctx.store.delete_floating_ip(floating_ip_id).await {
            Ok(()) => {
                ctx.audit
                    .record_mutation(
                        &principal,
                        Action::FloatingIpDelete,
                        request_id,
                        Some(format!("FloatingIp::\"{floating_ip_id}\"")),
                        AuditOutcome::Success {
                            resource: Some(format!("FloatingIp::\"{floating_ip_id}\"")),
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
                        Action::FloatingIpDelete,
                        request_id,
                        Some(format!("FloatingIp::\"{floating_ip_id}\"")),
                        store_error_to_audit_outcome(&e),
                        serde_json::Value::Null,
                    )
                    .await;
                Err(store_error_to_http(e))
            }
        }
    }

    async fn attach_project_floating_ip(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectFloatingIpPath>,
        body: TypedBody<AttachFloatingIpRequest>,
    ) -> Result<HttpResponseOk<FloatingIp>, HttpError> {
        let ctx = rqctx.context();
        let TenantProjectFloatingIpPath {
            tenant_id,
            project_id,
            floating_ip_id,
        } = path.into_inner();
        let principal = authenticate_and_authorize_in_tenant(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::FloatingIpAttach,
            tenant_id,
        )
        .await?;
        let request_id = parse_request_id(&rqctx);
        let req = body.into_inner();

        // Defence-in-depth on the FloatingIp itself.
        let fip = ctx
            .store
            .get_floating_ip(floating_ip_id)
            .await
            .map_err(store_error_to_http)?;
        if fip.tenant_id != tenant_id || fip.project_id != project_id {
            return Err(not_found());
        }
        match ctx
            .store
            .attach_floating_ip(floating_ip_id, req.nic_id)
            .await
        {
            Ok(updated) => {
                ctx.audit
                    .record_mutation(
                        &principal,
                        Action::FloatingIpAttach,
                        request_id,
                        Some(format!("FloatingIp::\"{floating_ip_id}\"")),
                        AuditOutcome::Success {
                            resource: Some(format!("FloatingIp::\"{floating_ip_id}\"")),
                        },
                        serde_json::json!({
                            "tenant_id": tenant_id,
                            "project_id": project_id,
                            "nic_id": req.nic_id,
                        }),
                    )
                    .await;
                Ok(HttpResponseOk(updated))
            }
            Err(e) => {
                ctx.audit
                    .record_mutation(
                        &principal,
                        Action::FloatingIpAttach,
                        request_id,
                        Some(format!("FloatingIp::\"{floating_ip_id}\"")),
                        store_error_to_audit_outcome(&e),
                        serde_json::Value::Null,
                    )
                    .await;
                Err(store_error_to_http(e))
            }
        }
    }

    async fn detach_project_floating_ip(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectFloatingIpPath>,
    ) -> Result<HttpResponseOk<FloatingIp>, HttpError> {
        let ctx = rqctx.context();
        let TenantProjectFloatingIpPath {
            tenant_id,
            project_id,
            floating_ip_id,
        } = path.into_inner();
        let principal = authenticate_and_authorize_in_tenant(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::FloatingIpDetach,
            tenant_id,
        )
        .await?;
        let request_id = parse_request_id(&rqctx);

        let fip = ctx
            .store
            .get_floating_ip(floating_ip_id)
            .await
            .map_err(store_error_to_http)?;
        if fip.tenant_id != tenant_id || fip.project_id != project_id {
            return Err(not_found());
        }
        match ctx.store.detach_floating_ip(floating_ip_id).await {
            Ok(updated) => {
                ctx.audit
                    .record_mutation(
                        &principal,
                        Action::FloatingIpDetach,
                        request_id,
                        Some(format!("FloatingIp::\"{floating_ip_id}\"")),
                        AuditOutcome::Success {
                            resource: Some(format!("FloatingIp::\"{floating_ip_id}\"")),
                        },
                        serde_json::json!({
                            "tenant_id": tenant_id,
                            "project_id": project_id,
                        }),
                    )
                    .await;
                Ok(HttpResponseOk(updated))
            }
            Err(e) => {
                ctx.audit
                    .record_mutation(
                        &principal,
                        Action::FloatingIpDetach,
                        request_id,
                        Some(format!("FloatingIp::\"{floating_ip_id}\"")),
                        store_error_to_audit_outcome(&e),
                        serde_json::Value::Null,
                    )
                    .await;
                Err(store_error_to_http(e))
            }
        }
    }

    // ----- CN heartbeat / status (slice D) -----

    async fn agent_heartbeat(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseOk<()>, HttpError> {
        let ctx = rqctx.context();
        let principal = authenticate_and_authorize(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::AgentHeartbeat,
        )
        .await?;
        // Heartbeat REQUIRES a bound key — there's no other way
        // to know which CN to attribute the ping to. Unbound
        // keys (legacy operator-minted) get 403.
        let server_uuid = require_bound_cn(&principal)?;
        ctx.store
            .update_cn_last_seen(server_uuid, chrono::Utc::now())
            .await
            .map_err(store_error_to_http)?;
        // Heartbeat is a hot path; we deliberately don't audit
        // every ping. The Cn record's `last_seen` is the
        // observable signal an operator cares about.
        Ok(HttpResponseOk(()))
    }

    async fn agent_status(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<AgentStatusRequest>,
    ) -> Result<HttpResponseOk<()>, HttpError> {
        let ctx = rqctx.context();
        let principal = authenticate_and_authorize(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::AgentStatus,
        )
        .await?;
        let server_uuid = require_bound_cn(&principal)?;
        let req = body.into_inner();
        let now = chrono::Utc::now();
        let payload = req.payload;
        ctx.store
            .update_cn_status(server_uuid, payload.clone(), now)
            .await
            .map_err(store_error_to_http)?;
        // Status updates are also hot (~once per minute or
        // when zoneevent fires); no per-update audit. A future
        // slice may sample at low frequency for forensics.
        //
        // Classifier pass is best-effort: parse the report, run the
        // pure classifier, and fold per-VM outcomes (LegacyVm
        // upsert, Orphan/StaleFingerprint warnings) into the store.
        // Any failure is logged but does NOT fail the agent's
        // status post -- the heartbeater retries on its own cadence
        // and we'd rather drop one classifier pass than 503 an
        // operational heartbeat.
        if let Err(e) = run_classifier_pass(ctx, server_uuid, &payload, now).await {
            tracing::warn!(
                error = %e,
                server_uuid = %server_uuid,
                "classifier pass failed; status post still acked",
            );
        }
        Ok(HttpResponseOk(()))
    }

    async fn agent_report_network_realization(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<NetworkRealizationRequest>,
    ) -> Result<HttpResponseOk<()>, HttpError> {
        let ctx = rqctx.context();
        let principal = authenticate_and_authorize(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::NetworkRealizationReport,
        )
        .await?;
        let bound_cn = require_bound_cn(&principal)?;
        let req = body.into_inner();
        enforce_realizer_belongs_to_bound_cn(req.realizer, bound_cn)?;
        ensure_realization_resource_exists(ctx.store.as_ref(), req.resource).await?;
        ctx.store
            .record_network_realization(
                req.resource,
                req.realizer,
                req.generation,
                req.status,
                req.message,
            )
            .await
            .map_err(store_error_to_http)?;
        // Realization reports are state-sample traffic, not an
        // operator mutation stream. The per-resource realization
        // rows are the durable signal; auditing every periodic
        // report would make the audit chain noisy.
        Ok(HttpResponseOk(()))
    }

    // ----- CN registration / approval (slice C) -----

    async fn agent_register(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<RegisterCnRequest>,
    ) -> Result<HttpResponseOk<RegisterCnResponse>, HttpError> {
        let ctx = rqctx.context();
        // Cedar gate (anonymous → public-actions list).
        let principal = authenticate_and_authorize(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::AgentRegister,
        )
        .await?;
        let request_id = parse_request_id(&rqctx);
        let req = body.into_inner();
        let now = chrono::Utc::now();

        let cn = ctx
            .store
            .register_cn(
                req.server_uuid,
                req.hostname.clone(),
                req.admin_ip,
                req.sysinfo.clone(),
                now,
            )
            .await
            .map_err(store_error_to_http)?;

        // Auto-approve path: register_cn returned a fresh Approved
        // record without a bound key. Mint the key + wire it in so
        // the agent's first long-poll can retrieve it.
        let mut effective = cn.clone();
        if effective.state == CnState::Approved && effective.bound_api_key_id.is_none() {
            match mint_and_attach_cn_credential(ctx, &principal, request_id, &effective).await {
                Ok(updated) => effective = updated,
                Err(http) => return Err(http),
            }
        }

        ctx.audit
            .record_mutation(
                &principal,
                Action::AgentRegister,
                request_id,
                Some(format!("Cn::\"{}\"", effective.server_uuid)),
                AuditOutcome::Success {
                    resource: Some(format!("Cn::\"{}\"", effective.server_uuid)),
                },
                serde_json::json!({
                    "server_uuid": effective.server_uuid,
                    "hostname": req.hostname,
                    "admin_ip": req.admin_ip,
                    "state": effective.state,
                    "auto_approved": effective.state == CnState::Approved
                        && effective.approved_at == Some(now),
                }),
            )
            .await;

        Ok(HttpResponseOk(RegisterCnResponse {
            server_uuid: effective.server_uuid,
            state: effective.state,
            claim_code: effective
                .claim_code
                .as_deref()
                .map(tritond_store::format_claim_code),
            claim_code_expires_at: effective.claim_code_expires_at,
            poll_token: effective.poll_token,
        }))
    }

    async fn agent_register_status(
        rqctx: RequestContext<Self::Context>,
        query: Query<RegisterStatusQuery>,
    ) -> Result<HttpResponseOk<RegisterStatusResponse>, HttpError> {
        let ctx = rqctx.context();
        // Cedar gate (anonymous → public-actions list).
        authenticate_and_authorize(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::AgentRegisterStatus,
        )
        .await?;
        let q = query.into_inner();

        // Long-poll: spin until state flips, an Approved record has
        // a credential to retrieve, or we hit the deadline. The
        // 30s wall-clock cap matches typical operator-side approve
        // latency and keeps idle connections from accumulating.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let poll_interval = std::time::Duration::from_millis(500);

        loop {
            let cn = match ctx.store.get_cn_by_poll_token(&q.poll_token).await {
                Ok(c) => c,
                Err(StoreError::NotFound) => {
                    return Err(HttpError::for_client_error(
                        Some("NotFound".to_string()),
                        ClientErrorStatusCode::NOT_FOUND,
                        "unknown poll token".to_string(),
                    ));
                }
                Err(e) => return Err(store_error_to_http(e)),
            };

            if cn.state == CnState::Approved {
                let credential = ctx
                    .store
                    .consume_cn_pending_credential(&q.poll_token)
                    .await
                    .map_err(store_error_to_http)?;
                return Ok(HttpResponseOk(RegisterStatusResponse {
                    state: cn.state,
                    api_key: credential,
                }));
            }
            if cn.state == CnState::Disabled {
                return Ok(HttpResponseOk(RegisterStatusResponse {
                    state: cn.state,
                    api_key: None,
                }));
            }

            if std::time::Instant::now() >= deadline {
                return Ok(HttpResponseOk(RegisterStatusResponse {
                    state: cn.state,
                    api_key: None,
                }));
            }
            tokio::time::sleep(poll_interval).await;
        }
    }

    async fn list_cns(
        rqctx: RequestContext<Self::Context>,
        query: Query<CnListQuery>,
    ) -> Result<HttpResponseOk<Vec<CnView>>, HttpError> {
        let ctx = rqctx.context();
        authenticate_and_authorize(&rqctx, &ctx.auth, &ctx.audit, &ctx.store, Action::CnList)
            .await?;
        let cns = ctx
            .store
            .list_cns(query.into_inner().state)
            .await
            .map_err(store_error_to_http)?;
        Ok(HttpResponseOk(cns.into_iter().map(CnView::from).collect()))
    }

    async fn get_cn(
        rqctx: RequestContext<Self::Context>,
        path: Path<CnPath>,
    ) -> Result<HttpResponseOk<CnView>, HttpError> {
        let ctx = rqctx.context();
        authenticate_and_authorize(&rqctx, &ctx.auth, &ctx.audit, &ctx.store, Action::CnGet)
            .await?;
        let cn = ctx
            .store
            .get_cn(path.into_inner().server_uuid)
            .await
            .map_err(store_error_to_http)?;
        Ok(HttpResponseOk(CnView::from(cn)))
    }

    async fn approve_cn(
        rqctx: RequestContext<Self::Context>,
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

        let principal = authenticate_and_authorize(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::CnApprove,
        )
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

    async fn disable_cn(
        rqctx: RequestContext<Self::Context>,
        path: Path<CnPath>,
    ) -> Result<HttpResponseOk<CnView>, HttpError> {
        let ctx = rqctx.context();
        let principal = authenticate_and_authorize(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::CnDisable,
        )
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

    async fn set_cn_role(
        rqctx: RequestContext<Self::Context>,
        path: Path<CnPath>,
        body: TypedBody<SetCnRoleRequest>,
    ) -> Result<HttpResponseOk<CnView>, HttpError> {
        let ctx = rqctx.context();
        let principal = authenticate_and_authorize(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::CnSetRole,
        )
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

    async fn get_auto_approve_window(
        rqctx: RequestContext<Self::Context>,
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

    async fn open_auto_approve_window(
        rqctx: RequestContext<Self::Context>,
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
                + chrono::Duration::from_std(clamped)
                    .unwrap_or_else(|_| chrono::Duration::seconds(0)),
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

    async fn close_auto_approve_window(
        rqctx: RequestContext<Self::Context>,
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

    async fn list_config(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseOk<Vec<ConfigEntry>>, HttpError> {
        let ctx = rqctx.context();
        authenticate_and_authorize(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::ConfigList,
        )
        .await?;
        let settings = ctx
            .store
            .get_settings()
            .await
            .map_err(store_error_to_http)?;
        let entries = ConfigKey::ALL
            .into_iter()
            .map(|k| build_config_entry(k, &settings))
            .collect();
        Ok(HttpResponseOk(entries))
    }

    async fn get_config(
        rqctx: RequestContext<Self::Context>,
        path: Path<ConfigKeyPath>,
    ) -> Result<HttpResponseOk<ConfigEntry>, HttpError> {
        let ctx = rqctx.context();
        authenticate_and_authorize(&rqctx, &ctx.auth, &ctx.audit, &ctx.store, Action::ConfigGet)
            .await?;
        let key = config_key_or_404(&path.into_inner().key)?;
        let settings = ctx
            .store
            .get_settings()
            .await
            .map_err(store_error_to_http)?;
        Ok(HttpResponseOk(build_config_entry(key, &settings)))
    }

    async fn set_config(
        rqctx: RequestContext<Self::Context>,
        path: Path<ConfigKeyPath>,
        body: TypedBody<SetConfigRequest>,
    ) -> Result<HttpResponseOk<ConfigEntry>, HttpError> {
        let ctx = rqctx.context();
        let principal = authenticate_and_authorize(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::ConfigSet,
        )
        .await?;
        let request_id = parse_request_id(&rqctx);
        let key = config_key_or_404(&path.into_inner().key)?;
        let new_value = body.into_inner().value;

        let mut settings = ctx
            .store
            .get_settings()
            .await
            .map_err(store_error_to_http)?;
        let previous = settings.get(key);
        settings.set(key, new_value).map_err(|e| match e {
            ConfigError::InvalidValue { key, message } => HttpError::for_bad_request(
                Some("BadRequest".to_string()),
                format!("invalid value for {key}: {message}"),
            ),
            ConfigError::UnknownKey(k) => HttpError::for_client_error(
                Some("NotFound".to_string()),
                ClientErrorStatusCode::NOT_FOUND,
                format!("unknown config key: {k}"),
            ),
        })?;
        ctx.store
            .put_settings(settings.clone())
            .await
            .map_err(store_error_to_http)?;
        ctx.audit
            .record_mutation(
                &principal,
                Action::ConfigSet,
                request_id,
                Some(format!("Config::\"{}\"", key.as_str())),
                AuditOutcome::Success {
                    resource: Some(format!("Config::\"{}\"", key.as_str())),
                },
                serde_json::json!({
                    "key": key.as_str(),
                    "previous": previous,
                    "value": settings.get(key),
                }),
            )
            .await;
        Ok(HttpResponseOk(build_config_entry(key, &settings)))
    }

    async fn reset_config(
        rqctx: RequestContext<Self::Context>,
        path: Path<ConfigKeyPath>,
    ) -> Result<HttpResponseOk<ConfigEntry>, HttpError> {
        let ctx = rqctx.context();
        let principal = authenticate_and_authorize(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::ConfigReset,
        )
        .await?;
        let request_id = parse_request_id(&rqctx);
        let key = config_key_or_404(&path.into_inner().key)?;

        let mut settings = ctx
            .store
            .get_settings()
            .await
            .map_err(store_error_to_http)?;
        let previous = settings.get(key);
        settings.reset(key);
        ctx.store
            .put_settings(settings.clone())
            .await
            .map_err(store_error_to_http)?;
        ctx.audit
            .record_mutation(
                &principal,
                Action::ConfigReset,
                request_id,
                Some(format!("Config::\"{}\"", key.as_str())),
                AuditOutcome::Success {
                    resource: Some(format!("Config::\"{}\"", key.as_str())),
                },
                serde_json::json!({
                    "key": key.as_str(),
                    "previous": previous,
                    "value": settings.get(key),
                }),
            )
            .await;
        Ok(HttpResponseOk(build_config_entry(key, &settings)))
    }

    async fn list_legacy_cns(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseOk<Vec<LegacyCnSummary>>, HttpError> {
        let ctx = rqctx.context();
        authenticate_and_authorize(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::LegacyCnList,
        )
        .await?;
        let cns = ctx
            .store
            .list_cns(None)
            .await
            .map_err(store_error_to_http)?;
        let mut out = Vec::with_capacity(cns.len());
        for cn in cns {
            let managed = ctx
                .store
                .list_instances_for_cn(cn.server_uuid)
                .await
                .map_err(store_error_to_http)?;
            let legacy = ctx
                .store
                .list_legacy_vms_for_cn(cn.server_uuid)
                .await
                .map_err(store_error_to_http)?;
            out.push(LegacyCnSummary {
                server_uuid: cn.server_uuid,
                hostname: cn.hostname,
                state: cn.state,
                last_seen: cn.last_seen,
                managed_instance_count: managed.len(),
                legacy_vm_count: legacy.len(),
            });
        }
        Ok(HttpResponseOk(out))
    }

    async fn list_legacy_vms(
        rqctx: RequestContext<Self::Context>,
        query: Query<LegacyVmListQuery>,
    ) -> Result<HttpResponseOk<Vec<LegacyVm>>, HttpError> {
        let ctx = rqctx.context();
        authenticate_and_authorize(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::LegacyVmList,
        )
        .await?;
        let q = query.into_inner();
        let vms = match q.host_cn {
            Some(cn) => ctx.store.list_legacy_vms_for_cn(cn).await,
            None => ctx.store.list_legacy_vms().await,
        }
        .map_err(store_error_to_http)?;
        Ok(HttpResponseOk(vms))
    }

    async fn get_legacy_vm(
        rqctx: RequestContext<Self::Context>,
        path: Path<LegacyVmPath>,
    ) -> Result<HttpResponseOk<LegacyVm>, HttpError> {
        let ctx = rqctx.context();
        authenticate_and_authorize(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::LegacyVmGet,
        )
        .await?;
        let smartos_uuid = path.into_inner().smartos_uuid;
        let vm = ctx
            .store
            .get_legacy_vm(smartos_uuid)
            .await
            .map_err(store_error_to_http)?;
        Ok(HttpResponseOk(vm))
    }

    // ----- Storage clusters (operator-only) -----

    async fn list_storage_clusters(
        rqctx: RequestContext<Self::Context>,
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

    async fn create_storage_cluster(
        rqctx: RequestContext<Self::Context>,
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

    async fn get_storage_cluster(
        rqctx: RequestContext<Self::Context>,
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

    async fn delete_storage_cluster(
        rqctx: RequestContext<Self::Context>,
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

    async fn probe_storage_cluster_health(
        rqctx: RequestContext<Self::Context>,
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

    async fn get_storage_cluster_summary(
        rqctx: RequestContext<Self::Context>,
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

    async fn list_storage_cluster_nodes(
        rqctx: RequestContext<Self::Context>,
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
        let (_, client) = crate::storage::client_for(&ctx.store, id).await?;
        let nodes = client
            .list_nodes()
            .await
            .map_err(crate::storage::mantad_error_to_http)?;
        Ok(HttpResponseOk(
            nodes.into_iter().map(crate::storage::node_from).collect(),
        ))
    }

    async fn get_storage_cluster_node(
        rqctx: RequestContext<Self::Context>,
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
        let (_, client) = crate::storage::client_for(&ctx.store, p.id).await?;
        let node = client
            .get_node(p.node_id)
            .await
            .map_err(crate::storage::mantad_error_to_http)?;
        Ok(HttpResponseOk(crate::storage::node_from(node)))
    }

    async fn add_storage_cluster_node(
        rqctx: RequestContext<Self::Context>,
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
        let (_, client) = crate::storage::client_for(&ctx.store, id).await?;
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

    async fn remove_storage_cluster_node(
        rqctx: RequestContext<Self::Context>,
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
        let (_, client) = crate::storage::client_for(&ctx.store, p.id).await?;
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

    async fn drain_storage_cluster_node(
        rqctx: RequestContext<Self::Context>,
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

    async fn undrain_storage_cluster_node(
        rqctx: RequestContext<Self::Context>,
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

    async fn reweight_storage_cluster_node(
        rqctx: RequestContext<Self::Context>,
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
        let (_, client) = crate::storage::client_for(&ctx.store, p.id).await?;
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

    async fn get_storage_cluster_membership(
        rqctx: RequestContext<Self::Context>,
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
        let (_, client) = crate::storage::client_for(&ctx.store, id).await?;
        let m = client
            .membership()
            .await
            .map_err(crate::storage::mantad_error_to_http)?;
        Ok(HttpResponseOk(crate::storage::membership_from(m)))
    }

    async fn list_storage_cluster_buckets(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterPath>,
        query: Query<tritond_api::StorageBucketListQuery>,
    ) -> Result<HttpResponseOk<Vec<StorageBucket>>, HttpError> {
        let ctx = rqctx.context();
        authenticate_and_authorize(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::StorageBucketList,
        )
        .await?;
        let id = path.into_inner().id;
        let with_stats = query.into_inner().stats.unwrap_or(false);
        let (_, client) = crate::storage::client_for(&ctx.store, id).await?;
        let buckets = client
            .list_buckets(with_stats)
            .await
            .map_err(crate::storage::mantad_error_to_http)?;
        Ok(HttpResponseOk(
            buckets
                .into_iter()
                .map(crate::storage::bucket_from)
                .collect(),
        ))
    }

    async fn get_storage_cluster_bucket(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterBucketPath>,
    ) -> Result<HttpResponseOk<StorageBucket>, HttpError> {
        let ctx = rqctx.context();
        authenticate_and_authorize(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::StorageBucketGet,
        )
        .await?;
        let p = path.into_inner();
        let (_, client) = crate::storage::client_for(&ctx.store, p.id).await?;
        let b = client
            .get_bucket(&p.bucket)
            .await
            .map_err(crate::storage::mantad_error_to_http)?;
        Ok(HttpResponseOk(crate::storage::bucket_from(b)))
    }

    async fn create_storage_cluster_bucket(
        rqctx: RequestContext<Self::Context>,
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
        let request_id = parse_request_id(&rqctx);
        let id = path.into_inner().id;
        let req = body.into_inner();
        let payload = serde_json::json!({
            "name": req.name,
            "owner": req.owner,
        });
        let (_, client) = crate::storage::client_for(&ctx.store, id).await?;
        let mantad_req = crate::storage::create_bucket_request_to(req);
        match client.create_bucket(&mantad_req).await {
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

    async fn delete_storage_cluster_bucket(
        rqctx: RequestContext<Self::Context>,
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
        let request_id = parse_request_id(&rqctx);
        let p = path.into_inner();
        let (_, client) = crate::storage::client_for(&ctx.store, p.id).await?;
        match client.delete_bucket(&p.bucket).await {
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

    async fn list_storage_cluster_objects(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterBucketPath>,
        query: Query<tritond_api::StorageObjectsQuery>,
    ) -> Result<HttpResponseOk<StorageObjectsPage>, HttpError> {
        let ctx = rqctx.context();
        authenticate_and_authorize(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::StorageObjectList,
        )
        .await?;
        let p = path.into_inner();
        let q = crate::storage::objects_query_to(query.into_inner());
        let (_, client) = crate::storage::client_for(&ctx.store, p.id).await?;
        let page = client
            .list_objects(&p.bucket, &q)
            .await
            .map_err(crate::storage::mantad_error_to_http)?;
        Ok(HttpResponseOk(crate::storage::objects_page_from(page)))
    }

    async fn list_storage_cluster_users(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterPath>,
    ) -> Result<HttpResponseOk<Vec<StorageUser>>, HttpError> {
        let ctx = rqctx.context();
        authenticate_and_authorize(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::StorageUserList,
        )
        .await?;
        let id = path.into_inner().id;
        let (_, client) = crate::storage::client_for(&ctx.store, id).await?;
        let users = client
            .list_users()
            .await
            .map_err(crate::storage::mantad_error_to_http)?;
        Ok(HttpResponseOk(
            users.into_iter().map(crate::storage::user_from).collect(),
        ))
    }

    async fn create_storage_cluster_user(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterPath>,
        body: TypedBody<tritond_api::StorageCreateUserRequest>,
    ) -> Result<HttpResponseCreated<StorageUser>, HttpError> {
        let ctx = rqctx.context();
        let principal = authenticate_and_authorize(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::StorageUserCreate,
        )
        .await?;
        let request_id = parse_request_id(&rqctx);
        let id = path.into_inner().id;
        let req = body.into_inner();
        let payload = serde_json::json!({ "name": req.name });
        let (_, client) = crate::storage::client_for(&ctx.store, id).await?;
        let mantad_req = crate::storage::create_user_request_to(req);
        match client.create_user(&mantad_req).await {
            Ok(u) => {
                let view = crate::storage::user_from(u);
                ctx.audit
                    .record_mutation(
                        &principal,
                        Action::StorageUserCreate,
                        request_id,
                        Some(format!("StorageUser::\"{}\"", view.name)),
                        AuditOutcome::Success {
                            resource: Some(format!("StorageUser::\"{}\"", view.name)),
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
                        Action::StorageUserCreate,
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

    async fn get_storage_cluster_user(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterUserPath>,
    ) -> Result<HttpResponseOk<StorageUser>, HttpError> {
        let ctx = rqctx.context();
        authenticate_and_authorize(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::StorageUserGet,
        )
        .await?;
        let p = path.into_inner();
        let (_, client) = crate::storage::client_for(&ctx.store, p.id).await?;
        let u = client
            .get_user(&p.user)
            .await
            .map_err(crate::storage::mantad_error_to_http)?;
        Ok(HttpResponseOk(crate::storage::user_from(u)))
    }

    async fn delete_storage_cluster_user(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterUserPath>,
    ) -> Result<HttpResponseDeleted, HttpError> {
        let ctx = rqctx.context();
        let principal = authenticate_and_authorize(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::StorageUserDelete,
        )
        .await?;
        let request_id = parse_request_id(&rqctx);
        let p = path.into_inner();
        let (_, client) = crate::storage::client_for(&ctx.store, p.id).await?;
        match client.delete_user(&p.user).await {
            Ok(()) => {
                ctx.audit
                    .record_mutation(
                        &principal,
                        Action::StorageUserDelete,
                        request_id,
                        Some(format!("StorageUser::\"{}\"", p.user)),
                        AuditOutcome::Success {
                            resource: Some(format!("StorageUser::\"{}\"", p.user)),
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
                        Action::StorageUserDelete,
                        request_id,
                        Some(format!("StorageUser::\"{}\"", p.user)),
                        audit_outcome,
                        serde_json::Value::Null,
                    )
                    .await;
                Err(http_err)
            }
        }
    }

    async fn list_storage_cluster_access_keys(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterUserPath>,
    ) -> Result<HttpResponseOk<Vec<StorageAccessKey>>, HttpError> {
        let ctx = rqctx.context();
        authenticate_and_authorize(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::StorageAccessKeyList,
        )
        .await?;
        let p = path.into_inner();
        let (_, client) = crate::storage::client_for(&ctx.store, p.id).await?;
        let keys = client
            .list_access_keys(&p.user)
            .await
            .map_err(crate::storage::mantad_error_to_http)?;
        Ok(HttpResponseOk(
            keys.into_iter()
                .map(crate::storage::access_key_from)
                .collect(),
        ))
    }

    async fn create_storage_cluster_access_key(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterUserPath>,
    ) -> Result<HttpResponseCreated<StorageAccessKey>, HttpError> {
        let ctx = rqctx.context();
        let principal = authenticate_and_authorize(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::StorageAccessKeyCreate,
        )
        .await?;
        let request_id = parse_request_id(&rqctx);
        let p = path.into_inner();
        let payload = serde_json::json!({ "user": p.user });
        let (_, client) = crate::storage::client_for(&ctx.store, p.id).await?;
        match client.create_access_key(&p.user).await {
            Ok(k) => {
                let view = crate::storage::access_key_from(k);
                // Audit captures only the AKID — the cleartext
                // secret is in the response and must not enter the
                // audit chain.
                ctx.audit
                    .record_mutation(
                        &principal,
                        Action::StorageAccessKeyCreate,
                        request_id,
                        Some(format!("StorageAccessKey::\"{}\"", view.access_key_id)),
                        AuditOutcome::Success {
                            resource: Some(format!("StorageAccessKey::\"{}\"", view.access_key_id)),
                        },
                        serde_json::json!({
                            "user": view.user,
                            "access_key_id": view.access_key_id,
                        }),
                    )
                    .await;
                Ok(HttpResponseCreated(view))
            }
            Err(e) => {
                let (http_err, audit_outcome) = crate::storage::mantad_error_to_http_audit(e);
                ctx.audit
                    .record_mutation(
                        &principal,
                        Action::StorageAccessKeyCreate,
                        request_id,
                        None,
                        audit_outcome,
                        payload,
                    )
                    .await;
                Err(http_err)
            }
        }
    }

    async fn delete_storage_cluster_access_key(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterAccessKeyPath>,
    ) -> Result<HttpResponseDeleted, HttpError> {
        let ctx = rqctx.context();
        let principal = authenticate_and_authorize(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::StorageAccessKeyDelete,
        )
        .await?;
        let request_id = parse_request_id(&rqctx);
        let p = path.into_inner();
        let (_, client) = crate::storage::client_for(&ctx.store, p.id).await?;
        match client.delete_access_key(&p.access_key_id).await {
            Ok(()) => {
                ctx.audit
                    .record_mutation(
                        &principal,
                        Action::StorageAccessKeyDelete,
                        request_id,
                        Some(format!("StorageAccessKey::\"{}\"", p.access_key_id)),
                        AuditOutcome::Success {
                            resource: Some(format!("StorageAccessKey::\"{}\"", p.access_key_id)),
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
                        Action::StorageAccessKeyDelete,
                        request_id,
                        Some(format!("StorageAccessKey::\"{}\"", p.access_key_id)),
                        audit_outcome,
                        serde_json::Value::Null,
                    )
                    .await;
                Err(http_err)
            }
        }
    }

    async fn list_storage_cluster_user_policies(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterUserPath>,
    ) -> Result<HttpResponseOk<Vec<String>>, HttpError> {
        let ctx = rqctx.context();
        authenticate_and_authorize(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::StorageUserPolicyList,
        )
        .await?;
        let p = path.into_inner();
        let (_, client) = crate::storage::client_for(&ctx.store, p.id).await?;
        let policies = client
            .list_user_policies(&p.user)
            .await
            .map_err(crate::storage::mantad_error_to_http)?;
        Ok(HttpResponseOk(policies))
    }

    async fn get_storage_cluster_user_policy(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterUserPolicyPath>,
    ) -> Result<HttpResponseOk<serde_json::Value>, HttpError> {
        let ctx = rqctx.context();
        authenticate_and_authorize(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::StorageUserPolicyGet,
        )
        .await?;
        let p = path.into_inner();
        let (_, client) = crate::storage::client_for(&ctx.store, p.id).await?;
        let doc = client
            .get_user_policy(&p.user, &p.policy)
            .await
            .map_err(crate::storage::mantad_error_to_http)?;
        Ok(HttpResponseOk(doc))
    }

    async fn put_storage_cluster_user_policy(
        rqctx: RequestContext<Self::Context>,
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

    async fn delete_storage_cluster_user_policy(
        rqctx: RequestContext<Self::Context>,
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

    async fn set_storage_cluster_presigner(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterPath>,
        body: TypedBody<SetPresignerRequest>,
    ) -> Result<HttpResponseOk<StorageClusterView>, HttpError> {
        let ctx = rqctx.context();
        let principal = authenticate_and_authorize(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::StorageClusterSetPresigner,
        )
        .await?;
        let request_id = parse_request_id(&rqctx);
        let id = path.into_inner().id;
        let req = body.into_inner();
        // Empty strings on the wire mean "clear the credentials" —
        // map them to None so the store layer's contract (Some+Some
        // or None+None) is honored.
        let akid = if req.access_key_id.is_empty() {
            None
        } else {
            Some(req.access_key_id.clone())
        };
        let secret = if req.secret_access_key.is_empty() {
            None
        } else {
            Some(req.secret_access_key)
        };
        // Audit payload deliberately captures only AKID + endpoint
        // — the secret is opaque to the audit chain just like
        // mantad's admin token.
        let audit_payload = serde_json::json!({
            "s3_endpoint": req.s3_endpoint,
            "access_key_id": akid,
        });
        match ctx
            .store
            .update_storage_cluster_presigner(id, req.s3_endpoint, akid, secret)
            .await
        {
            Ok(cluster) => {
                let view: StorageClusterView = cluster.into();
                ctx.audit
                    .record_mutation(
                        &principal,
                        Action::StorageClusterSetPresigner,
                        request_id,
                        Some(format!("StorageCluster::\"{id}\"")),
                        AuditOutcome::Success {
                            resource: Some(format!("StorageCluster::\"{id}\"")),
                        },
                        audit_payload,
                    )
                    .await;
                Ok(HttpResponseOk(view))
            }
            Err(e) => {
                ctx.audit
                    .record_mutation(
                        &principal,
                        Action::StorageClusterSetPresigner,
                        request_id,
                        Some(format!("StorageCluster::\"{id}\"")),
                        store_error_to_audit_outcome(&e),
                        audit_payload,
                    )
                    .await;
                Err(store_error_to_http(e))
            }
        }
    }

    async fn presign_storage_cluster_object_put(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterPath>,
        body: TypedBody<PresignPutRequest>,
    ) -> Result<HttpResponseOk<PresignResponse>, HttpError> {
        let ctx = rqctx.context();
        let principal = authenticate_and_authorize(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::StorageObjectPresignPut,
        )
        .await?;
        let request_id = parse_request_id(&rqctx);
        let id = path.into_inner().id;
        let req = body.into_inner();
        let resp = crate::storage::mint_presigned_url(
            &ctx.store,
            id,
            "PUT",
            &req.bucket,
            &req.key,
            req.expires_secs,
        )
        .await?;
        ctx.audit
            .record_mutation(
                &principal,
                Action::StorageObjectPresignPut,
                request_id,
                Some(format!("StorageObject::\"{}/{}\"", req.bucket, req.key)),
                AuditOutcome::Success {
                    resource: Some(format!("StorageObject::\"{}/{}\"", req.bucket, req.key)),
                },
                serde_json::json!({
                    "bucket": req.bucket,
                    "key": req.key,
                    "expires_secs": req.expires_secs,
                }),
            )
            .await;
        Ok(HttpResponseOk(resp))
    }

    async fn presign_storage_cluster_object_get(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterPath>,
        body: TypedBody<PresignGetRequest>,
    ) -> Result<HttpResponseOk<PresignResponse>, HttpError> {
        let ctx = rqctx.context();
        // Reads still get audited via authenticate_and_authorize
        // (Allow event), but we don't emit a record_mutation —
        // the GET URL doesn't change cluster state.
        authenticate_and_authorize(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::StorageObjectPresignGet,
        )
        .await?;
        let id = path.into_inner().id;
        let req = body.into_inner();
        let resp = crate::storage::mint_presigned_url(
            &ctx.store,
            id,
            "GET",
            &req.bucket,
            &req.key,
            req.expires_secs,
        )
        .await?;
        Ok(HttpResponseOk(resp))
    }

    // ----- Metrics -----

    async fn agent_metrics_ingest(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<tritond_metrics::SampleBatch>,
    ) -> Result<HttpResponseUpdatedNoContent, HttpError> {
        let ctx = rqctx.context();
        // Piggyback on AgentStatus's authz envelope: same scope
        // (Agent), same auditing characteristics (high-frequency
        // sample stream, no per-call audit). When per-action
        // granularity is needed for forensics we'll add a dedicated
        // AgentMetricsIngest variant.
        let principal = authenticate_and_authorize(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::AgentStatus,
        )
        .await?;
        let _server_uuid = require_bound_cn(&principal)?;

        let batch = body.into_inner();
        if batch.samples.len() > tritond_metrics::SampleBatch::MAX_SAMPLES {
            return Err(HttpError::for_client_error(
                None,
                dropshot::ClientErrorStatusCode::PAYLOAD_TOO_LARGE,
                format!(
                    "metrics batch of {} samples exceeds limit of {}",
                    batch.samples.len(),
                    tritond_metrics::SampleBatch::MAX_SAMPLES,
                ),
            ));
        }

        // Best-effort: a metrics-store hiccup must not 5xx the agent
        // and put it into backoff. Log and ack.
        if let Err(e) = ctx.metrics.insert(&batch.samples).await {
            tracing::warn!(error = %e, count = batch.samples.len(), "metrics insert failed");
        }
        Ok(HttpResponseUpdatedNoContent())
    }

    async fn instance_metrics_range(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectInstancePath>,
        query: Query<MetricsRangeQuery>,
    ) -> Result<HttpResponseOk<tritond_metrics::RangeResult>, HttpError> {
        let ctx = rqctx.context();
        let TenantProjectInstancePath {
            tenant_id,
            project_id,
            instance_id,
        } = path.into_inner();
        // Reuse InstanceGet authz: read access to the named
        // instance is the same trust envelope as the metrics view.
        authenticate_and_authorize_in_tenant(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::InstanceGet,
            tenant_id,
        )
        .await?;

        // Verify the instance actually belongs to this tenant +
        // project. Mirrors `get_project_instance` -- we never want
        // to leak metrics across the tenant boundary even if the
        // metrics store happens to hold matching samples.
        let instance = ctx
            .store
            .get_instance(instance_id)
            .await
            .map_err(store_error_to_http)?;
        if instance.tenant_id != tenant_id || instance.project_id != project_id {
            return Err(not_found());
        }

        let q = query.into_inner();
        let (since, until, step) = resolve_metrics_range(q.range.as_deref())?;
        let schema = q
            .schema
            .unwrap_or_else(|| tritond_metrics::schema::schemas::CPU_PER_ZONE.to_string());

        let range_query = tritond_metrics::RangeQuery {
            schema,
            // Filter on instance_id only: it's globally unique, and
            // the agent's per-zone samples don't carry tenant_id in
            // their identity (the agent doesn't know it). The
            // tenant/project ownership check above already gates
            // access to this instance's data.
            instance_id: Some(instance_id),
            tenant_id: None,
            cn_id: None,
            since,
            until,
            step,
        };
        let result = ctx
            .metrics
            .query_range(&range_query)
            .await
            .map_err(metrics_error_to_http)?;
        Ok(HttpResponseOk(result))
    }

    async fn cn_metrics_range(
        rqctx: RequestContext<Self::Context>,
        path: Path<CnPath>,
        query: Query<MetricsRangeQuery>,
    ) -> Result<HttpResponseOk<tritond_metrics::RangeResult>, HttpError> {
        let ctx = rqctx.context();
        let server_uuid = path.into_inner().server_uuid;
        // Same authz envelope as `get_cn` -- fleet-read access to
        // CN inventory implies read access to per-CN metrics.
        authenticate_and_authorize(&rqctx, &ctx.auth, &ctx.audit, &ctx.store, Action::CnGet)
            .await?;
        // Verify the CN actually exists. Without this, a stale UUID
        // returns an empty series with no signal that the CN is
        // unknown to the control plane.
        ctx.store
            .get_cn(server_uuid)
            .await
            .map_err(store_error_to_http)?;

        let q = query.into_inner();
        let (since, until, step) = resolve_metrics_range(q.range.as_deref())?;
        let schema = q
            .schema
            .unwrap_or_else(|| tritond_metrics::schema::schemas::CPU_PER_CN.to_string());

        let range_query = tritond_metrics::RangeQuery {
            schema,
            instance_id: None,
            tenant_id: None,
            cn_id: Some(server_uuid),
            since,
            until,
            step,
        };
        let result = ctx
            .metrics
            .query_range(&range_query)
            .await
            .map_err(metrics_error_to_http)?;
        Ok(HttpResponseOk(result))
    }

    // ----- Logs -----

    async fn agent_logs_ingest(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<tritond_logs::LogBatch>,
    ) -> Result<HttpResponseUpdatedNoContent, HttpError> {
        let ctx = rqctx.context();
        // Same authz envelope as metrics ingest: Agent scope, bound
        // CN. We don't dedicate a Cedar action for now -- log batches
        // and status reports are the same trust shape.
        let principal = authenticate_and_authorize(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::AgentStatus,
        )
        .await?;
        let _server_uuid = require_bound_cn(&principal)?;

        let batch = body.into_inner();
        if batch.lines.len() > tritond_logs::LogBatch::MAX_LINES {
            return Err(HttpError::for_client_error(
                None,
                dropshot::ClientErrorStatusCode::PAYLOAD_TOO_LARGE,
                format!(
                    "log batch of {} lines exceeds limit of {}",
                    batch.lines.len(),
                    tritond_logs::LogBatch::MAX_LINES,
                ),
            ));
        }

        // Fail-open: a log-store hiccup must not put the agent into
        // backoff. Log + ack.
        if let Err(e) = ctx.logs.insert(batch).await {
            tracing::warn!(error = %e, "log batch insert failed");
        }
        Ok(HttpResponseUpdatedNoContent())
    }

    async fn instance_logs_tail(
        rqctx: RequestContext<Self::Context>,
        path: Path<InstanceLogsPath>,
        query: Query<LogTailQuery>,
    ) -> Result<HttpResponseOk<tritond_logs::LogTailResult>, HttpError> {
        let ctx = rqctx.context();
        let InstanceLogsPath {
            tenant_id,
            project_id,
            instance_id,
            source,
        } = path.into_inner();
        authenticate_and_authorize_in_tenant(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::InstanceGet,
            tenant_id,
        )
        .await?;

        let instance = ctx
            .store
            .get_instance(instance_id)
            .await
            .map_err(store_error_to_http)?;
        if instance.tenant_id != tenant_id || instance.project_id != project_id {
            return Err(not_found());
        }

        let parsed_source: tritond_logs::LogSource =
            source
                .parse()
                .map_err(|e: tritond_logs::types::UnknownLogSource| {
                    HttpError::for_bad_request(None, e.to_string())
                })?;

        let q = query.into_inner();
        let lines_req = q.lines.unwrap_or(500);
        let tq = tritond_logs::LogTailQuery {
            instance_id,
            source: parsed_source,
            lines: lines_req,
            before_seq: q.before_seq,
        };
        let result = ctx.logs.tail(&tq).await.map_err(logs_error_to_http)?;
        Ok(HttpResponseOk(result))
    }
}

/// Convert a short range identifier (`5m`, `1h`, `30d`) into the
/// corresponding `(since, until, step)` triple. Step values are sized
/// so each range yields ~60-100 buckets, which matches the SVG width
/// of the V5 dashboard's chart panels.
fn resolve_metrics_range(
    range: Option<&str>,
) -> Result<
    (
        chrono::DateTime<chrono::Utc>,
        chrono::DateTime<chrono::Utc>,
        chrono::Duration,
    ),
    HttpError,
> {
    let until = chrono::Utc::now();
    let (window, step) = match range.unwrap_or("1h") {
        "5m" => (chrono::Duration::minutes(5), chrono::Duration::seconds(5)),
        "15m" => (chrono::Duration::minutes(15), chrono::Duration::seconds(15)),
        "1h" => (chrono::Duration::hours(1), chrono::Duration::seconds(60)),
        "6h" => (chrono::Duration::hours(6), chrono::Duration::minutes(5)),
        "24h" => (chrono::Duration::hours(24), chrono::Duration::minutes(15)),
        "7d" => (chrono::Duration::days(7), chrono::Duration::hours(1)),
        "30d" => (chrono::Duration::days(30), chrono::Duration::hours(6)),
        other => {
            return Err(HttpError::for_bad_request(
                None,
                format!(
                    "unsupported range '{other}'; expected one of 5m, 15m, 1h, 6h, 24h, 7d, 30d"
                ),
            ));
        }
    };
    Ok((until - window, until, step))
}

/// Single source of truth for cross-scope image visibility.
///
/// Returns `true` if `principal` can see `image`. Used by every
/// image read path (`get_image`, the per-scope list handlers) and
/// by the instance-create reference check; a wrong answer here
/// is a cross-tenant information leak.
///
/// Behaviour:
/// * Root operators (`is_root == true`) can see everything.
/// * `Public` is visible to every principal — authenticated *and*
///   anonymous (Cedar lets the latter through on the global
///   public-actions rule for `image_get`).
/// * `Silo { silo_id }` is visible iff the principal's cached
///   silo_id matches.
/// * `Tenant { tenant_id }` is visible iff the principal's
///   tenant_id matches.
/// * `Project { project_id }` resolves the project to its
///   tenant; visible iff `project.tenant_id == principal.tenant_id`.
///   (Phase 0 = "any tenant member sees any project image"; a
///   future slice can tighten to per-project membership.)
/// * `User { user_id }` is visible iff the principal's user_id
///   matches.
pub async fn image_visible_to(
    image: &Image,
    principal: &Principal,
    store: &dyn Store,
) -> Result<bool, StoreError> {
    // Root sees everything regardless of scope.
    if let Principal::Operator { is_root: true, .. } = principal {
        return Ok(true);
    }
    match &image.scope {
        ImageScope::Public => Ok(true),
        ImageScope::Silo { silo_id } => Ok(principal_silo_id(principal) == Some(*silo_id)),
        ImageScope::Tenant { tenant_id } => Ok(principal_tenant_id(principal) == Some(*tenant_id)),
        ImageScope::Project { project_id } => {
            // Phase 0: any member of the project's tenant.
            let Some(my_tenant) = principal_tenant_id(principal) else {
                return Ok(false);
            };
            match store.get_project(*project_id).await {
                Ok(project) => Ok(project.tenant_id == my_tenant),
                Err(StoreError::NotFound) => Ok(false),
                Err(e) => Err(e),
            }
        }
        ImageScope::User { user_id } => Ok(principal_user_id(principal) == Some(*user_id)),
        // ImageScope is `#[non_exhaustive]`. New variants must
        // be classified explicitly in this gate; until then they
        // deny by default to avoid silent visibility bugs.
        _ => Ok(false),
    }
}

/// Stricter than [`image_visible_to`]: returns `true` if the
/// principal is allowed to delete `image`. The ownership rules
/// match the URL-vs-scope structure:
/// * `Public` — root only.
/// * `Silo` / `Tenant` / `Project` — any tenant member of the
///   resolved tenant (Phase 0); cross-tenant returns false.
/// * `User` — the owning user only.
async fn image_deletable_by(
    image: &Image,
    principal: &Principal,
    store: &dyn Store,
) -> Result<bool, StoreError> {
    if let Principal::Operator { is_root: true, .. } = principal {
        return Ok(true);
    }
    match &image.scope {
        // Public is operator turf.
        ImageScope::Public => Ok(false),
        // Silo / Tenant / Project follow the same visibility
        // gate as reads (Phase 0 = same-tenant access). A future
        // slice can split delete from read for these scopes.
        ImageScope::Silo { .. } | ImageScope::Tenant { .. } | ImageScope::Project { .. } => {
            image_visible_to(image, principal, store).await
        }
        ImageScope::User { user_id } => Ok(principal_user_id(principal) == Some(*user_id)),
        // Defensive default for future variants.
        _ => Ok(false),
    }
}

/// Single source of truth for cross-scope SSH-key visibility.
/// Mirrors [`image_visible_to`] — see Slice F. Used by every
/// ssh-key read path (`get_ssh_key`, the per-scope list
/// handlers) and by the instance-create reference check; a
/// wrong answer here is a cross-tenant information leak.
///
/// Behaviour:
/// * Root operators (`is_root == true`) can see everything.
/// * `Public` is visible to every principal — authenticated *and*
///   anonymous (Cedar lets the latter through on the global
///   public-actions rule for `ssh_key_get`).
/// * `Silo { silo_id }` is visible iff the principal's cached
///   silo_id matches.
/// * `Tenant { tenant_id }` is visible iff the principal's
///   tenant_id matches.
/// * `Project { project_id }` resolves the project to its
///   tenant; visible iff `project.tenant_id == principal.tenant_id`.
///   (Phase 0 = "any tenant member sees any project key"; a
///   future slice can tighten to per-project membership.)
/// * `User { user_id }` is visible iff the principal's user_id
///   matches.
pub async fn ssh_key_visible_to(
    key: &SshKey,
    principal: &Principal,
    store: &dyn Store,
) -> Result<bool, StoreError> {
    // Root sees everything regardless of scope.
    if let Principal::Operator { is_root: true, .. } = principal {
        return Ok(true);
    }
    match &key.scope {
        SshKeyScope::Public => Ok(true),
        SshKeyScope::Silo { silo_id } => Ok(principal_silo_id(principal) == Some(*silo_id)),
        SshKeyScope::Tenant { tenant_id } => Ok(principal_tenant_id(principal) == Some(*tenant_id)),
        SshKeyScope::Project { project_id } => {
            // Phase 0: any member of the project's tenant.
            let Some(my_tenant) = principal_tenant_id(principal) else {
                return Ok(false);
            };
            match store.get_project(*project_id).await {
                Ok(project) => Ok(project.tenant_id == my_tenant),
                Err(StoreError::NotFound) => Ok(false),
                Err(e) => Err(e),
            }
        }
        SshKeyScope::User { user_id } => Ok(principal_user_id(principal) == Some(*user_id)),
        // SshKeyScope is `#[non_exhaustive]`. New variants must
        // be classified explicitly in this gate; until then they
        // deny by default to avoid silent visibility bugs.
        _ => Ok(false),
    }
}

/// Stricter than [`ssh_key_visible_to`]: returns `true` if the
/// principal is allowed to delete `key`. The ownership rules
/// match the URL-vs-scope structure (same shape as
/// [`image_deletable_by`]):
/// * `Public` — root only.
/// * `Silo` / `Tenant` / `Project` — any tenant member of the
///   resolved tenant (Phase 0); cross-tenant returns false.
/// * `User` — the owning user only.
async fn ssh_key_deletable_by(
    key: &SshKey,
    principal: &Principal,
    store: &dyn Store,
) -> Result<bool, StoreError> {
    if let Principal::Operator { is_root: true, .. } = principal {
        return Ok(true);
    }
    match &key.scope {
        // Public is operator turf.
        SshKeyScope::Public => Ok(false),
        // Silo / Tenant / Project follow the same visibility
        // gate as reads (Phase 0 = same-tenant access).
        SshKeyScope::Silo { .. } | SshKeyScope::Tenant { .. } | SshKeyScope::Project { .. } => {
            ssh_key_visible_to(key, principal, store).await
        }
        SshKeyScope::User { user_id } => Ok(principal_user_id(principal) == Some(*user_id)),
        _ => Ok(false),
    }
}

/// Shared API-edge helper used by every per-scope
/// `create_ssh_key_*` handler: parse the openssh string,
/// compute the SHA-256 fingerprint, and on a parse failure
/// record a 400 audit event for the supplied principal +
/// extras blob and return the HTTP error to surface. On
/// success returns the canonical fingerprint.
async fn parse_and_audit_ssh_key(
    ctx: &ApiContext,
    principal: &Principal,
    request_id: Option<Uuid>,
    req: &NewSshKey,
    extras: serde_json::Value,
) -> Result<String, HttpError> {
    match parse_ssh_public_key(&req.public_key) {
        Ok(fp) => Ok(fp),
        Err(msg) => {
            ctx.audit
                .record_mutation(
                    principal,
                    Action::SshKeyCreate,
                    request_id,
                    None,
                    AuditOutcome::ClientError {
                        code: 400,
                        message: msg.clone(),
                    },
                    extras,
                )
                .await;
            Err(HttpError::for_bad_request(
                Some("BadRequest".to_string()),
                msg,
            ))
        }
    }
}

async fn audit_ssh_key_create_success(
    ctx: &ApiContext,
    principal: &Principal,
    request_id: Option<Uuid>,
    key: &SshKey,
    mut extras: serde_json::Value,
) {
    if let serde_json::Value::Object(ref mut map) = extras {
        map.insert("name".to_string(), serde_json::json!(key.name));
        map.insert(
            "fingerprint".to_string(),
            serde_json::json!(key.fingerprint),
        );
    }
    ctx.audit
        .record_mutation(
            principal,
            Action::SshKeyCreate,
            request_id,
            Some(format!("SshKey::\"{}\"", key.id)),
            AuditOutcome::Success {
                resource: Some(format!("SshKey::\"{}\"", key.id)),
            },
            extras,
        )
        .await;
}

async fn audit_ssh_key_create_failure(
    ctx: &ApiContext,
    principal: &Principal,
    request_id: Option<Uuid>,
    err: &StoreError,
) {
    ctx.audit
        .record_mutation(
            principal,
            Action::SshKeyCreate,
            request_id,
            None,
            store_error_to_audit_outcome(err),
            serde_json::Value::Null,
        )
        .await;
}

/// Shared sha256 / size_bytes API-edge validation used by every
/// per-scope `create_image_*` handler. On a validation failure,
/// records a 400 audit event for the supplied principal +
/// extras blob, and returns the HTTP error to surface. On
/// success returns `None` — the handler proceeds.
async fn validate_image_request(
    req: &NewImage,
    ctx: &ApiContext,
    principal: &Principal,
    request_id: Option<Uuid>,
    extras: serde_json::Value,
) -> Option<HttpError> {
    if let Err(msg) = validate_sha256(&req.sha256) {
        ctx.audit
            .record_mutation(
                principal,
                Action::ImageCreate,
                request_id,
                None,
                AuditOutcome::ClientError {
                    code: 400,
                    message: msg.clone(),
                },
                extras,
            )
            .await;
        return Some(HttpError::for_bad_request(
            Some("BadRequest".to_string()),
            msg,
        ));
    }
    if req.size_bytes == 0 {
        let msg = "size_bytes must be greater than zero".to_string();
        ctx.audit
            .record_mutation(
                principal,
                Action::ImageCreate,
                request_id,
                None,
                AuditOutcome::ClientError {
                    code: 400,
                    message: msg.clone(),
                },
                extras,
            )
            .await;
        return Some(HttpError::for_bad_request(
            Some("BadRequest".to_string()),
            msg,
        ));
    }
    None
}

async fn audit_image_create_success(
    ctx: &ApiContext,
    principal: &Principal,
    request_id: Option<Uuid>,
    image: &Image,
    mut extras: serde_json::Value,
) {
    if let serde_json::Value::Object(ref mut map) = extras {
        map.insert("name".to_string(), serde_json::json!(image.name));
        map.insert("sha256".to_string(), serde_json::json!(image.sha256));
    }
    ctx.audit
        .record_mutation(
            principal,
            Action::ImageCreate,
            request_id,
            Some(format!("Image::\"{}\"", image.id)),
            AuditOutcome::Success {
                resource: Some(format!("Image::\"{}\"", image.id)),
            },
            extras,
        )
        .await;
}

async fn audit_image_create_failure(
    ctx: &ApiContext,
    principal: &Principal,
    request_id: Option<Uuid>,
    err: &StoreError,
) {
    ctx.audit
        .record_mutation(
            principal,
            Action::ImageCreate,
            request_id,
            None,
            store_error_to_audit_outcome(err),
            serde_json::Value::Null,
        )
        .await;
}

fn mint_token_pair(auth: &AuthService, user_id: Uuid) -> Result<TokenResponse, HttpError> {
    let (access_token, access_expires_at) = mint_access(auth.jwt_key(), user_id)
        .map_err(|e| HttpError::for_internal_error(format!("mint access: {e}")))?;
    let (refresh_token, refresh_expires_at) = mint_refresh(auth.jwt_key(), user_id)
        .map_err(|e| HttpError::for_internal_error(format!("mint refresh: {e}")))?;
    Ok(TokenResponse {
        access_token,
        refresh_token,
        access_expires_at,
        refresh_expires_at,
    })
}

/// Parse a `/v2/config/{key}` path segment into a [`ConfigKey`], or
/// `404` for an unrecognised name.
fn config_key_or_404(raw: &str) -> Result<ConfigKey, HttpError> {
    ConfigKey::from_wire(raw).ok_or_else(|| {
        HttpError::for_client_error(
            Some("NotFound".to_string()),
            ClientErrorStatusCode::NOT_FOUND,
            format!("unknown config key: {raw}"),
        )
    })
}

/// Build the wire view of one config key against a `Settings` snapshot,
/// flagging any legacy env var currently shadowing it at boot.
fn build_config_entry(key: ConfigKey, settings: &tritond_store::Settings) -> ConfigEntry {
    ConfigEntry {
        key: key.as_str().to_string(),
        value: settings.get(key),
        default: tritond_store::Settings::default().get(key),
        env_override: crate::settings::env_override_for(key).map(str::to_string),
        restart_required: key.restart_required(),
        description: key.description().to_string(),
    }
}

/// Build the [`ApiDescription`] for `tritond`.
pub fn api_description() -> Result<ApiDescription<ApiContext>> {
    tritond_api::tritond_api_mod::api_description::<TritondServiceImpl>()
        .map_err(|e| anyhow::anyhow!("failed to build API description: {e}"))
}

/// Start a Dropshot server with a freshly-constructed in-memory store
/// and a fresh random JWT key. Convenience wrapper for tests and
/// `main` paths that don't need bootstrap-from-store semantics.
pub async fn start_server(bind_address: &str) -> Result<HttpServer<ApiContext>> {
    let context = ApiContext::in_memory().context("build in-memory api context")?;
    start_server_with_context(bind_address, context).await
}

/// Start a Dropshot server backed by an externally-built [`ApiContext`].
///
/// Also spawns the in-process stub provisioner (see
/// [`crate::provisioner`]) so any provisioning jobs the API
/// handlers enqueue get processed. The provisioner runs as a
/// detached tokio task and exits when the runtime shuts down. A
/// future deploy with a real per-CN `tritonagent` will skip the
/// stub spawn (gated by config).
pub async fn start_server_with_context(
    bind_address: &str,
    context: ApiContext,
) -> Result<HttpServer<ApiContext>> {
    let parsed: SocketAddr = bind_address
        .parse()
        .with_context(|| format!("invalid bind address: {bind_address}"))?;

    let config_dropshot = ConfigDropshot {
        bind_address: parsed,
        // The default 1 KB body cap is too small for `/v2/agent/register`,
        // which carries the full SmartOS `sysinfo` JSON (tens of KB on a
        // production CN). 1 MB is plenty for any expected payload and
        // still bounds an abusive client.
        default_request_body_max_bytes: 1_048_576,
        ..Default::default()
    };

    let log = ConfigLogging::StderrTerminal {
        level: ConfigLoggingLevel::Info,
    }
    .to_logger("tritond")
    .map_err(|e| anyhow::anyhow!("failed to construct logger: {e}"))?;

    let api = api_description()?;
    // Spawn the stub provisioner before starting the HTTP server
    // so the queue is being drained from the moment handlers can
    // accept requests. Tests / real-agent deploys can opt out via
    // `ApiContext::without_in_process_provisioner`.
    if context.spawn_in_process_provisioner {
        let _provisioner = provisioner::spawn(Arc::clone(&context.store));
    }

    // The sweeper runs alongside the in-process stub or a real
    // agent — its job is to reap claims that *no* worker
    // completed (agent crash, partition). Configurable per
    // [`ApiContext::with_sweeper`]; tests typically leave it
    // off for deterministic state.
    if let Some(sw) = context.sweeper {
        let _sweeper = sweeper::spawn(
            Arc::clone(&context.store),
            Arc::clone(&context.audit),
            sw.interval,
            sw.stale_after,
        );
    }

    // The DHCP-lease reconciler (γ.3) walks list_all_dhcp_leases
    // periodically and reaps orphaned, unpinned, stale leases. See
    // dhcp_reconciler module docs for the exact GC criteria.
    // Configurable per [`ApiContext::with_dhcp_reconciler`]; tests
    // typically leave it off so explicit IPAM-state assertions
    // aren't raced.
    if let Some(rc) = context.dhcp_reconciler {
        let _reconciler =
            dhcp_reconciler::spawn(Arc::clone(&context.store), Arc::clone(&context.audit), rc);
    }

    let server = HttpServerStarter::new(&config_dropshot, api, context, &log)
        .map_err(|e| anyhow::anyhow!("failed to start HTTP server: {e}"))?
        .start();

    Ok(server)
}
