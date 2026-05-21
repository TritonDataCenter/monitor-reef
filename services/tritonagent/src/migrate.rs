// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! On-CN live-migration listener + outbound dialer (LM-3).
//!
//! Lives at the same architectural layer as
//! [`crate::console`]: a small TLS WebSocket server bound to the CN
//! admin IP that tritond reaches by pinning the listener's SPKI
//! (exchanged at registration). The route is
//! `GET /migrate/{migration_id}` with a `?ticket=` HS256 JWT minted
//! by tritond via [`tritond_auth::MigrateTicketKey`].
//!
//! Unlike the console, the migration flow has *two* parties — source
//! and target both run a tritonagent. The target hosts this listener;
//! the source uses [`dial`] to open the outbound WebSocket and feed
//! it into [`tritond_vmm_migrate::OutboundMigration`]. Both sides
//! wrap the WebSocket in a [`Transport`] implementation that
//! plumbs binary frames into the state machine's
//! [`tritond_vmm_migrate::Message`] codec.
//!
//! ## Auth model
//!
//! tritond mints two tickets per migration when the migration saga
//! reaches the data-channel step:
//!
//! * outbound, given to the source agent's `dial` call.
//! * inbound, written into the target's `MigrationRecord` so the
//!   listener can recognise which migration an inbound dial
//!   fulfils.
//!
//! Both tickets bind `(source_cn, target_cn, vm_uuid, migration_id,
//! role)` and live ~10 minutes. The listener verifies the source's
//! presented ticket against its own [`MigrateTicketKey`] (different
//! key per CN, exchanged at registration), against the role
//! `Outbound`, and against the migration_id it knows is in flight.
//!
//! ## What's wired vs. deferred (LM-3 scope)
//!
//! LM-3 ships the listener, the dialer, the Transport adapter, and
//! the wiring to construct an `InboundMigration` / `OutboundMigration`.
//! The actual driving of those state machines — including the
//! `bhyve_ctl` calls, the saga callbacks for `pause_complete` /
//! `switch_complete`, and the FDB `MigrationRecord` lookups — lands
//! with LM-5 when the migration saga is the orchestrator. For now
//! the listener has a placeholder runner that closes the WebSocket
//! cleanly so end-to-end testing can still walk the auth path.

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use axum::Router;
use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum_server::tls_rustls::RustlsConfig;
use futures_util::StreamExt as _;
use serde::Deserialize;
use tokio::sync::Mutex as TokioMutex;
use tracing::{info, warn};
use tritond_auth::{MIGRATE_TICKET_KEY_BYTES, MigrateRole, MigrateTicketKey};
use tritond_vmm_migrate::zfs_stream::ZfsReceiver;
use tritond_vmm_migrate::{Message, Transport};
use uuid::Uuid;

use crate::console_creds::ConsoleTls;
use crate::zfs;

/// Default port the listener binds. Plan §D.3: 4568 picks a fresh
/// number rather than reusing legacy's 4567 so mixed-version
/// environments fail loudly instead of silently misrouting.
pub const DEFAULT_MIGRATE_LISTEN_PORT: u16 = 4568;

/// Configuration for [`serve`].
pub struct MigrateListenerConfig {
    /// Address to bind: the admin IPv4 plus the chosen migrate port.
    pub bind: SocketAddr,
    /// TLS material (self-signed; tritond pins the SPKI). Reuses the
    /// same shape as the console listener so the agent can mint one
    /// cert and serve both listeners off it.
    pub tls: ConsoleTls,
    /// Per-CN migrate-ticket key (HS256 secret).
    pub migrate_ticket_key: [u8; MIGRATE_TICKET_KEY_BYTES],
    /// This CN's server UUID — verifier checks the ticket's
    /// `target_cn` matches.
    pub server_uuid: Uuid,
}

/// Shared handler state.
struct MigrateState {
    migrate_ticket_key: [u8; MIGRATE_TICKET_KEY_BYTES],
    server_uuid: Uuid,
}

