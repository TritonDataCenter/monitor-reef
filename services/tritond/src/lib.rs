// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Triton Cloud control plane daemon.
//!
//! Phase 0 surface: `/v2/health` plus silo create/read against a
//! pluggable [`Store`]. The default backend is [`MemStore`] (in-process,
//! ephemeral); the production FoundationDB backend lands in a follow-up
//! commit and slots in via the same trait without changes to the
//! handler code.
//!
//! The library exposes the building blocks (`TritondServiceImpl`,
//! `ApiContext`, `api_description`, `start_server`) so integration
//! tests can spin up the service in-process; the binary is a thin
//! wrapper.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use dropshot::{
    ApiDescription, ConfigDropshot, ConfigLogging, ConfigLoggingLevel, HttpError,
    HttpResponseCreated, HttpResponseOk, HttpServer, HttpServerStarter, Path, RequestContext,
    TypedBody,
};
use tritond_api::{
    HealthResponse, SiloPath, TritondApi,
    types::{NewSilo, Silo},
};
use tritond_store::{MemStore, Store, StoreError};

/// Service version, populated from Cargo at build time.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Default bind address for the Dropshot HTTP server.
pub const DEFAULT_BIND_ADDRESS: &str = "127.0.0.1:8080";

/// Shared state for API handlers.
///
/// Holds the [`Store`] handle and any other process-wide collaborators
/// (Cedar policy bundle, OIDC validator) once they land. The store is
/// kept behind a trait object so the binary, the integration tests,
/// and any future swappable backend (e.g. FoundationDB) all flow
/// through the same call site.
pub struct ApiContext {
    pub store: Arc<dyn Store>,
}

impl ApiContext {
    pub fn new(store: Arc<dyn Store>) -> Self {
        Self { store }
    }

    /// Convenience constructor that uses an in-process [`MemStore`].
    pub fn in_memory() -> Self {
        Self::new(Arc::new(MemStore::new()))
    }
}

/// Concrete implementor of [`TritondApi`]. Stateless; per-request state
/// reaches handlers via [`RequestContext::context`].
pub enum TritondServiceImpl {}

impl TritondApi for TritondServiceImpl {
    type Context = ApiContext;

    async fn health(
        _rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseOk<HealthResponse>, HttpError> {
        Ok(HttpResponseOk(HealthResponse {
            status: "ok".to_string(),
            version: VERSION.to_string(),
        }))
    }

    async fn create_silo(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<NewSilo>,
    ) -> Result<HttpResponseCreated<Silo>, HttpError> {
        let silo = rqctx
            .context()
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
        let silo_id = path.into_inner().silo_id;
        let silo = rqctx
            .context()
            .store
            .get_silo(silo_id)
            .await
            .map_err(store_error_to_http)?;
        Ok(HttpResponseOk(silo))
    }
}

/// Map a [`StoreError`] to the appropriate HTTP response.
fn store_error_to_http(err: StoreError) -> HttpError {
    match err {
        StoreError::NotFound => HttpError::for_client_error(
            Some("NotFound".to_string()),
            dropshot::ClientErrorStatusCode::NOT_FOUND,
            "not found".to_string(),
        ),
        StoreError::Conflict(msg) => HttpError::for_client_error(
            Some("Conflict".to_string()),
            dropshot::ClientErrorStatusCode::CONFLICT,
            msg,
        ),
        StoreError::Backend(msg) => HttpError::for_internal_error(msg),
    }
}

/// Build the [`ApiDescription`] for `tritond`. The same description is
/// used by the binary and by integration tests.
pub fn api_description() -> Result<ApiDescription<ApiContext>> {
    tritond_api::tritond_api_mod::api_description::<TritondServiceImpl>()
        .map_err(|e| anyhow::anyhow!("failed to build API description: {e}"))
}

/// Start a Dropshot server with a freshly-constructed in-memory store.
/// Convenience wrapper for tests and `main`.
pub async fn start_server(bind_address: &str) -> Result<HttpServer<ApiContext>> {
    start_server_with_store(bind_address, Arc::new(MemStore::new())).await
}

/// Start a Dropshot server backed by the supplied [`Store`].
pub async fn start_server_with_store(
    bind_address: &str,
    store: Arc<dyn Store>,
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
    let context = ApiContext::new(store);

    let server = HttpServerStarter::new(&config_dropshot, api, context, &log)
        .map_err(|e| anyhow::anyhow!("failed to start HTTP server: {e}"))?
        .start();

    Ok(server)
}
