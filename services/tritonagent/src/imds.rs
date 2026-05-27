// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! On-CN IMDSv2 listener. The proteus kmod redirects the magic
//! address to this listener and SNATs the guest to a per-port
//! pseudo-address; the originating port is recovered via the binding
//! table — never from anything the guest sends. Tokens are HS256
//! bound to `(port_id, instance_id)`.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::{
    Extension, Router,
    extract::{ConnectInfo, Path, Request, State},
    http::{HeaderMap, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, put},
};
use tokio::net::TcpListener;
use tracing::{debug, info, warn};
use tritond_auth::{
    IMDS_TOKEN_KEY_BYTES, IMDS_TOKEN_TTL_DEFAULT_SECS, IMDS_TOKEN_TTL_MAX_SECS,
    IMDS_TOKEN_TTL_MIN_SECS, ImdsTokenKey,
};

use crate::imds_bindings::{ImdsBindingTable, ResolvedBinding};
use crate::imds_data::{RealizedDataSource, RealizedFetchError, RealizedViewCache};
use crate::imds_ratelimit::PerInstanceRateLimiter;

/// HTTP header carrying the requested session-token TTL (AWS-spec).
const TOKEN_TTL_HEADER: &str = "x-aws-ec2-metadata-token-ttl-seconds";

/// HTTP header carrying the IMDSv2 session token (AWS-spec).
const TOKEN_HEADER: &str = "x-aws-ec2-metadata-token";

/// Per-CN configuration for the IMDS listener.
pub struct ImdsListenerConfig {
    /// Address to bind. See module docs.
    pub bind: SocketAddr,
    /// Per-CN HS256 key for IMDSv2 session tokens. Persisted by
    /// tritond against the CN record and re-delivered on every
    /// registration so a CN reboot doesn't invalidate live tokens.
    pub token_key_bytes: [u8; IMDS_TOKEN_KEY_BYTES],
    /// The agent's reverse-lookup table mapping `pseudo_src -> (port_id,
    /// instance_id)`. Populated by the proteus apply path (a follow-up
    /// commit hooks `proteus::apply_blueprint`). Cheaply cloneable;
    /// the listener task and the apply path share the same Arc.
    pub bindings: ImdsBindingTable,
    /// Realized-view data source the GET handlers read through. The
    /// daemon wraps this in a `RealizedViewCache` so the hot path
    /// doesn't pay a tritond round trip per request; cache misses
    /// fetch through this source. See `IMDS_DESIGN.md` §3 -- the
    /// swappable data-source trait whose default impl is the
    /// tritond `/v2/instances/{id}/realized-meta` client (later, a
    /// direct restricted FDB read).
    pub realized_source: Arc<dyn RealizedDataSource>,
    /// Tritond client used by the PUT writeback path
    /// (`/triton/guest/{*key}`). The realized-source trait carries
    /// reads; writes go directly through the typed client so the
    /// agent → tritond contract is enforced at the type level.
    pub tritond_client: Arc<tritond_client::Client>,
}

/// Shared listener state passed to every handler.
#[derive(Clone)]
struct ImdsState {
    token_key: Arc<ImdsTokenKey>,
    bindings: ImdsBindingTable,
    realized: RealizedViewCache,
    rate_limit: PerInstanceRateLimiter,
    tritond_client: Arc<tritond_client::Client>,
}

