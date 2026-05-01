// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Triton Cloud control plane daemon.
//!
//! Phase 0: serves the minimal `TritondApi` surface (`/v2/health`) over
//! Dropshot. Subsequent phases wire FoundationDB-backed state, Cedar
//! policy evaluation, OIDC token validation, and the rest of the API
//! surface described in DESIGN.md §14.
//!
//! The library exposes the building blocks (`TritondServiceImpl`,
//! `api_description`, `start_server`) so integration tests can spin up
//! the service in-process; the binary is a thin wrapper.

use anyhow::{Context, Result};
use dropshot::{
    ApiDescription, ConfigDropshot, ConfigLogging, ConfigLoggingLevel, HttpError, HttpResponseOk,
    HttpServer, HttpServerStarter, RequestContext,
};
use std::net::SocketAddr;
use tritond_api::{HealthResponse, TritondApi};

/// Service version, populated from Cargo at build time.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Default bind address for the Dropshot HTTP server.
pub const DEFAULT_BIND_ADDRESS: &str = "127.0.0.1:8080";

/// Shared state for API handlers.
///
/// Phase 0 holds nothing of substance. Phase 1 adds the FoundationDB
/// handle, Cedar policy bundle, and OIDC validator.
pub struct ApiContext {}

impl ApiContext {
    pub fn new() -> Self {
        Self {}
    }
}

impl Default for ApiContext {
    fn default() -> Self {
        Self::new()
    }
}

/// Concrete implementor of `TritondApi`. Stateless; all per-request
/// state lives in `ApiContext` and is reached via
/// `RequestContext::context()`.
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
}

/// Build the `ApiDescription` for `tritond`. The same description is
/// used by the binary and by integration tests.
pub fn api_description() -> Result<ApiDescription<ApiContext>> {
    tritond_api::tritond_api_mod::api_description::<TritondServiceImpl>()
        .map_err(|e| anyhow::anyhow!("failed to build API description: {e}"))
}

/// Start a Dropshot server and return the running handle. The caller is
/// responsible for awaiting completion (or letting the handle drop in
/// tests, which shuts the server down).
pub async fn start_server(bind_address: &str) -> Result<HttpServer<ApiContext>> {
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
    let context = ApiContext::new();

    let server = HttpServerStarter::new(&config_dropshot, api, context, &log)
        .map_err(|e| anyhow::anyhow!("failed to start HTTP server: {e}"))?
        .start();

    Ok(server)
}
