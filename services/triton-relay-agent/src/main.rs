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

use anyhow::{Context, Result};
use serde::Deserialize;
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

    info!("connecting to {}", config.relay_endpoint);
    let (ws, _) = connect_async(&config.relay_endpoint)
        .await
        .context("WebSocket connect to relay endpoint")?;

    let ws_compat = WsCompat::new(ws);
    let mut conn = Connection::new(ws_compat, Config::default(), Mode::Server);

    info!("connected; waiting for streams");

    loop {
        let stream = std::future::poll_fn(|cx| conn.poll_next_inbound(cx)).await;
        match stream {
            Some(Ok(stream)) => {
                tokio::spawn(async move {
                    if let Err(e) = handle_stream(stream).await {
                        warn!("stream handler error: {e}");
                    }
                });
            }
            Some(Err(e)) => {
                error!("yamux error: {e}");
                break;
            }
            None => {
                info!("relay connection closed by server");
                break;
            }
        }
    }

    Ok(())
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