fn router(state: ImdsState) -> Router {
    // Mint is the only un-token-gated endpoint (the design ships
    // IMDSv2-only -- the PUT obtains the token every other request
    // then needs to carry).
    let mint = Router::new()
        .route("/latest/api/token", put(put_token))
        .with_state(state.clone());

    // Everything else is gated by `require_imds_token` which extracts
    // the token header, resolves the peer via the binding table, and
    // verifies the token's bound `(port_id, instance_id)` matches the
    // request's derived identity. On any failure -> 401 (the variants
    // are deliberately collapsed so the verifier isn't an oracle).
    let gated = Router::new()
        // AWS-compatible computed surface. Directory listings
        // (`/latest/meta-data` etc. without a trailing key) still
        // 501 until the directory-listing helper lands.
        .route("/latest/meta-data", get(aws_meta_data_root))
        .route("/latest/meta-data/{*key}", get(aws_meta_data_get))
        .route("/latest/user-data", get(aws_user_data_get))
        .route("/latest/dynamic", get(aws_dynamic_root))
        .route("/latest/dynamic/{*key}", get(aws_dynamic_get))
        // Triton-native surface.
        .route("/triton/dynamic/realized", get(triton_realized_get))
        .route("/triton/{tree}", get(triton_tree_root))
        .route("/triton/{tree}/{*key}", get(triton_get))
        // Guest writeback (only `triton/guest/*` ever accepted; the
        // PUT/DELETE-side authorisation -- writeback enabled? key
        // pinned RO? value within caps? -- lives in the handler).
        // GET must be re-attached here because this route is more
        // specific than the `/triton/{tree}/{*key}` rule above; axum
        // routes GETs against it and returns 405 unless GET is
        // explicitly listed. The `/triton/guest/{*key}` shape only
        // has one path param (`guest` is a literal), so it gets its
        // own handler — `triton_get`'s `Path<(String, String)>`
        // extractor would fail to bind here.
        .route(
            "/triton/guest/{*key}",
            get(triton_guest_get)
                .put(triton_guest_put)
                .delete(not_implemented),
        )
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_imds_token,
        ))
        .with_state(state);

    mint.merge(gated)
}

/// Placeholder for handlers that haven't landed yet. Returns 501
/// with a pointer to the design doc. Replaced piecewise.
async fn not_implemented() -> impl IntoResponse {
    (
        StatusCode::NOT_IMPLEMENTED,
        "IMDS handler not yet implemented (IM-4 in progress; see IMDS_DESIGN.md)
",
    )
}

/// Token-verification middleware for every non-mint endpoint. Pulls
/// the `X-aws-ec2-metadata-token` header, recovers the peer's
/// `(port_id, instance_id)` from the binding table, and verifies the
/// token against that pair. On any failure -> 401 (collapsed so the
/// verifier doesn't double as an oracle distinguishing "wrong token"
/// from "wrong scope"). On success, the resolved binding is stashed
/// in request extensions so handlers can read it without re-looking
/// it up.
async fn require_imds_token(
    State(state): State<ImdsState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    mut req: Request,
    next: Next,
) -> Response {
    // Look up the peer first -- a request from an unknown virtual
    // wire is 401 regardless of any token it might carry. We collapse
    // "unknown peer" into the same status as "bad token" so a probe
    // can't tell the difference.
    let Some(binding) = state.bindings.lookup(peer.ip()) else {
        debug!(peer = %peer, "imds: unknown peer");
        return (
            StatusCode::UNAUTHORIZED,
            "invalid IMDS token
",
        )
            .into_response();
    };
    let Some(token) = headers.get(TOKEN_HEADER).and_then(|v| v.to_str().ok()) else {
        return (
            StatusCode::UNAUTHORIZED,
            "missing IMDS token
",
        )
            .into_response();
    };
    if state
        .token_key
        .verify(token, binding.port_id, binding.instance_id)
        .is_err()
    {
        return (
            StatusCode::UNAUTHORIZED,
            "invalid IMDS token
",
        )
            .into_response();
    }
    if !state.rate_limit.check(binding.instance_id) {
        debug!(instance_id = %binding.instance_id, "imds: rate limited");
        return (
            StatusCode::TOO_MANY_REQUESTS,
            "rate limit exceeded
",
        )
            .into_response();
    }
    req.extensions_mut().insert(binding);
    next.run(req).await
}

