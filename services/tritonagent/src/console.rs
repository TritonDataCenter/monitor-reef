// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! On-CN serial / VNC console listener.
//!
//! tritond proxies a browser's console session to here as the last hop:
//! `browser → admin-backend → tritond → tritonagent → guest console`.
//! This module runs a small **TLS WebSocket server bound to the CN
//! admin IP**; tritond pins the listener's TLS leaf-cert SPKI (exchanged
//! at registration) so a hijacked admin IP cannot MITM the byte stream.
//!
//! The single route is `GET /console/{vm_uuid}` (WebSocket upgrade) with
//! query params `kind` (`serial` / `vnc`), `brand` (the zone brand), and
//! `ticket` (a short-lived HS256 JWT minted by tritond with the per-CN
//! [`ConsoleTicketKey`]). The handler:
//!
//! 1. Verifies the ticket against `(server_uuid, vm_uuid, kind)` — on
//!    any failure, returns 401 without upgrading the socket.
//! 2. Looks the zone up via `zoneadm` (404 if absent, 409 if not
//!    running).
//! 3. Picks the target Unix-domain socket by `(kind, brand)` —
//!    mirroring `smartos-live/src/vm/sbin/vmadmd.js`:
//!      * VNC, brand bhyve/kvm → `<zonepath>/root/tmp/vm.vnc` (no
//!        handshake);
//!      * serial, brand kvm → `<zonepath>/root/tmp/vm.console` (no
//!        handshake);
//!      * serial, any other brand → `/var/run/zones/<zonename>.console_sock`,
//!        and write the zlogin `IDENT C 0\n` handshake (expect an `OK`
//!        line back) before bytes flow.
//! 4. Pumps bytes verbatim between the WebSocket (binary frames) and
//!    the UDS until either side closes.
//!
//! `.console_sock` and the qemu sockets are single-consumer; a second
//! concurrent attach to the same VM cleanly fails to connect, which is
//! surfaced as a "console busy" close.

use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use axum::Router;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum_server::tls_rustls::RustlsConfig;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tracing::{debug, info, warn};
use tritond_auth::{CONSOLE_TICKET_KEY_BYTES, ConsoleKind, ConsoleTicketKey};
use tritond_cn_platform::smartos::zoneadm::{ZoneInfo, ZoneadmError, ZoneadmTool};
use uuid::Uuid;

use crate::console_creds::ConsoleTls;

/// zlogin-`C` handshake line written to a zoneadmd `.console_sock`
/// before bytes flow. (See `vmadmd.js`; the locale is `C`, flags `0`.)
const ZLOGIN_HANDSHAKE: &[u8] = b"IDENT C 0\n";

/// How long to wait for the `OK` ack after writing the handshake.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

/// Idle-session cap: if neither side sends a byte for this long, close.
/// Console sessions are interactive; a half-day silence is a leak.
const IDLE_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// Read buffer size for the UDS→WS direction.
const UDS_READ_BUF: usize = 8 * 1024;

/// Configuration for [`serve`].
pub struct ConsoleListenerConfig {
    /// Address to bind: the admin IPv4 plus the chosen console port.
    pub bind: SocketAddr,
    /// TLS material (self-signed; tritond pins the SPKI).
    pub tls: ConsoleTls,
    /// Per-CN console-ticket key (HS256 secret).
    pub console_ticket_key: [u8; CONSOLE_TICKET_KEY_BYTES],
    /// This CN's SmartOS server UUID — must match the `cn` claim in the
    /// ticket.
    pub server_uuid: Uuid,
    /// `zoneadm` wrapper used to resolve zonename / zonepath / brand /
    /// run state. Tests inject a mock-binary variant.
    pub zoneadm: ZoneadmTool,
    /// Root directory for edge-instance manifests. Unused today —
    /// tritond does not yet send an `edge_instance_id` query param — but
    /// threaded through so the future edge case (a microVM whose
    /// virtio-console UDS path lives in the edge manifest, not under a
    /// zonepath) drops in without a signature change.
    #[allow(dead_code)]
    pub edge_root: PathBuf,
}

/// Shared handler state.
struct ConsoleState {
    console_ticket_key: [u8; CONSOLE_TICKET_KEY_BYTES],
    server_uuid: Uuid,
    zoneadm: ZoneadmTool,
    #[allow(dead_code)]
    edge_root: PathBuf,
}

