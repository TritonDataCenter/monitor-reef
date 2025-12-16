<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

Copyright 2025 Edgecast Cloud LLC.
-->

# VNC Proxy Implementation Plan

**Date:** 2025-12-16
**Status:** ✅ COMPLETE
**Priority:** P2 Enhancement
**Related:** Instance VNC command - fully implemented with proxy support

## Overview

Enhance the `triton instance vnc` command to include a built-in TCP-to-WebSocket proxy, allowing users to connect standard VNC clients (like TigerVNC, RealVNC, macOS Screen Sharing) directly to their bhyve/KVM instances without needing external tools like noVNC or websockify.

## Implementation Summary

The VNC proxy has been fully implemented in `cli/triton-cli/src/commands/instance/vnc.rs`.

### Features Implemented

1. **TCP Proxy Mode (default)**: Listens on a local TCP port (default 5900) and bridges to CloudAPI WebSocket endpoint
2. **WebSocket Proxy Mode (`--websocket`)**: Runs a local WebSocket server for browser-based noVNC clients
3. **URL-only Mode (`--url-only`)**: Just prints the WebSocket URL for scripting
4. **HTTP Signature Authentication**: Automatically signs WebSocket upgrade requests
5. **Graceful Shutdown**: Ctrl+C handling with clean connection teardown

### Command Interface

```bash
# Start TCP proxy for native VNC clients (default behavior)
$ triton instance vnc myvm
VNC proxy listening on localhost:5900
Connect your VNC client to: localhost:5900
Press Ctrl+C to stop

# Specify custom port
$ triton instance vnc myvm --port 5901

# Start WebSocket proxy for browser-based noVNC
$ triton instance vnc myvm --websocket
VNC WebSocket proxy listening on ws://localhost:6080
Open in browser: https://novnc.com/noVNC/vnc.html?host=localhost&port=6080&encrypt=false
Press Ctrl+C to stop

# WebSocket mode with custom port
$ triton instance vnc myvm --websocket --ws-port 6081

# Just print URL (current behavior, for scripting/external tools)
$ triton instance vnc myvm --url-only
wss://cloudapi.example.com/myaccount/machines/abc123/vnc

# Open VNC client automatically (optional, platform-dependent)
$ triton instance vnc myvm --open
```

---

## Technical Design

### Architecture

**TCP Proxy Mode (default)** - For native VNC clients:

```
┌─────────────┐     TCP      ┌─────────────┐   WebSocket   ┌─────────────┐
│  VNC Client │◄────────────►│ triton vnc  │◄─────────────►│  CloudAPI   │
│ (TigerVNC)  │  localhost   │   proxy     │   wss://...   │  VNC WS EP  │
└─────────────┘    :5900     └─────────────┘               └─────────────┘
```

**WebSocket Proxy Mode (`--websocket`)** - For browser-based noVNC:

```
┌─────────────┐   WebSocket   ┌─────────────┐   WebSocket   ┌─────────────┐
│   Browser   │◄─────────────►│ triton vnc  │◄─────────────►│  CloudAPI   │
│   (noVNC)   │  ws://local   │  WS server  │   wss://...   │  VNC WS EP  │
└─────────────┘    :6080      └─────────────┘               └─────────────┘
```

### Components

#### 1. TCP Listener
- Bind to `127.0.0.1:<port>` (localhost only for security)
- Accept single connection (VNC is point-to-point)
- Option to accept multiple sequential connections