/// `PUT /latest/api/token` -- mint an IMDSv2 session token bound to
/// the connection's derived `(port_id, instance_id)`. See
/// `IMDS_DESIGN.md` §3 (the `PUT /token` flow + the "no IMDSv1, ever"
/// rule).
///
/// Behaviour:
///
/// 1. The peer address comes from axum's `ConnectInfo` (set by
///    `into_make_service_with_connect_info` -- wired by `start()`).
/// 2. The agent's binding table resolves the peer to
///    `(port_id, instance_id)`. An unknown peer -> 403 (the design's
///    "unknown virtual wire" rule).
/// 3. The `X-aws-ec2-metadata-token-ttl-seconds` header is required
///    (AWS-spec). Missing -> 400. Out-of-range values get clamped to
///    `[IMDS_TOKEN_TTL_MIN_SECS, IMDS_TOKEN_TTL_MAX_SECS]` rather than
///    rejected, mirroring AWS leniency on the upper bound.
/// 4. We mint with the per-CN `ImdsTokenKey` and return the token
///    bytes verbatim. The response IP TTL clamp (the design's SSRF
///    relay mitigation; §3, §6) lands in a follow-up commit -- it
///    needs `setsockopt` on the per-response side which axum doesn't
///    expose directly. The token itself is already useless on
///    another VM because `ImdsTokenKey::verify` re-checks the bound
///    `(port_id, instance_id)` against the request's derived
///    identity.
async fn put_token(
    State(state): State<ImdsState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let Some(binding) = state.bindings.lookup(peer.ip()) else {
        debug!(peer = %peer, "imds: PUT /token from unknown peer");
        return (
            StatusCode::FORBIDDEN,
            "unknown peer
",
        )
            .into_response();
    };
    if !imds_enabled(&state, binding.instance_id).await {
        return (StatusCode::NOT_FOUND, "imds disabled\n").into_response();
    }
    let ttl = match parse_ttl_header(&headers) {
        Ok(t) => t,
        Err(msg) => {
            return (StatusCode::BAD_REQUEST, msg).into_response();
        }
    };
    let ResolvedBinding {
        port_id,
        instance_id,
    } = binding;
    match state.token_key.mint(port_id, instance_id, ttl) {
        Ok(token) => {
            debug!(
                instance_id = %instance_id,
                port_id = %port_id,
                ttl = ttl,
                "imds: PUT /token minted"
            );
            (StatusCode::OK, token).into_response()
        }
        Err(e) => {
            warn!(error = ?e, "imds: PUT /token mint failed (unexpected)");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "mint failed
",
            )
                .into_response()
        }
    }
}

/// Parse + clamp the `X-aws-ec2-metadata-token-ttl-seconds` header.
/// `Err(_)` -> 400 body; `Ok(_)` -> a TTL inside the allowed range.
fn parse_ttl_header(headers: &HeaderMap) -> Result<i64, &'static str> {
    let raw = match headers.get(TOKEN_TTL_HEADER) {
        Some(v) => v,
        None => {
            return Err("missing X-aws-ec2-metadata-token-ttl-seconds header
");
        }
    };
    let s = raw.to_str().map_err(|_| {
        "non-ascii X-aws-ec2-metadata-token-ttl-seconds header
"
    })?;
    let n: i64 = s.parse().map_err(|_| {
        "X-aws-ec2-metadata-token-ttl-seconds must be an integer
"
    })?;
    if n < IMDS_TOKEN_TTL_MIN_SECS {
        return Ok(IMDS_TOKEN_TTL_MIN_SECS);
    }
    if n > IMDS_TOKEN_TTL_MAX_SECS {
        return Ok(IMDS_TOKEN_TTL_MAX_SECS);
    }
    Ok(n)
}

#[allow(dead_code)]
fn _ttl_default_marker() -> i64 {
    IMDS_TOKEN_TTL_DEFAULT_SECS
}

/// Spawn the IMDS listener. Returns once the socket is bound; the
/// serving future runs detached.
pub async fn start(cfg: ImdsListenerConfig) -> Result<()> {
    let state = ImdsState {
        token_key: Arc::new(ImdsTokenKey::from_bytes(cfg.token_key_bytes)),
        bindings: cfg.bindings,
        realized: RealizedViewCache::new(cfg.realized_source),
        rate_limit: PerInstanceRateLimiter::new(),
        tritond_client: cfg.tritond_client,
    };
    let app = router(state);
    let listener = TcpListener::bind(cfg.bind)
        .await
        .with_context(|| format!("imds: bind {}", cfg.bind))?;
    info!(bind = %cfg.bind, "imds: listening");
    tokio::spawn(async move {
        let svc = app.into_make_service_with_connect_info::<SocketAddr>();
        if let Err(e) = axum::serve(listener, svc).await {
            warn!(error = %e, "imds: serve loop exited");
        }
    });
    Ok(())
}

