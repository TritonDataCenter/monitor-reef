// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Triton Cloud control plane daemon.
//!
//! Phase 0e ships `/v2/health`, the silo CRUD primitives, and the
//! operator-auth surface (`/v2/auth/login`, `/v2/auth/refresh`,
//! `/v2/auth/api-keys`). The store is pluggable ([`MemStore`] for
//! tests, `FdbStore` in production); the auth service holds the
//! cluster-wide HS256 signing key and the embedded Cedar policy
//! bundle.
//!
//! The library exposes the building blocks (`TritondServiceImpl`,
//! `ApiContext`, `api_description`, `start_server*`,
//! `bootstrap::ensure`) so integration tests can spin up the service
//! in-process; the binary is a thin wrapper around them.

pub mod audit;
pub mod auth;
pub mod bootstrap;
pub mod provisioner;
pub mod rate_limit;
pub mod sweeper;

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use base64::Engine;
use dropshot::{
    ApiDescription, ClientErrorStatusCode, ConfigDropshot, ConfigLogging, ConfigLoggingLevel,
    HttpError, HttpResponseCreated, HttpResponseDeleted, HttpResponseOk, HttpServer,
    HttpServerStarter, Path, Query, RequestContext, TypedBody,
};
use proteus_api::blueprint::{
    ClientLinkConfig, PORT_BLUEPRINT_SCHEMA_V0, PluginConfigBytes, PortBlueprint, PortLimits,
};
use proteus_api::ids::{
    Generation as ProteusGeneration, NetworkId as ProteusNetworkId, PortId as ProteusPortId,
};
use triton_vpc::TRITON_VPC_BLUEPRINT_SCHEMA_V1;
use triton_vpc::tritond_intent_v1::{
    FloatingIpAttachmentIntentV1, FloatingIpIntentV1, NatGatewayIntentV1, NicIntentV1,
    RouteIntentV1, RouteTargetIntentV1, SubnetIntentV1, TritondPortIntentV1, VpcIntentV1,
};
use tritond_api::{
    AgentJobPath, AgentPortBlueprint, AgentPortBlueprintPath, AgentStatusRequest, ApiKeyCreated,
    ApiKeyPath, ApproveCnRequest, AttachFloatingIpRequest, AuditEventList, AuditEventPath,
    AuditListQuery, AuditVerifyQuery, AuditVerifyResponse, ClaimJobRequest, ClaimJobResponse,
    CnListQuery, CnPath, CompleteJobRequest, HealthResponse, ImagePath, InstanceDeleteQuery,
    LoginRequest, NetworkRealizationRequest, NewApiKey, NewIdpConfig, NewImageFromBundle,
    OpenAutoApproveRequest, ProvisioningBlueprint, RefreshRequest, RegisterCnRequest,
    RegisterCnResponse, RegisterStatusQuery, RegisterStatusResponse, SiloPath, SiloTenantPath,
    SshKeyPath, TenantIdpPath, TenantPath, TenantProjectFloatingIpPath,
    TenantProjectInstanceDiskPath, TenantProjectInstanceNicPath, TenantProjectInstancePath,
    TenantProjectPath, TenantProjectVpcNatGatewayPath, TenantProjectVpcPath,
    TenantProjectVpcRouteTablePath, TenantProjectVpcRouteTableRoutePath,
    TenantProjectVpcSubnetPath, TokenResponse, TritondApi,
    types::{
        ApiKeyView, AuditEvent, AutoApproveWindow, CnView, Disk, FloatingIp, IdpConfigView, Image,
        ImageCompatibility, ImageScope, Instance, JobKind, JobOutcome, JobStatus, LifecycleState,
        LifecycleStateKind, NatGateway, NetworkResourceId, NewFloatingIp, NewImage, NewInstance,
        NewJob, NewNatGateway, NewProject, NewQuota, NewRoute, NewRouteTable, NewSilo, NewSshKey,
        NewSubnet, NewTenant, NewVpc, Nic, Project, ProvisioningJob, Quota, RealizerId, Route,
        RouteTable, RouteTarget, Silo, SshKey, SshKeyScope, Subnet, Tenant, Vpc,
    },
};
use tritond_audit::{Actor as AuditActor, MemChain, Outcome as AuditOutcome};
use tritond_auth::OidcConfig;
use tritond_auth::{
    JwtKey, TokenKind, generate_api_key, mint_access, mint_refresh, verify, verify_password,
};
use tritond_store::{
    AUTO_APPROVE_WINDOW_MAX, ApiKey, ApiKeyScope, Cn, CnState, IdpConfig, MemStore, Store,
    StoreError, normalize_claim_code,
};
use uuid::Uuid;

use crate::audit::AuditService;
use crate::auth::{
    Action, AuthService, Principal, authenticate_and_authorize, authenticate_and_authorize_in_silo,
    authenticate_and_authorize_in_tenant, require_authenticated,
};
use crate::rate_limit::{IpRateLimiter, LoginRateLimiter};

/// Service version, populated from Cargo at build time.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Default bind address for the Dropshot HTTP server.
pub const DEFAULT_BIND_ADDRESS: &str = "127.0.0.1:8080";

/// Shared state for API handlers.
pub struct ApiContext {
    pub store: Arc<dyn Store>,
    pub auth: Arc<AuthService>,
    pub audit: Arc<AuditService>,
    /// Per-source-IP throttle on `POST /v2/auth/login`. See
    /// [`crate::rate_limit`] for the shape of the limiter and why it
    /// only fronts login.
    pub login_rate_limiter: Arc<LoginRateLimiter>,
    /// Per-source-IP throttle on `POST /v2/cns/approve`. Independent
    /// bucket-set from the login limiter so a brute-force on one
    /// surface doesn't drain the other's budget.
    pub cn_approve_rate_limiter: Arc<IpRateLimiter>,
    /// When `false`, [`start_server_with_context`] does *not*
    /// spawn the in-process stub provisioner. The agent integration
    /// test sets this so a real `tritonagent` (or its test stand-in)
    /// can claim jobs without racing the stub. Defaults to `true`.
    pub spawn_in_process_provisioner: bool,
    /// Stale-claim sweeper config. When `Some(...)`,
    /// [`start_server_with_context`] spawns the sweeper task
    /// from [`crate::sweeper::spawn`] with the given interval +
    /// staleness threshold. Defaults to `None` so test contexts
    /// don't get an unexpected background task that would
    /// interfere with explicit job-state assertions.
    pub sweeper: Option<SweeperConfig>,
}

