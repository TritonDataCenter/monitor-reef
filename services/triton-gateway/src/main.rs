// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! triton-gateway: Temporary reverse proxy that sits in front of tritonapi
//! and CloudAPI during the strangler fig migration.
//!
//! Routes implemented endpoints to triton-api-server (Dropshot) and will
//! eventually proxy everything else to CloudAPI. Dies when tritonapi is
//! complete.

use std::sync::Arc;

use anyhow::Result;
use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use http::uri::PathAndQuery;
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use serde::Deserialize;
use tracing::{error, info};

#[derive(Clone)]
struct GatewayState {
    /// HTTP client for proxying to backends.
    client: Client<hyper_util::client::legacy::connect::HttpConnector, Body>,
    /// Base URL for triton-api-server (e.g. "http://127.0.0.1:8080").
    tritonapi_url: String,
}

#[derive(Deserialize)]
struct GatewayConfig {
    #[serde(default = "default_bind_address")]
    bind_address: String,
    #[serde(default = "default_tritonapi_url")]
    tritonapi_url: String,
}

fn default_bind_address() -> String {
    "0.0.0.0:80".to_string()
}

fn default_tritonapi_url() -> String {
    "http://127.0.0.1:8080".to_string()
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            bind_address: default_bind_address(),
            tritonapi_url: default_tritonapi_url(),
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new("triton_gateway=info"))
        .init();

    let config = load_config();

    let client = Client::builder(TokioExecutor::new()).build_http();

    let state = Arc::new(GatewayState {
        client,
        tritonapi_url: config.tritonapi_url.clone(),
    });

    // Routes that tritonapi handles -- forwarded to triton-api-server.
    // This list grows as tritonapi gains endpoints.
    let app = Router::new()
        .route("/ping", get(proxy_to_tritonapi))
        // TODO: Add /auth/* routes when tritonapi implements them
        // Everything else will eventually proxy to CloudAPI.
        .fallback(cloudapi_stub)
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&config.bind_address).await?;
    info!("triton-gateway listening on {}", config.bind_address);
    info!("proxying to tritonapi at {}", config.tritonapi_url);

    axum::serve(listener, app).await?;

    Ok(())
}

/// Load config from TRITON__CONFIG_FILE env var, falling back to defaults.
fn load_config() -> GatewayConfig {
    let path = std::env::var("TRITON__CONFIG_FILE").ok();
    if let Some(ref path) = path {
        match std::fs::read_to_string(path) {
            Ok(contents) => match serde_json::from_str(&contents) {
                Ok(config) => {
                    info!("loaded config from {}", path);
                    return config;
                }
                Err(e) => {
                    error!("failed to parse config from {}: {}", path, e);
                }
            },
            Err(e) => {
                error!("failed to read config from {}: {}", path, e);
            }
        }
    }
    info!("using default config");
    GatewayConfig::default()
}

/// Forward a request to triton-api-server, preserving method/path/headers/body.
async fn proxy_to_tritonapi(
    State(state): State<Arc<GatewayState>>,
    req: axum::extract::Request,
) -> Response {
    match forward_request(&state, &state.tritonapi_url, req).await {
        Ok(resp) => resp,
        Err(e) => {
            error!("proxy to tritonapi failed: {}", e);
            StatusCode::BAD_GATEWAY.into_response()
        }
    }
}

/// Stub handler for routes that will eventually proxy to CloudAPI.
async fn cloudapi_stub() -> impl IntoResponse {
    (
        StatusCode::NOT_IMPLEMENTED,
        "triton-gateway: CloudAPI proxy not yet implemented\n",
    )
}

/// Forward an HTTP request to a backend, returning the response.
async fn forward_request(
    state: &GatewayState,
    base_url: &str,
    req: axum::extract::Request,
) -> Result<Response, anyhow::Error> {
    let (parts, body) = req.into_parts();

    let path = parts
        .uri
        .path_and_query()
        .map(PathAndQuery::as_str)
        .unwrap_or("/");

    let uri: hyper::Uri = format!("{}{}", base_url, path).parse()?;

    let mut builder = hyper::Request::builder().method(parts.method).uri(uri);

    // Copy headers, skipping hop-by-hop headers.
    for (name, value) in &parts.headers {
        if !is_hop_by_hop(name.as_str()) {
            builder = builder.header(name, value);
        }
    }

    let proxy_req = builder.body(body)?;
    let resp = state.client.request(proxy_req).await?;

    let (resp_parts, resp_body) = resp.into_parts();
    Ok(Response::from_parts(resp_parts, Body::new(resp_body)))
}

/// Returns true for HTTP hop-by-hop headers that should not be forwarded.
fn is_hop_by_hop(name: &str) -> bool {
    matches!(
        name,
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailers"
            | "transfer-encoding"
            | "upgrade"
    )
}