/// Fetch the realized view for the request's instance (resolved by
/// the middleware) and serve the entry at `full_key`, if any.
/// `full_key` is the *storage-namespace* key (e.g. `meta-data/
/// instance-id`, `config/ntp-servers`), **not** the URL path -- the
/// caller builds it from the path-param.
/// Read the realized `triton/config/imds/enabled` flag for this
/// instance. Defaults to `true` if absent / not a boolean / fetch
/// failed -- a tritond hiccup must not lock guests out of IMDS, only
/// an explicit operator decision. See `IMDS_DESIGN.md` §3.
async fn imds_enabled(state: &ImdsState, instance_id: uuid::Uuid) -> bool {
    let entries = match state.realized.get(instance_id).await {
        Ok(v) => v,
        Err(_) => return true, // tritond down -> stay open, not closed
    };
    entries
        .iter()
        .find(|e| e.key == "config/imds/enabled")
        .and_then(|e| e.value.value.as_bool())
        .unwrap_or(true)
}

async fn serve_key_for_binding(
    state: &ImdsState,
    binding: ResolvedBinding,
    full_key: &str,
) -> Response {
    if !imds_enabled(state, binding.instance_id).await {
        return (StatusCode::NOT_FOUND, "imds disabled\n").into_response();
    }
    let entries = match state.realized.get(binding.instance_id).await {
        Ok(v) => v,
        Err(RealizedFetchError::NotFound) => {
            return (StatusCode::NOT_FOUND, "not found\n").into_response();
        }
        Err(RealizedFetchError::Backend(e)) => {
            warn!(error = %e, "imds: realized view unavailable");
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "realized view unavailable\n",
            )
                .into_response();
        }
    };
    let Some(entry) = entries
        .iter()
        .find(|e| e.value.guest_visible && e.key == full_key)
    else {
        // No exact match -- but the URL might be addressing a
        // directory rather than a leaf. AWS IMDS convention: a
        // trailing-slash URL (or any prefix that has children)
        // lists those children. Try the listing fallback before
        // returning 404.
        let dir_prefix = if full_key.ends_with('/') {
            full_key.to_string()
        } else {
            format!("{full_key}/")
        };
        let listing = list_children(&entries, &dir_prefix);
        if !listing.is_empty() {
            return (StatusCode::OK, listing).into_response();
        }
        return (StatusCode::NOT_FOUND, "not found\n").into_response();
    };
    // Strings serialise as themselves (no surrounding quotes); every
    // other JSON shape serialises with `application/json`. Matches
    // AWS IMDS conventions -- a `local-ipv4` is `10.0.0.42\n` not
    // `"10.0.0.42"`.
    match &entry.value.value {
        serde_json::Value::String(s) => (StatusCode::OK, s.clone()).into_response(),
        v => {
            let body = serde_json::to_vec(v).unwrap_or_default();
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "application/json")],
                body,
            )
                .into_response()
        }
    }
}

async fn aws_meta_data_get(
    State(state): State<ImdsState>,
    Extension(binding): Extension<ResolvedBinding>,
    Path(key): Path<String>,
) -> Response {
    serve_key_for_binding(&state, binding, &format!("meta-data/{key}")).await
}

async fn aws_user_data_get(
    State(state): State<ImdsState>,
    Extension(binding): Extension<ResolvedBinding>,
) -> Response {
    serve_key_for_binding(&state, binding, "user-data").await
}

async fn aws_dynamic_get(
    State(state): State<ImdsState>,
    Extension(binding): Extension<ResolvedBinding>,
    Path(key): Path<String>,
) -> Response {
    // IMDS_DESIGN.md §3: `/latest/dynamic/iam/...` -> 404 until
    // identityd (IM-7). Everything else under `/latest/dynamic/` we
    // serve from the realized view under the `dynamic/` prefix.
    if key.starts_with("iam/") || key == "iam" {
        return (StatusCode::NOT_FOUND, "not found\n").into_response();
    }
    serve_key_for_binding(&state, binding, &format!("dynamic/{key}")).await
}

