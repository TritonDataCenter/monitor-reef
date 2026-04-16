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
use hyper_util::rt::{TokioExecutor, TokioIo};
use serde::Deserialize;
use tracing::{Instrument, error, info, info_span, warn};

/// Per-request identifier used for log correlation across gateway and backends.
/// Stored in request extensions so the proxy handler can read it and forward
/// it as an `X-Request-Id` header to the upstream.
#[derive(Clone)]
struct RequestId(String);

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
        .layer(axum::middleware::from_fn(request_id_middleware))
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

/// Middleware that establishes a per-request `RequestId` and a tracing span.
///
/// Behavior:
/// - If the incoming request has an `X-Request-Id` header, use it verbatim.
/// - Otherwise generate a fresh UUIDv4 (hyphenated lowercase).
/// - The ID is stored in request extensions so `forward_request` can
///   propagate it as `X-Request-Id` to the backend.
/// - The downstream future is instrumented with an `http_request` span
///   carrying `method`, `path`, and `request_id` fields so every log line
///   emitted during the request is tagged.
/// - The response echoes `X-Request-Id` back to the client.
async fn request_id_middleware(
    mut req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let request_id: String = req
        .headers()
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    req.extensions_mut().insert(RequestId(request_id.clone()));

    let span = info_span!(
        "http_request",
        method = %req.method(),
        path = %req.uri().path(),
        request_id = %request_id,
    );

    let mut response = next.run(req).instrument(span).await;

    // Echo the header to the client so they can correlate their request
    // with our logs. If the (possibly client-supplied) value isn't a valid
    // HeaderValue, silently drop -- we still logged with it.
    if let Ok(hv) = http::HeaderValue::from_str(&request_id) {
        response.headers_mut().insert("x-request-id", hv);
    }

    response
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
    dispatch(&state, &state.tritonapi, peer_addr, req, "tritonapi").await
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
    dispatch(&state, target, peer_addr, req, "CloudAPI").await
}

/// Inspect the incoming request and route it to either the WebSocket tunnel
/// or the plain HTTP forwarder. The `label` is used purely for error logs so
/// operators can tell which backend choked.
async fn dispatch(
    state: &GatewayState,
    target: &BackendTarget,
    peer_addr: SocketAddr,
    req: axum::extract::Request,
    label: &str,
) -> Response {
    if is_websocket_upgrade(req.headers()) {
        match forward_websocket(state, target, peer_addr, req).await {
            Ok(resp) => resp,
            Err(e) => {
                error!("websocket proxy to {} failed: {}", label, e);
                StatusCode::BAD_GATEWAY.into_response()
            }
        }
    } else {
        match forward_request(state, target, peer_addr, req).await {
            Ok(resp) => resp,
            Err(e) => {
                error!("proxy to {} failed: {}", label, e);
                StatusCode::BAD_GATEWAY.into_response()
            }
        }
    }
}

