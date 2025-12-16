// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Instance VNC command
//!
//! Provides VNC console access for bhyve/KVM instances with built-in proxy support.
//!
//! ## Features
//!
//! - **TCP Proxy Mode (default)**: Listens on a local TCP port and bridges to CloudAPI
//!   WebSocket endpoint. Allows standard VNC clients (TigerVNC, RealVNC, etc.) to connect.
//!
//! - **WebSocket Proxy Mode (`--websocket`)**: Runs a local WebSocket server that bridges
//!   to CloudAPI. Allows browser-based noVNC clients to connect.
//!
//! - **URL-only Mode (`--url-only`)**: Just prints the WebSocket URL without starting a proxy.

use std::sync::Arc;

use anyhow::{Result, anyhow};
use axum::{
    Router,
    extract::ws::{Message as AxumMessage, WebSocket as AxumWebSocket},
    extract::{State, WebSocketUpgrade},
    response::IntoResponse,
    routing::get,
};
use clap::Args;
use cloudapi_client::{ClientInfo, TypedClient};
use futures_util::{SinkExt, StreamExt};
use http::Uri;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream,
    tungstenite::{
        Message,
        handshake::client::generate_key,
        protocol::WebSocketConfig,
    },
};

use super::get::resolve_instance;

#[derive(Args, Clone)]
pub struct VncArgs {
    /// Instance name, short ID, or UUID
    pub instance: String,

    /// Local TCP port for native VNC clients (default mode)
    #[arg(long, default_value = "5900")]
    pub port: u16,

    /// Start WebSocket server for browser-based noVNC instead of TCP proxy
    #[arg(long)]
    pub websocket: bool,

    /// Local WebSocket port for noVNC (used with --websocket)
    #[arg(long, default_value = "6080")]
    pub ws_port: u16,

    /// Just print the WebSocket URL, don't start proxy
    #[arg(long)]
    pub url_only: bool,

    /// Bind address (default: 127.0.0.1)
    #[arg(long, default_value = "127.0.0.1")]
    pub bind: String,
}

/// VNC connection info (returned for JSON output)
#[derive(serde::Serialize)]
pub struct VncInfo {
    pub instance: String,
    pub url: String,
}

/// State shared with WebSocket proxy handlers
struct WsProxyState {
    vnc_url: String,
    auth_config: triton_auth::AuthConfig,
}

pub async fn run(args: VncArgs, client: &TypedClient, json: bool) -> Result<()> {
    let account = &client.auth_config().account;
    let machine_id = resolve_instance(&args.instance, client).await?;

    // Build WebSocket URL
    let base_url = client.inner().baseurl();
    let ws_url = base_url
        .replace("https://", "wss://")
        .replace("http://", "ws://");
    let vnc_url = format!("{}/{}/machines/{}/vnc", ws_url, account, machine_id);

    // URL-only mode
    if args.url_only {
        if json {
            let info = VncInfo {
                instance: machine_id,
                url: vnc_url,
            };
            crate::output::json::print_json(&info)?;
        } else {
            println!("{}", vnc_url);
        }
        return Ok(());
    }

    // Choose proxy mode
    if args.websocket {
        run_websocket_mode(&args, vnc_url, client, json).await
    } else {
        run_tcp_mode(&args, vnc_url, client, json).await
    }
}

