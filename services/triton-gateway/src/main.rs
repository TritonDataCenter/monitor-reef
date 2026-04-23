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
use axum::routing::any;
use http::header::{HOST, HeaderName, HeaderValue};
use http::uri::{Authority, PathAndQuery, Scheme};
use hyper_util::client::legacy::Client;
use hyper_util::rt::{TokioExecutor, TokioIo};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, Error as RustlsError, SignatureScheme};
use serde::Deserialize;
use tracing::{Instrument, error, info, info_span, warn};
use triton_auth::{AuthConfig, KeySource};
use triton_auth_session::{Claims, JwksClient};

/// Build a gateway-originated error response in the legacy CloudAPI wire
/// shape (`{code, message, request_id}`). Used for failures that never
/// reach upstream (missing config, auth failure, signing failure); the
/// gateway is transparent on the proxy path, so its own synthetic errors
/// match the shape unmodified cloudapi clients already parse.
fn gateway_error_response(
    status: StatusCode,
    code: &str,
    message: impl Into<String>,
    request_id: &str,
) -> Response {
    let body = serde_json::json!({
        "code": code,
        "message": message.into(),
        "request_id": request_id,
    });
    (status, axum::Json(body)).into_response()
}

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
    /// HTTP(S) client for proxying to backends. The connector accepts both
    /// `http://` (tritonapi on loopback) and `https://` (CloudAPI) upstreams.
    client: Client<
        hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>,
        Body,
    >,
    /// Parsed backend for triton-api-server.
    tritonapi: BackendTarget,
    /// Parsed backend for CloudAPI, if configured.
    cloudapi: Option<BackendTarget>,
    /// JWKS consumer for verifying tritonapi-issued access tokens. Middleware
    /// uses this on every request except the public `/v1/auth/*` bootstrap
    /// endpoints. `None` disables auth entirely (dev without tritonapi).
    jwks: Option<Arc<JwksClient>>,
    /// CloudAPI signer: the operator SSH key used to sign requests to
    /// CloudAPI on the authenticated user's behalf. `None` disables the
    /// signer (valid JWT → request passes through unsigned and CloudAPI
    /// will reject with InvalidCredentials, which is fine during
    /// bringup).
    cloudapi_signer: Option<Arc<AuthConfig>>,
}

#[derive(Deserialize)]
struct GatewayConfig {
    #[serde(default = "default_bind_address")]
    bind_address: String,
    #[serde(default = "default_tritonapi_url")]
    tritonapi_url: String,
    #[serde(default)]
    cloudapi_url: Option<String>,
    /// Verify upstream TLS certificates. `None` (missing) is treated as `true`
    /// (verify). Setting `false` is required for COAL / dev DCs that use a
    /// self-signed CloudAPI cert; it must never be used in production.
    #[serde(default)]
    tls_verify: Option<bool>,
    /// URL of the tritonapi JWKS document. When unset, defaults to
    /// `<tritonapi_url>/v1/auth/jwks.json`. Setting this to empty string
    /// disables gateway-side JWT verification (dev only).
    #[serde(default)]
    jwks_url: Option<String>,
    /// Optional CloudAPI signer settings. When present, valid-JWT
    /// requests bound for CloudAPI have their path rewritten from
    /// `/my/...` to `/{claims.username}/...` and are signed with this
    /// operator key before being forwarded. When absent, CloudAPI
    /// requests are forwarded verbatim (useful before the signer key
    /// has been provisioned).
    #[serde(default)]
    cloudapi_signer: Option<CloudapiSignerConfig>,
}

/// Mirrors user-portal's `CloudApiConfig` fields: `account` +
/// `key_id` required, `key_file` optional (auto-detect via agent /
/// `~/.ssh/` when absent).
#[derive(Deserialize)]
struct CloudapiSignerConfig {
    /// Operator account login whose SSH key signs the outbound requests.
    account: String,
    /// SSH key fingerprint (MD5 or SHA256). Required; used as the
    /// `keyId` path in the HTTP Signature header and as a lookup key
    /// when `key_file` is not set.
    key_id: String,
    /// Filesystem path to the PEM-encoded private key. When `None`,
    /// `KeySource::auto` searches the SSH agent and `~/.ssh/` for a
    /// key matching `key_id`.
    #[serde(default)]
    key_file: Option<String>,
}

