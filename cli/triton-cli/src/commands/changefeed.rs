// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Changefeed command for real-time VM updates
//!
//! Subscribe to CloudAPI's feed of VM changes via WebSocket.

use anyhow::{Result, anyhow};
use clap::Args;
use futures_util::{SinkExt, StreamExt};
use http::Uri;
use tokio::net::TcpStream;
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream,
    tungstenite::{Message, handshake::client::generate_key, protocol::WebSocketConfig},
};
use triton_gateway_client::{
    ChangefeedMessage, ChangefeedResource, ChangefeedSubResource, ChangefeedSubscription,
    ClientInfo, TypedClient,
};

use crate::output::enum_to_display;

#[derive(Args, Clone)]
pub struct ChangefeedArgs {
    /// Filter to specific instance UUIDs (comma-separated)
    #[arg(long, value_delimiter = ',')]
    pub instances: Vec<String>,
}

pub async fn run(args: ChangefeedArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let account = client.effective_account();

    // Build WebSocket URL
    let base_url = client.inner().baseurl();
    let ws_url = base_url
        .replace("https://", "wss://")
        .replace("http://", "ws://");
    let changefeed_url = format!("{}/{}/changefeed", ws_url, account);

    // Connect to CloudAPI WebSocket
    println!("Connecting to CloudAPI changefeed...");
    let ssh_cfg = client.ssh_auth_config().ok_or_else(|| {
        anyhow!(
            "`triton changefeed` requires an SSH-key profile; Bearer/JWT profiles \
             are not supported for WebSocket upgrade."
        )
    })?;
    let mut ws_stream = connect_authenticated_websocket(&changefeed_url, ssh_cfg).await?;
    println!("Connected. Subscribing to VM changes...");

    // Send subscription message
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

    // Handle incoming messages with Ctrl+C support
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
        // Raw JSON output
        println!("{}", text);
    } else {
        // Parse and format nicely
        let msg: ChangefeedMessage = serde_json::from_str(text)?;

        // Format timestamp (published is a string containing millisecond Unix timestamp)
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

/// Connect to the CloudAPI WebSocket endpoint with HTTP signature authentication
async fn connect_authenticated_websocket(
    ws_url: &str,
    auth_config: &triton_auth::AuthConfig,
) -> Result<WebSocketStream<MaybeTlsStream<TcpStream>>> {
    let uri: Uri = ws_url.parse()?;

    let host = uri
        .host()
        .ok_or_else(|| anyhow!("No host in WebSocket URL"))?;
    let path = uri.path_and_query().map(|pq| pq.as_str()).unwrap_or("/");

    // Sign the request using triton-auth
    let (date_header, auth_header) = triton_auth::sign_request(auth_config, "GET", path).await?;

    // Build WebSocket request with auth headers
    let request = http::Request::builder()
        .uri(ws_url)
        .header("Host", host)
        .header("Date", &date_header)
        .header("Authorization", &auth_header)
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Version", "13")
        .header("Sec-WebSocket-Key", generate_key())
        .body(())?;

    // Configure WebSocket
    let ws_config = WebSocketConfig::default();

    // Connect with timeout
    let connect_future =
        tokio_tungstenite::connect_async_with_config(request, Some(ws_config), false);
    let (ws_stream, _response) =
        tokio::time::timeout(std::time::Duration::from_secs(30), connect_future)
            .await
            .map_err(|_| anyhow!("Connection timeout after 30 seconds"))??;

    Ok(ws_stream)
}