/// Query parameters on `GET /console/{vm_uuid}`.
#[derive(Debug, Deserialize)]
struct ConsoleParams {
    /// `serial` or `vnc`.
    kind: ConsoleKind,
    /// Zone brand (`kvm` / `bhyve` / `lx` / `joyent-minimal` / ...).
    /// Advisory: the authoritative brand is whatever `zoneadm` reports;
    /// this is used only to disambiguate the VNC eligibility check when
    /// `zoneadm` is unavailable, and is logged.
    #[serde(default)]
    brand: String,
    /// The console ticket (HS256 JWT).
    ticket: String,
}

/// Build the axum router. Exposed for tests so they can serve it over
/// plain HTTP; production wraps it in TLS via [`serve`].
fn build_router(cfg_state: Arc<ConsoleState>) -> Router {
    Router::new()
        .route("/console/{vm_uuid}", get(console_ws))
        .with_state(cfg_state)
}

/// Run the TLS WebSocket console listener until the process exits.
pub async fn serve(cfg: ConsoleListenerConfig) -> Result<()> {
    let tls_config = RustlsConfig::from_pem(cfg.tls.cert_pem.clone(), cfg.tls.key_pem.clone())
        .await
        .context("build rustls config for the console listener")?;

    let state = Arc::new(ConsoleState {
        console_ticket_key: cfg.console_ticket_key,
        server_uuid: cfg.server_uuid,
        zoneadm: cfg.zoneadm,
        edge_root: cfg.edge_root,
    });
    let app = build_router(state);

    info!(bind = %cfg.bind, "console listener started");
    axum_server::bind_rustls(cfg.bind, tls_config)
        .serve(app.into_make_service())
        .await
        .with_context(|| format!("serve console listener on {}", cfg.bind))
}

/// `GET /console/{vm_uuid}` — verify the ticket, resolve the target
/// socket, and (if all checks pass) upgrade the WebSocket and proxy.
async fn console_ws(
    ws: WebSocketUpgrade,
    AxumPath(vm_uuid): AxumPath<Uuid>,
    Query(params): Query<ConsoleParams>,
    State(state): State<Arc<ConsoleState>>,
) -> Response {
    // 1. Ticket verification. Any failure → 401, no upgrade.
    if let Err(e) = ConsoleTicketKey::from_bytes(state.console_ticket_key).verify(
        &params.ticket,
        state.server_uuid,
        vm_uuid,
        params.kind,
    ) {
        warn!(vm_uuid = %vm_uuid, kind = %params.kind, error = %e, "console: rejecting ticket");
        return (StatusCode::UNAUTHORIZED, "invalid console ticket").into_response();
    }

    // 2. Resolve the zone.
    let zinfo = match state.zoneadm.lookup(&vm_uuid.to_string()).await {
        Ok(z) => z,
        Err(ZoneadmError::NotFound { .. }) => {
            return (StatusCode::NOT_FOUND, "no such zone on this host").into_response();
        }
        Err(e) => {
            warn!(vm_uuid = %vm_uuid, error = %e, "console: zoneadm lookup failed");
            return (StatusCode::BAD_GATEWAY, "could not look up the zone").into_response();
        }
    };
    if !zinfo.is_running() {
        return (
            StatusCode::CONFLICT,
            "the zone is not running; start it before opening a console",
        )
            .into_response();
    }

    // 3. Pick the target UDS + whether the zlogin handshake is needed.
    let target = match resolve_target(&zinfo, params.kind, &params.brand) {
        Ok(t) => t,
        Err(reason) => return (StatusCode::CONFLICT, reason).into_response(),
    };
    debug!(
        vm_uuid = %vm_uuid,
        kind = %params.kind,
        zone_brand = %zinfo.brand,
        query_brand = %params.brand,
        socket = %target.socket.display(),
        handshake = target.needs_handshake,
        "console: upgrading websocket",
    );

    // 4. Upgrade and proxy.
    ws.on_upgrade(move |socket| async move {
        if let Err(e) = proxy(socket, &target.socket, target.needs_handshake).await {
            warn!(socket = %target.socket.display(), error = %e, "console: session ended with error");
        }
    })
}

/// The resolved console target: which UDS to dial, and whether to run
/// the zlogin handshake first.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ConsoleTarget {
    socket: PathBuf,
    needs_handshake: bool,
}

