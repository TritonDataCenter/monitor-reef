// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Kelp relay agent — runs inside the customer fabric zone.
//!
//! Establishes an outbound WebSocket to TritonAPI, presents the connection as a
//! yamux server, and for each inbound stream: reads the `host:port\n` target
//! header, dials that target on the fabric network, and bridges bytes.
//!
//! If the server closes the connection or is unreachable the agent reconnects
//! with exponential backoff (1s → 2s → 4s … capped at 30s).

use anyhow::{Context, Result};
use serde::Deserialize;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio_tungstenite::connect_async;
use tokio_util::compat::FuturesAsyncReadCompatExt as _;
use tracing::{error, info, warn};
use triton_relay_protocol::{WsCompat, bridge, read_connect_target};
use yamux::{Config, Connection, Mode};

#[derive(Deserialize)]
struct AgentConfig {
    relay_endpoint: String,
}

fn version_string() -> &'static str {
    concat!(
        "triton-relay-agent ",
        env!("CARGO_PKG_VERSION"),
        " (",
        env!("GIT_COMMIT_SHORT"),
        env!("GIT_DIRTY_SUFFIX"),
        ")"
    )
}

const BACKOFF_INITIAL: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(30);

/// Maximum time to wait for a new inbound yamux stream before probing the
/// connection by reconnecting. The underlying WebSocket may appear alive at the
/// TCP level while the server-side session has been discarded; this timeout
/// ensures we detect and recover from that state within a bounded window.
const IDLE_RECONNECT_TIMEOUT: Duration = Duration::from_secs(120);

#[tokio::main]
async fn main() -> Result<()> {
    if std::env::args().nth(1).as_deref() == Some("version") {
        println!("{}", version_string());
        return Ok(());
    }

    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(
            "triton_relay_agent=info",
        ))
        .init();

    let config_path = std::env::var("TRITON_RELAY_AGENT_CONFIG")
        .or_else(|_| {
            std::env::args()
                .nth(1)
                .ok_or(std::env::VarError::NotPresent)
        })
        .context(
            "provide config path via TRITON_RELAY_AGENT_CONFIG env var \
             or as the first CLI argument",
        )?;

    let config_text = tokio::fs::read_to_string(&config_path)
        .await
        .with_context(|| format!("read config from {config_path}"))?;
    let config: AgentConfig =
        serde_json::from_str(&config_text).context("parse agent config JSON")?;

    let mut backoff = BACKOFF_INITIAL;

    loop {
        info!("connecting to {}", config.relay_endpoint);
        match connect_async(&config.relay_endpoint).await {
            Ok((ws, _)) => {
                backoff = BACKOFF_INITIAL;
                let ws_compat = WsCompat::new(ws);
                let mut conn =
                    Connection::new(ws_compat, Config::default(), Mode::Server);
                info!("connected; waiting for streams");

                loop {
                    let next = tokio::time::timeout(
                        IDLE_RECONNECT_TIMEOUT,
                        std::future::poll_fn(|cx| conn.poll_next_inbound(cx)),
                    )
                    .await;
                    match next {
                        Ok(Some(Ok(stream))) => {
                            tokio::spawn(async move {
                                if let Err(e) = handle_stream(stream).await {
                                    warn!("stream handler error: {e}");
                                }
                            });
                        }
                        Ok(Some(Err(e))) => {
                            error!("yamux error: {e}");
                            break;
                        }
                        Ok(None) => {
                            info!("relay connection closed by server");
                            break;
                        }
                        Err(_elapsed) => {
                            // No stream activity for IDLE_RECONNECT_TIMEOUT. The
                            // TCP socket may be alive while the server-side session
                            // is gone. Reconnect to re-register with the server.
                            warn!(
                                timeout_secs = IDLE_RECONNECT_TIMEOUT.as_secs(),
                                "idle timeout: reconnecting to verify liveness"
                            );
                            break;
                        }
                    }
                }
            }
            Err(e) => {
                error!("connect failed: {e}");
            }
        }

        info!(
            delay_secs = backoff.as_secs(),
            "reconnecting after delay"
        );
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
}

async fn handle_stream(mut stream: yamux::Stream) -> Result<()> {
    let target = read_connect_target(&mut stream)
        .await
        .context("read connect target")?;

    info!("stream opened, dialing {target}");

    let mut tcp = TcpStream::connect(&target)
        .await
        .with_context(|| format!("TCP connect to {target}"))?;

    let mut stream_compat = stream.compat();
    let result = bridge(&mut stream_compat, &mut tcp).await;

    match result {
        Ok((a_to_b, b_to_a)) => {
            info!("stream to {target} closed: {a_to_b} bytes in, {b_to_a} bytes out");
        }
        Err(e) => {
            warn!("stream to {target} error: {e}");
        }
    }

    Ok(())
}
