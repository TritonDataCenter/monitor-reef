// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! The browser ↔ tritond ↔ tritonagent console proxy.
//!
//! tritond is the only authorisation point for serial / VNC consoles
//! (see `service_impl::instance_console` / `legacy_vm_console`). Once a
//! caller is authorised, this module:
//!
//! 1. mints a short-lived per-CN console ticket bound to
//!    `(server_uuid, vm_uuid, kind)`,
//! 2. dials the agent's on-host console listener over TLS, **pinning**
//!    the listener's SubjectPublicKeyInfo SHA-256 (reported at CN
//!    registration) so a hijacked admin IP cannot MITM the byte stream,
//! 3. copies WebSocket binary/text frames verbatim in both directions
//!    until either side closes.
//!
//! No protocol parsing happens in the path — `kind=serial` carries raw
//! serial bytes, `kind=vnc` carries raw RFB.

use std::net::Ipv4Addr;
use std::sync::Arc;

use dropshot::{Path, Query, RequestContext, WebsocketChannelResult, WebsocketConnection};
use futures_util::{SinkExt, StreamExt};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, Error as TlsError, SignatureScheme};
use sha2::{Digest, Sha256};
use tokio_tungstenite::Connector;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::Role;
use tokio_tungstenite::tungstenite::protocol::frame::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use uuid::Uuid;

use tritond_api::types::{Instance, InstanceBrand, LegacyVm, LifecycleStateKind};
use tritond_api::{ConsoleQuery, LegacyVmPath, TenantProjectInstancePath};
use tritond_audit::Outcome as AuditOutcome;
use tritond_auth::{ConsoleKind, ConsoleTicketKey, DEFAULT_TICKET_TTL_SECS};
use tritond_store::Cn;

use crate::auth::{
    Action, Principal, authenticate_and_authorize, authenticate_and_authorize_in_tenant,
};
use crate::context::ApiContext;

/// The Dropshot side of the proxy, wrapped as a server-role WebSocket.
type DownstreamWs = tokio_tungstenite::WebSocketStream<dropshot::WebsocketConnectionRaw>;

/// Wrap the upgraded HTTP connection as a server-role WebSocket so the
/// handler can talk frames before/after deciding whether to proxy.
pub(crate) async fn accept(upgraded: WebsocketConnection) -> DownstreamWs {
    tokio_tungstenite::WebSocketStream::from_raw_socket(upgraded.into_inner(), Role::Server, None)
        .await
}

/// Cleanly close the downstream WebSocket with a policy-violation code
/// and a human-readable reason. Used for every authorised-but-rejected
/// case (instance not running, no console listener on the CN, VNC asked
/// for on a brand without a framebuffer, upstream dial failed, …) so the
/// browser surfaces *why* rather than just dropping.
pub(crate) async fn reject(mut ws: DownstreamWs, reason: &str) -> WebsocketChannelResult {
    let _ = ws
        .send(Message::Close(Some(CloseFrame {
            code: CloseCode::Policy,
            reason: reason.to_string().into(),
        })))
        .await;
    let _ = ws.close(None).await;
    Ok(())
}

/// Everything the proxy needs once the caller is authorised.
pub(crate) struct ConsoleTarget {
    pub server_uuid: Uuid,
    pub admin_ip: Ipv4Addr,
    pub console_port: u16,
    /// SHA-256 of the agent listener's TLS SubjectPublicKeyInfo.
    pub spki_sha256: [u8; 32],
    /// Per-CN HS256 key for minting the console ticket.
    pub console_ticket_key: [u8; 32],
    pub vm_uuid: Uuid,
    /// SmartOS brand string passed through to the agent (`bhyve`,
    /// `joyent-minimal`, `kvm`, …, or `not-applicable`).
    pub brand: String,
    pub kind: ConsoleKind,
}