/// Choose the console UDS path + handshake requirement from the zone's
/// brand and the requested kind. Mirrors `vmadmd.js:spawnConsoleProxy`.
///
/// Returns `Err(reason)` (a stable string suitable for an HTTP 409
/// body) when the requested kind is not available for this brand.
fn resolve_target(
    zinfo: &ZoneInfo,
    kind: ConsoleKind,
    query_brand: &str,
) -> Result<ConsoleTarget, &'static str> {
    let brand = zinfo.brand.as_str();
    match kind {
        ConsoleKind::Vnc => {
            // Only KVM / bhyve expose a framebuffer. Trust `zoneadm`'s
            // brand; fall back to the query brand only if `zoneadm`
            // reported something unhelpful (shouldn't happen).
            let has_fb = matches!(brand, "kvm" | "bhyve")
                || (brand.is_empty() && matches!(query_brand, "kvm" | "bhyve"));
            if !has_fb {
                return Err("this zone has no VNC framebuffer (brand is not bhyve or kvm)");
            }
            Ok(ConsoleTarget {
                socket: zinfo.vnc_socket(),
                needs_handshake: false,
            })
        }
        ConsoleKind::Serial => {
            if brand == "kvm" || (brand.is_empty() && query_brand == "kvm") {
                // KVM's serial line is a plain UDS, no handshake.
                Ok(ConsoleTarget {
                    socket: zinfo.kvm_serial_socket(),
                    needs_handshake: false,
                })
            } else {
                // bhyve / joyent / joyent-minimal / lx: the serial line
                // is the zoneadmd zone console — needs the `IDENT C 0\n`
                // handshake.
                Ok(ConsoleTarget {
                    socket: zinfo.zone_console_socket(),
                    needs_handshake: true,
                })
            }
        }
    }
}

/// Connect to `socket`, optionally run the zlogin handshake, then pump
/// bytes verbatim between the WebSocket and the UDS until either side
/// closes. Returns `Err` only on a connect/handshake failure or an I/O
/// error on the UDS; the WebSocket is always closed before returning.
async fn proxy(ws: WebSocket, socket: &Path, needs_handshake: bool) -> Result<()> {
    let mut uds = match UnixStream::connect(socket).await {
        Ok(s) => s,
        Err(e) => {
            // The qemu/zoneadmd sockets accept exactly one client; a
            // second attach (ECONNREFUSED / EADDRINUSE-ish) or a
            // missing socket lands here. Close the ws with a reason.
            let reason = if e.kind() == io::ErrorKind::NotFound {
                "console socket does not exist (is the VM up?)"
            } else {
                "console is busy or unavailable (another session may hold it)"
            };
            close_ws(ws, reason).await;
            return Err(anyhow!("connect {}: {e}", socket.display()));
        }
    };

    if needs_handshake && let Err(e) = run_zlogin_handshake(&mut uds).await {
        close_ws(ws, "console handshake with zoneadmd failed").await;
        return Err(e).context("zlogin console handshake");
    }

    pump(ws, uds).await
}

/// Write `IDENT C 0\n` to the zoneadmd console socket and wait for an
/// `OK` line back (within [`HANDSHAKE_TIMEOUT`]). Mirrors `vmadmd.js`.
async fn run_zlogin_handshake(uds: &mut UnixStream) -> Result<()> {
    uds.write_all(ZLOGIN_HANDSHAKE)
        .await
        .context("write zlogin handshake")?;
    uds.flush().await.context("flush zlogin handshake")?;

    let mut buf = Vec::with_capacity(64);
    let line = tokio::time::timeout(HANDSHAKE_TIMEOUT, async {
        let mut byte = [0u8; 1];
        loop {
            let n = uds.read(&mut byte).await?;
            if n == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "zoneadmd closed the console socket during the handshake",
                ));
            }
            if byte[0] == b'\n' {
                break;
            }
            buf.push(byte[0]);
            if buf.len() > 256 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "zoneadmd handshake response line too long",
                ));
            }
        }
        Ok::<_, io::Error>(())
    })
    .await
    .map_err(|_| {
        anyhow!("zoneadmd did not ack the console handshake within {HANDSHAKE_TIMEOUT:?}")
    })?
    .context("read zoneadmd handshake ack")?;

    let _ = line;
    let resp = String::from_utf8_lossy(&buf);
    if resp.starts_with("OK") {
        Ok(())
    } else {
        Err(anyhow!(
            "zoneadmd rejected the console handshake: {}",
            resp.chars().take(80).collect::<String>()
        ))
    }
}