/// Cadence and staleness threshold for the
/// [`crate::sweeper`] background task. See module docs.
#[derive(Debug, Clone, Copy)]
pub struct SweeperConfig {
    pub interval: std::time::Duration,
    pub stale_after: std::time::Duration,
}

impl ApiContext {
    pub fn new(store: Arc<dyn Store>, auth: Arc<AuthService>, audit: Arc<AuditService>) -> Self {
        Self {
            store,
            auth,
            audit,
            login_rate_limiter: Arc::new(LoginRateLimiter::new()),
            cn_approve_rate_limiter: Arc::new(IpRateLimiter::for_cn_approve()),
            spawn_in_process_provisioner: true,
            sweeper: None,
        }
    }

    /// Replace the default CN-approve rate limiter — integration
    /// tests use this to install a tighter quota than production
    /// without slowing the login bucket.
    #[must_use]
    pub fn with_cn_approve_rate_limiter(mut self, limiter: Arc<IpRateLimiter>) -> Self {
        self.cn_approve_rate_limiter = limiter;
        self
    }

    /// Enable the stale-claim sweeper at the given cadence.
    /// Used by `main` (env-driven) and by integration tests
    /// that want to exercise sweeper behavior with tight
    /// thresholds. Defaults to `None`.
    #[must_use]
    pub fn with_sweeper(mut self, cfg: SweeperConfig) -> Self {
        self.sweeper = Some(cfg);
        self
    }

    /// Disable the in-process stub provisioner — the agent
    /// integration test uses this so a test-side claim doesn't
    /// race the stub. Production deploys with a real `tritonagent`
    /// will eventually call this too.
    #[must_use]
    pub fn without_in_process_provisioner(mut self) -> Self {
        self.spawn_in_process_provisioner = false;
        self
    }

    /// Build a context backed by a fresh in-memory store, a fresh
    /// random JWT key, and an in-memory audit chain. Convenient for
    /// integration tests.
    pub fn in_memory() -> Result<Self> {
        let store: Arc<dyn Store> = Arc::new(MemStore::new());
        let auth = Arc::new(AuthService::new(JwtKey::generate())?);
        let audit = Arc::new(AuditService::new(Arc::new(MemChain::new())));
        Ok(Self::new(store, auth, audit))
    }