/// Mint a ticket, dial the agent listener with the pinned cert, and pump
/// bytes both ways until either side closes. Any failure closes the
/// downstream with a reason and returns `Ok(())` (the channel just ends);
/// hard errors are logged.
pub(crate) async fn proxy_console(
    downstream: DownstreamWs,
    target: ConsoleTarget,
) -> WebsocketChannelResult {
    let ticket = match ConsoleTicketKey::from_bytes(target.console_ticket_key).mint(
        target.server_uuid,
        target.vm_uuid,
        target.kind,
        DEFAULT_TICKET_TTL_SECS,
    ) {
        Ok(t) => t,
        Err(e) => {
            tracing::error!(error = %e, "console: minting ticket failed");
            return reject(downstream, "internal error minting console ticket").await;
        }
    };

    let url = format!(
        "wss://{}:{}/console/{}?kind={}&brand={}&ticket={}",
        target.admin_ip,
        target.console_port,
        target.vm_uuid,
        target.kind.as_str(),
        urlencoding::encode(&target.brand),
        urlencoding::encode(&ticket),
    );

    // Build the client config off an explicit crypto provider rather
    // than the process default: that way unit/integration tests (which
    // don't run `main`'s `install_default`) don't panic in
    // `ClientConfig::builder()`. In the daemon the installed default
    // and `aws_lc_rs::default_provider()` are the same provider.
    let provider = rustls::crypto::CryptoProvider::get_default()
        .cloned()
        .unwrap_or_else(|| Arc::new(rustls::crypto::aws_lc_rs::default_provider()));
    let tls_config = match ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
    {
        Ok(b) => b
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(SpkiPinVerifier::new(target.spki_sha256)))
            .with_no_client_auth(),
        Err(e) => {
            tracing::error!(error = %e, "console: building TLS client config failed");
            return reject(downstream, "internal TLS configuration error").await;
        }
    };

    let connector = Connector::Rustls(Arc::new(tls_config));
    let (upstream, _resp) =
        match tokio_tungstenite::connect_async_tls_with_config(url, None, false, Some(connector))
            .await
        {
            Ok(pair) => pair,
            Err(e) => {
                tracing::warn!(
                    server_uuid = %target.server_uuid,
                    vm_uuid = %target.vm_uuid,
                    error = %e,
                    "console: dialling agent listener failed",
                );
                return reject(
                    downstream,
                    "could not reach the console listener on the host",
                )
                .await;
            }
        };

    pump(downstream, upstream).await;
    Ok(())
}

/// Copy WebSocket frames between the two streams until either side
/// closes or errors. Text/Binary are relayed verbatim; a Close on either
/// side is forwarded to the other and ends the pump. (Ping/Pong are
/// answered by tokio-tungstenite itself on the next read/write of each
/// stream.)
async fn pump<S>(mut downstream: DownstreamWs, mut upstream: S)
where
    S: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>>
        + SinkExt<Message, Error = tokio_tungstenite::tungstenite::Error>
        + Unpin,
{
    loop {
        tokio::select! {
            from_browser = downstream.next() => {
                match from_browser {
                    Some(Ok(msg)) => {
                        let is_close = matches!(msg, Message::Close(_));
                        if let Message::Text(_) | Message::Binary(_) | Message::Close(_) = msg
                            && upstream.send(msg).await.is_err()
                        {
                            break;
                        }
                        if is_close {
                            break;
                        }
                    }
                    _ => break,
                }
            }
            from_agent = upstream.next() => {
                match from_agent {
                    Some(Ok(msg)) => {
                        let is_close = matches!(msg, Message::Close(_));
                        if let Message::Text(_) | Message::Binary(_) | Message::Close(_) = msg
                            && downstream.send(msg).await.is_err()
                        {
                            break;
                        }
                        if is_close {
                            break;
                        }
                    }
                    _ => break,
                }
            }
        }
    }
    let _ = downstream.close(None).await;
    let _ = upstream.close().await;
}

/// A [`ServerCertVerifier`] that ignores the certificate chain and host
/// name entirely and instead pins the SHA-256 of the leaf certificate's
/// SubjectPublicKeyInfo (DER). The agent listener uses a self-signed
/// cert; tritond learns its SPKI fingerprint at CN registration and
/// pins it here, so a process that hijacks the admin IP cannot present a
/// different (even validly-signed) certificate.
#[derive(Debug)]
pub(crate) struct SpkiPinVerifier {
    expected: [u8; 32],
    /// Signature-verification algorithms from the process-default
    /// crypto provider — used to validate the handshake signature once
    /// the SPKI pin matches.
    sig_algs: rustls::crypto::WebPkiSupportedAlgorithms,
}

impl SpkiPinVerifier {
    pub(crate) fn new(expected: [u8; 32]) -> Self {
        let provider = rustls::crypto::CryptoProvider::get_default()
            .cloned()
            .unwrap_or_else(|| Arc::new(rustls::crypto::aws_lc_rs::default_provider()));
        Self {
            expected,
            sig_algs: provider.signature_verification_algorithms,
        }
    }
}

impl ServerCertVerifier for SpkiPinVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, TlsError> {
        let (_, cert) = x509_parser::parse_x509_certificate(end_entity.as_ref())
            .map_err(|_| TlsError::InvalidCertificate(rustls::CertificateError::BadEncoding))?;
        let spki_der = cert.tbs_certificate.subject_pki.raw;
        let got: [u8; 32] = Sha256::digest(spki_der).into();
        // Constant-time compare so a partial match leaks nothing timing-wise.
        if constant_time_eq(&got, &self.expected) {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(TlsError::General(
                "console listener cert SPKI pin mismatch".to_string(),
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.sig_algs)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.sig_algs)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.sig_algs.supported_schemes()
    }
}