fn build_cloudapi_signer(cfg: &CloudapiSignerConfig) -> AuthConfig {
    let key_source = match &cfg.key_file {
        Some(path) => KeySource::file(path),
        None => KeySource::auto(&cfg.key_id),
    };
    AuthConfig::new(&cfg.account, key_source)
}

fn default_bind_address() -> String {
    // Loopback-only: haproxy terminates TLS on :443 and proxies to us
    // here. Nothing outside the zone should reach this port directly.
    "127.0.0.1:80".to_string()
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
            tls_verify: None,
            jwks_url: None,
            cloudapi_signer: None,
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new("triton_gateway=info"))
        .init();

    let config = load_config().await?;

    // rustls 0.23 requires a process-global `CryptoProvider` to be installed
    // before any `ClientConfig` is built. We use the `ring` backend (pulled in
    // by the `ring` feature of both `rustls` and `hyper-rustls`). This is a
    // no-op if something else in the process already installed one.
    if rustls::crypto::ring::default_provider()
        .install_default()
        .is_err()
    {
        // Another caller beat us to it (only matters in tests / embedding).
        // Safe to ignore -- whatever is installed is a valid provider.
    }

    let client = build_proxy_client(config.tls_verify).await?;

    let tritonapi = BackendTarget::parse(&config.tritonapi_url)?;
    let cloudapi = match config.cloudapi_url.as_deref() {
        Some(url) if !url.is_empty() => Some(BackendTarget::parse(url)?),
        _ => None,
    };

    // Default the JWKS URL to `<tritonapi_url>/v1/auth/jwks.json`; an
    // explicit empty string turns auth off entirely (dev only).
    let jwks_url = match config.jwks_url.as_deref() {
        Some("") => None,
        Some(url) => Some(url.to_string()),
        None => Some(format!(
            "{}/v1/auth/jwks.json",
            config.tritonapi_url.trim_end_matches('/')
        )),
    };
    let insecure_upstream = !config.tls_verify.unwrap_or(true);
    let jwks = match jwks_url {
        Some(url) => {
            let http = triton_tls::build_http_client(insecure_upstream)
                .await
                .context("build HTTP client for JWKS fetch")?;
            let client = JwksClient::new(url.clone(), http);
            // arch-lint: allow(no-error-swallowing) reason="JWKS prime is best-effort; first request reattempts via lazy cache-miss refresh"
            if let Err(e) = client.refresh().await {
                warn!("initial JWKS fetch from {url} failed: {e}. Will retry on first request.");
            } else {
                info!("JWKS primed from {url}");
            }
            Some(client)
        }
        None => {
            warn!("JWKS disabled; gateway will NOT verify JWTs (dev only)");
            None
        }
    };

    let cloudapi_signer = config.cloudapi_signer.as_ref().map(|cfg| {
        info!(
            "CloudAPI signer configured: account={} key_id={} key_file={:?}",
            cfg.account, cfg.key_id, cfg.key_file
        );
        Arc::new(build_cloudapi_signer(cfg))
    });
    if cloudapi_signer.is_none() && cloudapi.is_some() {
        warn!(
            "CloudAPI proxy is configured but no cloudapi_signer is set; \
             valid-JWT requests will be forwarded unsigned and CloudAPI will \
             reject them. Configure [cloudapi_signer] to enable the operator \
             impersonation path."
        );
    }

    let state = Arc::new(GatewayState {
        client,
        tritonapi: tritonapi.clone(),
        cloudapi: cloudapi.clone(),
        jwks,
        cloudapi_signer,
    });

    // All tritonapi-native routes live under /v1/*. Everything else proxies
    // to CloudAPI, which keeps the routing rule a one-liner: the prefix is
    // the contract, not a per-endpoint allowlist.
    let app = Router::new()
        .route("/v1/{*rest}", any(proxy_to_tritonapi))
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
    // Fall back to ctrl_c-only if the SIGTERM handler can't be installed --
    // a pure-unix tokio runtime shouldn't fail, but we don't want shutdown
    // to be the thing that breaks if it does.
    let mut sigterm = signal(SignalKind::terminate()).ok();
    let sigterm_fut = async {
        match sigterm.as_mut() {
            Some(s) => {
                s.recv().await;
            }
            None => std::future::pending::<()>().await,
        }
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {},
        _ = sigterm_fut => {},
    }
    info!("shutdown signal received, draining in-flight requests");
}

