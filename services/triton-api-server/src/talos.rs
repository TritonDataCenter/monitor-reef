// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Talos gRPC client backed by the triton-relay tunnel.
//!
//! Proto types are compiled at build time from the vendored Talos v1.7.6
//! proto files at `proto/machine/machine.proto` and
//! `proto/common/common.proto`. Re-generate with `make talos-proto-gen`
//! (requires protoc) when upgrading Talos.
//!
//! Both maintenance mode (self-signed cert, no client auth) and
//! authenticated mode (mTLS via talosconfig credentials) use TLS over
//! the yamux relay stream; the difference is the certificate verifier.

use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use anyhow::{Context as _, Result};
use hyper_util::client::legacy::connect::{Connected, Connection};
use hyper_util::rt::TokioIo;
use rustls::DigitallySignedStruct;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use tokio_util::compat::FuturesAsyncReadCompatExt;
use tonic::transport::{Channel, Endpoint};
use tower::service_fn;

use crate::relay::RelayState;

// ---------------------------------------------------------------------------
// Proto module declarations — code lives in OUT_DIR at compile time.
// ---------------------------------------------------------------------------

pub mod proto {
    pub mod google {
        pub mod rpc {
            tonic::include_proto!("google.rpc");
        }
    }

    pub mod common {
        tonic::include_proto!("common");
    }

    pub mod machine {
        tonic::include_proto!("machine");
    }
}

// Re-exports used by the bootstrap endpoint (Step 4).
pub use proto::machine::machine_service_client::MachineServiceClient;
pub use proto::machine::{ApplyConfigurationRequest, BootstrapRequest, BootstrapResponse};

// ---------------------------------------------------------------------------
// IO adapter: yamux::Stream → TLS → hyper::rt::{Read, Write} + Connection
// ---------------------------------------------------------------------------

type TlsStream = tokio_rustls::client::TlsStream<tokio_util::compat::Compat<yamux::Stream>>;

struct RelayIo(TokioIo<TlsStream>);

impl Unpin for RelayIo {}

impl Connection for RelayIo {
    fn connected(&self) -> Connected {
        Connected::new()
    }
}

impl hyper::rt::Read for RelayIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: hyper::rt::ReadBufCursor<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_read(cx, buf)
    }
}

impl hyper::rt::Write for RelayIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.0).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_shutdown(cx)
    }
}

// ---------------------------------------------------------------------------
// TLS helpers
// ---------------------------------------------------------------------------

/// Skip all certificate verification (maintenance-mode self-signed certs).
#[derive(Debug)]
struct SkipVerification;

impl ServerCertVerifier for SkipVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        use rustls::SignatureScheme;
        vec![
            SignatureScheme::RSA_PKCS1_SHA256,
            SignatureScheme::RSA_PKCS1_SHA384,
            SignatureScheme::RSA_PKCS1_SHA512,
            SignatureScheme::RSA_PSS_SHA256,
            SignatureScheme::RSA_PSS_SHA384,
            SignatureScheme::RSA_PSS_SHA512,
            SignatureScheme::ECDSA_NISTP256_SHA256,
            SignatureScheme::ECDSA_NISTP384_SHA384,
            SignatureScheme::ECDSA_NISTP521_SHA512,
            SignatureScheme::ED25519,
        ]
    }
}

fn maintenance_tls_config() -> Result<rustls::ClientConfig> {
    let mut config = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(SkipVerification))
        .with_no_client_auth();
    config.alpn_protocols = vec![b"h2".to_vec()];
    Ok(config)
}

/// Build a mTLS config from PEM-encoded CA, client cert, and client key.
pub fn mtls_config(ca_pem: &[u8], cert_pem: &[u8], key_pem: &[u8]) -> Result<rustls::ClientConfig> {
    use rustls::pki_types::PrivateKeyDer;
    use rustls_pemfile::{certs, private_key};

    let mut root_store = rustls::RootCertStore::empty();
    for cert in certs(&mut io::Cursor::new(ca_pem)) {
        root_store
            .add(cert.context("parsing Talos CA certificate")?)
            .context("adding Talos CA to root store")?;
    }

    let chain: Vec<CertificateDer<'static>> = certs(&mut io::Cursor::new(cert_pem))
        .collect::<io::Result<_>>()
        .context("parsing Talos client certificate")?;

    let key: PrivateKeyDer<'static> = private_key(&mut io::Cursor::new(key_pem))
        .context("reading Talos client key")?
        .ok_or_else(|| anyhow::anyhow!("no private key found in key PEM"))?;

    let mut config = rustls::ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_client_auth_cert(chain, key)
        .context("building mTLS client config")?;
    config.alpn_protocols = vec![b"h2".to_vec()];
    Ok(config)
}

// ---------------------------------------------------------------------------
// TalosClient: gRPC client backed by the relay tunnel
// ---------------------------------------------------------------------------

pub struct TalosClient {
    inner: MachineServiceClient<Channel>,
}

