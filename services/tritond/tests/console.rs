// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

#![allow(clippy::unwrap_used, clippy::expect_used)]

//! End-to-end tests for the serial / VNC console proxy `#[channel]`s
//! (`/v2/admin/legacy/vms/{uuid}/console`, plus the precondition
//! branches on `/v2/tenants/.../instances/{id}/console`).
//!
//! Strategy: stand up a tritond, register + approve a fake CN that
//! reports a console listener port and an rcgen-self-signed cert's SPKI
//! fingerprint, run a tiny in-process TLS WebSocket "agent" on that port
//! that echoes bytes, then drive the proxy from a `tokio-tungstenite`
//! client through tritond's channel and assert the bytes round-trip and
//! that the authz / precondition rejections come back as clean WS
//! Closes (the HTTP 101 is already sent by the time the handler runs).

use std::sync::Arc;

use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use sha2::{Digest, Sha256};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tritond::audit::AuditService;
use tritond::auth::AuthService;
use tritond::{ApiContext, start_server_with_context};
use tritond_audit::MemChain;
use tritond_auth::{JwtKey, RedactedString, hash_password};
use tritond_client::Client;
use tritond_client::types::{ApproveCnRequest, LoginRequest, RegisterCnRequest};
use tritond_store::{AdoptableState, LegacyVm, MemStore, Store, User, VmState};
use uuid::Uuid;

const ROOT_PASSWORD: &str = "correct horse battery staple";
const PLAIN_USER_PASSWORD: &str = "another passphrase entirely";

struct TestServer {
    server: dropshot::HttpServer<ApiContext>,
    store: Arc<dyn Store>,
}

impl TestServer {
    async fn start() -> Self {
        let store: Arc<dyn Store> = Arc::new(MemStore::new());
        let root = User {
            id: Uuid::new_v4(),
            username: "root".to_string(),
            password_hash: hash_password(&RedactedString::from(ROOT_PASSWORD))
                .await
                .unwrap(),
            is_root: true,
            fleet_admin: true,
            created_at: Utc::now(),
            tenant_id: None,
            federation: None,
        };
        store.create_user(root).await.unwrap();
        let plain = User {
            id: Uuid::new_v4(),
            username: "plain".to_string(),
            password_hash: hash_password(&RedactedString::from(PLAIN_USER_PASSWORD))
                .await
                .unwrap(),
            is_root: false,
            fleet_admin: false,
            created_at: Utc::now(),
            tenant_id: None,
            federation: None,
        };
        store.create_user(plain).await.unwrap();

        let auth = Arc::new(AuthService::new(JwtKey::generate()).unwrap());
        let audit = Arc::new(AuditService::new(Arc::new(MemChain::new())));
        let context =
            ApiContext::new(Arc::clone(&store), auth, audit).without_in_process_provisioner();
        let server = start_server_with_context("127.0.0.1:0", context)
            .await
            .unwrap();
        Self { server, store }
    }

    fn bind(&self) -> std::net::SocketAddr {
        self.server.local_addr()
    }

    fn anonymous_client(&self) -> Client {
        Client::new(&format!("http://{}", self.bind()))
    }

    fn bearer_client(&self, token: &str) -> Client {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {token}").parse().unwrap(),
        );
        let reqwest = reqwest::Client::builder()
            .default_headers(headers)
            .build()
            .unwrap();
        Client::new_with_client(&format!("http://{}", self.bind()), reqwest)
    }

    async fn token_for(&self, username: &str, password: &str) -> String {
        self.anonymous_client()
            .login()
            .body(LoginRequest {
                username: username.to_string(),
                password: password.to_string(),
            })
            .send()
            .await
            .unwrap()
            .into_inner()
            .access_token
    }

    async fn close(self) {
        self.server.close().await.unwrap();
    }
}