/// Returns true if the request headers indicate a WebSocket upgrade handshake.
///
/// RFC 6455: a client initiates a WebSocket handshake with `Upgrade: websocket`
/// and `Connection: Upgrade`. The Connection header may carry additional tokens
/// (e.g. `keep-alive, Upgrade`), so we split on commas and match case-insensitively.
fn is_websocket_upgrade(headers: &http::HeaderMap) -> bool {
    let connection = headers
        .get(http::header::CONNECTION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let upgrade = headers
        .get(http::header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    connection
        .split(',')
        .any(|s| s.trim().eq_ignore_ascii_case("upgrade"))
        && upgrade.eq_ignore_ascii_case("websocket")
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
    let proxy_req = build_outbound_request(target, peer_addr, parts, body, false)?;
    let resp = state.client.request(proxy_req).await?;

    let (resp_parts, resp_body) = resp.into_parts();
    Ok(Response::from_parts(resp_parts, Body::new(resp_body)))
}

/// Forward a WebSocket upgrade handshake to a backend and, on a successful
/// 101 response, tunnel raw bytes between the client and backend.
///
/// This does not parse WebSocket frames. Once both sides have completed the
/// HTTP/1.1 upgrade, we simply shovel bytes in both directions until either
/// side closes. This is how nginx's `proxy_pass` (with `Upgrade`/`Connection`
/// forwarded) handles WebSocket, and it avoids pulling in a full WebSocket
/// implementation for what is fundamentally a transparent tunnel.
///
/// Caveats:
/// - Only `http://` backends are supported today. The shared `state.client`
///   is built via `build_http()`; a `wss://` backend would require an HTTPS
///   connector. If we ever need that, add it here.
/// - If the backend returns anything other than `101 Switching Protocols`
///   we pass the response through verbatim (e.g. a 401 or 404), and no
///   tunnel is started.
async fn forward_websocket(
    state: &GatewayState,
    target: &BackendTarget,
    peer_addr: SocketAddr,
    mut req: axum::extract::Request,
) -> Result<Response, anyhow::Error> {
    // Grab the client-side upgrade future BEFORE consuming the request. This
    // future will only resolve once axum/hyper has flushed our 101 response
    // back to the client -- at which point the TCP stream becomes a raw byte
    // pipe we can splice to the backend.
    let client_on_upgrade = hyper::upgrade::on(&mut req);

    let (parts, body) = req.into_parts();
    let proxy_req = build_outbound_request(target, peer_addr, parts, body, true)?;
    let mut resp = state.client.request(proxy_req).await?;

    // If the backend refused the upgrade (auth failure, wrong path, etc.)
    // just forward whatever it returned. The client will see the error and
    // no tunnel is started.
    if resp.status() != StatusCode::SWITCHING_PROTOCOLS {
        let (resp_parts, resp_body) = resp.into_parts();
        return Ok(Response::from_parts(resp_parts, Body::new(resp_body)));
    }

    // Backend agreed to upgrade. Grab the backend-side upgrade future and
    // spawn the bidirectional copy. Both futures resolve after their
    // respective 101 responses have been flushed over the wire.
    let backend_on_upgrade = hyper::upgrade::on(&mut resp);

    tokio::spawn(async move {
        let (client_upgraded, backend_upgraded) =
            match tokio::try_join!(client_on_upgrade, backend_on_upgrade) {
                Ok(pair) => pair,
                Err(e) => {
                    warn!("websocket upgrade failed: {}", e);
                    return;
                }
            };

        // `Upgraded` implements hyper's Read/Write; TokioIo adapts that to
        // tokio's AsyncRead/AsyncWrite so copy_bidirectional can drive it.
        let mut client_io = TokioIo::new(client_upgraded);
        let mut backend_io = TokioIo::new(backend_upgraded);

        match tokio::io::copy_bidirectional(&mut client_io, &mut backend_io).await {
            Ok((from_client, from_backend)) => {
                info!(
                    client_bytes = from_client,
                    backend_bytes = from_backend,
                    "websocket tunnel closed"
                );
            }
            Err(e) => {
                // EOF on one side mid-stream is normal for WebSocket close;
                // only log at warn to aid debugging of actually-broken tunnels.
                warn!("websocket tunnel closed with error: {}", e);
            }
        }
    });

    // Hand the backend's 101 back to the client. Axum/hyper will flush these
    // headers and then resolve our `client_on_upgrade` future with the raw
    // stream. Do NOT consume the body here -- there is none, and touching it
    // would interfere with the upgrade machinery.
    let (resp_parts, resp_body) = resp.into_parts();
    Ok(Response::from_parts(resp_parts, Body::new(resp_body)))
}

/// Build the outbound `hyper::Request` forwarded to a backend, shared between
/// the HTTP and WebSocket paths.
///
/// When `preserve_upgrade` is true, the `Upgrade`, `Connection`, and
/// `Sec-WebSocket-*` headers are copied through so the backend sees a real
/// upgrade handshake. Normally these are hop-by-hop and stripped.
fn build_outbound_request(
    target: &BackendTarget,
    peer_addr: SocketAddr,
    parts: http::request::Parts,
    body: Body,
    preserve_upgrade: bool,
) -> Result<hyper::Request<Body>, anyhow::Error> {
    // Pull the request ID our middleware stashed in extensions so we can
    // forward it to the backend. Always present in practice (the middleware
    // is registered on every route) but fall back to None defensively.
    let request_id = parts.extensions.get::<RequestId>().map(|r| r.0.clone());

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
    //
    // For WebSocket upgrades we deliberately keep Connection/Upgrade (and
    // the Sec-WebSocket-* headers, which aren't hop-by-hop anyway but are
    // listed here for clarity). These are normally stripped as hop-by-hop
    // but must flow end-to-end during the upgrade handshake, otherwise the
    // backend won't know to switch protocols.
    for (name, value) in &parts.headers {
        let n = name.as_str();
        if is_hop_by_hop(n) && !(preserve_upgrade && is_websocket_handshake_header(n)) {
            continue;
        }
        if n == "host"
            || n == "x-forwarded-for"
            || n == "x-forwarded-proto"
            || n == "x-forwarded-host"
            || n == "x-real-ip"
            || n == "x-request-id"
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
    // Propagate the request ID to the backend so its logs correlate with
    // ours. If for some reason we have no stored ID (shouldn't happen --
    // the middleware runs on every route), skip rather than fabricate one
    // here.
    if let Some(rid) = request_id.as_deref()
        && let Ok(hv) = HeaderValue::from_str(rid)
    {
        builder = builder.header(HeaderName::from_static("x-request-id"), hv);
    }

    Ok(builder.body(body)?)
}

/// Headers that are hop-by-hop in the general case but must be forwarded
/// verbatim during a WebSocket upgrade handshake.
fn is_websocket_handshake_header(name: &str) -> bool {
    matches!(name, "connection" | "upgrade")
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
