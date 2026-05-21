// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Local relay bridge for Kelp cluster access.
//!
//! Connects to the triton-api-server relay WebSocket endpoint and listens
//! locally so kubectl (and other tools) can reach the cluster's Kubernetes
//! API through the relay tunnel without any direct fabric-network routing
//! from the developer workstation.

use anyhow::{Context, Result};
use clap::Args;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::connect_async;
use tokio_util::compat::FuturesAsyncReadCompatExt as _;
use tracing::{info, warn};
use triton_gateway_client::TypedClient;
use triton_relay_protocol::{WsCompat, bridge, write_connect_target};
use yamux::{Config, Connection, Mode, Stream};

#[derive(Args, Clone)]
pub struct RelayBridgeArgs {
    /// Cluster UUID, short ID prefix, or name
    pub cluster: String,

    /// Local address to listen on
    #[arg(long, default_value = "127.0.0.1:6443")]
    pub listen: String,

    /// Override the fabric target (default: derived from the cluster endpoint)
    #[arg(long)]
    pub target: Option<String>,
}

pub async fn run(args: RelayBridgeArgs, client: &TypedClient, base_url: &str) -> Result<()> {
    let cluster = super::resolve_cluster(&args.cluster, client).await?;

    let endpoint = cluster.endpoint.as_deref().ok_or_else(|| {
        anyhow::anyhow!(
            "cluster '{}' has no endpoint yet (still provisioning?)",
            cluster.name
        )
    })?;

    // Derive the fabric target from the cluster's stored endpoint URL.
    // endpoint is "https://<ip>:6443"; strip the scheme to get "<ip>:6443".
    let target = if let Some(t) = args.target {
        t
    } else {
        endpoint
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .to_string()
    };

    // Build the relay WebSocket URL from the base URL the CLI is pointed at.
    let relay_url = format!(
        "{}/v1/k8s/relay/connect",
        base_url.trim_end_matches('/').replace("http://", "ws://").replace("https://", "wss://")
    );

    info!("cluster '{}' endpoint: {}", cluster.name, endpoint);
    info!("relay: {}", relay_url);
    info!("target: {}", target);

    let (ws, _) = connect_async(&relay_url)
        .await
        .with_context(|| format!("WebSocket connect to {relay_url}"))?;

    let ws_compat = WsCompat::new(ws);
    let conn = Connection::new(ws_compat, Config::default(), Mode::Client);

    let (stream_tx, stream_rx) = mpsc::channel::<oneshot::Sender<Result<Stream>>>(32);
    tokio::spawn(run_client_driver(conn, stream_rx));

    info!("listening on {}", args.listen);
    let listener = TcpListener::bind(&args.listen)
        .await
        .with_context(|| format!("bind on {}", args.listen))?;

    eprintln!(
        "Relay bridge ready. kubectl is now routed through the Triton relay.\n  Local:  {}\n  Target: {}",
        args.listen, target
    );

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

async fn handle_connection(mut stream: Stream, mut tcp: TcpStream, target: &str) -> Result<()> {
    write_connect_target(&mut stream, target)
        .await
        .context("write connect target")?;

    let mut stream_compat = stream.compat();
    let (a_to_b, b_to_a) = bridge(&mut stream_compat, &mut tcp)
        .await
        .context("bridge")?;

    info!("stream to {target} closed: {a_to_b} bytes in, {b_to_a} bytes out");
    Ok(())
}

async fn run_client_driver<T>(
    mut conn: Connection<T>,
    mut req_rx: mpsc::Receiver<oneshot::Sender<Result<Stream>>>,
) where
    T: futures_util::io::AsyncRead + futures_util::io::AsyncWrite + Unpin + Send + 'static,
{
    let mut pending: Option<oneshot::Sender<Result<Stream>>> = None;

    std::future::poll_fn(move |cx| {
        use std::task::Poll;

        if pending.is_none() {
            match req_rx.poll_recv(cx) {
                Poll::Ready(Some(tx)) => pending = Some(tx),
                Poll::Ready(None) => return Poll::Ready(()),
                Poll::Pending => {}
            }
        }

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