/// Bidirectionally copy bytes between the WebSocket and the UDS.
///
/// WS → UDS: `Binary` / `Text` frame bodies are written verbatim;
/// `Ping` is answered with `Pong`; `Close` ends the session; `Pong` is
/// ignored. UDS → WS: raw bytes are sent as `Binary` frames. An EOF or
/// error on either side closes the other.
async fn pump(ws: WebSocket, mut uds: UnixStream) -> Result<()> {
    let (mut ws_tx, mut ws_rx) = ws.split();
    let mut uds_buf = vec![0u8; UDS_READ_BUF];

    loop {
        tokio::select! {
            // Idle guard.
            () = tokio::time::sleep(IDLE_TIMEOUT) => {
                debug!("console: idle timeout; closing");
                let _ = ws_tx.send(Message::Close(None)).await;
                break;
            }
            from_ws = ws_rx.next() => {
                match from_ws {
                    Some(Ok(Message::Binary(data))) => {
                        if uds.write_all(&data).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(Message::Text(text))) => {
                        if uds.write_all(text.as_bytes()).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(Message::Ping(payload))) => {
                        if ws_tx.send(Message::Pong(payload)).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(Message::Pong(_))) => {}
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                }
            }
            from_uds = uds.read(&mut uds_buf) => {
                match from_uds {
                    Ok(0) => {
                        // UDS EOF: console hung up.
                        let _ = ws_tx.send(Message::Close(None)).await;
                        break;
                    }
                    Ok(n) => {
                        let chunk = uds_buf[..n].to_vec();
                        if ws_tx.send(Message::Binary(chunk.into())).await.is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        }
    }

    let _ = ws_tx.send(Message::Close(None)).await;
    let _ = ws_tx.close().await;
    let _ = uds.shutdown().await;
    Ok(())
}

/// Best-effort close of the WebSocket with a short reason string.
async fn close_ws(ws: WebSocket, reason: &str) {
    let frame = axum::extract::ws::CloseFrame {
        code: axum::extract::ws::close_code::ERROR,
        reason: reason.to_string().into(),
    };
    let mut ws = ws;
    let _ = ws.send(Message::Close(Some(frame))).await;
    let _ = ws.close().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    use tokio::net::UnixListener;
    use tokio_tungstenite::tungstenite::Message as TMessage;

    fn sample_zone(brand: &str, state: &str, zonepath: &str) -> ZoneInfo {
        ZoneInfo {
            zoneid: Some(7),
            zonename: "11111111-1111-1111-1111-111111111111".to_string(),
            state: state.to_string(),
            zonepath: PathBuf::from(zonepath),
            uuid: Some(Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap()),
            brand: brand.to_string(),
        }
    }

    #[test]
    fn resolve_target_kvm_serial_picks_vm_console() {
        let z = sample_zone("kvm", "running", "/zones/z");
        let t = resolve_target(&z, ConsoleKind::Serial, "kvm").unwrap();
        assert_eq!(t.socket, PathBuf::from("/zones/z/root/tmp/vm.console"));
        assert!(!t.needs_handshake);
    }

    #[test]
    fn resolve_target_kvm_vnc_picks_vm_vnc() {
        let z = sample_zone("kvm", "running", "/zones/z");
        let t = resolve_target(&z, ConsoleKind::Vnc, "kvm").unwrap();
        assert_eq!(t.socket, PathBuf::from("/zones/z/root/tmp/vm.vnc"));
        assert!(!t.needs_handshake);
    }

    #[test]
    fn resolve_target_bhyve_serial_picks_console_sock_with_handshake() {
        let z = sample_zone("bhyve", "running", "/zones/z");
        let t = resolve_target(&z, ConsoleKind::Serial, "bhyve").unwrap();
        assert_eq!(
            t.socket,
            PathBuf::from("/var/run/zones/11111111-1111-1111-1111-111111111111.console_sock")
        );
        assert!(t.needs_handshake);
    }

    #[test]
    fn resolve_target_joyent_minimal_serial_uses_zone_console() {
        let z = sample_zone("joyent-minimal", "running", "/zones/z");
        let t = resolve_target(&z, ConsoleKind::Serial, "joyent-minimal").unwrap();
        assert!(t.needs_handshake);
        assert!(t.socket.to_string_lossy().ends_with(".console_sock"));
    }

    #[test]
    fn resolve_target_vnc_on_non_fb_brand_is_rejected() {
        let z = sample_zone("joyent-minimal", "running", "/zones/z");
        assert!(resolve_target(&z, ConsoleKind::Vnc, "joyent-minimal").is_err());
    }

    /// A `UnixListener` standing in for a zoneadmd `.console_sock`: it
    /// expects the `IDENT C 0\n` handshake, replies `OK\n`, then echoes
    /// whatever the client writes.
    async fn fake_console_sock_with_handshake(path: PathBuf) -> tokio::task::JoinHandle<()> {
        let listener = UnixListener::bind(&path).unwrap();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            // Read the handshake line.
            let mut got = Vec::new();
            let mut b = [0u8; 1];
            loop {
                let n = sock.read(&mut b).await.unwrap();
                if n == 0 {
                    return;
                }
                if b[0] == b'\n' {
                    break;
                }
                got.push(b[0]);
            }
            assert_eq!(got, b"IDENT C 0");
            sock.write_all(b"OK\n").await.unwrap();
            // Echo loop.
            let mut buf = [0u8; 256];
            loop {
                match sock.read(&mut buf).await {
                    Ok(0) | Err(_) => return,
                    Ok(n) => {
                        if sock.write_all(&buf[..n]).await.is_err() {
                            return;
                        }
                    }
                }
            }
        })
    }

    #[tokio::test]
    async fn proxy_does_handshake_and_pumps_bytes_both_ways() {
        let dir = tempfile::tempdir().unwrap();
        let sock_path = dir.path().join("zone.console_sock");
        let _server = fake_console_sock_with_handshake(sock_path.clone()).await;

        // Exercise `proxy` directly via a minimal upgrade route — it is
        // the unit under test for the `IDENT C 0\n` handshake + the
        // bidirectional byte pump. (Ticket auth and zoneadm resolution
        // are covered by the other tests.)
        let sock_for_handler = sock_path.clone();
        let app = Router::new().route(
            "/c",
            axum::routing::any(move |ws: WebSocketUpgrade| {
                let s = sock_for_handler.clone();
                async move {
                    ws.on_upgrade(move |socket| async move {
                        let _ = proxy(socket, &s, true).await;
                    })
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let (client, _resp) = tokio_tungstenite::connect_async(format!("ws://{addr}/c"))
            .await
            .unwrap();

        let (mut tx, mut rx) = client.split();
        tx.send(TMessage::Binary(b"hello console".to_vec().into()))
            .await
            .unwrap();
        // First non-control frame back should be the echoed bytes
        // (proving the handshake completed and bytes flow both ways).
        let echoed = loop {
            match rx.next().await.unwrap().unwrap() {
                TMessage::Binary(b) => break b,
                TMessage::Ping(_) | TMessage::Pong(_) => continue,
                other => panic!("unexpected frame: {other:?}"),
            }
        };
        assert_eq!(&echoed[..], b"hello console");
    }

    #[tokio::test]
    async fn bad_ticket_returns_401_without_upgrade() {
        // A *well-formed* WS upgrade request carrying a garbage ticket
        // must be rejected with 401 — the socket never upgrades. (We
        // dial with tokio-tungstenite so the request has the upgrade
        // headers axum's `WebSocketUpgrade` extractor requires;
        // tungstenite surfaces the 401 as `Error::Http`.)
        let key = ConsoleTicketKey::generate();
        let server_uuid = Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap();
        let state = Arc::new(ConsoleState {
            console_ticket_key: *key.bytes(),
            server_uuid,
            // A zoneadm pointed at a nonexistent binary — it must never
            // be called because the ticket check fails first.
            zoneadm: ZoneadmTool::with_bin("/nonexistent/zoneadm"),
            edge_root: PathBuf::from("/tmp"),
        });
        let app = build_router(state);
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let vm_uuid = "11111111-1111-1111-1111-111111111111";
        let res = tokio_tungstenite::connect_async(format!(
            "ws://{addr}/console/{vm_uuid}?kind=serial&brand=bhyve&ticket=not.a.jwt"
        ))
        .await;
        match res {
            Ok(_) => panic!("expected the bad ticket to be rejected, not upgraded"),
            Err(tokio_tungstenite::tungstenite::Error::Http(resp)) => {
                assert_eq!(resp.status(), 401);
            }
            Err(other) => panic!("expected an HTTP 401, got: {other}"),
        }
    }

    #[tokio::test]
    async fn tls_listener_comes_up_and_pinned_client_connects() {
        // Generate the agent's self-signed cert via the same path the
        // listener uses, stand the TLS listener up, then dial it with a
        // tokio-tungstenite client that pins exactly that SPKI (the way
        // tritond does). We only need the TLS handshake + WS upgrade to
        // succeed — the ticket is intentionally garbage so the handler
        // closes the socket right after; that's a clean negative on the
        // *application* layer, which is enough to prove the transport.
        use rustls::client::danger::{
            HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier,
        };
        use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
        use rustls::{DigitallySignedStruct, SignatureScheme};
        use sha2::{Digest, Sha256};

        // The TLS server/client configs need a process-default crypto
        // provider; `main` installs it, tests must too. Idempotent.
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

        let dir = tempfile::tempdir().unwrap();
        let cred = dir.path().join("credentials");
        let tls = crate::console_creds::load_or_init_tls(&cred, Some(Ipv4Addr::LOCALHOST)).unwrap();
        let pinned_spki: [u8; 32] = {
            let mut h = [0u8; 32];
            h.copy_from_slice(&hex::decode(&tls.spki_sha256_hex).unwrap());
            h
        };

        let key = ConsoleTicketKey::generate();
        let server_uuid = Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap();
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let cfg = ConsoleListenerConfig {
            bind: addr,
            tls: tls.clone(),
            console_ticket_key: *key.bytes(),
            server_uuid,
            zoneadm: ZoneadmTool::with_bin("/nonexistent/zoneadm"),
            edge_root: PathBuf::from("/tmp"),
        };
        tokio::spawn(async move {
            let _ = serve(cfg).await;
        });
        // Give the listener a moment to bind.
        tokio::time::sleep(Duration::from_millis(150)).await;

        #[derive(Debug)]
        struct PinVerifier {
            expected: [u8; 32],
            provider: Arc<rustls::crypto::CryptoProvider>,
        }
        impl ServerCertVerifier for PinVerifier {
            fn verify_server_cert(
                &self,
                end_entity: &CertificateDer<'_>,
                _i: &[CertificateDer<'_>],
                _s: &ServerName<'_>,
                _o: &[u8],
                _n: UnixTime,
            ) -> Result<ServerCertVerified, rustls::Error> {
                let (_, cert) =
                    x509_parser::parse_x509_certificate(end_entity.as_ref()).map_err(|_| {
                        rustls::Error::InvalidCertificate(rustls::CertificateError::BadEncoding)
                    })?;
                let got: [u8; 32] = Sha256::digest(cert.tbs_certificate.subject_pki.raw).into();
                if got == self.expected {
                    Ok(ServerCertVerified::assertion())
                } else {
                    Err(rustls::Error::General("spki mismatch".into()))
                }
            }
            fn verify_tls12_signature(
                &self,
                m: &[u8],
                c: &CertificateDer<'_>,
                d: &DigitallySignedStruct,
            ) -> Result<HandshakeSignatureValid, rustls::Error> {
                rustls::crypto::verify_tls12_signature(
                    m,
                    c,
                    d,
                    &self.provider.signature_verification_algorithms,
                )
            }
            fn verify_tls13_signature(
                &self,
                m: &[u8],
                c: &CertificateDer<'_>,
                d: &DigitallySignedStruct,
            ) -> Result<HandshakeSignatureValid, rustls::Error> {
                rustls::crypto::verify_tls13_signature(
                    m,
                    c,
                    d,
                    &self.provider.signature_verification_algorithms,
                )
            }
            fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
                self.provider
                    .signature_verification_algorithms
                    .supported_schemes()
            }
        }

        let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
        let client_cfg = rustls::ClientConfig::builder_with_provider(provider.clone())
            .with_safe_default_protocol_versions()
            .unwrap()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(PinVerifier {
                expected: pinned_spki,
                provider,
            }))
            .with_no_client_auth();
        let connector = tokio_tungstenite::Connector::Rustls(Arc::new(client_cfg));
        let url = format!(
            "wss://{addr}/console/11111111-1111-1111-1111-111111111111?kind=serial&brand=bhyve&ticket=garbage"
        );
        // The TLS handshake must succeed. The HTTP layer then returns
        // 401 (bad ticket) — tokio-tungstenite surfaces that as an
        // error *after* a successful TLS handshake, which is exactly
        // the negative we expect; a TLS failure would look different.
        let res =
            tokio_tungstenite::connect_async_tls_with_config(url, None, false, Some(connector))
                .await;
        match res {
            Ok(_) => panic!("expected the bad-ticket handler to reject the upgrade"),
            Err(tokio_tungstenite::tungstenite::Error::Http(resp)) => {
                assert_eq!(resp.status(), 401);
            }
            Err(other) => panic!("expected an HTTP 401, got a transport error: {other}"),
        }
    }
}