async fn triton_get(
    State(state): State<ImdsState>,
    Extension(binding): Extension<ResolvedBinding>,
    Path((tree, key)): Path<(String, String)>,
) -> Response {
    serve_key_for_binding(&state, binding, &format!("{tree}/{key}")).await
}

/// Variant of [`triton_get`] for the `/triton/guest/{*key}` route,
/// which has a single path param because `guest` is a literal.
async fn triton_guest_get(
    State(state): State<ImdsState>,
    Extension(binding): Extension<ResolvedBinding>,
    Path(key): Path<String>,
) -> Response {
    serve_key_for_binding(&state, binding, &format!("guest/{key}")).await
}

/// `PUT /triton/guest/{*key}` — guest-VM-initiated writeback. The
/// request body is parsed as JSON; if it isn't valid JSON, the raw
/// UTF-8 body is wrapped as a JSON string (so a guest can `curl -d
/// hello-world` and get `"hello-world"` stored). The full key
/// written is `guest/{*key}`.
///
/// Server-side rules live in tritond
/// (`/v2/agent/instances/{id}/meta`): the key must already exist in
/// the realized view AND the existing entry must be
/// `guest_writable: true` AND the write lands at whichever scope
/// the entry already lives in (silo / tenant / project / instance).
/// No create path; flags are operator-only.
async fn triton_guest_put(
    State(state): State<ImdsState>,
    Extension(binding): Extension<ResolvedBinding>,
    Path(key): Path<String>,
    body: axum::body::Bytes,
) -> Response {
    let full_key = format!("guest/{key}");
    let value: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => match std::str::from_utf8(&body) {
            Ok(s) => serde_json::Value::String(s.to_string()),
            Err(_) => {
                return (StatusCode::BAD_REQUEST, "body must be UTF-8\n").into_response();
            }
        },
    };

    let req = tritond_client::types::SetGuestMetaRequest { value };
    match state
        .tritond_client
        .agent_set_instance_guest_meta()
        .instance_id(binding.instance_id)
        .key(&full_key)
        .body(req)
        .send()
        .await
    {
        Ok(resp) => {
            // Bust the cached realized view for this instance so the
            // very next GET refetches and sees the write. Without
            // this, the read-after-write window is the realized-view
            // cache's TTL (30s default).
            state.realized.invalidate(binding.instance_id).await;
            let body = serde_json::to_vec(&resp.into_inner()).unwrap_or_else(|_| b"{}".to_vec());
            (
                StatusCode::OK,
                [(axum::http::header::CONTENT_TYPE, "application/json")],
                body,
            )
                .into_response()
        }
        Err(e) => {
            let msg = e.to_string();
            let lower = msg.to_ascii_lowercase();
            // Coarse status mapping — progenitor's Error doesn't
            // expose a stable status-code variant, so we string-match
            // the Display form (matches what `imds_data.rs` does for
            // the GET path).
            let status = if lower.contains("403") || lower.contains("forbidden") {
                StatusCode::FORBIDDEN
            } else if lower.contains("404") || lower.contains("not found") {
                StatusCode::NOT_FOUND
            } else if lower.contains("400") || lower.contains("bad request") {
                StatusCode::BAD_REQUEST
            } else {
                tracing::warn!(error = %msg, "imds: agent writeback to tritond failed");
                StatusCode::BAD_GATEWAY
            };
            (status, format!("{msg}\n")).into_response()
        }
    }
}

/// `GET /triton/dynamic/realized` -- the explainability view: the
/// guest-visible subset of the realized merge, each leaf carrying
/// its provenance scope. Returns `application/json` as
/// `{ "<key>": { "value": <v>, "from": "<scope>" }, ... }`.
async fn triton_tree_root(
    State(state): State<ImdsState>,
    Extension(binding): Extension<ResolvedBinding>,
    Path(tree): Path<String>,
) -> Response {
    list_for_prefix(&state, binding, &format!("{tree}/")).await
}