#### 2. WebSocket Client
- Connect to CloudAPI VNC WebSocket endpoint
- Include HTTP signature authentication headers
- Handle TLS (wss://)
- Handle WebSocket upgrade handshake

#### 3. Bidirectional Bridge (TCP Mode)
- Forward TCP → WebSocket (binary frames)
- Forward WebSocket → TCP (binary frames)
- Handle connection close from either side
- Clean shutdown on Ctrl+C

#### 4. WebSocket Server (WebSocket Mode)
- Listen on `127.0.0.1:<ws-port>` (default 6080)
- Accept WebSocket upgrade requests from browsers
- Bridge browser WebSocket ↔ CloudAPI WebSocket
- Forward binary frames bidirectionally
- Print noVNC URL for easy browser access

### Authentication

CloudAPI uses HTTP signature authentication. For WebSocket connections, we need to sign the initial HTTP upgrade request:

```http
GET /myaccount/machines/abc123/vnc HTTP/1.1
Host: cloudapi.example.com
Upgrade: websocket
Connection: Upgrade
Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==
Sec-WebSocket-Version: 13
Date: Mon, 16 Dec 2025 12:00:00 GMT
Authorization: Signature keyId="/myaccount/keys/my-key",algorithm="rsa-sha256",headers="date",signature="..."
```

The `triton-auth` crate already provides HTTP signature generation. We need to:
1. Build the WebSocket upgrade request manually
2. Sign it using the existing auth infrastructure
3. Pass signed headers to the WebSocket client

### Binary Protocol Handling

VNC uses a binary protocol. WebSocket frames must be:
- Type: Binary (not Text)
- No modification to payload (pass-through)
- Preserve message boundaries where possible

---

## Implementation Plan

### Phase 1: Dependencies and Structure

#### 1.1 Add Dependencies to `cli/triton-cli/Cargo.toml`

```toml
[dependencies]
tokio-tungstenite = { version = "0.21", features = ["native-tls"] }
futures-util = "0.3"
http = "1.0"  # For building HTTP requests
```

#### 1.2 Restructure VNC Module

```
cli/triton-cli/src/commands/instance/
├── vnc.rs          # Main command and args
└── vnc/
    ├── mod.rs      # Re-exports
    ├── proxy.rs    # TCP-WebSocket proxy logic
    └── auth.rs     # WebSocket authentication helpers
```

Or keep it simple in a single file if complexity is manageable.

### Phase 2: Core Implementation

#### 2.1 Update VncArgs

```rust
#[derive(Args, Clone)]
pub struct VncArgs {
    /// Instance name, short ID, or UUID
    pub instance: String,

    /// Local TCP port for native VNC clients (default mode)
    #[arg(long, short, default_value = "5900")]
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
```

#### 2.2 WebSocket Connection with Authentication

```rust
use http::Request;
use tokio_tungstenite::tungstenite::handshake::client::generate_key;

async fn connect_authenticated_websocket(
    ws_url: &str,
    client: &TypedClient,
) -> Result<WebSocketStream<...>> {
    let uri: Uri = ws_url.parse()?;

    // Build the upgrade request
    let host = uri.host().unwrap();
    let path = uri.path();

    // Get current date for signing
    let date = chrono::Utc::now().format("%a, %d %b %Y %H:%M:%S GMT").to_string();

    // Sign the request using triton-auth
    let auth_header = client.auth_config().sign_request("GET", path, &date)?;

    // Build WebSocket request with auth headers
    let request = Request::builder()
        .uri(ws_url)
        .header("Host", host)
        .header("Date", &date)
        .header("Authorization", auth_header)
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Version", "13")
        .header("Sec-WebSocket-Key", generate_key())
        .body(())?;

    let (ws_stream, _response) = tokio_tungstenite::connect_async(request).await?;
    Ok(ws_stream)
}
```

#### 2.3 TCP-WebSocket Bridge

```rust
use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_tungstenite::WebSocketStream;
use tungstenite::Message;

async fn bridge_tcp_websocket(
    tcp_stream: TcpStream,
    ws_stream: WebSocketStream<...>,
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
            ws_write.send(Message::Binary(buf[..n].to_vec())).await?;
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
                Message::Close(_) => break,
                _ => {} // Ignore ping/pong/text
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
```

#### 2.4 Main Proxy Loop

```rust
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
            json::print_json(&serde_json::json!({"url": vnc_url}))?;
        } else {
            println!("{}", vnc_url);
        }
        return Ok(());
    }

    // Start proxy
    let bind_addr = format!("{}:{}", args.bind, args.port);
    let listener = TcpListener::bind(&bind_addr).await?;

    println!("VNC proxy listening on {}", bind_addr);
    println!("Connect your VNC client to: vnc://localhost:{}", args.port);
    println!("Press Ctrl+C to stop");
    println!();

    // Accept connections in a loop
    loop {
        let (tcp_stream, peer_addr) = listener.accept().await?;
        println!("Connection from {}", peer_addr);

        // Connect to CloudAPI WebSocket
        println!("Connecting to CloudAPI VNC endpoint...");
        let ws_stream = connect_authenticated_websocket(&vnc_url, client).await?;
        println!("Connected! Bridging VNC traffic...");

        // Bridge until disconnect
        match bridge_tcp_websocket(tcp_stream, ws_stream).await {
            Ok(()) => println!("Connection closed"),
            Err(e) => println!("Connection error: {}", e),
        }

        println!("Ready for new connection...");
    }
}
```

#### 2.5 WebSocket Server for noVNC Mode

```rust
use axum::{
    extract::{State, WebSocketUpgrade},
    response::IntoResponse,
    routing::get,
    Router,
};
use axum::extract::ws::{Message as AxumMessage, WebSocket as AxumWebSocket};

struct WsProxyState {
    vnc_url: String,
    client: TypedClient,
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<WsProxyState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws_connection(socket, state))
}

async fn handle_ws_connection(browser_ws: AxumWebSocket, state: Arc<WsProxyState>) {
    println!("Browser connected via WebSocket");

    // Connect to CloudAPI WebSocket
    let cloudapi_ws = match connect_authenticated_websocket(&state.vnc_url, &state.client).await {
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

async fn bridge_websockets(
    browser_ws: AxumWebSocket,
    cloudapi_ws: WebSocketStream<...>,
) -> Result<()> {
    let (mut browser_tx, mut browser_rx) = browser_ws.split();
    let (mut cloudapi_tx, mut cloudapi_rx) = cloudapi_ws.split();

    // Browser -> CloudAPI
    let browser_to_cloudapi = async {
        while let Some(msg) = browser_rx.next().await {
            match msg? {
                AxumMessage::Binary(data) => {
                    cloudapi_tx.send(Message::Binary(data)).await?;
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

pub async fn run_websocket_mode(args: &VncArgs, vnc_url: String, client: &TypedClient) -> Result<()> {
    let state = Arc::new(WsProxyState {
        vnc_url: vnc_url.clone(),
        client: client.clone(),
    });

    let app = Router::new()
        .route("/", get(ws_handler))
        .with_state(state);

    let bind_addr = format!("{}:{}", args.bind, args.ws_port);
    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;

    println!("VNC WebSocket proxy listening on ws://{}", bind_addr);
    println!();
    println!("Open in browser:");
    println!("  https://novnc.com/noVNC/vnc.html?host={}&port={}&encrypt=false",
             args.bind, args.ws_port);
    println!();
    println!("Press Ctrl+C to stop");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}

async fn shutdown_signal() {
    tokio::signal::ctrl_c().await.ok();
    println!("\nShutting down WebSocket proxy");
}
```

#### 2.6 Updated Main Entry Point

```rust
pub async fn run(args: VncArgs, client: &TypedClient, json: bool) -> Result<()> {
    let account = &client.auth_config().account;
    let machine_id = resolve_instance(&args.instance, client).await?;

    // Validate instance
    let machine = get_machine(&machine_id, client).await?;
    if machine.state != MachineState::Running {
        return Err(anyhow!("Instance must be running for VNC access"));
    }

    // Build WebSocket URL
    let base_url = client.inner().baseurl();
    let ws_url = base_url
        .replace("https://", "wss://")
        .replace("http://", "ws://");
    let vnc_url = format!("{}/{}/machines/{}/vnc", ws_url, account, machine_id);

    // URL-only mode
    if args.url_only {
        if json {
            json::print_json(&serde_json::json!({"url": vnc_url}))?;
        } else {
            println!("{}", vnc_url);
        }
        return Ok(());
    }

    // Choose proxy mode
    if args.websocket {
        run_websocket_mode(&args, vnc_url, client).await
    } else {
        run_tcp_mode(&args, vnc_url, client).await
    }
}
```

### Phase 3: Error Handling and Polish

#### 3.1 Graceful Shutdown

Handle Ctrl+C gracefully:
```rust
tokio::select! {
    result = accept_loop() => result,
    _ = tokio::signal::ctrl_c() => {
        println!("\nShutting down VNC proxy");
        Ok(())
    }
}
```

#### 3.2 Error Messages

Provide helpful error messages:
- "Port already in use" - suggest different port
- "WebSocket connection failed" - check instance is running, is bhyve/KVM
- "Authentication failed" - check profile credentials

#### 3.3 Instance Validation

Before starting proxy, verify:
- Instance exists
- Instance is running
- Instance is bhyve or KVM brand (VNC only works for hardware VMs)

```rust
let machine = get_machine(&machine_id, client).await?;
if machine.state != MachineState::Running {
    return Err(anyhow!("Instance must be running for VNC access"));
}
match machine.brand {
    Brand::Bhyve | Brand::Kvm => {},
    _ => return Err(anyhow!("VNC is only available for bhyve/KVM instances")),
}
```

### Phase 4: Optional Enhancements

#### 4.1 Auto-Open VNC Client (Optional)

```rust
#[arg(long)]
pub open: bool,

// After proxy starts:
if args.open {
    let vnc_uri = format!("vnc://localhost:{}", args.port);
    open::that(&vnc_uri)?; // Uses system default VNC handler
}
```

Requires adding `open` crate dependency.

#### 4.2 Connection Timeout

Add timeout for WebSocket connection:
```rust
#[arg(long, default_value = "30")]
pub timeout: u64,

tokio::time::timeout(
    Duration::from_secs(args.timeout),
    connect_authenticated_websocket(&vnc_url, client)
).await??
```

#### 4.3 Single-Connection Mode

Exit after first connection closes (useful for scripting):
```rust
#[arg(long)]
pub once: bool,

if args.once {
    break; // Exit after first connection
}
```

---

## Testing Plan

### Unit Tests

1. **URL construction** - Verify WebSocket URL is built correctly
2. **Auth header generation** - Verify HTTP signatures are valid

### Integration Tests

1. **Mock WebSocket server** - Test proxy with a mock VNC-over-WebSocket server
2. **Connection lifecycle** - Test connect, data transfer, disconnect

### Manual Testing

1. **Against real CloudAPI** - Test with actual bhyve/KVM instance
2. **TCP Proxy Mode - Various VNC clients**:
   - TigerVNC
   - RealVNC
   - macOS Screen Sharing
   - Remmina (Linux)
3. **WebSocket Proxy Mode - Browser testing**:
   - noVNC via novnc.com hosted client
   - Chrome, Firefox, Safari compatibility
   - Test connection lifecycle (connect, use, disconnect)
4. **Error scenarios**:
   - Instance not running
   - Instance is a zone (not KVM)
   - Invalid credentials
   - Network interruption
   - Port already in use

---

## Security Considerations

1. **Localhost binding by default** - Prevents external access to proxy
2. **Single connection** - VNC is point-to-point, don't allow multiple clients
3. **No credential caching** - Use existing profile authentication
4. **TLS validation** - Ensure WebSocket connection validates server certificates

---

## Dependencies

### New Crate Dependencies

| Crate | Version | Purpose |
|-------|---------|---------|
| `tokio-tungstenite` | 0.21+ | WebSocket client (for CloudAPI connection) |
| `axum` | 0.7+ | WebSocket server (for noVNC mode) |
| `futures-util` | 0.3 | Stream utilities |
| `http` | 1.0 | HTTP request building |
| `open` | 5.0 | (Optional) Open VNC client |

### Existing Dependencies Used

- `tokio` - Async runtime, TCP listener
- `triton-auth` - HTTP signature authentication
- `anyhow` - Error handling

---

## File Changes Summary

| File | Change |
|------|--------|
| `cli/triton-cli/Cargo.toml` | Add tokio-tungstenite, axum, futures-util, http |
| `cli/triton-cli/src/commands/instance/vnc.rs` | Rewrite with TCP and WebSocket proxy functionality |
| `libs/triton-auth/src/lib.rs` | May need to expose signing for custom requests |

---

## Estimated Effort

| Phase | Effort |
|-------|--------|
| Phase 1: Dependencies and structure | 30 min |
| Phase 2: Core implementation | 2-3 hours |
| Phase 3: Error handling and polish | 1-2 hours |
| Phase 4: Optional enhancements | 1-2 hours |
| Testing | 1-2 hours |
| **Total** | **5-9 hours** |

---

## Risks and Mitigations

| Risk | Mitigation |
|------|------------|
| CloudAPI WebSocket auth differs from REST | Investigate actual auth mechanism, may need CloudAPI changes |
| Binary protocol corruption | Use binary WebSocket frames, extensive testing |
| Connection stability | Implement reconnection logic, clear error messages |
| Platform differences | Test on macOS, Linux; document any platform-specific issues |

---

## Future Enhancements

1. **Bundled noVNC** - Serve noVNC HTML/JS locally instead of linking to novnc.com
2. **Multiple instance support** - Proxy to multiple instances on different ports
3. **SSH tunnel integration** - Combine with SSH for remote access
4. **Recording** - Option to record VNC session
5. **Clipboard sharing** - If supported by CloudAPI endpoint
6. **Auto-open browser** - `--open` flag with `--websocket` to launch browser automatically

---

## References

- [RFC 6455 - The WebSocket Protocol](https://tools.ietf.org/html/rfc6455)
- [RFB Protocol (VNC)](https://github.com/rfbproto/rfbproto)
- [tokio-tungstenite documentation](https://docs.rs/tokio-tungstenite)
- [noVNC - HTML5 VNC Client](https://novnc.com/)