    /// Replace the default login rate limiter — used by integration
    /// tests that need a tighter quota than production. Returns
    /// `self` so test setup can chain off `ApiContext::in_memory()`.
    #[must_use]
    pub fn with_login_rate_limiter(mut self, limiter: Arc<LoginRateLimiter>) -> Self {
        self.login_rate_limiter = limiter;
        self
    }
}

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
        let blueprint = build_blueprint(ctx.store.as_ref(), &job).await?;
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

        let instance = match ctx.store.create_instance(tenant_id, project_id, req).await {
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

        // Enqueue the provisioning job. The stub provisioner (or
        // a real per-CN agent in the future) will pick it up and
        // drive Pending → Provisioning → Running. The response
        // returns the instance in `Pending` — clients poll the
        // get endpoint to observe the transition.
        if let Err(e) = ctx
            .store
            .enqueue_job(NewJob {
                kind: JobKind::Provision {
                    instance_id: instance.id,
                },
                // Phase 0: leave placement to whichever agent
                // claims first. A future scheduler will populate
                // this from a real placement decision.
                target_cn_uuid: None,
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
                        target_cn_uuid: None,
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
        ctx.store
            .update_cn_status(server_uuid, req.payload, chrono::Utc::now())
            .await
            .map_err(store_error_to_http)?;
        // Status updates are also hot (~once per minute or
        // when zoneevent fires); no per-update audit. A future
        // slice may sample at low frequency for forensics.
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
}

/// Mint a fresh per-CN API key, persist it (with bound_to_cn set
/// to the CN's server_uuid + scope = Agent), and atomically wire
/// it onto the Cn record via `approve_cn`. On error, audits the
/// failure with the supplied principal + request_id and returns
/// a 500.
///
/// The CN's "owning user" is the principal who triggered the
/// approval. For the operator approval path that's the operator's
/// user_id; for the auto-approve path (anonymous) we fall back to
/// the bootstrap root operator's id so the key has a real owner
/// in the existing per-user list. (A future slice may give CNs
/// their own User-equivalent.)
async fn mint_and_attach_cn_credential(
    ctx: &ApiContext,
    principal: &crate::auth::Principal,
    request_id: Option<Uuid>,
    cn: &Cn,
) -> Result<Cn, HttpError> {
    let owner_user_id = require_authenticated(principal.clone())
        .map(|(uid, _)| uid)
        .unwrap_or_else(|_| {
            // Anonymous principal (auto-approve path). Use a
            // best-effort sentinel so the key has a non-null
            // owner field; a future slice will introduce a
            // dedicated "system" user for keys created without
            // an operator.
            Uuid::nil()
        });

    let material = generate_api_key()
        .await
        .map_err(|e| HttpError::for_internal_error(format!("generate api key: {e}")))?;
    let key_id = Uuid::new_v4();
    let record = ApiKey {
        id: key_id,
        user_id: owner_user_id,
        description: format!("agent: cn {}", cn.server_uuid),
        lookup_id: material.lookup_id.clone(),
        hash: material.hash,
        scope: ApiKeyScope::Agent,
        bound_to_cn: Some(cn.server_uuid),
        created_at: chrono::Utc::now(),
    };
    ctx.store
        .create_api_key(record)
        .await
        .map_err(store_error_to_http)?;

    let now = chrono::Utc::now();
    let updated = match ctx
        .store
        .approve_cn(cn.server_uuid, key_id, material.plaintext, now)
        .await
    {
        Ok(updated) => updated,
        Err(e) => {
            // Key created but approve failed. Audit so an operator
            // can clean up the orphan key.
            ctx.audit
                .record_mutation(
                    principal,
                    Action::CnApprove,
                    request_id,
                    Some(format!("Cn::\"{}\"", cn.server_uuid)),
                    AuditOutcome::ServerError {
                        message: format!("orphaned api key {key_id}: {e}"),
                    },
                    serde_json::json!({
                        "server_uuid": cn.server_uuid,
                        "orphaned_api_key_id": key_id,
                    }),
                )
                .await;
            return Err(store_error_to_http(e));
        }
    };
    Ok(updated)
}

/// 403 if the request didn't come from a bound API key. Used
/// by handlers that *only* make sense for a per-CN agent (the
/// heartbeat / status endpoints), since there's no other way
/// to know which CN to attribute the call to.
fn require_bound_cn(principal: &crate::auth::Principal) -> Result<Uuid, HttpError> {
    crate::auth::principal_bound_cn(principal).ok_or_else(|| {
        HttpError::for_client_error(
            Some("Forbidden".to_string()),
            ClientErrorStatusCode::FORBIDDEN,
            "this endpoint requires a CN-bound api key (the per-CN keys minted by /v2/cn-approvals)".to_string(),
        )
    })
}

/// 403 if the job's `claimed_by` (which the agent set when
/// it claimed) doesn't match the bound key's CN. Used by
/// `agent_complete_job` and `agent_job_blueprint` so a bound
/// key for CN-A can't operate on a job claimed by CN-B.
fn enforce_job_belongs_to_bound_cn(job: &ProvisioningJob, bound_cn: Uuid) -> Result<(), HttpError> {
    // `claimed_by` is free-text on the wire today; bound agents
    // are required to set it to their server_uuid string.
    let Some(ref claimed_by) = job.claimed_by else {
        return Err(HttpError::for_client_error(
            Some("Forbidden".to_string()),
            ClientErrorStatusCode::FORBIDDEN,
            "job has no claimer; bound key cannot operate on it".to_string(),
        ));
    };
    let claimed_uuid = Uuid::parse_str(claimed_by).map_err(|_| {
        HttpError::for_client_error(
            Some("Forbidden".to_string()),
            ClientErrorStatusCode::FORBIDDEN,
            "job claimed_by is not a uuid; bound key cannot match it".to_string(),
        )
    })?;
    crate::auth::enforce_cn_binding(Some(bound_cn), claimed_uuid)
}

/// 403 if a CN-bound key tries to write a realization row for a
/// different CN realizer. Edge-cluster realization rows are reported
/// by a tritonagent running on an edge CN, so the caller must still
/// be CN-bound but the row key is the edge-cluster id.
fn enforce_realizer_belongs_to_bound_cn(
    realizer: RealizerId,
    bound_cn: Uuid,
) -> Result<(), HttpError> {
    match realizer {
        RealizerId::Cn { id } => crate::auth::enforce_cn_binding(Some(bound_cn), id),
        RealizerId::EdgeCluster { .. } => Ok(()),
        _ => Err(bad_request("unsupported realizer kind")),
    }
}

async fn ensure_realization_resource_exists(
    store: &dyn Store,
    resource: NetworkResourceId,
) -> Result<(), HttpError> {
    match resource {
        NetworkResourceId::Vpc { id } => store.get_vpc(id).await.map(|_| ()),
        NetworkResourceId::Subnet { id } => store.get_subnet(id).await.map(|_| ()),
        NetworkResourceId::RouteTable { id } => store.get_route_table(id).await.map(|_| ()),
        NetworkResourceId::Route { id } => store.get_route(id).await.map(|_| ()),
        NetworkResourceId::NatGateway { id } => store.get_nat_gateway(id).await.map(|_| ()),
        NetworkResourceId::FloatingIp { id } => store.get_floating_ip(id).await.map(|_| ()),
        NetworkResourceId::SecurityGroup { .. }
        | NetworkResourceId::SecurityGroupRule { .. }
        | NetworkResourceId::NicSecurityGroupAttachment { .. } => return Err(not_found()),
        _ => return Err(not_found()),
    }
    .map_err(store_error_to_http)
}

/// Stable label for a principal in audit/window-tracking JSON.
/// Compact form so the audit blob stays single-line.
fn principal_label(principal: &crate::auth::Principal) -> String {
    use crate::auth::Principal;
    match principal {
        Principal::Operator { user_id, .. } => user_id.to_string(),
        Principal::Anonymous => "anonymous".to_string(),
    }
}

/// Token-only enum used by `instance_lifecycle_transition` to pick
/// the matching `JobKind` after the CAS lands. We don't pass a
/// `JobKind` directly because that would require the caller to
/// already know the `instance_id`, which only becomes available
/// inside the helper.
#[derive(Debug, Clone, Copy)]
enum JobKindTemplate {
    Provision,
    Stop,
    Restart,
}

impl JobKindTemplate {
    fn for_instance(self, instance_id: Uuid) -> JobKind {
        match self {
            JobKindTemplate::Provision => JobKind::Provision { instance_id },
            JobKindTemplate::Stop => JobKind::Stop { instance_id },
            JobKindTemplate::Restart => JobKind::Restart { instance_id },
        }
    }
}

/// Shared helper for the three lifecycle-transition handlers. Does
/// auth, the path-recheck, the store CAS, the optional job
/// enqueue, and the audit emission.
///
/// `enqueue` is `Some(JobKindTemplate)` for endpoints whose
/// follow-on transitions are agent-driven (start/stop/restart);
/// the CAS to the *transitional* state runs first (so we get the
/// right 409 on a stale state), then the job is enqueued. If the
/// enqueue fails after a successful CAS, the instance is left in
/// the transitional state and the caller gets a 5xx; a future
/// slice can move CAS+enqueue into a single FDB transaction.
async fn instance_lifecycle_transition(
    rqctx: RequestContext<ApiContext>,
    path: Path<TenantProjectInstancePath>,
    action: Action,
    expected_from: &[LifecycleStateKind],
    to: LifecycleState,
    enqueue: Option<JobKindTemplate>,
) -> Result<HttpResponseOk<Instance>, HttpError> {
    let ctx = rqctx.context();
    let TenantProjectInstancePath {
        tenant_id,
        project_id,
        instance_id,
    } = path.into_inner();
    let principal = authenticate_and_authorize_in_tenant(
        &rqctx, &ctx.auth, &ctx.audit, &ctx.store, action, tenant_id,
    )
    .await?;
    let request_id = parse_request_id(&rqctx);

    // Defence-in-depth on tenant+project before we try to transition.
    let instance = ctx
        .store
        .get_instance(instance_id)
        .await
        .map_err(store_error_to_http)?;
    if instance.tenant_id != tenant_id || instance.project_id != project_id {
        return Err(not_found());
    }

    let updated = match ctx
        .store
        .transition_instance_lifecycle(instance_id, expected_from, to.clone())
        .await
    {
        Ok(i) => i,
        Err(e) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    action,
                    request_id,
                    Some(format!("Instance::\"{instance_id}\"")),
                    store_error_to_audit_outcome(&e),
                    serde_json::Value::Null,
                )
                .await;
            return Err(store_error_to_http(e));
        }
    };

    if let Some(template) = enqueue
        && let Err(e) = ctx
            .store
            .enqueue_job(NewJob {
                kind: template.for_instance(instance_id),
                target_cn_uuid: None,
            })
            .await
    {
        ctx.audit
            .record_mutation(
                &principal,
                action,
                request_id,
                Some(format!("Instance::\"{instance_id}\"")),
                store_error_to_audit_outcome(&e),
                serde_json::Value::Null,
            )
            .await;
        return Err(store_error_to_http(e));
    }

    ctx.audit
        .record_mutation(
            &principal,
            action,
            request_id,
            Some(format!("Instance::\"{instance_id}\"")),
            AuditOutcome::Success {
                resource: Some(format!("Instance::\"{instance_id}\"")),
            },
            serde_json::json!({
                "tenant_id": tenant_id,
                "project_id": project_id,
                "to_state": format!("{:?}", to.kind()),
            }),
        )
        .await;
    Ok(HttpResponseOk(updated))
}