fn fixture_sysinfo(uuid: Uuid, hostname: &str) -> serde_json::Value {
    serde_json::json!({
        "UUID": uuid.to_string(),
        "Hostname": hostname,
        "Boot Time": "1700000000",
    })
}

/// rcgen-generate a self-signed cert for `127.0.0.1` and return
/// (cert DER, key PKCS#8 DER, SHA-256 of the SubjectPublicKeyInfo).
fn self_signed_for_localhost() -> (Vec<u8>, Vec<u8>, [u8; 32]) {
    let ck = rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_string()]).unwrap();
    let cert_der = ck.cert.der().to_vec();
    let key_der = ck.signing_key.serialize_der();
    let (_, parsed) = x509_parser::parse_x509_certificate(&cert_der).unwrap();
    let spki = parsed.tbs_certificate.subject_pki.raw;
    let hash: [u8; 32] = Sha256::digest(spki).into();
    (cert_der, key_der, hash)
}

/// Register + approve a CN that reports the given console listener port
/// and TLS SPKI fingerprint. Returns the CN's `server_uuid`.
async fn register_and_approve_cn(
    test: &TestServer,
    root_token: &str,
    admin_ip: &str,
    console_port: u16,
    spki_hex: &str,
) -> Uuid {
    let server_uuid = Uuid::new_v4();
    let anon = test.anonymous_client();
    let registered = anon
        .agent_register()
        .body(RegisterCnRequest {
            server_uuid,
            hostname: "cn-console".to_string(),
            admin_ip: Some(admin_ip.parse().unwrap()),
            sysinfo: fixture_sysinfo(server_uuid, "cn-console"),
            console_listen_port: Some(console_port),
            console_tls_spki_sha256_hex: Some(spki_hex.to_string()),
        })
        .send()
        .await
        .unwrap()
        .into_inner();
    let claim = registered.claim_code.unwrap();
    let session = test.bearer_client(root_token);
    session
        .approve_cn()
        .body(ApproveCnRequest { code: claim })
        .send()
        .await
        .expect("approve must succeed");
    // Drain the API key + console-ticket key to the agent (first
    // long-poll-after-approval) — not strictly needed but mirrors the
    // real flow; the console key stays on the Cn record afterwards.
    let _ = anon
        .agent_register_status()
        .poll_token(&registered.poll_token)
        .send()
        .await
        .unwrap();
    server_uuid
}

fn legacy_vm_fixture(host_cn: Uuid, smartos_uuid: Uuid, brand: &str) -> LegacyVm {
    let now = Utc::now();
    LegacyVm {
        smartos_uuid,
        host_cn_uuid: host_cn,
        legacy_owner_uuid: Some(Uuid::nil()),
        alias: Some("legacy-zone".to_string()),
        brand: Some(brand.to_string()),
        state: Some(VmState::Running),
        zone_state: Some("running".to_string()),
        memory_bytes: Some(512 * 1024 * 1024),
        quota_bytes: Some(20 * 1024 * 1024 * 1024),
        cpu_cap: Some(200),
        last_modified: Some("2026-05-08T10:00:00Z".to_string()),
        nics: Vec::new(),
        adoptable: AdoptableState::Unevaluated,
        first_seen_at: now,
        last_seen_at: now,
    }
}

/// Start a tiny TLS WebSocket "agent" on a fresh `127.0.0.1` port that
/// echoes every binary/text frame back. Returns the bound port; the
/// task runs until the process exits.
async fn spawn_echo_agent(cert_der: Vec<u8>, key_der: Vec<u8>) -> u16 {
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
    use tokio_rustls::TlsAcceptor;

    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(cert_der)],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key_der)),
        )
        .unwrap();
    let acceptor = TlsAcceptor::from(Arc::new(config));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    tokio::spawn(async move {
        loop {
            let Ok((tcp, _)) = listener.accept().await else {
                break;
            };
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let Ok(tls) = acceptor.accept(tcp).await else {
                    return;
                };
                let Ok(mut ws) = tokio_tungstenite::accept_async(tls).await else {
                    return;
                };
                while let Some(Ok(msg)) = ws.next().await {
                    match msg {
                        Message::Binary(_) | Message::Text(_) => {
                            if ws.send(msg).await.is_err() {
                                break;
                            }
                        }
                        Message::Close(_) => break,
                        _ => {}
                    }
                }
            });
        }
    });
    port
}

