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

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use axum::Router;
use axum::body::Body;
use axum::extract::{ConnectInfo, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use http::header::{HOST, HeaderName, HeaderValue};
use http::uri::{Authority, PathAndQuery, Scheme};
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use serde::Deserialize;
use tracing::{error, info};

/// Parsed backend target: scheme + authority for a given upstream. This is
/// computed once at startup so hot-path forwarding doesn't re-parse URLs
/// and can't produce `//` from naive string concatenation.
#[derive(Clone, Debug)]
struct BackendTarget {
    scheme: Scheme,
    authority: Authority,
}

impl BackendTarget {
    fn parse(raw: &str) -> Result<Self> {
        let uri: hyper::Uri = raw
            .parse()
            .with_context(|| format!("invalid backend URL: {}", raw))?;
        let scheme = uri
            .scheme()
            .cloned()
            .ok_or_else(|| anyhow!("backend URL missing scheme: {}", raw))?;
        let authority = uri
            .authority()
            .cloned()
            .ok_or_else(|| anyhow!("backend URL missing authority: {}", raw))?;
        Ok(Self { scheme, authority })
    }

    /// Render as a string for logging (e.g. "http://127.0.0.1:8080").
    fn display(&self) -> String {
        format!("{}://{}", self.scheme, self.authority)
    }
}

#[derive(Clone)]
struct GatewayState {
    /// HTTP client for proxying to backends.
    client: Client<hyper_util::client::legacy::connect::HttpConnector, Body>,
    /// Parsed backend for triton-api-server.
    tritonapi: BackendTarget,
    /// Parsed backend for CloudAPI, if configured.
    cloudapi: Option<BackendTarget>,
}

#[derive(Deserialize)]
struct GatewayConfig {
    #[serde(default = "default_bind_address")]
    bind_address: String,
    #[serde(default = "default_tritonapi_url")]
    tritonapi_url: String,
    #[serde(default)]
    cloudapi_url: Option<String>,
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
            cloudapi_url: None,
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new("triton_gateway=info"))
        .init();

    let config = load_config()?;

    let client = Client::builder(TokioExecutor::new()).build_http();

    let tritonapi = BackendTarget::parse(&config.tritonapi_url)?;
    let cloudapi = match config.cloudapi_url.as_deref() {
        Some(url) if !url.is_empty() => Some(BackendTarget::parse(url)?),
        _ => None,
    };

    let state = Arc::new(GatewayState {
        client,
        tritonapi: tritonapi.clone(),
        cloudapi: cloudapi.clone(),
    });

    // Routes that tritonapi handles -- forwarded to triton-api-server.
    // This list grows as tritonapi gains endpoints.
    let app = Router::new()
        .route("/ping", get(proxy_to_tritonapi))
        // TODO: Add /auth/* routes when tritonapi implements them
        // Everything else proxies to CloudAPI.
        .fallback(proxy_to_cloudapi)
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&config.bind_address).await?;
    info!("triton-gateway listening on {}", config.bind_address);
    info!("proxying to tritonapi at {}", tritonapi.display());
    match &cloudapi {
        Some(target) => info!("proxying to CloudAPI at {}", target.display()),
        None => info!("CloudAPI proxy DISABLED (no cloudapi_url configured)"),
    }

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await?;

    Ok(())
}

/// Await either SIGTERM or SIGINT (Ctrl-C), whichever arrives first.
///
/// SMF's `stop` method sends SIGTERM; Ctrl-C in a dev shell sends SIGINT.
/// When the signal arrives we log and return, letting the caller (axum's
/// `with_graceful_shutdown`) drain in-flight requests. SMF's
/// `timeout_seconds` on the stop method is the hard backstop if draining
/// takes too long.
async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    let mut sigterm = match signal(SignalKind::terminate()) {
        Ok(s) => s,
        Err(e) => {
            error!("failed to install SIGTERM handler: {}", e);
            return;
        }
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {},
        _ = sigterm.recv() => {},
    }
    info!("shutdown signal received, draining in-flight requests");
}

/// Load config from TRITON__CONFIG_FILE env var.
///
/// If the env var is unset, returns defaults (useful for dev).
/// If the env var is set but the file cannot be read or parsed, returns
/// an error so the process exits non-zero -- SMF will mark the service in
/// maintenance and an operator will notice.
fn load_config() -> Result<GatewayConfig> {
    let Some(path) = std::env::var("TRITON__CONFIG_FILE").ok() else {
        info!("TRITON__CONFIG_FILE not set; using default config");
        return Ok(GatewayConfig::default());
    };

    let contents = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read config from {}", path))?;
    let config: GatewayConfig = serde_json::from_str(&contents)
        .with_context(|| format!("failed to parse config from {}", path))?;
    info!("loaded config from {}", path);
    Ok(config)
}