async fn triton_realized_get(
    State(state): State<ImdsState>,
    Extension(binding): Extension<ResolvedBinding>,
) -> Response {
    if !imds_enabled(&state, binding.instance_id).await {
        return (StatusCode::NOT_FOUND, "imds disabled\n").into_response();
    }
    let entries = match state.realized.get(binding.instance_id).await {
        Ok(v) => v,
        Err(RealizedFetchError::NotFound) => {
            return (StatusCode::NOT_FOUND, "not found\n").into_response();
        }
        Err(RealizedFetchError::Backend(e)) => {
            warn!(error = %e, "imds: realized view unavailable");
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "realized view unavailable\n",
            )
                .into_response();
        }
    };
    let mut out = serde_json::Map::new();
    for e in entries.iter().filter(|e| e.value.guest_visible) {
        let provenance = match e.from {
            tritond_client::types::MetaProvenance::Silo => "silo",
            tritond_client::types::MetaProvenance::Tenant => "tenant",
            tritond_client::types::MetaProvenance::Project => "project",
            tritond_client::types::MetaProvenance::Instance => "instance",
            tritond_client::types::MetaProvenance::System => "system",
        };
        out.insert(
            e.key.clone(),
            serde_json::json!({ "value": e.value.value, "from": provenance }),
        );
    }
    let body = serde_json::to_vec(&serde_json::Value::Object(out)).unwrap_or_default();
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        body,
    )
        .into_response()
}

/// Build the AWS-style directory listing for `prefix` over the
/// realized view: every immediate child segment, with a trailing
/// `/` on segments that themselves have grand-children. Only
/// `guest_visible` entries contribute. Sorted (AWS doesn't
/// guarantee an order, but a stable order is friendlier for the
/// human eye).
fn list_children(entries: &[tritond_client::types::RealizedMetaEntry], prefix: &str) -> String {
    use std::collections::BTreeSet;
    let mut names: BTreeSet<String> = BTreeSet::new();
    for e in entries.iter().filter(|e| e.value.guest_visible) {
        let Some(rest) = e.key.strip_prefix(prefix) else {
            continue;
        };
        if rest.is_empty() {
            continue;
        }
        let segment = match rest.find('/') {
            Some(i) => format!("{}/", &rest[..i]),
            None => rest.to_string(),
        };
        names.insert(segment);
    }
    let mut body = names.into_iter().collect::<Vec<_>>().join("\n");
    if !body.is_empty() {
        body.push('\n');
    }
    body
}

async fn list_for_prefix(state: &ImdsState, binding: ResolvedBinding, prefix: &str) -> Response {
    if !imds_enabled(state, binding.instance_id).await {
        return (StatusCode::NOT_FOUND, "imds disabled\n").into_response();
    }
    let entries = match state.realized.get(binding.instance_id).await {
        Ok(v) => v,
        Err(RealizedFetchError::NotFound) => {
            return (StatusCode::NOT_FOUND, "not found\n").into_response();
        }
        Err(RealizedFetchError::Backend(e)) => {
            warn!(error = %e, "imds: realized view unavailable");
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "realized view unavailable\n",
            )
                .into_response();
        }
    };
    let body = list_children(&entries, prefix);
    if body.is_empty() {
        return (StatusCode::NOT_FOUND, "not found\n").into_response();
    }
    (StatusCode::OK, body).into_response()
}

async fn aws_meta_data_root(
    State(state): State<ImdsState>,
    Extension(binding): Extension<ResolvedBinding>,
) -> Response {
    list_for_prefix(&state, binding, "meta-data/").await
}

