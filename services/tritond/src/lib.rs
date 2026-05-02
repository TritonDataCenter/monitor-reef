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

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use dropshot::{
    ApiDescription, ClientErrorStatusCode, ConfigDropshot, ConfigLogging, ConfigLoggingLevel,
    HttpError, HttpResponseCreated, HttpResponseDeleted, HttpResponseOk, HttpServer,
    HttpServerStarter, Path, Query, RequestContext, TypedBody,
};
use tritond_api::{
    ApiKeyCreated, ApiKeyPath, AttachFloatingIpRequest, AuditEventList, AuditEventPath,
    AuditListQuery, AuditVerifyQuery, AuditVerifyResponse, HealthResponse, LoginRequest, NewApiKey,
    NewIdpConfig, RefreshRequest, SiloImagePath, SiloPath, SiloProjectFloatingIpPath,
    SiloProjectInstanceDiskPath, SiloProjectInstanceNicPath, SiloProjectInstancePath,
    SiloProjectPath, SiloProjectVpcPath, SiloProjectVpcSubnetPath, SiloSshKeyPath, TokenResponse,
    TritondApi,
    types::{
        ApiKeyView, AuditEvent, Disk, FloatingIp, IdpConfigView, Image, Instance, JobKind,
        LifecycleState, LifecycleStateKind, NewFloatingIp, NewImage, NewInstance, NewJob,
        NewProject, NewQuota, NewSilo, NewSshKey, NewSubnet, NewVpc, Nic, Project, Quota, Silo,
        SshKey, Subnet, Vpc,
    },
};
use tritond_audit::{Actor as AuditActor, MemChain, Outcome as AuditOutcome};
use tritond_auth::OidcConfig;
use tritond_auth::{
    JwtKey, TokenKind, generate_api_key, mint_access, mint_refresh, verify, verify_password,
};
use tritond_store::{ApiKey, IdpConfig, MemStore, Store, StoreError};
use uuid::Uuid;

use crate::audit::AuditService;
use crate::auth::{
    Action, AuthService, authenticate_and_authorize, authenticate_and_authorize_in_silo,
    require_authenticated,
};
use crate::rate_limit::LoginRateLimiter;

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
}