/// Pull a bearer token from either `Authorization: Bearer …` or the
/// `auth` cookie. Browsers use the cookie, CLIs use the header.
fn extract_token(headers: &http::HeaderMap) -> Option<String> {
    if let Some(auth) = headers
        .get(http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        && let Some(token) = auth.strip_prefix("Bearer ")
    {
        return Some(token.to_string());
    }
    if let Some(cookie) = headers
        .get(http::header::COOKIE)
        .and_then(|v| v.to_str().ok())
    {
        for part in cookie.split(';') {
            if let Some(value) = part.trim().strip_prefix("auth=") {
                return Some(value.to_string());
            }
        }
    }
    None
}

/// Verify a JWT and stash the `Claims` in request extensions. Called by
/// the CloudAPI proxy handler only: the gateway intentionally stays out
/// of `/v1/*` auth decisions since tritonapi enforces its own policy
/// per endpoint (including intentionally-public routes like `/v1/ping`
/// that haproxy uses for its backend health check).
///
/// Returns `Err(Response)` with a 401 that should be returned to the
/// client when the token is missing or invalid.
async fn authenticate_cloudapi_request(
    jwks: &JwksClient,
    req: &mut axum::extract::Request,
) -> Result<(), Response> {
    let request_id = req
        .extensions()
        .get::<RequestId>()
        .map(|r| r.0.clone())
        .unwrap_or_default();
    let Some(token) = extract_token(req.headers()) else {
        return Err(gateway_error_response(
            StatusCode::UNAUTHORIZED,
            "MissingAuthToken",
            "missing auth token",
            &request_id,
        ));
    };
    match jwks.verify_token(&token).await {
        Ok(claims) => {
            req.extensions_mut().insert(Arc::new(claims));
            Ok(())
        }
        Err(e) => {
            warn!("JWT verification failed: {e}");
            Err(gateway_error_response(
                StatusCode::UNAUTHORIZED,
                "InvalidAuthToken",
                "invalid auth token",
                &request_id,
            ))
        }
    }
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
async fn load_config() -> Result<GatewayConfig> {
    let Some(path) = std::env::var("TRITON__CONFIG_FILE").ok() else {
        info!("TRITON__CONFIG_FILE not set; using default config");
        return Ok(GatewayConfig::default());
    };

    let contents = tokio::fs::read_to_string(&path)
        .await
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
/// or 501 if no CloudAPI URL is configured. Enforces a valid JWT first
/// (when JWKS is configured), then — when the signer is configured —
/// rewrites the path from `/my/...` to `/{claims.username}/...`, strips
/// the client's JWT credentials, and signs the outbound request with
/// the operator SSH key.
async fn proxy_to_cloudapi(
    State(state): State<Arc<GatewayState>>,
    ConnectInfo(peer_addr): ConnectInfo<SocketAddr>,
    mut req: axum::extract::Request,
) -> Response {
    // Pull the gateway-assigned request ID up front so any gateway-
    // originated error path below can stamp it into its response body.
    let gateway_request_id = req
        .extensions()
        .get::<RequestId>()
        .map(|r| r.0.clone())
        .unwrap_or_default();

    let Some(target) = state.cloudapi.as_ref() else {
        return gateway_error_response(
            StatusCode::NOT_IMPLEMENTED,
            "CloudapiProxyDisabled",
            "triton-gateway: no cloudapi_url configured",
            &gateway_request_id,
        );
    };

    // Branch on the client's auth scheme. Only Bearer-JWT traffic gets
    // verified + resigned with the operator key (the tritonapi profile
    // path). HTTP Signature and unauthenticated requests pass through
    // verbatim so legacy cloudapi clients -- node-triton,
    // terraform-provider-triton, anything speaking the draft-cavage HTTP
    // Signature dialect cloudapi understands -- work unmodified against
    // the gateway. For HTTP Signature requests, cloudapi itself verifies
    // the signature against the user's SSH key in UFDS; the gateway
    // deliberately stays out of that path.
    if matches!(
        triton_auth::auth_scheme::classify(req.headers()),
        triton_auth::auth_scheme::AuthScheme::Bearer(_)
    ) {
        if let Some(jwks) = state.jwks.as_ref()
            && let Err(resp) = authenticate_cloudapi_request(jwks, &mut req).await
        {
            return resp;
        }
        if let Some(signer) = state.cloudapi_signer.as_ref()
            && let Err(resp) = sign_cloudapi_request(signer, &mut req).await
        {
            return resp;
        }
    }

    // Responses from upstream pass through verbatim — the gateway is a
    // thin proxy on the /{account}/* surface so unmodified cloudapi
    // clients (node-triton, terraform) see the wire format they expect.
    dispatch(&state, target, peer_addr, req, "CloudAPI").await
}

/// Rewrite `/my/...` to `/{claims.username}/...`, strip the JWT
/// credentials the client sent, and replace them with an HTTP Signature
/// header signed by the operator key. CloudAPI honors `isOperator`, so
/// the operator-signed request scoped to the user's account path is
/// authorized exactly as if the user had signed it themselves.
async fn sign_cloudapi_request(
    signer: &AuthConfig,
    req: &mut axum::extract::Request,
) -> Result<(), Response> {
    let request_id = req
        .extensions()
        .get::<RequestId>()
        .map(|r| r.0.clone())
        .unwrap_or_default();
    let claims = req
        .extensions()
        .get::<Arc<Claims>>()
        .cloned()
        .ok_or_else(|| {
            // Should be unreachable: authenticate_cloudapi_request always
            // inserts Claims before we get here. Belt-and-braces 500.
            error!("sign_cloudapi_request called without Claims in extensions");
            gateway_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "MissingClaims",
                "missing claims",
                &request_id,
            )
        })?;

    // Rewrite /my/... paths so CloudAPI scopes to the authenticated user.
    rewrite_my_prefix(req, &claims.username).map_err(|e| {
        error!("failed to rewrite request URI: {e}");
        gateway_error_response(
            StatusCode::BAD_REQUEST,
            "MalformedRequestUri",
            "malformed request URI",
            &request_id,
        )
    })?;

    req.headers_mut().remove(http::header::AUTHORIZATION);
    req.headers_mut().remove(http::header::COOKIE);

    let method = req.method().as_str().to_lowercase();
    let path_and_query = req
        .uri()
        .path_and_query()
        .map(|p| p.as_str().to_string())
        .unwrap_or_else(|| "/".to_string());

    let (date_header, auth_header) = triton_auth::sign_request(signer, &method, &path_and_query)
        .await
        .map_err(|e| {
            error!("CloudAPI signing failed: {e}");
            gateway_error_response(
                StatusCode::BAD_GATEWAY,
                "SigningFailed",
                "signing failed",
                &request_id,
            )
        })?;

    let date_value = HeaderValue::from_str(&date_header).map_err(|e| {
        error!("Date header not ASCII-safe: {e}");
        gateway_error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "BadDateHeader",
            "bad date header",
            &request_id,
        )
    })?;
    let auth_value = HeaderValue::from_str(&auth_header).map_err(|e| {
        error!("Authorization header not ASCII-safe: {e}");
        gateway_error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "BadSignatureHeader",
            "bad signature header",
            &request_id,
        )
    })?;

    req.headers_mut().insert(http::header::DATE, date_value);
    req.headers_mut()
        .insert(http::header::AUTHORIZATION, auth_value);
    Ok(())
}