async fn aws_dynamic_root(
    State(state): State<ImdsState>,
    Extension(binding): Extension<ResolvedBinding>,
) -> Response {
    list_for_prefix(&state, binding, "dynamic/").await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    fn fixed_state() -> ImdsState {
        // A static (empty) data source keeps the unit tests
        // hermetic -- no tritond, no network. Real wiring lives in
        // tritonagent's main loop.
        struct EmptySource;
        #[async_trait::async_trait]
        impl RealizedDataSource for EmptySource {
            async fn get(
                &self,
                _: uuid::Uuid,
            ) -> Result<
                Vec<tritond_client::types::RealizedMetaEntry>,
                crate::imds_data::RealizedFetchError,
            > {
                Ok(vec![])
            }
        }
        ImdsState {
            token_key: Arc::new(ImdsTokenKey::from_bytes([0u8; IMDS_TOKEN_KEY_BYTES])),
            bindings: ImdsBindingTable::new(),
            realized: RealizedViewCache::new(Arc::new(EmptySource)),
            rate_limit: PerInstanceRateLimiter::new(),
            // Bogus base URL — the test router never PUTs, and a
            // PUT lookup that did fire would error out at send-time
            // rather than reaching a real server. Keeps the unit
            // test hermetic.
            tritond_client: Arc::new(tritond_client::Client::new("http://127.0.0.1:0")),
        }
    }

    #[test]
    fn router_builds() {
        let _: Router = router(fixed_state());
    }

    #[test]
    fn ttl_header_required() {
        let h = HeaderMap::new();
        assert!(parse_ttl_header(&h).is_err());
    }

    #[test]
    fn ttl_clamps_low_and_high() {
        let mk = |v: &str| {
            let mut h = HeaderMap::new();
            h.insert(TOKEN_TTL_HEADER, v.parse().unwrap());
            h
        };
        assert_eq!(parse_ttl_header(&mk("0")).unwrap(), IMDS_TOKEN_TTL_MIN_SECS);
        assert_eq!(
            parse_ttl_header(&mk("999999")).unwrap(),
            IMDS_TOKEN_TTL_MAX_SECS
        );
        assert_eq!(parse_ttl_header(&mk("300")).unwrap(), 300);
    }

    #[test]
    fn ttl_rejects_garbage() {
        let mut h = HeaderMap::new();
        h.insert(TOKEN_TTL_HEADER, "abc".parse().unwrap());
        assert!(parse_ttl_header(&h).is_err());
    }

    #[tokio::test]
    async fn mint_then_verify_round_trip() {
        // Build a state with a real binding, mint a token via the
        // handler's path, then verify with the same key + bound IDs.
        let bindings = ImdsBindingTable::new();
        let pseudo = IpAddr::V4(Ipv4Addr::new(127, 1, 0, 5));
        let port = uuid::Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();
        let instance = uuid::Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap();
        bindings.insert(pseudo, port, instance);

        let key = ImdsTokenKey::from_bytes([42u8; IMDS_TOKEN_KEY_BYTES]);
        let token = key.mint(port, instance, 300).unwrap();
        assert!(key.verify(&token, port, instance).is_ok());

        let lookup = bindings.lookup(pseudo).expect("registered");
        assert_eq!(lookup.port_id, port);
        assert_eq!(lookup.instance_id, instance);
    }

    /// `list_children` over a small mixed view: sorted segments
    /// with trailing `/` for non-leaf children, drops
    /// `guest_visible == false` entries, returns the empty string
    /// when nothing matches.
    #[test]
    fn list_children_emits_aws_directory_shape() {
        use tritond_client::types::{MetaProvenance, MetaValue, RealizedMetaEntry};
        fn entry(key: &str, visible: bool) -> RealizedMetaEntry {
            RealizedMetaEntry {
                key: key.to_string(),
                from: MetaProvenance::System,
                value: MetaValue {
                    value: serde_json::json!("x"),
                    guest_visible: visible,
                    guest_writable: false,
                    updated_by: "test".to_string(),
                    updated_at: chrono::Utc::now(),
                },
            }
        }
        let v = vec![
            entry("meta-data/instance-id", true),
            entry("meta-data/local-ipv4", true),
            entry("meta-data/network/interfaces/macs/02:00/vpc-id", true),
            entry("meta-data/secret", false),   // dropped
            entry("triton/system/brand", true), // outside the prefix
        ];
        let listing = list_children(&v, "meta-data/");
        assert_eq!(listing, "instance-id\nlocal-ipv4\nnetwork/\n");
        let any = list_children(&v, "");
        assert!(any.contains("meta-data/"));
        assert!(any.contains("triton/"));
        let nope = list_children(&v, "bogus/");
        assert!(nope.is_empty());
    }
}
