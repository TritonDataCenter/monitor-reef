// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Kelp relay bridge — runs on the developer workstation.
//!
//! Opens a persistent WebSocket to TritonAPI's `/v1/k8s/relay/connect` endpoint
//! (yamux client side) and a local TCP listener.  For each accepted TCP
//! connection it opens a new yamux stream, writes the fabric target header, and
//! bridges bytes bidirectionally.
//!
//! Run two instances to expose both Talos and Kubernetes APIs simultaneously:
//!
//! ```text
//! triton-relay-bridge --relay-url ws://host:8080/v1/k8s/relay/connect --cluster <uuid> --listen 127.0.0.1:6443
//! triton-relay-bridge --relay-url ws://host:8080/v1/k8s/relay/connect --cluster <uuid> --listen 127.0.0.1:50000
//! ```
//!
//! The control-plane IP is fetched automatically from the relay-info endpoint;
//! the port is taken from `--listen`.

use anyhow::{Context, Result};
use clap::Parser;
use serde::Deserialize;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::connect_async;
use tokio_util::compat::FuturesAsyncReadCompatExt as _;
use tracing::{info, warn};
use triton_relay_protocol::{WsCompat, bridge, write_connect_target};
use yamux::{Config, Connection, Mode, Stream};

#[derive(Parser)]
#[command(about = "Local relay bridge for Kelp cluster access")]
struct Args {
    /// WebSocket URL of the relay connect endpoint
    /// (e.g. ws://tritonapi:8080/v1/k8s/relay/connect)
    #[arg(long)]
    relay_url: String,

    /// Cluster UUID — the control-plane IP is looked up automatically
    #[arg(long)]
    cluster: String,

    /// Local address to listen on (e.g. 127.0.0.1:6443)
    #[arg(long)]
    listen: String,
}

#[derive(Deserialize)]
struct RelayInfo {
    control_plane_ip: String,
}

fn version_string() -> &'static str {
    concat!(
        "triton-relay-bridge ",
        env!("CARGO_PKG_VERSION"),
        " (",
        env!("GIT_COMMIT_SHORT"),
        env!("GIT_DIRTY_SUFFIX"),
        ")"
    )
}

#[tokio::main]
async fn main() -> Result<()> {
    if std::env::args().nth(1).as_deref() == Some("version") {
        println!("{}", version_string());
        return Ok(());
    }

    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(
            "triton_relay_bridge=info",
        ))
        .init();

    // reqwest uses rustls; install a provider before any TLS handshake.
    let _ = rustls::crypto::ring::default_provider().install_default();

    let args = Args::parse();

    // Derive HTTP base URL from the WebSocket relay URL:
    // ws://host:port/... → http://host:port
    // wss://host:port/... → https://host:port
    let http_base = args
        .relay_url
        .replacen("wss://", "https://", 1)
        .replacen("ws://", "http://", 1);
    let http_base = http_base
        .split('/')
        .take(3)
        .collect::<Vec<_>>()
        .join("/");

    // Fetch the control-plane IP from the relay-info endpoint.
    let info_url = format!("{http_base}/v1/k8s/relay/info/{}", args.cluster);
    info!("fetching relay info from {info_url}");
    let relay_info: RelayInfo = reqwest::get(&info_url)
        .await
        .with_context(|| format!("GET {info_url}"))?
        .error_for_status()
        .context("relay info endpoint returned error")?
        .json()
        .await
        .context("deserialize relay info")?;

    // The target port matches --listen (same port, different host).
    let listen_port = args
        .listen
        .rsplit(':')
        .next()
        .context("--listen must be host:port")?;
    let target = format!("{}:{}", relay_info.control_plane_ip, listen_port);
    info!("resolved target: {target}");

    info!("connecting to relay at {}", args.relay_url);
    let (ws, _) = connect_async(&args.relay_url)
        .await
        .context("WebSocket connect to relay endpoint")?;

    let ws_compat = WsCompat::new(ws);
    let conn = Connection::new(ws_compat, Config::default(), Mode::Client);

    // Channel for requesting outbound yamux streams from the connection driver.
    let (stream_tx, stream_rx) = mpsc::channel::<oneshot::Sender<Result<Stream>>>(32);

    tokio::spawn(run_client_driver(conn, stream_rx));

    info!("listening on {}", args.listen);
    let listener = TcpListener::bind(&args.listen)
        .await
        .with_context(|| format!("bind on {}", args.listen))?;

    loop {
        let (tcp, peer) = listener.accept().await.context("TCP accept")?;
        info!("accepted connection from {peer}");

        let (reply_tx, reply_rx) = oneshot::channel();
        if stream_tx.send(reply_tx).await.is_err() {
            warn!("relay connection closed; stopping");
            break;
        }
        let stream = match reply_rx.await {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => {
                warn!("could not open yamux stream: {e}");
                continue;
            }
            Err(_) => {
                warn!("relay driver task gone; stopping");
                break;
            }
        };

        let target = target.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, tcp, &target).await {
                warn!("connection error: {e}");
            }
        });
    }

    Ok(())
}

/// Bridge a single accepted TCP connection through the yamux stream.
async fn handle_connection(mut stream: Stream, mut tcp: TcpStream, target: &str) -> Result<()> {
    write_connect_target(&mut stream, target)
        .await
        .context("write connect target")?;

    let mut stream_compat = stream.compat();
    let (a_to_b, b_to_a) = bridge(&mut stream_compat, &mut tcp)
        .await
        .context("bridge")?;

    info!("stream to {target} closed: {a_to_b} bytes bridge→target, {b_to_a} bytes target→bridge");
    Ok(())
}

/// Drive a yamux client connection.
///
/// Processes stream-open requests from `req_rx` via `poll_new_outbound` and
/// also drains inbound streams (the server should not open any, but we drive
/// the connection to process yamux keepalive frames).
async fn run_client_driver<T>(
    mut conn: Connection<T>,
    mut req_rx: mpsc::Receiver<oneshot::Sender<Result<Stream>>>,
) where
    T: futures_util::io::AsyncRead + futures_util::io::AsyncWrite + Unpin + Send + 'static,
{
    let mut pending: Option<oneshot::Sender<Result<Stream>>> = None;

    std::future::poll_fn(move |cx| {
        use std::task::Poll;

        // Accept a new stream-open request if none is pending.
        if pending.is_none() {
            match req_rx.poll_recv(cx) {
                Poll::Ready(Some(tx)) => pending = Some(tx),
                Poll::Ready(None) => return Poll::Ready(()),
                Poll::Pending => {}
            }
        }

        // Try to open the outbound stream.
        if pending.is_some() {
            match conn.poll_new_outbound(cx) {
                Poll::Ready(Ok(stream)) => {
                    if let Some(tx) = pending.take() {
                        let _ = tx.send(Ok(stream));
                    }
                }
                Poll::Ready(Err(e)) => {
                    if let Some(tx) = pending.take() {
                        let _ = tx.send(Err(anyhow::anyhow!("{e}")));
                    }
                    return Poll::Ready(());
                }
                Poll::Pending => {}
            }
        }

        // Drive inbound to process protocol frames; the relay server should
        // not open streams to the bridge, but we still need to run this for
        // keepalives and window-update acknowledgements.
        loop {
            match conn.poll_next_inbound(cx) {
                Poll::Ready(Some(Ok(s))) => drop(s),
                Poll::Ready(Some(Err(_))) | Poll::Ready(None) => return Poll::Ready(()),
                Poll::Pending => break,
            }
        }

        Poll::Pending
    })
    .await;

    info!("relay connection closed");
}