/// Query parameters on `GET /migrate/{migration_id}`.
#[derive(Debug, Deserialize)]
struct MigrateParams {
    /// The migrate ticket (HS256 JWT) minted by tritond with the
    /// outbound role. Source presents it; target listener verifies.
    ticket: String,
    /// Source CN uuid (claimed; the listener will refuse if it
    /// doesn't match the ticket's binding).
    source_cn: Uuid,
    /// VM uuid the migration moves (claimed; ditto).
    vm: Uuid,
}

/// Query parameters on `GET /migrate/{migration_id}/zfs`.
#[derive(Debug, Deserialize)]
struct MigrateZfsParams {
    /// Migrate ticket scoped to [`MigrateRole::ZfsSource`].
    ticket: String,
    /// Source CN uuid.
    source_cn: Uuid,
    /// VM uuid (used to bind the ticket).
    vm: Uuid,
    /// Local dataset name to `zfs recv` into (e.g.
    /// `zones/7c9a4f88-1ab2-4cd4-9b21-7e2c8f9a1b3d-disk0`). The
    /// migration saga supplies this; the listener doesn't
    /// re-derive it from `vm` because the dataset path layout
    /// depends on the storage profile / pool the saga chose.
    dataset: String,
}

/// Build the Axum router. Exposed for tests so they can serve it
/// over plain HTTP; production wraps it in TLS via [`serve`].
fn build_router(state: Arc<MigrateState>) -> Router {
    Router::new()
        // Memory channel — the LM-3 WebSocket the migration's
        // OutboundMigration state machine connects to.
        .route("/migrate/{migration_id}", get(migrate_ws))
        // ZFS channel — the LM-4 WebSocket the source agent's
        // ZfsSender connects to. Separate route + separate
        // ticket role (`MigrateRole::ZfsTarget`) so a leaked
        // memory-channel ticket can't be replayed against it.
        .route("/migrate/{migration_id}/zfs", get(migrate_zfs_ws))
        .with_state(state)
}

/// Run the TLS WebSocket migrate listener until the process exits.
pub async fn serve(cfg: MigrateListenerConfig) -> Result<()> {
    let tls_config = RustlsConfig::from_pem(cfg.tls.cert_pem.clone(), cfg.tls.key_pem.clone())
        .await
        .context("build rustls config for the migrate listener")?;

    let state = Arc::new(MigrateState {
        migrate_ticket_key: cfg.migrate_ticket_key,
        server_uuid: cfg.server_uuid,
    });
    let app = build_router(state);

    info!(bind = %cfg.bind, "migrate listener started");
    axum_server::bind_rustls(cfg.bind, tls_config)
        .serve(app.into_make_service())
        .await
        .with_context(|| format!("serve migrate listener on {}", cfg.bind))
}

/// `GET /migrate/{migration_id}` — verify the ticket, then upgrade
/// the WebSocket and hand the byte stream to the inbound state
/// machine via the [`AxumWsTransport`] adapter.
async fn migrate_ws(
    ws: WebSocketUpgrade,
    AxumPath(migration_id): AxumPath<Uuid>,
    Query(params): Query<MigrateParams>,
    State(state): State<Arc<MigrateState>>,
) -> Response {
    // The listener trusts the ticket as the source of truth; the
    // query params just disambiguate which migration the caller
    // intends. The ticket's bindings (source_cn / target_cn / vm /
    // migration_id / role) are checked against the verifier's
    // expectations below — a mismatch here means the source agent
    // is connecting with a ticket scoped to a different migration.
    let key = MigrateTicketKey::from_bytes(state.migrate_ticket_key);
    if let Err(e) = key.verify(
        &params.ticket,
        params.source_cn,
        state.server_uuid,
        params.vm,
        migration_id,
        MigrateRole::Outbound,
    ) {
        warn!(
            %migration_id, source_cn = %params.source_cn, vm = %params.vm,
            error = %e,
            "migrate: rejecting ticket",
        );
        return (StatusCode::UNAUTHORIZED, "invalid migrate ticket").into_response();
    }

    info!(
        %migration_id, source_cn = %params.source_cn, vm = %params.vm,
        "migrate: upgrading websocket",
    );

    ws.on_upgrade(move |socket| async move {
        // LM-3 placeholder: the LM-5 saga will own the
        // InboundMigration::run call here, including the bhyve_ctl
        // hooks and progress callbacks. For LM-3 we close cleanly
        // so the end-to-end auth path is testable + the listener
        // doesn't leak open sockets.
        let mut transport = AxumWsTransport::new(socket);
        if let Err(e) = transport.close().await {
            warn!(error = %e, "migrate: failed to close placeholder socket");
        }
        info!(%migration_id, "migrate: placeholder inbound session closed (LM-5 wires the saga driver)");
    })
}