/// Run the TCP proxy mode for native VNC clients
async fn run_tcp_mode(
    args: &VncArgs,
    vnc_url: String,
    client: &TypedClient,
    _json: bool,
) -> Result<()> {
    let bind_addr = format!("{}:{}", args.bind, args.port);
    let listener = TcpListener::bind(&bind_addr).await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::AddrInUse {
            anyhow!(
                "Port {} is already in use. Try a different port with --port",
                args.port
            )
        } else {
            anyhow!("Failed to bind to {}: {}", bind_addr, e)
        }
    })?;

    println!("VNC proxy listening on {}", bind_addr);
    println!("Connect your VNC client to: vnc://localhost:{}", args.port);
    println!("Press Ctrl+C to stop");
    println!();

    // Accept connections in a loop with Ctrl+C handling
    loop {
        tokio::select! {
            result = listener.accept() => {
                let (tcp_stream, peer_addr) = result?;
                println!("Connection from {}", peer_addr);

                // Connect to CloudAPI WebSocket
                println!("Connecting to CloudAPI VNC endpoint...");
                match connect_authenticated_websocket(&vnc_url, client.auth_config()).await {
                    Ok(ws_stream) => {
                        println!("Connected! Bridging VNC traffic...");

                        // Bridge until disconnect
                        match bridge_tcp_websocket(tcp_stream, ws_stream).await {
                            Ok(()) => println!("Connection closed"),
                            Err(e) => println!("Connection error: {}", e),
                        }
                    }
                    Err(e) => {
                        eprintln!("Failed to connect to CloudAPI: {}", e);
                    }
                }

                println!("Ready for new connection...");
            }
            _ = tokio::signal::ctrl_c() => {
                println!("\nShutting down VNC proxy");
                return Ok(());
            }
        }
    }
}

/// Run the WebSocket proxy mode for noVNC browser clients
async fn run_websocket_mode(
    args: &VncArgs,
    vnc_url: String,
    client: &TypedClient,
    _json: bool,
) -> Result<()> {
    let state = Arc::new(WsProxyState {
        vnc_url: vnc_url.clone(),
        auth_config: client.auth_config().clone(),
    });

    // noVNC connects to /websockify by default, but also support root path
    let app = Router::new()
        .route("/", get(ws_handler))
        .route("/websockify", get(ws_handler))
        .with_state(state);

    let bind_addr = format!("{}:{}", args.bind, args.ws_port);
    let listener = TcpListener::bind(&bind_addr).await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::AddrInUse {
            anyhow!(
                "Port {} is already in use. Try a different port with --ws-port",
                args.ws_port
            )
        } else {
            anyhow!("Failed to bind to {}: {}", bind_addr, e)
        }
    })?;

    println!("VNC WebSocket proxy listening on ws://{}", bind_addr);
    println!();
    println!("Open in browser:");
    println!(
        "  https://novnc.com/noVNC/vnc.html?host={}&port={}&path=websockify&encrypt=false",
        args.bind, args.ws_port
    );
    println!();
    println!("Press Ctrl+C to stop");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}

/// WebSocket upgrade handler for noVNC clients
async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<WsProxyState>>,
) -> impl IntoResponse {
    // Accept binary subprotocol for VNC
    ws.protocols(["binary"])
        .on_upgrade(move |socket| handle_ws_connection(socket, state))
}

/// Handle a WebSocket connection from a browser client
async fn handle_ws_connection(browser_ws: AxumWebSocket, state: Arc<WsProxyState>) {
    println!("Browser connected via WebSocket");

    // Connect to CloudAPI WebSocket
    let cloudapi_ws =
        match connect_authenticated_websocket(&state.vnc_url, &state.auth_config).await {
            Ok(ws) => ws,
            Err(e) => {
                eprintln!("Failed to connect to CloudAPI: {}", e);
                return;
            }
        };

    println!("Connected to CloudAPI, bridging...");

    // Bridge the two WebSocket connections
    if let Err(e) = bridge_websockets(browser_ws, cloudapi_ws).await {
        eprintln!("WebSocket bridge error: {}", e);
    }

    println!("Browser disconnected");
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
    // Include binary subprotocol for VNC - some servers require this
    let request = http::Request::builder()
        .uri(ws_url)
        .header("Host", host)
        .header("Date", &date_header)
        .header("Authorization", &auth_header)
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Version", "13")
        .header("Sec-WebSocket-Key", generate_key())
        .header("Sec-WebSocket-Protocol", "binary")
        .body(())?;

    // Configure WebSocket with larger limits for VNC traffic
    // VNC can send large frames, and some servers may not strictly follow RFC limits
    let mut ws_config = WebSocketConfig::default();
    ws_config.max_frame_size = Some(16 * 1024 * 1024); // 16 MB max frame
    ws_config.max_message_size = Some(64 * 1024 * 1024); // 64 MB max message
    ws_config.accept_unmasked_frames = true; // Server-to-client frames shouldn't be masked per RFC

    // Connect with timeout
    let connect_future =
        tokio_tungstenite::connect_async_with_config(request, Some(ws_config), false);
    let (ws_stream, _response) =
        tokio::time::timeout(std::time::Duration::from_secs(30), connect_future)
            .await
            .map_err(|_| anyhow!("Connection timeout after 30 seconds"))??;

    Ok(ws_stream)
}

