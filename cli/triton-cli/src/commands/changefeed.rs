// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Changefeed command for real-time VM updates
//!
//! Subscribe to CloudAPI's feed of VM changes via WebSocket. The upgrade
//! request is authenticated out-of-band (HTTP Signature for SSH profiles,
//! Bearer JWT for tritonapi profiles); once the connection is upgraded,
//! the WS traffic itself isn't re-authenticated, so token expiry
//! mid-stream doesn't drop the connection — the server closes it
//! whenever it sees fit.

use anyhow::{Result, anyhow};
use clap::Args;
use cloudapi_client::{
    ChangefeedMessage, ChangefeedResource, ChangefeedSubResource, ChangefeedSubscription,
};
use futures_util::{SinkExt, StreamExt};
use http::Uri;
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio_tungstenite::{
    Connector, MaybeTlsStream, WebSocketStream,
    tungstenite::{Message, handshake::client::generate_key, protocol::WebSocketConfig},
};

use crate::client::AnyClient;
use crate::output::enum_to_display;

#[derive(Args, Clone)]
pub struct ChangefeedArgs {
    /// Filter to specific instance UUIDs (comma-separated)
    #[arg(long, value_delimiter = ',')]
    pub instances: Vec<String>,
}

pub async fn run(args: ChangefeedArgs, client: &AnyClient, use_json: bool) -> Result<()> {
    let account = client.effective_account();

    let base_url = client.baseurl();
    let ws_url = base_url
        .replace("https://", "wss://")
        .replace("http://", "ws://");
    let changefeed_url = format!("{}/{}/changefeed", ws_url, account);

    // Compute the auth headers for the WS upgrade via the shared
    // WebsocketAuth helper — SSH profiles produce an HTTP Signature,
    // tritonapi profiles produce a Bearer JWT.
    let uri: Uri = changefeed_url.parse()?;
    let path = uri.path_and_query().map(|pq| pq.as_str()).unwrap_or("/");
    let (date, authorization) = client.websocket_auth().headers(path).await?;
    let insecure = client.insecure();

    println!("Connecting to changefeed at {}...", base_url);
    let mut ws_stream =
        connect_authenticated_websocket(&changefeed_url, date.as_deref(), &authorization, insecure)
            .await?;
    println!("Connected. Subscribing to VM changes...");

    let vms = if args.instances.is_empty() {
        None
    } else {
        let parsed: Vec<uuid::Uuid> = args
            .instances
            .iter()
            .map(|s| {
                s.parse()
                    .map_err(|_| anyhow!("Invalid instance UUID: {}", s))
            })
            .collect::<Result<_>>()?;
        Some(parsed)
    };
    let subscription = ChangefeedSubscription {
        resource: ChangefeedResource::Vm,
        sub_resources: vec![
            ChangefeedSubResource::Alias,
            ChangefeedSubResource::CustomerMetadata,
            ChangefeedSubResource::Destroyed,
            ChangefeedSubResource::Nics,
            ChangefeedSubResource::OwnerUuid,
            ChangefeedSubResource::ServerUuid,
            ChangefeedSubResource::State,
            ChangefeedSubResource::Tags,
        ],
        vms,
    };

    let sub_json = serde_json::to_string(&subscription)?;
    ws_stream.send(Message::Text(sub_json.into())).await?;

    if args.instances.is_empty() {
        println!("Subscribed to all VM changes for account {}.", account);
    } else {
        println!(
            "Subscribed to changes for {} instance(s).",
            args.instances.len()
        );
    }
    println!("Press Ctrl+C to stop");
    println!();

    loop {
        tokio::select! {
            msg = ws_stream.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        handle_message(&text, use_json)?;
                    }
                    Some(Ok(Message::Close(_))) => {
                        println!("Connection closed by server");
                        break;
                    }
                    Some(Ok(_)) => {
                        // Ignore ping/pong and binary messages
                    }
                    Some(Err(e)) => {
                        tracing::error!("WebSocket error: {}", e);
                        break;
                    }
                    None => {
                        println!("Connection closed");
                        break;
                    }
                }
            }
            _ = tokio::signal::ctrl_c() => {
                println!("\nShutting down changefeed");
                ws_stream.close(None).await.ok();
                break;
            }
        }
    }

    Ok(())
}

/// Handle a changefeed message
fn handle_message(text: &str, use_json: bool) -> Result<()> {
    if use_json {
        println!("{}", text);
    } else {
        let msg: ChangefeedMessage = serde_json::from_str(text)?;
        let timestamp = msg
            .published
            .parse::<i64>()
            .ok()
            .and_then(chrono::DateTime::from_timestamp_millis)
            .map(|dt| dt.to_rfc3339())
            .unwrap_or_else(|| msg.published.clone());

        let id_str = msg.changed_resource_id.to_string();
        let short_id = &id_str[..8];

        let sub_resources: Vec<String> = msg
            .change_kind
            .sub_resources
            .iter()
            .map(enum_to_display)
            .collect();

        println!("Change ({}) =>", timestamp);
        println!("  modified: {}", sub_resources.join(", "));
        println!("  state: {}", msg.resource_state);
        println!("  object: {}", short_id);
    }

    Ok(())
}

/// Connect to the changefeed WebSocket with pre-computed auth headers and
/// a TLS connector that honors the profile's `insecure` flag.
async fn connect_authenticated_websocket(
    ws_url: &str,
    date: Option<&str>,
    authorization: &str,
    insecure: bool,
) -> Result<WebSocketStream<MaybeTlsStream<TcpStream>>> {
    let uri: Uri = ws_url.parse()?;
    let host = uri
        .host()
        .ok_or_else(|| anyhow!("No host in WebSocket URL"))?;

    let mut builder = http::Request::builder()
        .uri(ws_url)
        .header("Host", host)
        .header("Authorization", authorization)
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Version", "13")
        .header("Sec-WebSocket-Key", generate_key());
    if let Some(date) = date {
        builder = builder.header("Date", date);
    }
    let request = builder.body(())?;

    let ws_config = WebSocketConfig::default();

    let connector = {
        let tls_config = triton_tls::build_rustls_client_config(insecure).await;
        Some(Connector::Rustls(Arc::new(tls_config)))
    };

    let connect_future = tokio_tungstenite::connect_async_tls_with_config(
        request,
        Some(ws_config),
        false,
        connector,
    );
    let (ws_stream, _response) =
        tokio::time::timeout(std::time::Duration::from_secs(30), connect_future)
            .await
            .map_err(|_| anyhow!("Connection timeout after 30 seconds"))??;

    Ok(ws_stream)
}