/// Replace a leading `/my` path segment with `/{username}` in-place.
/// Paths that do not start with `/my/` (or equal `/my`) pass through
/// unchanged so the caller can address arbitrary account-scoped paths
/// directly (e.g. `/datacenters`, `/{other_account}/machines`).
fn rewrite_my_prefix(
    req: &mut axum::extract::Request,
    username: &str,
) -> Result<(), anyhow::Error> {
    let uri = req.uri().clone();
    let path = uri.path();
    let new_path_owned: String;
    let new_path = if path == "/my" {
        new_path_owned = format!("/{username}");
        new_path_owned.as_str()
    } else if let Some(rest) = path.strip_prefix("/my/") {
        new_path_owned = format!("/{username}/{rest}");
        new_path_owned.as_str()
    } else {
        return Ok(());
    };

    let new_pq = match uri.query() {
        Some(q) => PathAndQuery::try_from(format!("{new_path}?{q}"))?,
        None => PathAndQuery::try_from(new_path.to_string())?,
    };
    let mut parts = uri.into_parts();
    parts.path_and_query = Some(new_pq);
    *req.uri_mut() = hyper::Uri::from_parts(parts)?;
    Ok(())
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

    // Fire-and-forget: the spawned task outlives this handler and has no
    // meaningful way to propagate errors, so log them at the terminal edge.
    tokio::spawn(async move {
        drive_websocket_tunnel(client_on_upgrade, backend_on_upgrade)
            .await
            .unwrap_or_else(|e| warn!("websocket tunnel ended with error: {}", e));
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

/// Drive the bidirectional WebSocket tunnel to completion.
///
/// Waits for both the client-side and backend-side HTTP upgrades to
/// complete, then shovels bytes in both directions until either side
/// closes. Returns Ok with the transferred byte counts on a clean
/// close, or Err if the upgrade handshake or stream copy failed.
async fn drive_websocket_tunnel(
    client_on_upgrade: hyper::upgrade::OnUpgrade,
    backend_on_upgrade: hyper::upgrade::OnUpgrade,
) -> Result<(), anyhow::Error> {
    let (client_upgraded, backend_upgraded) =
        tokio::try_join!(client_on_upgrade, backend_on_upgrade)
            .context("websocket upgrade failed")?;

    // `Upgraded` implements hyper's Read/Write; TokioIo adapts that to
    // tokio's AsyncRead/AsyncWrite so copy_bidirectional can drive it.
    let mut client_io = TokioIo::new(client_upgraded);
    let mut backend_io = TokioIo::new(backend_upgraded);

    let (from_client, from_backend) =
        tokio::io::copy_bidirectional(&mut client_io, &mut backend_io).await?;

    info!(
        client_bytes = from_client,
        backend_bytes = from_backend,
        "websocket tunnel closed"
    );
    Ok(())
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

/// Build the shared proxy client, honoring the `tls_verify` config toggle.
///
/// `tls_verify` defaults to `true` (missing or explicit `true`). Setting it to
/// `false` disables certificate chain *and* hostname verification so the
/// gateway can reach COAL / dev DCs where CloudAPI uses a self-signed cert.
/// This is equivalent to `tls_verify = false` in user-portal's config.
async fn build_proxy_client(
    tls_verify: Option<bool>,
) -> Result<
    Client<hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>, Body>,
> {
    let verify = tls_verify.unwrap_or(true);
    let tls_config = if verify {
        let root_store = triton_tls::build_root_cert_store().await;
        rustls::ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth()
    } else {
        warn!(
            "TLS certificate verification DISABLED for upstream proxy \
             (tls_verify=false); safe only for COAL/dev"
        );
        rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoCertVerification))
            .with_no_client_auth()
    };

    let https = hyper_rustls::HttpsConnectorBuilder::new()
        .with_tls_config(tls_config)
        .https_or_http()
        .enable_http1()
        .build();

    Ok(Client::builder(TokioExecutor::new()).build(https))
}

/// `ServerCertVerifier` that accepts any certificate without checking.
///
/// Installed only when `tls_verify = false`. This mirrors reqwest's
/// `danger_accept_invalid_certs(true)` — the same behavior portal uses in
/// `triton_tls::build_http_client(insecure=true)`.
#[derive(Debug)]
struct NoCertVerification;

impl ServerCertVerifier for NoCertVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, RustlsError> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        // Advertise everything rustls can handle; we accept them all anyway.
        vec![
            SignatureScheme::RSA_PKCS1_SHA1,
            SignatureScheme::ECDSA_SHA1_Legacy,
            SignatureScheme::RSA_PKCS1_SHA256,
            SignatureScheme::ECDSA_NISTP256_SHA256,
            SignatureScheme::RSA_PKCS1_SHA384,
            SignatureScheme::ECDSA_NISTP384_SHA384,
            SignatureScheme::RSA_PKCS1_SHA512,
            SignatureScheme::ECDSA_NISTP521_SHA512,
            SignatureScheme::RSA_PSS_SHA256,
            SignatureScheme::RSA_PSS_SHA384,
            SignatureScheme::RSA_PSS_SHA512,
            SignatureScheme::ED25519,
            SignatureScheme::ED448,
        ]
    }
}