/// `GET /migrate/{migration_id}/zfs` — verify the ticket
/// (role=`ZfsSource`), spawn `zfs recv` against the requested
/// dataset, and pipe the WebSocket binary frames straight into
/// the child's stdin via a `ZfsReceiver`. The handler waits for
/// the receiver to drain + the child to exit before closing.
async fn migrate_zfs_ws(
    ws: WebSocketUpgrade,
    AxumPath(migration_id): AxumPath<Uuid>,
    Query(params): Query<MigrateZfsParams>,
    State(state): State<Arc<MigrateState>>,
) -> Response {
    let key = MigrateTicketKey::from_bytes(state.migrate_ticket_key);
    if let Err(e) = key.verify(
        &params.ticket,
        params.source_cn,
        state.server_uuid,
        params.vm,
        migration_id,
        MigrateRole::ZfsSource,
    ) {
        warn!(
            %migration_id, source_cn = %params.source_cn, vm = %params.vm,
            dataset = %params.dataset, error = %e,
            "migrate-zfs: rejecting ticket",
        );
        return (StatusCode::UNAUTHORIZED, "invalid migrate-zfs ticket").into_response();
    }

    info!(
        %migration_id, source_cn = %params.source_cn, vm = %params.vm,
        dataset = %params.dataset,
        "migrate-zfs: upgrading websocket",
    );

    // Spawn `zfs recv` BEFORE upgrading the WebSocket so a spawn
    // failure (zfs missing, bad dataset name) surfaces as a 503
    // body, not as an opaque WebSocket close-with-no-explanation.
    let mut child = match zfs::spawn_recv(&params.dataset) {
        Ok(c) => c,
        Err(e) => {
            warn!(
                %migration_id, dataset = %params.dataset, error = %e,
                "migrate-zfs: spawn zfs recv failed",
            );
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                format!("zfs recv spawn failed: {e}"),
            )
                .into_response();
        }
    };
    let stdin = match child.stdin.take() {
        Some(s) => s,
        None => {
            warn!(%migration_id, "migrate-zfs: zfs recv child has no piped stdin");
            // Best-effort cleanup: kill the process so we don't leak.
            let _ = child.start_kill();
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "zfs recv child missing stdin",
            )
                .into_response();
        }
    };

    ws.on_upgrade(move |socket| async move {
        let transport = AxumWsTransport::new(socket);
        let receiver = ZfsReceiver::new(transport, stdin);
        match receiver.run().await {
            Ok(bytes) => {
                info!(
                    %migration_id, dataset = %params.dataset, bytes,
                    "migrate-zfs: stream complete, waiting for zfs recv to exit",
                );
            }
            Err(e) => {
                warn!(
                    %migration_id, dataset = %params.dataset, error = %e,
                    "migrate-zfs: receiver errored; killing zfs recv",
                );
                let _ = child.start_kill();
            }
        }
        match child.wait().await {
            Ok(status) if status.success() => {
                info!(%migration_id, dataset = %params.dataset, "migrate-zfs: zfs recv exited 0");
            }
            Ok(status) => {
                warn!(
                    %migration_id, dataset = %params.dataset, ?status,
                    "migrate-zfs: zfs recv exited non-zero",
                );
            }
            Err(e) => {
                warn!(
                    %migration_id, dataset = %params.dataset, error = %e,
                    "migrate-zfs: zfs recv child wait failed",
                );
            }
        }
    })
}