/// Forward a request to triton-api-server, preserving method/path/headers/body.
async fn proxy_to_tritonapi(
    State(state): State<Arc<GatewayState>>,
    ConnectInfo(peer_addr): ConnectInfo<SocketAddr>,
    req: axum::extract::Request,
) -> Response {
    match forward_request(&state, &state.tritonapi, peer_addr, req).await {
        Ok(resp) => resp,
        Err(e) => {
            error!("proxy to tritonapi failed: {}", e);
            StatusCode::BAD_GATEWAY.into_response()
        }
    }
}

/// Forward a request to CloudAPI. Returns 502 if CloudAPI is unreachable,
/// or 501 if no CloudAPI URL is configured.
async fn proxy_to_cloudapi(
    State(state): State<Arc<GatewayState>>,
    ConnectInfo(peer_addr): ConnectInfo<SocketAddr>,
    req: axum::extract::Request,
) -> Response {
    let Some(target) = state.cloudapi.as_ref() else {
        return (
            StatusCode::NOT_IMPLEMENTED,
            "triton-gateway: no cloudapi_url configured\n",
        )
            .into_response();
    };
    match forward_request(&state, target, peer_addr, req).await {
        Ok(resp) => resp,
        Err(e) => {
            error!("proxy to CloudAPI failed: {}", e);
            StatusCode::BAD_GATEWAY.into_response()
        }
    }
}

/// Forward an HTTP request to a backend, returning the response.
///
/// Rewrites `Host` to the backend's authority (backends do virtual-host
/// routing) and adds the standard `X-Forwarded-*` / `X-Real-IP` headers so
/// CloudAPI audit logs record the real client IP rather than the gateway.
async fn forward_request(
    state: &GatewayState,
    target: &BackendTarget,
    peer_addr: SocketAddr,
    req: axum::extract::Request,
) -> Result<Response, anyhow::Error> {
    let (parts, body) = req.into_parts();

    // Preserve the original path+query exactly, falling back to "/".
    // Using a URI builder (vs. string concat) avoids producing "//" when
    // the configured base URL ends with "/" and the request path starts
    // with "/".
    let path_and_query: PathAndQuery = parts
        .uri
        .path_and_query()
        .cloned()
        .unwrap_or_else(|| PathAndQuery::from_static("/"));

    let uri = hyper::Uri::builder()
        .scheme(target.scheme.clone())
        .authority(target.authority.clone())
        .path_and_query(path_and_query)
        .build()?;

    // Capture the client's original Host header before we rewrite it, so we
    // can forward it as X-Forwarded-Host.
    let original_host = parts.headers.get(HOST).cloned();
    // X-Forwarded-Proto: honor existing value if the client was behind
    // another proxy; default to "http" (we don't terminate TLS).
    let forwarded_proto = parts
        .headers
        .get(HeaderName::from_static("x-forwarded-proto"))
        .cloned()
        .unwrap_or_else(|| HeaderValue::from_static("http"));
    // X-Forwarded-For: append our peer to any existing chain, or set it.
    let forwarded_for = match parts
        .headers
        .get(HeaderName::from_static("x-forwarded-for"))
    {
        Some(existing) => {
            let existing = existing.to_str().unwrap_or("");
            HeaderValue::from_str(&format!("{}, {}", existing, peer_addr.ip()))?
        }
        None => HeaderValue::from_str(&peer_addr.ip().to_string())?,
    };
    let real_ip = HeaderValue::from_str(&peer_addr.ip().to_string())?;
    let backend_host = HeaderValue::from_str(target.authority.as_str())?;

    let mut builder = hyper::Request::builder().method(parts.method).uri(uri);

    // Copy headers, skipping hop-by-hop headers and Host (we set our own
    // below) and any X-Forwarded-* / X-Real-IP we're about to replace.
    for (name, value) in &parts.headers {
        let n = name.as_str();
        if is_hop_by_hop(n) {
            continue;
        }
        if n == "host"
            || n == "x-forwarded-for"
            || n == "x-forwarded-proto"
            || n == "x-forwarded-host"
            || n == "x-real-ip"
        {
            continue;
        }
        builder = builder.header(name, value);
    }

    // Rewrite Host to the backend's authority.
    builder = builder.header(HOST, backend_host);
    // Forwarded headers for downstream audit logs / trust.
    builder = builder.header(HeaderName::from_static("x-forwarded-for"), forwarded_for);
    builder = builder.header(
        HeaderName::from_static("x-forwarded-proto"),
        forwarded_proto,
    );
    if let Some(h) = original_host {
        builder = builder.header(HeaderName::from_static("x-forwarded-host"), h);
    }
    builder = builder.header(HeaderName::from_static("x-real-ip"), real_ip);

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