impl TalosClient {
    /// Connect in maintenance mode: TLS with certificate verification skipped.
    ///
    /// Talos nodes in maintenance mode serve a self-signed certificate and
    /// accept any client connection before a machine config is applied.
    pub async fn connect_maintenance(relay: Arc<RelayState>) -> Result<Self> {
        let tls_config = maintenance_tls_config()?;
        Self::connect_with_tls(relay, tls_config).await
    }

    /// Connect with mutual TLS using talosconfig credentials.
    ///
    /// Used after bootstrap when the node has a full machine config. The
    /// Talos CA, client cert, and client key come from the talosconfig YAML
    /// stored in the cluster record.
    pub async fn connect_authenticated(
        relay: Arc<RelayState>,
        ca_pem: &[u8],
        cert_pem: &[u8],
        key_pem: &[u8],
    ) -> Result<Self> {
        let tls_config = mtls_config(ca_pem, cert_pem, key_pem)?;
        Self::connect_with_tls(relay, tls_config).await
    }

    async fn connect_with_tls(
        relay: Arc<RelayState>,
        tls_config: rustls::ClientConfig,
    ) -> Result<Self> {
        let connector = tokio_rustls::TlsConnector::from(Arc::new(tls_config));
        // "talos" is the SNI name; Talos ignores SNI in maintenance mode and
        // validates it against the cert's SAN in authenticated mode.
        let sni = ServerName::try_from("talos").map_err(|e| anyhow::anyhow!("invalid SNI: {e}"))?;

        // The dummy URI is passed to our connector but ignored — we always
        // route through the relay. The http:// scheme tells tonic to use h2c
        // rather than layering its own TLS (we handle TLS ourselves above).
        let endpoint = Endpoint::from_static("http://[::1]:50000");

        let channel = endpoint
            .connect_with_connector(service_fn(move |_| {
                let relay = relay.clone();
                let connector = connector.clone();
                let sni = sni.clone();
                async move {
                    let stream = relay.open_stream().await.map_err(|e| {
                        io::Error::new(io::ErrorKind::ConnectionRefused, e.to_string())
                    })?;
                    let compat = stream.compat();
                    let tls = connector.connect(sni, compat).await?;
                    Ok::<_, io::Error>(RelayIo(TokioIo::new(tls)))
                }
            }))
            .await
            .context("connecting to Talos through relay tunnel")?;

        Ok(Self {
            inner: MachineServiceClient::new(channel),
        })
    }

    // -----------------------------------------------------------------------
    // gRPC method wrappers
    // -----------------------------------------------------------------------

    /// Apply a Talos machine config to the node.
    ///
    /// `config_bytes` is the YAML machine configuration. `no_reboot` should
    /// be `true` when applying during initial bootstrap (maintenance mode
    /// always uses `NO_REBOOT` semantics internally, but setting it explicitly
    /// avoids unintended reboots on subsequent applies).
    pub async fn apply_configuration(
        &mut self,
        config_bytes: Vec<u8>,
        no_reboot: bool,
    ) -> Result<proto::machine::ApplyConfigurationResponse> {
        use proto::machine::apply_configuration_request::Mode;
        let mode = if no_reboot {
            Mode::NoReboot
        } else {
            Mode::Auto
        };
        let req = ApplyConfigurationRequest {
            data: config_bytes,
            mode: mode as i32,
            dry_run: false,
            try_mode_timeout: None,
        };
        self.inner
            .apply_configuration(req)
            .await
            .map(|r| r.into_inner())
            .map_err(|s| anyhow::anyhow!("ApplyConfiguration: {s}"))
    }

    /// Bootstrap etcd on the first control-plane node.
    ///
    /// Must be called exactly once, on the initial control-plane node, after
    /// `apply_configuration` has been applied and the node has rebooted into
    /// Talos (non-maintenance) mode.
    pub async fn bootstrap(&mut self) -> Result<BootstrapResponse> {
        let req = BootstrapRequest {
            recover_etcd: false,
            recover_skip_hash_check: false,
        };
        self.inner
            .bootstrap(req)
            .await
            .map(|r| r.into_inner())
            .map_err(|s| anyhow::anyhow!("Bootstrap: {s}"))
    }

    /// Retrieve the kubeconfig from the control-plane node.
    ///
    /// The RPC is server-streaming; this method collects all data chunks into
    /// a single byte vector containing the complete YAML kubeconfig.
    pub async fn kubeconfig(&mut self) -> Result<Vec<u8>> {
        let mut stream = self
            .inner
            .kubeconfig(())
            .await
            .map_err(|s| anyhow::anyhow!("Kubeconfig: {s}"))?
            .into_inner();

        let mut data = Vec::new();
        loop {
            match stream
                .message()
                .await
                .map_err(|s| anyhow::anyhow!("Kubeconfig stream: {s}"))?
            {
                Some(chunk) => data.extend_from_slice(&chunk.bytes),
                None => break,
            }
        }
        Ok(data)
    }
}