/// Audit + return a 400 in one shot. Used by `create_project_instance`
/// for cpu/memory size validation; can't easily live as a free
/// function because it borrows `ctx` and `principal`.
async fn reject_audit(
    ctx: &ApiContext,
    principal: &auth::Principal,
    action: Action,
    request_id: Option<Uuid>,
    message: &str,
    context: serde_json::Value,
) -> HttpError {
    ctx.audit
        .record_mutation(
            principal,
            action,
            request_id,
            None,
            AuditOutcome::ClientError {
                code: 400,
                message: message.to_string(),
            },
            context,
        )
        .await;
    HttpError::for_bad_request(Some("BadRequest".to_string()), message.to_string())
}

/// Parse an inbound openssh public-key string and return its
/// canonical SHA-256 fingerprint. Returns `Err` with a user-facing
/// message on parse failure (mapped to 400 by callers).
fn parse_ssh_public_key(public_key: &str) -> Result<String, String> {
    let parsed = ssh_key::PublicKey::from_openssh(public_key.trim())
        .map_err(|e| format!("invalid openssh public key: {e}"))?;
    Ok(parsed.fingerprint(ssh_key::HashAlg::Sha256).to_string())
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

fn principal_silo_id(p: &Principal) -> Option<Uuid> {
    match p {
        Principal::Operator { silo_id, .. } => *silo_id,
        Principal::Anonymous => None,
    }
}

fn principal_tenant_id(p: &Principal) -> Option<Uuid> {
    match p {
        Principal::Operator { tenant_id, .. } => *tenant_id,
        Principal::Anonymous => None,
    }
}

fn principal_user_id(p: &Principal) -> Option<Uuid> {
    match p {
        Principal::Operator { user_id, .. } => Some(*user_id),
        Principal::Anonymous => None,
    }
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

/// Validate an image's `sha256` field — must be exactly 64 lowercase
/// hex characters.
fn validate_sha256(s: &str) -> Result<(), String> {
    if s.len() != 64 {
        return Err(format!("sha256 must be 64 hex chars (got {})", s.len()));
    }
    if !s
        .chars()
        .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
    {
        return Err("sha256 must be lowercase hex (0-9, a-f)".to_string());
    }
    Ok(())
}

/// Generic 404 "not found" used by the defence-in-depth path checks.
/// Same shape as `store_error_to_http` for `StoreError::NotFound`,
/// just inlined so handlers don't have to roll a synthetic StoreError.
fn not_found() -> HttpError {
    HttpError::for_client_error(
        Some("NotFound".to_string()),
        ClientErrorStatusCode::NOT_FOUND,
        "not found".to_string(),
    )
}

fn bad_request(message: impl Into<String>) -> HttpError {
    HttpError::for_bad_request(Some("BadRequest".to_string()), message.into())
}

fn parse_request_id<T>(rqctx: &RequestContext<T>) -> Option<Uuid>
where
    T: dropshot::ServerContext,
{
    Uuid::parse_str(&rqctx.request_id).ok()
}

fn store_error_to_audit_outcome(err: &StoreError) -> AuditOutcome {
    match err {
        StoreError::NotFound => AuditOutcome::ClientError {
            code: 404,
            message: "not found".to_string(),
        },
        StoreError::Conflict(msg) => AuditOutcome::ClientError {
            code: 409,
            message: msg.clone(),
        },
        StoreError::Backend(msg) => AuditOutcome::ServerError {
            message: msg.clone(),
        },
    }
}

fn audit_error_to_http(err: tritond_audit::AuditError) -> HttpError {
    use tritond_audit::AuditError;
    let display = err.to_string();
    match err {
        AuditError::PastHead { .. } => HttpError::for_client_error(
            Some("NotFound".to_string()),
            ClientErrorStatusCode::NOT_FOUND,
            display,
        ),
        AuditError::Backend(msg) | AuditError::Serialise(msg) => HttpError::for_internal_error(msg),
        // ChainBroken or any future variant: surface as 500 with the
        // generic display impl so audit-runtime errors don't leak
        // structure-of-the-chain detail to the caller.
        _ => HttpError::for_internal_error(display),
    }
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

fn invalid_credentials() -> HttpError {
    HttpError::for_client_error(
        Some("Unauthenticated".to_string()),
        ClientErrorStatusCode::UNAUTHORIZED,
        "invalid credentials".to_string(),
    )
}

/// 429 Too Many Requests with a `Retry-After` header carrying the
/// number of seconds the client should wait before its next attempt.
/// Used by the login rate limiter — see [`crate::rate_limit`].
fn too_many_requests(retry_after: std::time::Duration) -> HttpError {
    // Always at least one second so a client that obeys the header
    // doesn't spin in a tight retry loop.
    let secs = retry_after.as_secs().max(1);
    let mut err = HttpError::for_client_error(
        Some("TooManyRequests".to_string()),
        ClientErrorStatusCode::TOO_MANY_REQUESTS,
        "rate limited; slow down and retry shortly".to_string(),
    );
    let mut headers = http::HeaderMap::new();
    if let Ok(value) = http::HeaderValue::from_str(&secs.to_string()) {
        headers.insert(http::header::RETRY_AFTER, value);
    }
    err.headers = Some(Box::new(headers));
    err
}

/// Drive the instance lifecycle forward in response to an agent
/// claiming a job. For Provision: Pending → Provisioning. Stop /
/// Restart already entered Stopping in the operator-facing
/// `instance_*` handler before the job was enqueued, so claim
/// has nothing to advance. CAS failures are logged but do not
/// propagate — the job is already in InProgress regardless.
async fn drive_lifecycle_for_claim(store: &dyn Store, job: &ProvisioningJob) {
    if matches!(job.kind, JobKind::Provision { .. }) {
        let instance_id = job.kind.instance_id();
        if let Err(e) = store
            .transition_instance_lifecycle(
                instance_id,
                &[LifecycleStateKind::Pending],
                LifecycleState::Provisioning,
            )
            .await
        {
            tracing::warn!(
                %instance_id,
                error = %e,
                "Pending → Provisioning lifecycle CAS failed at claim",
            );
        }
    }
}

/// Drive the instance lifecycle to its terminal state in
/// response to an agent reporting a job's outcome. Mapping:
///
/// | JobKind / Outcome      | Lifecycle target                 |
/// |------------------------|----------------------------------|
/// | Provision / Completed  | Provisioning → Running           |
/// | Stop / Completed       | Stopping → Stopped               |
/// | Restart / Completed    | Stopping → Running               |
/// | (any) / Failed{reason} | (current) → Failed{reason}       |
///
/// For Failed outcomes the CAS accepts any of the in-flight
/// states (Pending, Provisioning, Stopping) so a job that
/// failed before its claim-time advance still lands in Failed
/// rather than getting stuck. CAS failures (instance deleted
/// out from under the job, lifecycle drift) are logged.
pub(crate) async fn drive_lifecycle_for_complete(
    store: &dyn Store,
    job: &ProvisioningJob,
    outcome: &JobOutcome,
) {
    // Delete jobs run *after* the tritond record is gone, so
    // there is no lifecycle to transition. Skip cleanly to
    // avoid a noisy "instance not found" warning that would
    // fire on every successful zone teardown.
    if matches!(job.kind, JobKind::Delete { .. }) {
        return;
    }
    let instance_id = job.kind.instance_id();
    let (expected_from, target): (&[LifecycleStateKind], LifecycleState) =
        match (&job.kind, outcome) {
            (JobKind::Provision { .. }, JobOutcome::Completed) => {
                (&[LifecycleStateKind::Provisioning], LifecycleState::Running)
            }
            (JobKind::Stop { .. }, JobOutcome::Completed) => {
                (&[LifecycleStateKind::Stopping], LifecycleState::Stopped)
            }
            (JobKind::Restart { .. }, JobOutcome::Completed) => {
                (&[LifecycleStateKind::Stopping], LifecycleState::Running)
            }
            (_, JobOutcome::Failed { reason }) => (
                &[
                    LifecycleStateKind::Pending,
                    LifecycleStateKind::Provisioning,
                    LifecycleStateKind::Stopping,
                    LifecycleStateKind::Running,
                ],
                LifecycleState::Failed {
                    reason: reason.clone(),
                },
            ),
            _ => return,
        };
    if let Err(e) = store
        .transition_instance_lifecycle(instance_id, expected_from, target.clone())
        .await
    {
        tracing::warn!(
            %instance_id,
            kind = ?job.kind,
            ?target,
            error = %e,
            "lifecycle CAS failed at job complete",
        );
    }
}

/// Fetch a tritond image bundle from `bundle_url`, parse the
/// manifest, re-hash the content against the manifest's claimed
/// sha256, and return a [`NewImage`] populated from the
/// manifest. The bundle URL is recorded as the resulting Image's
/// `source_url` so the per-CN agent can fetch the same bundle
/// at provision time.
///
/// All manifest fields ride into the Image record verbatim
/// (name, version, sha256, size, compatibility, os_family).
/// `description` falls back to empty when the manifest doesn't
/// carry one.
///
/// The downloaded bundle is extracted to a `tempfile::TempDir`
/// that drops at function exit — tritond doesn't cache the
/// content, the agent re-downloads on first provision per CN.
async fn ingest_bundle(bundle_url: &str) -> anyhow::Result<NewImage> {
    use sha2::{Digest, Sha256};

    // Pre-configured TLS using webpki-roots. Same reason as the
    // agent: cold SmartOS GZ has no platform CA store.
    let mut root_store = rustls::RootCertStore::empty();
    root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let tls = rustls::ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_no_client_auth();
    let client = reqwest::Client::builder()
        .use_preconfigured_tls(tls)
        .build()
        .context("build bundle-fetch reqwest client")?;

    let work = tempfile::tempdir().context("create temp dir for bundle ingest")?;
    let bundle_path = work.path().join("bundle.tar");

    // Stream the bundle to disk so very large bundles don't
    // need to fit in memory.
    let bytes = client
        .get(bundle_url)
        .send()
        .await
        .with_context(|| format!("GET {bundle_url}"))?
        .error_for_status()
        .with_context(|| format!("HTTP error from {bundle_url}"))?
        .bytes()
        .await
        .with_context(|| format!("read bundle body from {bundle_url}"))?;
    // Phase 0 reads the entire bundle into memory before
    // writing — bundles for OS images are typically tens of MB
    // gzipped, well within tritond's RAM budget. A future slice
    // adds streaming when bundles routinely exceed ~1 GB.
    tokio::fs::write(&bundle_path, &bytes)
        .await
        .context("persist bundle to temp file")?;

    let extracted = tritond_image_manifest::extract_bundle(&bundle_path, work.path())
        .context("extract bundle tar")?;

    // Re-hash the content. The manifest's sha256 is operator-
    // provided (via the build CLI); we don't trust it without
    // verification, otherwise an attacker who controls the
    // bundle URL could substitute arbitrary content under any
    // claimed hash.
    let mut hasher = Sha256::new();
    let mut content_file = tokio::fs::File::open(&extracted.content_path)
        .await
        .context("open extracted content for hashing")?;
    use tokio::io::AsyncReadExt as _;
    let mut buf = vec![0u8; 1024 * 1024];
    let mut total: u64 = 0;
    loop {
        let n = content_file
            .read(&mut buf)
            .await
            .context("read extracted content")?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        total += n as u64;
    }
    let actual_sha256 = format!("{:x}", hasher.finalize());
    if actual_sha256 != extracted.manifest.content.sha256.to_ascii_lowercase() {
        anyhow::bail!(
            "bundle content sha256 mismatch: manifest claims {}, actual {actual_sha256}",
            extracted.manifest.content.sha256,
        );
    }
    if total != extracted.manifest.content.size {
        // Defensive — a length mismatch on a hash-matching
        // payload is impossible barring a sha256 collision,
        // but we surface it for diagnosability.
        anyhow::bail!(
            "bundle content size mismatch: manifest claims {}, actual {total}",
            extracted.manifest.content.size,
        );
    }

    Ok(NewImage {
        name: extracted.manifest.name,
        description: extracted.manifest.description,
        os: extracted.manifest.guest.os_family,
        version: extracted.manifest.version,
        size_bytes: extracted.manifest.content.size,
        sha256: extracted.manifest.content.sha256,
        source_url: Some(bundle_url.to_string()),
        id: None,
        compatibility: Some(ImageCompatibility {
            brand: extracted.manifest.compatibility.brand,
            arch: extracted.manifest.compatibility.arch,
            min_smartos_platform: extracted.manifest.compatibility.min_smartos_platform,
        }),
    })
}

/// Materialise the agent-side blueprint for a job. Resolves
/// instance + image + nics + disks + ssh public keys for a
/// `Provision`; returns just the instance (when still extant)
/// for `Stop` / `Restart`.
///
/// Errors from the store path bubble up as HTTP errors via
/// [`store_error_to_http`]. A concurrent operator delete that
/// removes the instance after the job was claimed surfaces as
/// `instance: None` rather than a 404; the agent then reports
/// `JobOutcome::Failed { reason: "instance gone" }`.
async fn build_blueprint(
    store: &dyn Store,
    job: &ProvisioningJob,
) -> Result<ProvisioningBlueprint, HttpError> {
    let instance_id = job.kind.instance_id();
    let instance = match store.get_instance(instance_id).await {
        Ok(i) => Some(i),
        Err(StoreError::NotFound) => None,
        Err(e) => return Err(store_error_to_http(e)),
    };

    // Stop / Restart only need the instance id; skip the full
    // resolve so a vanished image or NIC doesn't block the
    // agent from acting on a still-existing zone.
    // Provision needs the full resolve (image, NICs, disks,
    // ssh keys) so the agent can build a vmadm payload.
    // Stop / Restart / Delete only need the instance id, which
    // is on `job.kind`, so we short-circuit and let the agent
    // act on the kind alone. Delete in particular runs *after*
    // the tritond record is gone, so the instance lookup
    // intentionally returns `instance: None`.
    let needs_full_resolve = matches!(job.kind, JobKind::Provision { .. });
    if !needs_full_resolve {
        return Ok(ProvisioningBlueprint {
            job_id: job.id,
            kind: job.kind.clone(),
            instance,
            image: None,
            nics: Vec::new(),
            disks: Vec::new(),
            ssh_public_keys: Vec::new(),
        });
    }

    let Some(instance) = instance else {
        return Ok(ProvisioningBlueprint {
            job_id: job.id,
            kind: job.kind.clone(),
            instance: None,
            image: None,
            nics: Vec::new(),
            disks: Vec::new(),
            ssh_public_keys: Vec::new(),
        });
    };

    let image = match store.get_image(instance.image_id).await {
        Ok(img) => Some(img),
        Err(StoreError::NotFound) => None,
        Err(e) => return Err(store_error_to_http(e)),
    };
    let nics = store
        .list_nics_for_instance(instance.id)
        .await
        .map_err(store_error_to_http)?;
    let disks = store
        .list_disks_for_instance(instance.id)
        .await
        .map_err(store_error_to_http)?;

    let mut ssh_public_keys = Vec::with_capacity(instance.ssh_key_ids.len());
    for key_id in &instance.ssh_key_ids {
        // A key that vanished between instance create and job
        // claim is a transient inconsistency the agent shouldn't
        // crash on — skip and keep going.
        if let Ok(k) = store.get_ssh_key(*key_id).await {
            ssh_public_keys.push(k.public_key);
        }
    }

    Ok(ProvisioningBlueprint {
        job_id: job.id,
        kind: job.kind.clone(),
        instance: Some(instance),
        image,
        nics,
        disks,
        ssh_public_keys,
    })
}

const INITIAL_PROTEUS_PORT_GENERATION: u64 = 1;

/// Materialise the opaque Proteus `PortBlueprint` the bound CN agent
/// should apply for a NIC.
async fn build_port_blueprint(
    store: &dyn Store,
    port_id: Uuid,
    bound_cn: Uuid,
) -> Result<AgentPortBlueprint, HttpError> {
    let nic = store.get_nic(port_id).await.map_err(store_error_to_http)?;
    let instance = store
        .get_instance(nic.instance_id)
        .await
        .map_err(store_error_to_http)?;
    enforce_port_instance_claimed_by_bound_cn(store, instance.id, bound_cn).await?;

    let project = store
        .get_project(nic.project_id)
        .await
        .map_err(store_error_to_http)?;
    let tenant = store
        .get_tenant(nic.tenant_id)
        .await
        .map_err(store_error_to_http)?;
    let vpc = store
        .get_vpc(nic.vpc_id)
        .await
        .map_err(store_error_to_http)?;
    let subnet = store
        .get_subnet(nic.subnet_id)
        .await
        .map_err(store_error_to_http)?;

    if project.tenant_id != nic.tenant_id
        || tenant.id != nic.tenant_id
        || vpc.tenant_id != nic.tenant_id
        || vpc.project_id != nic.project_id
        || subnet.tenant_id != nic.tenant_id
        || subnet.project_id != nic.project_id
        || subnet.vpc_id != nic.vpc_id
        || instance.tenant_id != nic.tenant_id
        || instance.project_id != nic.project_id
    {
        return Err(not_found());
    }

    let routes = store
        .list_routes_in_table(subnet.route_table_id)
        .await
        .map_err(store_error_to_http)?;
    let nat_gateways = store
        .list_nat_gateways_in_vpc(vpc.id)
        .await
        .map_err(store_error_to_http)?;
    let floating_ips = store
        .list_floating_ips_in_project(project.id)
        .await
        .map_err(store_error_to_http)?;

    let generation = INITIAL_PROTEUS_PORT_GENERATION;
    let intent = TritondPortIntentV1 {
        silo_id: tenant.silo_id,
        tenant_id: nic.tenant_id,
        project_id: nic.project_id,
        vpc: VpcIntentV1 {
            id: vpc.id,
            tenant_id: vpc.tenant_id,
            project_id: vpc.project_id,
            main_route_table_id: vpc.main_route_table_id,
            name: vpc.name,
            description: vpc.description,
            vni: vpc.vni,
            ipv4_block: vpc.ipv4_block.map(|cidr| cidr.to_string()),
            ipv6_block: vpc.ipv6_block.map(|cidr| cidr.to_string()),
        },
        subnet: SubnetIntentV1 {
            id: subnet.id,
            tenant_id: subnet.tenant_id,
            project_id: subnet.project_id,
            vpc_id: subnet.vpc_id,
            route_table_id: subnet.route_table_id,
            name: subnet.name,
            description: subnet.description,
            ipv4_block: subnet.ipv4_block.map(|cidr| cidr.to_string()),
            ipv6_block: subnet.ipv6_block.map(|cidr| cidr.to_string()),
        },
        nic: NicIntentV1 {
            id: nic.id,
            tenant_id: nic.tenant_id,
            project_id: nic.project_id,
            instance_id: nic.instance_id,
            vpc_id: nic.vpc_id,
            subnet_id: nic.subnet_id,
            name: nic.name,
            mac: nic.mac.clone(),
            primary_ipv4: nic.primary_ipv4.map(|addr| addr.to_string()),
            primary_ipv6: nic.primary_ipv6.map(|addr| addr.to_string()),
        },
        instance_id: instance.id,
        port_id,
        routes: routes
            .iter()
            .map(route_intent)
            .collect::<Result<Vec<_>, _>>()?,
        nat_gateways: nat_gateways.iter().map(nat_gateway_intent).collect(),
        floating_ips: floating_ips.iter().map(floating_ip_intent).collect(),
        edge_clusters: Vec::new(),
    };

    let plugin_blueprint = intent.compile_blueprint().map_err(|err| {
        store_error_to_http(StoreError::Conflict(format!(
            "port blueprint is not currently compilable: {err}"
        )))
    })?;
    let plugin_bytes = postcard::to_allocvec(&plugin_blueprint).map_err(|err| {
        HttpError::for_internal_error(format!("encode Triton VPC blueprint: {err}"))
    })?;
    let port_blueprint = PortBlueprint {
        port_id: ProteusPortId(port_id),
        network_id: ProteusNetworkId::TRITON_VPC,
        schema_version: PORT_BLUEPRINT_SCHEMA_V0,
        generation: ProteusGeneration::new(generation),
        limits: PortLimits::DEFAULT,
        link: ClientLinkConfig {
            mtu: 1500,
            mac_address: Some(parse_mac_bytes(&nic.mac)?),
            vlan_id: None,
        },
        plugin_config: PluginConfigBytes::new(
            ProteusNetworkId::TRITON_VPC,
            TRITON_VPC_BLUEPRINT_SCHEMA_V1,
            plugin_bytes,
        ),
    };
    let port_bytes = postcard::to_allocvec(&port_blueprint).map_err(|err| {
        HttpError::for_internal_error(format!("encode Proteus port blueprint: {err}"))
    })?;
    let blueprint_postcard_base64 = base64::engine::general_purpose::STANDARD.encode(port_bytes);

    Ok(AgentPortBlueprint {
        port_id,
        generation,
        blueprint_postcard_base64,
    })
}

async fn enforce_port_instance_claimed_by_bound_cn(
    store: &dyn Store,
    instance_id: Uuid,
    bound_cn: Uuid,
) -> Result<(), HttpError> {
    let jobs = store
        .list_recent_jobs(1024)
        .await
        .map_err(store_error_to_http)?;
    for job in jobs
        .iter()
        .filter(|job| job.kind.instance_id() == instance_id)
        .filter(|job| matches!(job.status, JobStatus::InProgress))
    {
        if enforce_job_belongs_to_bound_cn(job, bound_cn).is_ok() {
            return Ok(());
        }
    }

    Err(HttpError::for_client_error(
        Some("Forbidden".to_string()),
        ClientErrorStatusCode::FORBIDDEN,
        "bound key has no in-progress claim for this port's instance".to_string(),
    ))
}

fn route_intent(route: &Route) -> Result<RouteIntentV1, HttpError> {
    Ok(RouteIntentV1 {
        id: route.id,
        tenant_id: route.tenant_id,
        project_id: route.project_id,
        vpc_id: route.vpc_id,
        route_table_id: route.route_table_id,
        name: route.name.clone(),
        description: route.description.clone(),
        destination: route.destination.to_string(),
        target: route_target_intent(&route.target)?,
    })
}

fn route_target_intent(target: &RouteTarget) -> Result<RouteTargetIntentV1, HttpError> {
    match target {
        RouteTarget::Blackhole => Ok(RouteTargetIntentV1::Blackhole),
        RouteTarget::Reject => Ok(RouteTargetIntentV1::Reject),
        RouteTarget::VirtualGateway => Ok(RouteTargetIntentV1::VirtualGateway),
        RouteTarget::NatGateway { nat_gateway_id } => Ok(RouteTargetIntentV1::NatGateway {
            nat_gateway_id: *nat_gateway_id,
        }),
        RouteTarget::FloatingIp { floating_ip_id } => Ok(RouteTargetIntentV1::FloatingIp {
            floating_ip_id: *floating_ip_id,
        }),
        _ => Err(HttpError::for_internal_error(
            "unsupported route target variant in port blueprint compiler".to_string(),
        )),
    }
}

fn nat_gateway_intent(nat: &NatGateway) -> NatGatewayIntentV1 {
    NatGatewayIntentV1 {
        id: nat.id,
        tenant_id: nat.tenant_id,
        project_id: nat.project_id,
        vpc_id: nat.vpc_id,
        name: nat.name.clone(),
        description: nat.description.clone(),
        public_address: nat.public_address.to_string(),
        edge_cluster_id: nat.edge_cluster_id,
        desired_generation: nat.desired_generation,
    }
}

fn floating_ip_intent(fip: &FloatingIp) -> FloatingIpIntentV1 {
    FloatingIpIntentV1 {
        id: fip.id,
        tenant_id: fip.tenant_id,
        project_id: fip.project_id,
        name: fip.name.clone(),
        description: fip.description.clone(),
        address: fip.address.to_string(),
        attached_to: fip
            .attached_to
            .as_ref()
            .map(|attachment| FloatingIpAttachmentIntentV1 {
                instance_id: attachment.instance_id,
                nic_id: attachment.nic_id,
            }),
        edge_cluster_id: None,
    }
}

fn parse_mac_bytes(value: &str) -> Result<[u8; 6], HttpError> {
    let mut mac = [0u8; 6];
    let mut count = 0usize;
    for (idx, part) in value.split(':').enumerate() {
        if idx >= mac.len() || part.len() != 2 {
            return Err(invalid_stored_mac(value));
        }
        mac[idx] = u8::from_str_radix(part, 16).map_err(|_| invalid_stored_mac(value))?;
        count += 1;
    }
    if count != mac.len() {
        return Err(invalid_stored_mac(value));
    }
    Ok(mac)
}

fn invalid_stored_mac(value: &str) -> HttpError {
    HttpError::for_internal_error(format!("stored NIC has invalid MAC address {value:?}"))
}

/// Map a [`StoreError`] to the appropriate HTTP response.
fn store_error_to_http(err: StoreError) -> HttpError {
    match err {
        StoreError::NotFound => HttpError::for_client_error(
            Some("NotFound".to_string()),
            ClientErrorStatusCode::NOT_FOUND,
            "not found".to_string(),
        ),
        StoreError::Conflict(msg) => HttpError::for_client_error(
            Some("Conflict".to_string()),
            ClientErrorStatusCode::CONFLICT,
            msg,
        ),
        StoreError::Backend(msg) => HttpError::for_internal_error(msg),
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

    let server = HttpServerStarter::new(&config_dropshot, api, context, &log)
        .map_err(|e| anyhow::anyhow!("failed to start HTTP server: {e}"))?
        .start();

    Ok(server)
}