impl ApiContext {
    pub fn new(store: Arc<dyn Store>, auth: Arc<AuthService>, audit: Arc<AuditService>) -> Self {
        Self {
            store,
            auth,
            audit,
            login_rate_limiter: Arc::new(LoginRateLimiter::new()),
        }
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

    async fn put_silo_idp(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloPath>,
        body: TypedBody<NewIdpConfig>,
    ) -> Result<HttpResponseCreated<IdpConfigView>, HttpError> {
        let ctx = rqctx.context();
        authenticate_and_authorize(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::SiloIdpSet,
        )
        .await?;
        let silo_id = path.into_inner().silo_id;
        // Confirm the silo exists; reject 404 cleanly rather than
        // dangling an IdP config off a non-existent silo.
        ctx.store
            .get_silo(silo_id)
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
            .discover(&silo_id.to_string(), &oidc_cfg)
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
            .put_idp_config(silo_id, config)
            .await
            .map_err(store_error_to_http)?;
        Ok(HttpResponseCreated(saved.into()))
    }

    async fn get_silo_idp(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloPath>,
    ) -> Result<HttpResponseOk<IdpConfigView>, HttpError> {
        let ctx = rqctx.context();
        authenticate_and_authorize(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::SiloIdpGet,
        )
        .await?;
        let silo_id = path.into_inner().silo_id;
        let config = ctx
            .store
            .get_idp_config(silo_id)
            .await
            .map_err(store_error_to_http)?;
        Ok(HttpResponseOk(config.into()))
    }

    async fn delete_silo_idp(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloPath>,
    ) -> Result<HttpResponseDeleted, HttpError> {
        let ctx = rqctx.context();
        authenticate_and_authorize(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::SiloIdpDelete,
        )
        .await?;
        let silo_id = path.into_inner().silo_id;
        ctx.store
            .delete_idp_config(silo_id)
            .await
            .map_err(store_error_to_http)?;
        ctx.auth.oidc().invalidate(&silo_id.to_string()).await;
        Ok(HttpResponseDeleted())
    }

    async fn list_silo_projects(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloPath>,
    ) -> Result<HttpResponseOk<Vec<Project>>, HttpError> {
        let ctx = rqctx.context();
        let silo_id = path.into_inner().silo_id;
        authenticate_and_authorize_in_silo(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::ProjectList,
            silo_id,
        )
        .await?;
        let projects = ctx
            .store
            .list_projects_in_silo(silo_id)
            .await
            .map_err(store_error_to_http)?;
        Ok(HttpResponseOk(projects))
    }

    async fn create_silo_project(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloPath>,
        body: TypedBody<NewProject>,
    ) -> Result<HttpResponseCreated<Project>, HttpError> {
        let ctx = rqctx.context();
        let silo_id = path.into_inner().silo_id;
        let principal = authenticate_and_authorize_in_silo(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::ProjectCreate,
            silo_id,
        )
        .await?;
        let request_id = parse_request_id(&rqctx);
        let req = body.into_inner();
        match ctx.store.create_project(silo_id, req).await {
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
                            "silo_id": silo_id,
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

    async fn get_silo_project(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloProjectPath>,
    ) -> Result<HttpResponseOk<Project>, HttpError> {
        let ctx = rqctx.context();
        let SiloProjectPath {
            silo_id,
            project_id,
        } = path.into_inner();
        authenticate_and_authorize_in_silo(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::ProjectGet,
            silo_id,
        )
        .await?;
        let project = ctx
            .store
            .get_project(project_id)
            .await
            .map_err(store_error_to_http)?;
        // Project found globally — confirm it actually belongs to the
        // path's silo. Cross-silo lookups (would-be probes) get the
        // same 404 as a missing project.
        if project.silo_id != silo_id {
            return Err(HttpError::for_client_error(
                Some("NotFound".to_string()),
                ClientErrorStatusCode::NOT_FOUND,
                "not found".to_string(),
            ));
        }
        Ok(HttpResponseOk(project))
    }

    async fn delete_silo_project(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloProjectPath>,
    ) -> Result<HttpResponseDeleted, HttpError> {
        let ctx = rqctx.context();
        let SiloProjectPath {
            silo_id,
            project_id,
        } = path.into_inner();
        let principal = authenticate_and_authorize_in_silo(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::ProjectDelete,
            silo_id,
        )
        .await?;
        let request_id = parse_request_id(&rqctx);

        // Confirm silo membership before deleting; cross-silo deletes
        // get a 404 like cross-silo gets.
        let project = ctx
            .store
            .get_project(project_id)
            .await
            .map_err(store_error_to_http)?;
        if project.silo_id != silo_id {
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
                serde_json::json!({ "silo_id": silo_id }),
            )
            .await;
        Ok(HttpResponseDeleted())
    }

    async fn list_project_vpcs(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloProjectPath>,
    ) -> Result<HttpResponseOk<Vec<Vpc>>, HttpError> {
        let ctx = rqctx.context();
        let SiloProjectPath {
            silo_id,
            project_id,
        } = path.into_inner();
        authenticate_and_authorize_in_silo(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::VpcList,
            silo_id,
        )
        .await?;

        // Verify the project actually lives in the path's silo. A
        // project_id that names some other silo's project is treated
        // as not-found; this stops cross-tenant enumeration via the
        // VPC list endpoint.
        let project = ctx
            .store
            .get_project(project_id)
            .await
            .map_err(store_error_to_http)?;
        if project.silo_id != silo_id {
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
        path: Path<SiloProjectPath>,
        body: TypedBody<NewVpc>,
    ) -> Result<HttpResponseCreated<Vpc>, HttpError> {
        let ctx = rqctx.context();
        let SiloProjectPath {
            silo_id,
            project_id,
        } = path.into_inner();
        let principal = authenticate_and_authorize_in_silo(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::VpcCreate,
            silo_id,
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
                    serde_json::json!({ "silo_id": silo_id, "project_id": project_id }),
                )
                .await;
            return Err(HttpError::for_bad_request(
                Some("BadRequest".to_string()),
                "vpc must specify ipv4_block, ipv6_block, or both".to_string(),
            ));
        }

        match ctx.store.create_vpc(silo_id, project_id, req).await {
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
                            "silo_id": silo_id,
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
        path: Path<SiloProjectVpcPath>,
    ) -> Result<HttpResponseOk<Vpc>, HttpError> {
        let ctx = rqctx.context();
        let SiloProjectVpcPath {
            silo_id,
            project_id,
            vpc_id,
        } = path.into_inner();
        authenticate_and_authorize_in_silo(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::VpcGet,
            silo_id,
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
        if vpc.silo_id != silo_id || vpc.project_id != project_id {
            return Err(not_found());
        }
        Ok(HttpResponseOk(vpc))
    }

    async fn delete_project_vpc(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloProjectVpcPath>,
    ) -> Result<HttpResponseDeleted, HttpError> {
        let ctx = rqctx.context();
        let SiloProjectVpcPath {
            silo_id,
            project_id,
            vpc_id,
        } = path.into_inner();
        let principal = authenticate_and_authorize_in_silo(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::VpcDelete,
            silo_id,
        )
        .await?;
        let request_id = parse_request_id(&rqctx);

        // Same defence-in-depth shape as get_project_vpc.
        let vpc = ctx
            .store
            .get_vpc(vpc_id)
            .await
            .map_err(store_error_to_http)?;
        if vpc.silo_id != silo_id || vpc.project_id != project_id {
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
                            "silo_id": silo_id,
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
        path: Path<SiloProjectVpcPath>,
    ) -> Result<HttpResponseOk<Vec<Subnet>>, HttpError> {
        let ctx = rqctx.context();
        let SiloProjectVpcPath {
            silo_id,
            project_id,
            vpc_id,
        } = path.into_inner();
        authenticate_and_authorize_in_silo(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::SubnetList,
            silo_id,
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
        if vpc.silo_id != silo_id || vpc.project_id != project_id {
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
        path: Path<SiloProjectVpcPath>,
        body: TypedBody<NewSubnet>,
    ) -> Result<HttpResponseCreated<Subnet>, HttpError> {
        let ctx = rqctx.context();
        let SiloProjectVpcPath {
            silo_id,
            project_id,
            vpc_id,
        } = path.into_inner();
        let principal = authenticate_and_authorize_in_silo(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::SubnetCreate,
            silo_id,
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
                        "silo_id": silo_id,
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
            .create_subnet(silo_id, project_id, vpc_id, req)
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
                            "silo_id": silo_id,
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
        path: Path<SiloProjectVpcSubnetPath>,
    ) -> Result<HttpResponseOk<Subnet>, HttpError> {
        let ctx = rqctx.context();
        let SiloProjectVpcSubnetPath {
            silo_id,
            project_id,
            vpc_id,
            subnet_id,
        } = path.into_inner();
        authenticate_and_authorize_in_silo(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::SubnetGet,
            silo_id,
        )
        .await?;
        let subnet = ctx
            .store
            .get_subnet(subnet_id)
            .await
            .map_err(store_error_to_http)?;
        // Defence-in-depth: subnet must live in path silo + project + vpc.
        if subnet.silo_id != silo_id || subnet.project_id != project_id || subnet.vpc_id != vpc_id {
            return Err(not_found());
        }
        Ok(HttpResponseOk(subnet))
    }

    async fn delete_vpc_subnet(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloProjectVpcSubnetPath>,
    ) -> Result<HttpResponseDeleted, HttpError> {
        let ctx = rqctx.context();
        let SiloProjectVpcSubnetPath {
            silo_id,
            project_id,
            vpc_id,
            subnet_id,
        } = path.into_inner();
        let principal = authenticate_and_authorize_in_silo(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::SubnetDelete,
            silo_id,
        )
        .await?;
        let request_id = parse_request_id(&rqctx);

        let subnet = ctx
            .store
            .get_subnet(subnet_id)
            .await
            .map_err(store_error_to_http)?;
        if subnet.silo_id != silo_id || subnet.project_id != project_id || subnet.vpc_id != vpc_id {
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
                    "silo_id": silo_id,
                    "project_id": project_id,
                    "vpc_id": vpc_id,
                }),
            )
            .await;
        Ok(HttpResponseDeleted())
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

        // Server-side parse + fingerprint compute. Bad openssh
        // payload → 400, never propagated to the store.
        let fingerprint = match parse_ssh_public_key(&req.public_key) {
            Ok(fp) => fp,
            Err(msg) => {
                ctx.audit
                    .record_mutation(
                        &principal,
                        Action::SshKeyCreate,
                        request_id,
                        None,
                        AuditOutcome::ClientError {
                            code: 400,
                            message: msg.clone(),
                        },
                        serde_json::json!({ "silo_id": silo_id }),
                    )
                    .await;
                return Err(HttpError::for_bad_request(
                    Some("BadRequest".to_string()),
                    msg,
                ));
            }
        };

        match ctx.store.create_ssh_key(silo_id, req, fingerprint).await {
            Ok(key) => {
                ctx.audit
                    .record_mutation(
                        &principal,
                        Action::SshKeyCreate,
                        request_id,
                        Some(format!("SshKey::\"{}\"", key.id)),
                        AuditOutcome::Success {
                            resource: Some(format!("SshKey::\"{}\"", key.id)),
                        },
                        serde_json::json!({
                            "silo_id": silo_id,
                            "name": key.name,
                            "fingerprint": key.fingerprint,
                        }),
                    )
                    .await;
                Ok(HttpResponseCreated(key))
            }
            Err(e) => {
                ctx.audit
                    .record_mutation(
                        &principal,
                        Action::SshKeyCreate,
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

    async fn get_silo_ssh_key(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloSshKeyPath>,
    ) -> Result<HttpResponseOk<SshKey>, HttpError> {
        let ctx = rqctx.context();
        let SiloSshKeyPath {
            silo_id,
            ssh_key_id,
        } = path.into_inner();
        authenticate_and_authorize_in_silo(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::SshKeyGet,
            silo_id,
        )
        .await?;
        let key = ctx
            .store
            .get_ssh_key(ssh_key_id)
            .await
            .map_err(store_error_to_http)?;
        if key.silo_id != silo_id {
            return Err(not_found());
        }
        Ok(HttpResponseOk(key))
    }

    async fn delete_silo_ssh_key(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloSshKeyPath>,
    ) -> Result<HttpResponseDeleted, HttpError> {
        let ctx = rqctx.context();
        let SiloSshKeyPath {
            silo_id,
            ssh_key_id,
        } = path.into_inner();
        let principal = authenticate_and_authorize_in_silo(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::SshKeyDelete,
            silo_id,
        )
        .await?;
        let request_id = parse_request_id(&rqctx);

        let key = ctx
            .store
            .get_ssh_key(ssh_key_id)
            .await
            .map_err(store_error_to_http)?;
        if key.silo_id != silo_id {
            return Err(not_found());
        }
        ctx.store
            .delete_ssh_key(ssh_key_id)
            .await
            .map_err(store_error_to_http)?;
        ctx.audit
            .record_mutation(
                &principal,
                Action::SshKeyDelete,
                request_id,
                Some(format!("SshKey::\"{ssh_key_id}\"")),
                AuditOutcome::Success {
                    resource: Some(format!("SshKey::\"{ssh_key_id}\"")),
                },
                serde_json::json!({ "silo_id": silo_id }),
            )
            .await;
        Ok(HttpResponseDeleted())
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

        // Format checks at the API edge so the store stays opaque to
        // hex/byte-size invariants.
        if let Err(msg) = validate_sha256(&req.sha256) {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::ImageCreate,
                    request_id,
                    None,
                    AuditOutcome::ClientError {
                        code: 400,
                        message: msg.clone(),
                    },
                    serde_json::json!({ "silo_id": silo_id }),
                )
                .await;
            return Err(HttpError::for_bad_request(
                Some("BadRequest".to_string()),
                msg,
            ));
        }
        if req.size_bytes == 0 {
            let msg = "size_bytes must be greater than zero".to_string();
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::ImageCreate,
                    request_id,
                    None,
                    AuditOutcome::ClientError {
                        code: 400,
                        message: msg.clone(),
                    },
                    serde_json::json!({ "silo_id": silo_id }),
                )
                .await;
            return Err(HttpError::for_bad_request(
                Some("BadRequest".to_string()),
                msg,
            ));
        }

        match ctx.store.create_image(silo_id, req).await {
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
                        }),
                    )
                    .await;
                Ok(HttpResponseCreated(image))
            }
            Err(e) => {
                ctx.audit
                    .record_mutation(
                        &principal,
                        Action::ImageCreate,
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

    async fn get_silo_image(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloImagePath>,
    ) -> Result<HttpResponseOk<Image>, HttpError> {
        let ctx = rqctx.context();
        let SiloImagePath { silo_id, image_id } = path.into_inner();
        authenticate_and_authorize_in_silo(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::ImageGet,
            silo_id,
        )
        .await?;
        let image = ctx
            .store
            .get_image(image_id)
            .await
            .map_err(store_error_to_http)?;
        if image.silo_id != silo_id {
            return Err(not_found());
        }
        Ok(HttpResponseOk(image))
    }

    async fn delete_silo_image(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloImagePath>,
    ) -> Result<HttpResponseDeleted, HttpError> {
        let ctx = rqctx.context();
        let SiloImagePath { silo_id, image_id } = path.into_inner();
        let principal = authenticate_and_authorize_in_silo(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::ImageDelete,
            silo_id,
        )
        .await?;
        let request_id = parse_request_id(&rqctx);

        let image = ctx
            .store
            .get_image(image_id)
            .await
            .map_err(store_error_to_http)?;
        if image.silo_id != silo_id {
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
                serde_json::json!({ "silo_id": silo_id }),
            )
            .await;
        Ok(HttpResponseDeleted())
    }

    async fn put_project_quota(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloProjectPath>,
        body: TypedBody<NewQuota>,
    ) -> Result<HttpResponseOk<Quota>, HttpError> {
        let ctx = rqctx.context();
        let SiloProjectPath {
            silo_id,
            project_id,
        } = path.into_inner();
        let principal = authenticate_and_authorize_in_silo(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::QuotaSet,
            silo_id,
        )
        .await?;
        let request_id = parse_request_id(&rqctx);
        let req = body.into_inner();

        match ctx.store.put_quota(silo_id, project_id, req).await {
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
                            "silo_id": silo_id,
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
        path: Path<SiloProjectPath>,
    ) -> Result<HttpResponseOk<Quota>, HttpError> {
        let ctx = rqctx.context();
        let SiloProjectPath {
            silo_id,
            project_id,
        } = path.into_inner();
        authenticate_and_authorize_in_silo(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::QuotaGet,
            silo_id,
        )
        .await?;
        let quota = ctx
            .store
            .get_quota(silo_id, project_id)
            .await
            .map_err(store_error_to_http)?;
        Ok(HttpResponseOk(quota))
    }

    async fn delete_project_quota(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloProjectPath>,
    ) -> Result<HttpResponseDeleted, HttpError> {
        let ctx = rqctx.context();
        let SiloProjectPath {
            silo_id,
            project_id,
        } = path.into_inner();
        let principal = authenticate_and_authorize_in_silo(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::QuotaDelete,
            silo_id,
        )
        .await?;
        let request_id = parse_request_id(&rqctx);

        match ctx.store.delete_quota(silo_id, project_id).await {
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
                            "silo_id": silo_id,
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
        path: Path<SiloProjectPath>,
    ) -> Result<HttpResponseOk<Vec<Instance>>, HttpError> {
        let ctx = rqctx.context();
        let SiloProjectPath {
            silo_id,
            project_id,
        } = path.into_inner();
        authenticate_and_authorize_in_silo(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::InstanceList,
            silo_id,
        )
        .await?;
        // Project must exist + be in this silo (matches the
        // list_project_vpcs / list_vpc_subnets pattern).
        let project = ctx
            .store
            .get_project(project_id)
            .await
            .map_err(store_error_to_http)?;
        if project.silo_id != silo_id {
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
        path: Path<SiloProjectPath>,
        body: TypedBody<NewInstance>,
    ) -> Result<HttpResponseCreated<Instance>, HttpError> {
        let ctx = rqctx.context();
        let SiloProjectPath {
            silo_id,
            project_id,
        } = path.into_inner();
        let principal = authenticate_and_authorize_in_silo(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::InstanceCreate,
            silo_id,
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
                serde_json::json!({ "silo_id": silo_id, "project_id": project_id }),
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
                serde_json::json!({ "silo_id": silo_id, "project_id": project_id }),
            )
            .await);
        }

        let instance = match ctx.store.create_instance(silo_id, project_id, req).await {
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
                    "silo_id": silo_id,
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
        path: Path<SiloProjectInstancePath>,
    ) -> Result<HttpResponseOk<Instance>, HttpError> {
        let ctx = rqctx.context();
        let SiloProjectInstancePath {
            silo_id,
            project_id,
            instance_id,
        } = path.into_inner();
        authenticate_and_authorize_in_silo(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::InstanceGet,
            silo_id,
        )
        .await?;
        let instance = ctx
            .store
            .get_instance(instance_id)
            .await
            .map_err(store_error_to_http)?;
        if instance.silo_id != silo_id || instance.project_id != project_id {
            return Err(not_found());
        }
        Ok(HttpResponseOk(instance))
    }

    async fn delete_project_instance(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloProjectInstancePath>,
    ) -> Result<HttpResponseDeleted, HttpError> {
        let ctx = rqctx.context();
        let SiloProjectInstancePath {
            silo_id,
            project_id,
            instance_id,
        } = path.into_inner();
        let principal = authenticate_and_authorize_in_silo(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::InstanceDelete,
            silo_id,
        )
        .await?;
        let request_id = parse_request_id(&rqctx);

        let instance = ctx
            .store
            .get_instance(instance_id)
            .await
            .map_err(store_error_to_http)?;
        if instance.silo_id != silo_id || instance.project_id != project_id {
            return Err(not_found());
        }
        match ctx.store.delete_instance(instance_id).await {
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
                            "silo_id": silo_id,
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
        path: Path<SiloProjectInstancePath>,
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
        path: Path<SiloProjectInstancePath>,
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
        path: Path<SiloProjectInstancePath>,
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
        path: Path<SiloProjectInstancePath>,
    ) -> Result<HttpResponseOk<Vec<Nic>>, HttpError> {
        let ctx = rqctx.context();
        let SiloProjectInstancePath {
            silo_id,
            project_id,
            instance_id,
        } = path.into_inner();
        authenticate_and_authorize_in_silo(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::NicList,
            silo_id,
        )
        .await?;
        // Defence-in-depth: instance must live in path's silo+project.
        let instance = ctx
            .store
            .get_instance(instance_id)
            .await
            .map_err(store_error_to_http)?;
        if instance.silo_id != silo_id || instance.project_id != project_id {
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
        path: Path<SiloProjectInstanceNicPath>,
    ) -> Result<HttpResponseOk<Nic>, HttpError> {
        let ctx = rqctx.context();
        let SiloProjectInstanceNicPath {
            silo_id,
            project_id,
            instance_id,
            nic_id,
        } = path.into_inner();
        authenticate_and_authorize_in_silo(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::NicGet,
            silo_id,
        )
        .await?;
        let nic = ctx
            .store
            .get_nic(nic_id)
            .await
            .map_err(store_error_to_http)?;
        // Defence-in-depth: NIC must live under all three path levels.
        if nic.silo_id != silo_id || nic.project_id != project_id || nic.instance_id != instance_id
        {
            return Err(not_found());
        }
        Ok(HttpResponseOk(nic))
    }

    async fn list_instance_disks(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloProjectInstancePath>,
    ) -> Result<HttpResponseOk<Vec<Disk>>, HttpError> {
        let ctx = rqctx.context();
        let SiloProjectInstancePath {
            silo_id,
            project_id,
            instance_id,
        } = path.into_inner();
        authenticate_and_authorize_in_silo(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::DiskList,
            silo_id,
        )
        .await?;
        // Defence-in-depth: instance must live in path silo+project.
        let instance = ctx
            .store
            .get_instance(instance_id)
            .await
            .map_err(store_error_to_http)?;
        if instance.silo_id != silo_id || instance.project_id != project_id {
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
        path: Path<SiloProjectInstanceDiskPath>,
    ) -> Result<HttpResponseOk<Disk>, HttpError> {
        let ctx = rqctx.context();
        let SiloProjectInstanceDiskPath {
            silo_id,
            project_id,
            instance_id,
            disk_id,
        } = path.into_inner();
        authenticate_and_authorize_in_silo(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::DiskGet,
            silo_id,
        )
        .await?;
        let disk = ctx
            .store
            .get_disk(disk_id)
            .await
            .map_err(store_error_to_http)?;
        // Defence-in-depth on all three parent ids.
        if disk.silo_id != silo_id
            || disk.project_id != project_id
            || disk.instance_id != instance_id
        {
            return Err(not_found());
        }
        Ok(HttpResponseOk(disk))
    }

    async fn list_project_floating_ips(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloProjectPath>,
    ) -> Result<HttpResponseOk<Vec<FloatingIp>>, HttpError> {
        let ctx = rqctx.context();
        let SiloProjectPath {
            silo_id,
            project_id,
        } = path.into_inner();
        authenticate_and_authorize_in_silo(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::FloatingIpList,
            silo_id,
        )
        .await?;
        // Defence-in-depth: project must live in path's silo.
        let project = ctx
            .store
            .get_project(project_id)
            .await
            .map_err(store_error_to_http)?;
        if project.silo_id != silo_id {
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
        path: Path<SiloProjectPath>,
        body: TypedBody<NewFloatingIp>,
    ) -> Result<HttpResponseCreated<FloatingIp>, HttpError> {
        let ctx = rqctx.context();
        let SiloProjectPath {
            silo_id,
            project_id,
        } = path.into_inner();
        let principal = authenticate_and_authorize_in_silo(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::FloatingIpCreate,
            silo_id,
        )
        .await?;
        let request_id = parse_request_id(&rqctx);
        let req = body.into_inner();

        match ctx.store.create_floating_ip(silo_id, project_id, req).await {
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
                            "silo_id": silo_id,
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
        path: Path<SiloProjectFloatingIpPath>,
    ) -> Result<HttpResponseOk<FloatingIp>, HttpError> {
        let ctx = rqctx.context();
        let SiloProjectFloatingIpPath {
            silo_id,
            project_id,
            floating_ip_id,
        } = path.into_inner();
        authenticate_and_authorize_in_silo(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::FloatingIpGet,
            silo_id,
        )
        .await?;
        let fip = ctx
            .store
            .get_floating_ip(floating_ip_id)
            .await
            .map_err(store_error_to_http)?;
        if fip.silo_id != silo_id || fip.project_id != project_id {
            return Err(not_found());
        }
        Ok(HttpResponseOk(fip))
    }

    async fn delete_project_floating_ip(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloProjectFloatingIpPath>,
    ) -> Result<HttpResponseDeleted, HttpError> {
        let ctx = rqctx.context();
        let SiloProjectFloatingIpPath {
            silo_id,
            project_id,
            floating_ip_id,
        } = path.into_inner();
        let principal = authenticate_and_authorize_in_silo(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::FloatingIpDelete,
            silo_id,
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
        if fip.silo_id != silo_id || fip.project_id != project_id {
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
                            "silo_id": silo_id,
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
        path: Path<SiloProjectFloatingIpPath>,
        body: TypedBody<AttachFloatingIpRequest>,
    ) -> Result<HttpResponseOk<FloatingIp>, HttpError> {
        let ctx = rqctx.context();
        let SiloProjectFloatingIpPath {
            silo_id,
            project_id,
            floating_ip_id,
        } = path.into_inner();
        let principal = authenticate_and_authorize_in_silo(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::FloatingIpAttach,
            silo_id,
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
        if fip.silo_id != silo_id || fip.project_id != project_id {
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
                            "silo_id": silo_id,
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
        path: Path<SiloProjectFloatingIpPath>,
    ) -> Result<HttpResponseOk<FloatingIp>, HttpError> {
        let ctx = rqctx.context();
        let SiloProjectFloatingIpPath {
            silo_id,
            project_id,
            floating_ip_id,
        } = path.into_inner();
        let principal = authenticate_and_authorize_in_silo(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::FloatingIpDetach,
            silo_id,
        )
        .await?;
        let request_id = parse_request_id(&rqctx);

        let fip = ctx
            .store
            .get_floating_ip(floating_ip_id)
            .await
            .map_err(store_error_to_http)?;
        if fip.silo_id != silo_id || fip.project_id != project_id {
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
                            "silo_id": silo_id,
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
    path: Path<SiloProjectInstancePath>,
    action: Action,
    expected_from: &[LifecycleStateKind],
    to: LifecycleState,
    enqueue: Option<JobKindTemplate>,
) -> Result<HttpResponseOk<Instance>, HttpError> {
    let ctx = rqctx.context();
    let SiloProjectInstancePath {
        silo_id,
        project_id,
        instance_id,
    } = path.into_inner();
    let principal = authenticate_and_authorize_in_silo(
        &rqctx, &ctx.auth, &ctx.audit, &ctx.store, action, silo_id,
    )
    .await?;
    let request_id = parse_request_id(&rqctx);

    // Defence-in-depth on silo+project before we try to transition.
    let instance = ctx
        .store
        .get_instance(instance_id)
        .await
        .map_err(store_error_to_http)?;
    if instance.silo_id != silo_id || instance.project_id != project_id {
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
                "silo_id": silo_id,
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
    // accept requests.
    let _provisioner = provisioner::spawn(Arc::clone(&context.store));

    let server = HttpServerStarter::new(&config_dropshot, api, context, &log)
        .map_err(|e| anyhow::anyhow!("failed to start HTTP server: {e}"))?
        .start();

    Ok(server)
}
