// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! On-CN IMDSv2 listener -- the in-VM half of the layered metadata
//! plane (`IMDS_DESIGN.md` §3, §4).
//!
//! A guest reaches this listener by talking to `169.254.169.254` (or
//! `fd00:ec2::254`); the proteus kmod redirects the flow via
//! `RouteTarget::LocalImds` to a CN-unique address on a dedicated
//! proteus-owned internal datalink, SNAT'ing the guest source to a
//! per-port pseudo-address. We `accept()` here, recover the
//! originating port from the peer address, mint or verify an HS256
//! session token bound to `(port_id, instance_id)`, and serve the
//! realized view.
//!
//! ## Current state (scaffold)
//!
//! This module is the load-bearing scaffold for IM-4: the
//! [`ImdsListenerConfig`] shape, the `start()` entry point, and the
//! HTTP router skeleton with all the IMDSv2 paths wired to
//! `not_implemented` placeholders. The real handlers (token mint,
//! token-verified GET surface, `triton/guest/*` writeback) land in
//! follow-up commits as IM-4 progresses; each can plug in without
//! reshaping the listener.
//!
//! Why scaffold-first: keeps `cargo build` green at every commit; lets
//! tritonagent's main loop wire the listener in early so the
//! registration plumbing for the per-CN [`tritond_auth::ImdsTokenKey`]
//! has somewhere to land; and the route table is a single grep target
//! when the real handlers come in.

use std::net::SocketAddr;

use anyhow::{Context, Result};
use axum::{
    Router,
    extract::Path,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, put},
};
use tokio::net::TcpListener;
use tracing::{info, warn};
use tritond_auth::{IMDS_TOKEN_KEY_BYTES, ImdsTokenKey};

/// Per-CN configuration for the IMDS listener. Built by the agent's
/// startup path from CLI/env + the registration response (which
/// delivers the per-CN [`ImdsTokenKey`] bytes alongside the existing
/// console-ticket key).
pub struct ImdsListenerConfig {
    /// Address to bind. The proteus kmod redirects guest traffic for
    /// `169.254.169.254` / `fd00:ec2::254` to this socket (on a
    /// dedicated proteus-owned internal datalink, not the CN admin IP
    /// -- see `IMDS_DESIGN.md` §2.1). The host-anycast IPs are added
    /// to the same listener so v6-only and routed-metadata guests hit
    /// the same code path.
    pub bind: SocketAddr,
    /// Per-CN HS256 key for IMDSv2 session tokens. Persisted by
    /// tritond against the CN record and re-delivered on every
    /// registration so a CN reboot doesn't invalidate live tokens.
    pub token_key_bytes: [u8; IMDS_TOKEN_KEY_BYTES],
}

/// Build the axum router with every IMDSv2 path wired to a
/// `not_implemented` placeholder. Real handlers replace these in
/// follow-up commits; the route shape stays fixed so anything
/// upstream (the route table, the test harness, the tracing
/// instrumentation) only needs to be set up once.
fn router(_state: ImdsState) -> Router {
    Router::new()
        // IMDSv2 token mint -- the only un-token-gated endpoint.
        .route("/latest/api/token", put(not_implemented))
        // AWS-compatible computed surface.
        .route("/latest/meta-data", get(not_implemented))
        .route("/latest/meta-data/{*key}", get(not_implemented))
        .route("/latest/user-data", get(not_implemented))
        .route("/latest/dynamic", get(not_implemented))
        .route("/latest/dynamic/{*key}", get(not_implemented))
        // Triton-native surface (stored + computed + the realized
        // explainability view).
        .route("/triton/{tree}/{*key}", get(not_implemented))
        .route("/triton/dynamic/realized", get(not_implemented))
        // Guest writeback (only `triton/guest/*` is ever accepted;
        // see `IMDS_DESIGN.md` §1.3 / §5).
        .route(
            "/triton/guest/{*key}",
            put(not_implemented).delete(not_implemented),
        )
}

/// Shared listener state passed to every handler. Empty for now;
/// the real impl carries the [`ImdsTokenKey`], the realized-view
/// cache, the per-port binding-table snapshot, and the rate
/// limiter.
#[derive(Clone)]
struct ImdsState {
    // ImdsTokenKey isn't Clone (zeroize::Drop) so we'll wrap it in
    // Arc when the real impl lands. Placeholder for now.
}

/// Placeholder handler. Returns 501 Not Implemented with a body
/// pointing at the design doc; replaced piecewise as the IM-4
/// commits land. Accepts a wildcard path so it matches every
/// scaffolded route.
async fn not_implemented(_path: Option<Path<String>>) -> impl IntoResponse {
    (
        StatusCode::NOT_IMPLEMENTED,
        "IMDS handler not yet implemented (IM-4 in progress; see IMDS_DESIGN.md)\n",
    )
}

/// Spawn the IMDS listener. Returns when the bound socket is ready;
/// the serving future runs detached. Errors during bind are
/// surfaced to the caller; per-connection errors are logged and
/// otherwise swallowed (one bad guest must not take the listener
/// down).
pub async fn start(cfg: ImdsListenerConfig) -> Result<()> {
    // Build the (currently empty) state. The token key isn't
    // exercised yet -- but constructing it here means a malformed
    // key bytes is caught at startup, not on first request.
    let _key = ImdsTokenKey::from_bytes(cfg.token_key_bytes);
    let state = ImdsState {};
    let app = router(state);
    let listener = TcpListener::bind(cfg.bind)
        .await
        .with_context(|| format!("imds: bind {}", cfg.bind))?;
    info!(bind = %cfg.bind, "imds: listening (scaffold; handlers not yet implemented)");
    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            warn!(error = %e, "imds: serve loop exited");
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Scaffold smoke: the router builds without panicking + the
    /// `ImdsState` type round-trips through `Clone` (so the
    /// future-state expansion doesn't accidentally drop the trait).
    /// Real per-route assertions land alongside the real handlers in
    /// the follow-up IM-4 commits.
    #[test]
    fn router_builds() {
        let _: Router = router(ImdsState {});
        let s = ImdsState {};
        let _ = s.clone();
    }

    /// `ImdsTokenKey::from_bytes` accepts the bytes we'd hand it at
    /// startup -- guarding against a future refactor that breaks the
    /// constructor signature this scaffold depends on.
    #[test]
    fn token_key_constructs_from_bytes() {
        let _ = ImdsTokenKey::from_bytes([0u8; IMDS_TOKEN_KEY_BYTES]);
    }
}