/// Connect a WebSocket client to tritond at `path`, attaching a bearer
/// token. Returns the established stream (after the HTTP 101).
async fn connect_console(
    addr: std::net::SocketAddr,
    path: &str,
    token: &str,
) -> Result<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    tokio_tungstenite::tungstenite::Error,
> {
    let url = format!("ws://{addr}{path}");
    let mut req = url.into_client_request().unwrap();
    req.headers_mut()
        .insert("authorization", format!("Bearer {token}").parse().unwrap());
    let (ws, _resp) = tokio_tungstenite::connect_async(req).await?;
    Ok(ws)
}

/// Pull the next message off the stream with a short timeout.
async fn next_msg(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> Option<Message> {
    match tokio::time::timeout(std::time::Duration::from_secs(5), ws.next()).await {
        Ok(Some(Ok(m))) => Some(m),
        _ => None,
    }
}

#[tokio::test]
async fn legacy_console_proxies_bytes_through_the_agent() {
    let test = TestServer::start().await;
    let root = test.token_for("root", ROOT_PASSWORD).await;
    let (cert_der, key_der, spki) = self_signed_for_localhost();
    let port = spawn_echo_agent(cert_der, key_der).await;
    let cn = register_and_approve_cn(&test, &root, "127.0.0.1", port, &hex::encode(spki)).await;

    let smartos_uuid = Uuid::new_v4();
    test.store
        .upsert_legacy_vm(legacy_vm_fixture(cn, smartos_uuid, "joyent-minimal"))
        .await
        .unwrap();

    let mut ws = connect_console(
        test.bind(),
        &format!("/v2/admin/legacy/vms/{smartos_uuid}/console?kind=serial"),
        &root,
    )
    .await
    .expect("console channel should establish");

    ws.send(Message::Binary(b"hello console".to_vec().into()))
        .await
        .unwrap();
    let echoed = next_msg(&mut ws).await.expect("agent should echo bytes");
    match echoed {
        Message::Binary(b) => assert_eq!(b.as_ref(), b"hello console"),
        other => panic!("expected echoed binary, got {other:?}"),
    }

    // Tear the proxied connection down from the client so the proxy
    // task ends and dropshot's graceful shutdown can complete.
    let _ = ws.close(None).await;
    while next_msg(&mut ws).await.is_some() {}
    drop(ws);
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    test.close().await;
}

#[tokio::test]
async fn legacy_console_rejects_non_fleet_admin() {
    let test = TestServer::start().await;
    let root = test.token_for("root", ROOT_PASSWORD).await;
    let plain = test.token_for("plain", PLAIN_USER_PASSWORD).await;
    let (cert_der, key_der, spki) = self_signed_for_localhost();
    let port = spawn_echo_agent(cert_der, key_der).await;
    let cn = register_and_approve_cn(&test, &root, "127.0.0.1", port, &hex::encode(spki)).await;

    let smartos_uuid = Uuid::new_v4();
    test.store
        .upsert_legacy_vm(legacy_vm_fixture(cn, smartos_uuid, "joyent-minimal"))
        .await
        .unwrap();

    // Channel handshake still completes (101 already sent), then the
    // handler closes it because `plain` is not a fleet admin.
    let mut ws = connect_console(
        test.bind(),
        &format!("/v2/admin/legacy/vms/{smartos_uuid}/console?kind=serial"),
        &plain,
    )
    .await
    .expect("handshake completes before authz check");
    match next_msg(&mut ws).await {
        Some(Message::Close(Some(frame))) => {
            assert!(
                frame
                    .reason
                    .as_str()
                    .contains("not found or not accessible")
            );
        }
        other => panic!("expected a Close frame, got {other:?}"),
    }

    test.close().await;
}

#[tokio::test]
async fn legacy_console_rejects_vnc_on_non_hvm_brand() {
    let test = TestServer::start().await;
    let root = test.token_for("root", ROOT_PASSWORD).await;
    let (cert_der, key_der, spki) = self_signed_for_localhost();
    let port = spawn_echo_agent(cert_der, key_der).await;
    let cn = register_and_approve_cn(&test, &root, "127.0.0.1", port, &hex::encode(spki)).await;

    let smartos_uuid = Uuid::new_v4();
    test.store
        .upsert_legacy_vm(legacy_vm_fixture(cn, smartos_uuid, "joyent-minimal"))
        .await
        .unwrap();

    let mut ws = connect_console(
        test.bind(),
        &format!("/v2/admin/legacy/vms/{smartos_uuid}/console?kind=vnc"),
        &root,
    )
    .await
    .expect("handshake completes before brand check");
    match next_msg(&mut ws).await {
        Some(Message::Close(Some(frame))) => {
            assert!(frame.reason.as_str().contains("no VNC framebuffer"));
        }
        other => panic!("expected a Close frame, got {other:?}"),
    }

    test.close().await;
}

#[tokio::test]
async fn legacy_console_rejects_when_cn_has_no_console_listener() {
    let test = TestServer::start().await;
    let root = test.token_for("root", ROOT_PASSWORD).await;

    // Register + approve a CN *without* reporting a console listener.
    let server_uuid = Uuid::new_v4();
    let anon = test.anonymous_client();
    let registered = anon
        .agent_register()
        .body(RegisterCnRequest {
            server_uuid,
            hostname: "cn-nolistener".to_string(),
            admin_ip: Some("127.0.0.1".parse().unwrap()),
            sysinfo: fixture_sysinfo(server_uuid, "cn-nolistener"),
            console_listen_port: None,
            console_tls_spki_sha256_hex: None,
        })
        .send()
        .await
        .unwrap()
        .into_inner();
    test.bearer_client(&root)
        .approve_cn()
        .body(ApproveCnRequest {
            code: registered.claim_code.unwrap(),
        })
        .send()
        .await
        .unwrap();

    let smartos_uuid = Uuid::new_v4();
    test.store
        .upsert_legacy_vm(legacy_vm_fixture(
            server_uuid,
            smartos_uuid,
            "joyent-minimal",
        ))
        .await
        .unwrap();

    let mut ws = connect_console(
        test.bind(),
        &format!("/v2/admin/legacy/vms/{smartos_uuid}/console?kind=serial"),
        &root,
    )
    .await
    .expect("handshake completes before the no-listener check");
    match next_msg(&mut ws).await {
        Some(Message::Close(Some(frame))) => {
            assert!(frame.reason.as_str().contains("console not available"));
        }
        other => panic!("expected a Close frame, got {other:?}"),
    }

    test.close().await;
}

#[tokio::test]
async fn instance_console_rejects_unknown_instance() {
    let test = TestServer::start().await;
    let root = test.token_for("root", ROOT_PASSWORD).await;
    let tenant_id = Uuid::new_v4();
    let project_id = Uuid::new_v4();
    let instance_id = Uuid::new_v4();

    let mut ws = connect_console(
        test.bind(),
        &format!(
            "/v2/tenants/{tenant_id}/projects/{project_id}/instances/{instance_id}/console?kind=serial"
        ),
        &root,
    )
    .await
    .expect("handshake completes before the not-found check");
    match next_msg(&mut ws).await {
        Some(Message::Close(Some(frame))) => {
            assert!(
                frame
                    .reason
                    .as_str()
                    .contains("not found or not accessible")
            );
        }
        other => panic!("expected a Close frame, got {other:?}"),
    }

    test.close().await;
}
