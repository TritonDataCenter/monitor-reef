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

pub mod auth;
pub mod bootstrap;

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use dropshot::{
    ApiDescription, ClientErrorStatusCode, ConfigDropshot, ConfigLogging, ConfigLoggingLevel,
    HttpError, HttpResponseCreated, HttpResponseDeleted, HttpResponseOk, HttpServer,
    HttpServerStarter, Path, RequestContext, TypedBody,
};
use tritond_api::{
    ApiKeyCreated, ApiKeyPath, HealthResponse, LoginRequest, NewApiKey, RefreshRequest, SiloPath,
    TokenResponse, TritondApi,
    types::{ApiKeyView, NewSilo, Silo},
};
use tritond_auth::{
    JwtKey, TokenKind, generate_api_key, mint_access, mint_refresh, verify, verify_password,
};
use tritond_store::{ApiKey, MemStore, Store, StoreError};
use uuid::Uuid;

use crate::auth::{Action, AuthService, authenticate_and_authorize, require_authenticated};

/// Service version, populated from Cargo at build time.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Default bind address for the Dropshot HTTP server.
pub const DEFAULT_BIND_ADDRESS: &str = "127.0.0.1:8080";

/// Shared state for API handlers.
pub struct ApiContext {
    pub store: Arc<dyn Store>,
    pub auth: Arc<AuthService>,
}

impl ApiContext {
    pub fn new(store: Arc<dyn Store>, auth: Arc<AuthService>) -> Self {
        Self { store, auth }
    }

    /// Build a context backed by a fresh in-memory store and a fresh
    /// random JWT key. Convenient for integration tests.
    pub fn in_memory() -> Result<Self> {
        let store: Arc<dyn Store> = Arc::new(MemStore::new());
        let auth = Arc::new(AuthService::new(JwtKey::generate())?);
        Ok(Self::new(store, auth))
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
        authenticate_and_authorize(&rqctx, &ctx.auth, &ctx.store, Action::Health).await?;
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
        authenticate_and_authorize(&rqctx, &ctx.auth, &ctx.store, Action::CreateSilo).await?;
        let silo = ctx
            .store
            .create_silo(body.into_inner())
            .await
            .map_err(store_error_to_http)?;
        Ok(HttpResponseCreated(silo))
    }

    async fn get_silo(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloPath>,
    ) -> Result<HttpResponseOk<Silo>, HttpError> {
        let ctx = rqctx.context();
        authenticate_and_authorize(&rqctx, &ctx.auth, &ctx.store, Action::GetSilo).await?;
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
        // Cedar still gates login (the public-actions rule), partly so
        // the policy bundle is the single source of truth for what an
        // unauth'd caller can do.
        authenticate_and_authorize(&rqctx, &ctx.auth, &ctx.store, Action::Login).await?;
        let req = body.into_inner();

        let user = match ctx.store.get_user_by_username(&req.username).await {
            Ok(user) => user,
            Err(StoreError::NotFound) => return Err(invalid_credentials()),
            Err(e) => return Err(store_error_to_http(e)),
        };
        let password_ok = verify_password(&req.password, &user.password_hash)
            .await
            .map_err(|e| HttpError::for_internal_error(format!("verify password: {e}")))?;
        if !password_ok {
            return Err(invalid_credentials());
        }

        let response = mint_token_pair(&ctx.auth, user.id)?;
        Ok(HttpResponseOk(response))
    }

    async fn refresh(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<RefreshRequest>,
    ) -> Result<HttpResponseOk<TokenResponse>, HttpError> {
        let ctx = rqctx.context();
        authenticate_and_authorize(&rqctx, &ctx.auth, &ctx.store, Action::Refresh).await?;
        let req = body.into_inner();

        let claims = verify(ctx.auth.jwt_key(), &req.refresh_token, TokenKind::Refresh)
            .map_err(|_| invalid_credentials())?;
        // Confirm the user still exists; deactivated users can't
        // silently extend their session via stored refresh tokens.
        ctx.store
            .get_user_by_id(claims.sub)
            .await
            .map_err(|_| invalid_credentials())?;

        let response = mint_token_pair(&ctx.auth, claims.sub)?;
        Ok(HttpResponseOk(response))
    }

    async fn create_api_key(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<NewApiKey>,
    ) -> Result<HttpResponseCreated<ApiKeyCreated>, HttpError> {
        let ctx = rqctx.context();
        let principal =
            authenticate_and_authorize(&rqctx, &ctx.auth, &ctx.store, Action::CreateApiKey).await?;
        let (user_id, _) = require_authenticated(principal)?;
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
            created_at: chrono::Utc::now(),
        };
        let saved = ctx
            .store
            .create_api_key(record)
            .await
            .map_err(store_error_to_http)?;
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
        let principal =
            authenticate_and_authorize(&rqctx, &ctx.auth, &ctx.store, Action::ListApiKeys).await?;
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
        let principal =
            authenticate_and_authorize(&rqctx, &ctx.auth, &ctx.store, Action::DeleteApiKey).await?;
        let (user_id, _) = require_authenticated(principal)?;
        let key_id = path.into_inner().api_key_id;
        ctx.store
            .delete_api_key(user_id, key_id)
            .await
            .map_err(store_error_to_http)?;
        Ok(HttpResponseDeleted())
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

    let server = HttpServerStarter::new(&config_dropshot, api, context, &log)
        .map_err(|e| anyhow::anyhow!("failed to start HTTP server: {e}"))?
        .start();

    Ok(server)
}