/// Bridge TCP stream to WebSocket stream (for native VNC clients)
async fn bridge_tcp_websocket(
    tcp_stream: TcpStream,
    ws_stream: WebSocketStream<MaybeTlsStream<TcpStream>>,
) -> Result<()> {
    let (mut tcp_read, mut tcp_write) = tcp_stream.into_split();
    let (mut ws_write, mut ws_read) = ws_stream.split();

    // TCP -> WebSocket task
    let tcp_to_ws = async {
        let mut buf = vec![0u8; 65536];
        loop {
            let n = tcp_read.read(&mut buf).await?;
            if n == 0 {
                break; // TCP closed
            }
            // Copy the data to avoid borrow issues
            let data = buf[..n].to_vec();
            ws_write.send(Message::Binary(data.into())).await?;
        }
        ws_write.close().await?;
        Ok::<_, anyhow::Error>(())
    };

    // WebSocket -> TCP task
    let ws_to_tcp = async {
        while let Some(msg) = ws_read.next().await {
            match msg? {
                Message::Binary(data) => {
                    tcp_write.write_all(&data).await?;
                }
                Message::Text(text) => {
                    // VNC handshake may come as text frames
                    tcp_write.write_all(text.as_bytes()).await?;
                }
                Message::Close(_) => break,
                _ => {} // Ignore ping/pong
            }
        }
        Ok::<_, anyhow::Error>(())
    };

    // Run both directions concurrently
    tokio::select! {
        result = tcp_to_ws => result?,
        result = ws_to_tcp => result?,
    }

    Ok(())
}

/// Bridge two WebSocket connections (for noVNC browser clients)
async fn bridge_websockets(
    browser_ws: AxumWebSocket,
    cloudapi_ws: WebSocketStream<MaybeTlsStream<TcpStream>>,
) -> Result<()> {
    let (mut browser_tx, mut browser_rx) = browser_ws.split();
    let (mut cloudapi_tx, mut cloudapi_rx) = cloudapi_ws.split();

    // Browser -> CloudAPI
    let browser_to_cloudapi = async {
        while let Some(msg) = browser_rx.next().await {
            match msg? {
                AxumMessage::Binary(data) => {
                    cloudapi_tx
                        .send(Message::Binary(data.to_vec().into()))
                        .await?;
                }
                AxumMessage::Close(_) => break,
                _ => {}
            }
        }
        Ok::<_, anyhow::Error>(())
    };

    // CloudAPI -> Browser
    let cloudapi_to_browser = async {
        while let Some(msg) = cloudapi_rx.next().await {
            match msg? {
                Message::Binary(data) => {
                    browser_tx.send(AxumMessage::Binary(data)).await?;
                }
                Message::Close(_) => break,
                _ => {}
            }
        }
        Ok::<_, anyhow::Error>(())
    };

    tokio::select! {
        result = browser_to_cloudapi => result?,
        result = cloudapi_to_browser => result?,
    }

    Ok(())
}

/// Wait for Ctrl+C shutdown signal
async fn shutdown_signal() {
    tokio::signal::ctrl_c().await.ok();
    println!("\nShutting down WebSocket proxy");
}