// ──────────────────────────────────────────────────────────────────
// Outbound dialer.
// ──────────────────────────────────────────────────────────────────

/// Parameters for [`dial`]: everything the source side needs to
/// open the WebSocket to a target tritonagent.
pub struct DialParams {
    /// `https://<target_admin_ip>:<port>` — the listener's base URL.
    /// The dialer converts this to `wss://...` for the upgrade.
    pub base_url: String,
    /// Migration record id (path parameter).
    pub migration_id: Uuid,
    /// Source CN uuid (query parameter; the target uses it to
    /// re-verify the ticket's binding).
    pub source_cn: Uuid,
    /// VM uuid (query parameter).
    pub vm_uuid: Uuid,
    /// HS256 JWT minted by tritond with `MigrateRole::Outbound`.
    pub ticket: String,
}

/// LM-3 stub for the source-side dialer. The full implementation
/// pulls in `tokio-tungstenite` + the SPKI pin (mirroring the
/// admin-backend's console proxy). Landing it here without the
/// LM-5 saga to invoke it would be code we can't exercise, so we
/// keep the signature stable and panic if anyone tries to use it
/// before LM-5 wires it up.
///
/// When LM-5 needs this, the body becomes:
///
/// 1. Build the `wss://...` URL from `params.base_url` + path +
///    query.
/// 2. Connect via `tokio_tungstenite::connect_async_tls_with_config`
///    with the pinned SPKI from the registration response.
/// 3. Wrap the `WebSocketStream` in [`TungsteniteTransport`].
/// 4. Hand the transport to
///    `tritond_vmm_migrate::OutboundMigration::new(...).run().await`.
pub async fn dial(_params: DialParams) -> io::Result<Box<dyn Transport>> {
    Err(io::Error::other(
        "migrate::dial not wired yet: LM-3 lands the listener, LM-5 wires the source side",
    ))
}

/// Parameters for [`dial_zfs`]: the source side opens the ZFS
/// WebSocket against the target's `/migrate/{id}/zfs` endpoint,
/// then a separate caller pipes the local `zfs send` stdout into
/// a `ZfsSender` wrapping the returned transport.
///
/// LM-4 ships the parameter shape; the body lands with LM-5 for
/// the same reason as [`dial`] — without a saga to invoke it,
/// the dialer has no live use case to test against. Splitting
/// into a stub keeps the signature stable so LM-5 doesn't need
/// to invent a new module.
pub struct DialZfsParams {
    /// `https://<target_admin_ip>:<port>`.
    pub base_url: String,
    /// Migration record id.
    pub migration_id: Uuid,
    /// Source CN uuid (binding claim).
    pub source_cn: Uuid,
    /// VM uuid (binding claim).
    pub vm_uuid: Uuid,
    /// Target dataset name (`zones/<inst>` etc.). Threaded
    /// through to the target's `?dataset=` query parameter so the
    /// listener can `zfs recv` it.
    pub target_dataset: String,
    /// HS256 JWT minted by tritond with `MigrateRole::ZfsSource`.
    pub ticket: String,
}

/// LM-4 stub for the source-side ZFS dialer; see [`dial`]'s doc
/// comment for the LM-5 wiring shape (the body is the same modulo
/// the route + the role).
pub async fn dial_zfs(_params: DialZfsParams) -> io::Result<Box<dyn Transport>> {
    Err(io::Error::other(
        "migrate::dial_zfs not wired yet: LM-4 lands the listener + Transport adapter, LM-5 wires the source dialer",
    ))
}