/// Branch-free 32-byte equality.
fn constant_time_eq(a: &[u8; 32], b: &[u8; 32]) -> bool {
    let mut diff = 0u8;
    for i in 0..32 {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

// ---------------------------------------------------------------------------
// Channel handlers (dispatched from `service_impl`).
// ---------------------------------------------------------------------------

/// Resolve a CN's console endpoint fields into a [`ConsoleTarget`], or
/// `Err(reason)` if the CN has no usable console listener.
fn target_from_cn(
    cn: &Cn,
    vm_uuid: Uuid,
    brand: String,
    kind: ConsoleKind,
) -> Result<ConsoleTarget, &'static str> {
    let admin_ip = cn.admin_ip.ok_or("the host has not reported an admin IP")?;
    let console_port = cn.console_listen_port.ok_or(
        "console not available — the agent on this host has not registered a console listener",
    )?;
    let spki_sha256 = cn.console_tls_spki_sha256.ok_or(
        "console not available — the agent on this host has not registered a TLS fingerprint",
    )?;
    let console_ticket_key = cn
        .console_ticket_key
        .ok_or("console not available — this host has no console-ticket key (re-approve the CN)")?;
    Ok(ConsoleTarget {
        server_uuid: cn.server_uuid,
        admin_ip,
        console_port,
        spki_sha256,
        console_ticket_key,
        vm_uuid,
        brand,
        kind,
    })
}

/// Append a best-effort `console.open` event to the audit chain.
async fn audit_console_open(
    ctx: &ApiContext,
    principal: &Principal,
    request_id: Option<Uuid>,
    vm_uuid: Uuid,
    server_uuid: Uuid,
    kind: ConsoleKind,
    legacy: bool,
) {
    let resource = if legacy {
        format!("LegacyVm::\"{vm_uuid}\"")
    } else {
        format!("Instance::\"{vm_uuid}\"")
    };
    ctx.audit
        .record_mutation(
            principal,
            Action::InstanceConsole,
            request_id,
            Some(resource.clone()),
            AuditOutcome::Success {
                resource: Some(resource),
            },
            serde_json::json!({
                "event": "console.open",
                "vm_uuid": vm_uuid,
                "server_uuid": server_uuid,
                "kind": kind.as_str(),
                "legacy": legacy,
            }),
        )
        .await;
}

/// `GET /v2/tenants/{tenant}/projects/{project}/instances/{instance}/console`
/// — browser-facing serial / VNC console for a managed instance.
///
/// The HTTP 101 is already sent by the time this runs, so every check
/// after the upgrade reports its verdict to the browser via a clean
/// WebSocket Close (`reject`), never an HTTP status.
pub(crate) async fn instance_console(
    rqctx: RequestContext<ApiContext>,
    path: Path<TenantProjectInstancePath>,
    query: Query<ConsoleQuery>,
    upgraded: WebsocketConnection,
) -> WebsocketChannelResult {
    let ctx = rqctx.context();
    let TenantProjectInstancePath {
        tenant_id,
        project_id,
        instance_id,
    } = path.into_inner();
    let kind = query.into_inner().kind;
    let request_id = Uuid::parse_str(&rqctx.request_id).ok();

    let ws = accept(upgraded).await;

    // Authorise. The tenant-scoped helper returns 404 on deny, which
    // we conflate with "not found" in the close reason so a probe can't
    // distinguish the two.
    let principal = match authenticate_and_authorize_in_tenant(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::InstanceConsole,
        tenant_id,
    )
    .await
    {
        Ok(p) => p,
        Err(_) => return reject(ws, "instance not found or not accessible").await,
    };

    let instance: Instance = match ctx.store.get_instance(instance_id).await {
        Ok(i) if i.tenant_id == tenant_id && i.project_id == project_id => i,
        _ => return reject(ws, "instance not found or not accessible").await,
    };

    if instance.lifecycle.kind() != LifecycleStateKind::Running {
        return reject(ws, "instance is not running; no console available").await;
    }
    // We deliberately do NOT gate VNC on `instance.brand` here: that
    // field is only populated at create time from the image's
    // compatibility block, so it's `NotApplicable` (i.e. "unknown")
    // for older instances and for images that carry no compat block --
    // including plenty of real bhyve VMs. The agent's console listener
    // resolves the *actual* zone brand via `zoneadm` and rejects a VNC
    // attach on a brand without a framebuffer; that's the single
    // source of truth. The brand we pass downstream below is just an
    // advisory hint for that check's fallback path.
    let Some(host_cn_uuid) = instance.host_cn_uuid else {
        return reject(ws, "instance has not been placed on a host yet").await;
    };
    let cn = match ctx.store.get_cn(host_cn_uuid).await {
        Ok(c) => c,
        Err(_) => return reject(ws, "the host for this instance is no longer registered").await,
    };
    let brand = if instance.brand == InstanceBrand::NotApplicable {
        "not-applicable".to_string()
    } else {
        instance.brand.as_str().to_string()
    };
    let target = match target_from_cn(&cn, instance_id, brand, kind) {
        Ok(t) => t,
        Err(reason) => return reject(ws, reason).await,
    };

    audit_console_open(
        ctx,
        &principal,
        request_id,
        instance_id,
        cn.server_uuid,
        kind,
        false,
    )
    .await;

    proxy_console(ws, target).await
}

/// `GET /v2/admin/legacy/vms/{smartos_uuid}/console` — operator console
/// for a discovered (non-managed) zone. Fleet-admin only.
pub(crate) async fn legacy_vm_console(
    rqctx: RequestContext<ApiContext>,
    path: Path<LegacyVmPath>,
    query: Query<ConsoleQuery>,
    upgraded: WebsocketConnection,
) -> WebsocketChannelResult {
    let ctx = rqctx.context();
    let smartos_uuid = path.into_inner().smartos_uuid;
    let kind = query.into_inner().kind;
    let request_id = Uuid::parse_str(&rqctx.request_id).ok();

    let ws = accept(upgraded).await;

    // Same fleet-admin gate as `legacy_vm_get` / `list_legacy_vms`.
    let principal = match authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::LegacyVmGet,
    )
    .await
    {
        Ok(p) => p,
        Err(_) => return reject(ws, "zone not found or not accessible").await,
    };

    let vm: LegacyVm = match ctx.store.get_legacy_vm(smartos_uuid).await {
        Ok(v) => v,
        Err(_) => return reject(ws, "zone not found or not accessible").await,
    };

    let brand = vm
        .brand
        .clone()
        .unwrap_or_else(|| "not-applicable".to_string());
    // As with managed instances: don't reject VNC here on the stored
    // brand. The agent re-derives the live zone brand via `zoneadm`
    // and is the authority on whether a framebuffer exists.
    let cn = match ctx.store.get_cn(vm.host_cn_uuid).await {
        Ok(c) => c,
        Err(_) => return reject(ws, "the host for this zone is no longer registered").await,
    };
    let target = match target_from_cn(&cn, smartos_uuid, brand, kind) {
        Ok(t) => t,
        Err(reason) => return reject(ws, reason).await,
    };

    audit_console_open(
        ctx,
        &principal,
        request_id,
        smartos_uuid,
        cn.server_uuid,
        kind,
        true,
    )
    .await;

    proxy_console(ws, target).await
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    /// rcgen-generate a self-signed cert and return (cert DER, SHA-256 of
    /// its SubjectPublicKeyInfo) — mirrors what the agent listener would
    /// present and report at registration.
    fn self_signed() -> (Vec<u8>, [u8; 32]) {
        let cert = rcgen::generate_simple_self_signed(vec!["console.test".to_string()]).unwrap();
        let der = cert.cert.der().to_vec();
        let (_, parsed) = x509_parser::parse_x509_certificate(&der).unwrap();
        let spki = parsed.tbs_certificate.subject_pki.raw;
        let hash: [u8; 32] = Sha256::digest(spki).into();
        (der, hash)
    }

    #[test]
    fn spki_pin_accepts_matching_cert() {
        let (der, hash) = self_signed();
        let verifier = SpkiPinVerifier::new(hash);
        let cert = CertificateDer::from(der);
        let res = verifier.verify_server_cert(
            &cert,
            &[],
            &ServerName::try_from("console.test").unwrap(),
            &[],
            UnixTime::now(),
        );
        assert!(res.is_ok());
    }

    #[test]
    fn spki_pin_rejects_mismatched_cert() {
        let (der, _hash) = self_signed();
        // Pin a different (all-zero) fingerprint.
        let verifier = SpkiPinVerifier::new([0u8; 32]);
        let cert = CertificateDer::from(der);
        let res = verifier.verify_server_cert(
            &cert,
            &[],
            &ServerName::try_from("console.test").unwrap(),
            &[],
            UnixTime::now(),
        );
        assert!(res.is_err());
    }

    #[test]
    fn spki_pin_rejects_garbage_cert() {
        let verifier = SpkiPinVerifier::new([1u8; 32]);
        let cert = CertificateDer::from(vec![0u8, 1, 2, 3, 4]);
        let res = verifier.verify_server_cert(
            &cert,
            &[],
            &ServerName::try_from("console.test").unwrap(),
            &[],
            UnixTime::now(),
        );
        assert!(res.is_err());
    }

    #[test]
    fn constant_time_eq_basic() {
        assert!(constant_time_eq(&[7u8; 32], &[7u8; 32]));
        let mut a = [7u8; 32];
        let b = [7u8; 32];
        a[31] = 8;
        assert!(!constant_time_eq(&a, &b));
    }
}