// ──────────────────────────────────────────────────────────────────
// Transport adapter: bridge axum's `WebSocket` to the migrate
// crate's `Transport` trait.
//
// We wrap the socket in a `tokio::sync::Mutex` because the
// `Transport::send` / `Transport::recv` calls each take `&mut self`
// but the migration state machine drives them sequentially —
// holding a mutex from `&mut self` is overkill; using one means
// `AxumWsTransport` can be moved into a spawn body that needs an
// owned value while keeping the borrow checker happy with the
// generic `Transport` shape.
// ──────────────────────────────────────────────────────────────────

/// `Transport` adapter for axum's WebSocket (target side: this is
/// what the listener hands the inbound state machine).
pub struct AxumWsTransport {
    socket: TokioMutex<WebSocket>,
}

impl AxumWsTransport {
    /// Wrap an upgraded axum WebSocket.
    #[must_use]
    pub fn new(socket: WebSocket) -> Self {
        Self {
            socket: TokioMutex::new(socket),
        }
    }
}

#[async_trait]
impl Transport for AxumWsTransport {
    async fn send(&mut self, msg: Message) -> io::Result<()> {
        let bytes = msg.encode();
        let mut g = self.socket.lock().await;
        g.send(WsMessage::Binary(bytes.into()))
            .await
            .map_err(|e| io::Error::new(io::ErrorKind::BrokenPipe, format!("ws send: {e}")))
    }

    async fn recv(&mut self) -> io::Result<Option<Message>> {
        let mut g = self.socket.lock().await;
        loop {
            match g.next().await {
                Some(Ok(WsMessage::Binary(b))) => {
                    let msg = Message::decode(&b).map_err(|e| {
                        io::Error::new(io::ErrorKind::InvalidData, format!("ws decode: {e}"))
                    })?;
                    return Ok(Some(msg));
                }
                Some(Ok(WsMessage::Ping(_) | WsMessage::Pong(_))) => {
                    // axum auto-pongs Pings; ignore both keepalive
                    // shapes here.
                    continue;
                }
                Some(Ok(WsMessage::Text(t))) => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("unexpected text frame: {t}"),
                    ));
                }
                Some(Ok(WsMessage::Close(_))) | None => return Ok(None),
                Some(Err(e)) => {
                    return Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        format!("ws recv: {e}"),
                    ));
                }
            }
        }
    }

    async fn close(&mut self) -> io::Result<()> {
        let mut g = self.socket.lock().await;
        g.send(WsMessage::Close(None))
            .await
            .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("ws close: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_listen_port_is_4568() {
        // Plan §D.3 pins this; the constant exists so a careless
        // hard-coded `4567` in a follow-up patch trips the
        // compile-time mismatch.
        assert_eq!(DEFAULT_MIGRATE_LISTEN_PORT, 4568);
    }

    #[test]
    fn dial_stub_returns_error_until_lm5() {
        // Documents the LM-3 deferral: until LM-5 wires this in,
        // calling dial must surface a clear error rather than
        // panicking or silently no-op'ing.
        let params = DialParams {
            base_url: "https://127.0.0.1:4568".to_string(),
            migration_id: Uuid::nil(),
            source_cn: Uuid::nil(),
            vm_uuid: Uuid::nil(),
            ticket: String::new(),
        };
        let result = tokio_test_block_on(dial(params));
        assert!(result.is_err());
    }

    #[test]
    fn dial_zfs_stub_returns_error_until_lm5() {
        // LM-4 ZFS dialer stub: same shape as `dial`.
        let params = DialZfsParams {
            base_url: "https://127.0.0.1:4568".to_string(),
            migration_id: Uuid::nil(),
            source_cn: Uuid::nil(),
            vm_uuid: Uuid::nil(),
            target_dataset: "zones/x".to_string(),
            ticket: String::new(),
        };
        let result = tokio_test_block_on(dial_zfs(params));
        assert!(result.is_err());
    }

    /// Tiny block-on for the dial-stub test. tokio_test is not in
    /// the workspace deps; using the existing tokio runtime is
    /// fine for a one-line assert.
    fn tokio_test_block_on<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("rt")
            .block_on(f)
    }
}
