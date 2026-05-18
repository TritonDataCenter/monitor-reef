# Plan: Kelp relay tunnel POC (SmartOS)

## Context

Per RFD-0192 §3, TritonAPI needs to reach Talos and K8s APIs inside
customer fabric networks without inbound firewall holes. The design
is a small Rust agent on the customer side that initiates an outbound
WebSocket to TritonAPI, multiplexes logical streams with yamux, and
bridges each stream to a fabric-local TCP target the server asks for.

The POC's goal: prove the byte pipe works end to end. **No auth, no
NAT-zone integration, no metadata-driven config, no JWT delivery, no
production hardening.** Just enough machinery that you can drop the
agent binary in a SmartOS zone, point `talosctl` or `kubectl` at a
local port, and have packets reach a Talos node or K8s API server
over the tunnel.

Earlier work (`feature/kelp-cluster-crud`) added `/v1/k8s/clusters/*`
CRUD endpoints. This branch adds the relay machinery alongside — no
new dependencies on cluster records (the POC hardcodes the routing).

## Branch

`feature/kelp-relay-poc` off `main` in `/home/travis/monitor-reef/`
on the SmartOS box. (Branch off `main`, not off
`feature/kelp-cluster-crud` — relay is logically independent and we
don't want to entangle the branches.)

## Architecture

```
your dev box (talosctl / kubectl)
            │
            │ TCP (e.g. 127.0.0.1:50000 for talos, 127.0.0.1:6443 for k8s)
            ▼
┌─────────────────────────────────────────┐
│ triton-relay-bridge (local CLI)         │  one or more local listeners,
│   for each conn: open yamux stream,     │  each pinned to a specific
│   send "host:port\n" target prefix,     │  fabric target. Connects via
│   then byte-pump.                       │  WebSocket to triton-api-server.
└─────────────────────────────────────────┘
            │
            │ WebSocket (yamux client)
            ▼
┌─────────────────────────────────────────┐
│ triton-api-server (Dropshot)            │  two #[channel] endpoints:
│   /v1/k8s/relay  ←── from agent          │  one for the agent (registers
│   /v1/k8s/relay/connect  ←── from bridge │  the tunnel), one for the
│   bridges yamux streams between them.   │  bridge (gets streams routed
│                                         │  to the registered tunnel).
└─────────────────────────────────────────┘
            │
            │ WebSocket (yamux server)
            ▼
┌─────────────────────────────────────────┐
│ triton-relay-agent (the binary you     │  yamux server. For each
│ drop in the SmartOS zone)              │  inbound stream:
│                                         │  1. read "host:port\n"
│                                         │  2. TCP dial that target
│                                         │  3. byte-pump in both
│                                         │     directions
└─────────────────────────────────────────┘
            │
            │ TCP on the fabric network
            ▼
   Talos node :50000     /     K8s API :6443
```

POC simplifications:
- Only **one** tunnel is registered at a time. No cluster ID routing.
  The triton-api-server holds an `Option<TunnelHandle>` and routes
  any incoming bridge connection to it.
- Stream framing is **one line of UTF-8 `host:port\n`** at the start
  of each stream, then opaque bytes.
- All three components have hardcoded URLs / ports from config files
  or CLI flags. No mdata, no JWT.

## Phase 1: SmartOS Rust toolchain sanity check

Before writing any relay code, make sure the existing workspace
builds and tests on SmartOS. Triton projects mostly target SmartOS /
illumos but the workspace is also exercised on Linux; minor toolchain
differences sometimes bite.

Steps:
1. `cd /home/travis/monitor-reef && git fetch && git checkout main`
2. `cargo build --workspace` — fix any toolchain / target issues
3. `cargo test -p triton-api-server` — verify the 26 existing tests
   pass

Stop and ask the user if any of this fails. Do not "fix" platform
issues silently — note them and confirm.

Create the branch: `git checkout -b feature/kelp-relay-poc`.

## Phase 2: Crate skeleton

Three new crates added to the workspace.

### `libs/triton-relay-protocol`

Shared framing helpers used by all three sides. Small library crate.
What goes in:
- `pub async fn read_connect_target(stream: &mut yamux::Stream) -> Result<String>`
  — reads bytes until `\n`, returns the UTF-8 `host:port` string.
- `pub async fn write_connect_target(stream: &mut yamux::Stream, target: &str) -> Result<()>`
- `pub async fn bridge(a: ..., b: ...) -> Result<()>` — bidirectional
  byte pump. Use `tokio::io::copy_bidirectional` over the two halves.
- Errors with `thiserror`.

### `services/triton-relay-agent`

The binary you drop in the SmartOS zone.

`Cargo.toml`:
- bin: `triton-relay-agent`
- deps: tokio (`full`), `tokio-tungstenite`, `yamux`, `anyhow`,
  `tracing`, `tracing-subscriber`, `serde` + `serde_json` for config,
  `triton-relay-protocol` (workspace path), `url`

`src/main.rs`:
- Parse a single config arg: path to a JSON file containing
  `{ "relay_endpoint": "ws://<host>:<port>/v1/k8s/relay" }`. CLI flag
  or env var `TRITON_RELAY_AGENT_CONFIG` for the path.
- Connect WebSocket to the endpoint.
- Wrap the WebSocket in `tokio_tungstenite`'s `WebSocketStream`, then
  bridge that to yamux. The standard pattern: implement
  `AsyncRead + AsyncWrite` over the WebSocket message stream (one
  binary frame = one chunk of bytes), feed it to `yamux::Connection`
  configured as server.
- For each inbound yamux stream:
  - `read_connect_target` to get `"10.0.0.5:50000"` or whatever
  - `tokio::net::TcpStream::connect(target)` on the fabric network
  - `bridge(stream, tcp)` — bidirectional byte pump until either
    side closes
- Log stream lifecycle at `info` (open with target, close with
  byte counts, errors).

### `cli/triton-relay-bridge`

User-side local listener. CLI tool you run on the box where
`talosctl` / `kubectl` lives.

`Cargo.toml`:
- bin: `triton-relay-bridge`
- deps: same flavour as the agent plus `clap` for arg parsing

`src/main.rs`:
- CLI args:
  - `--relay-url <ws://...>` — the bridge endpoint on triton-api-server
    (`/v1/k8s/relay/connect`)
  - `--listen <addr>` — local listen address, e.g. `127.0.0.1:50000`
  - `--target <host:port>` — fabric target, e.g. `10.0.0.5:50000`.
    Sent as the first line of each yamux stream.
- Open a persistent WebSocket to `--relay-url`, wrap in yamux as
  client.
- Bind a `TcpListener` on `--listen`.
- For each accepted TCP connection:
  - Open a new yamux outbound stream
  - Write `"<target>\n"`
  - Bridge the TCP and the stream

To exercise both APIs simultaneously, run two instances of the
bridge — one for Talos (`:50000` → `:50000`), one for K8s (`:6443` →
`:6443`).

## Phase 3: triton-api-server-side channel

Add a single Dropshot `#[channel]` endpoint that does both jobs (the
agent registering its tunnel, and bridge clients getting streams
proxied to that tunnel). Two URL paths, one shared registry.

### Trait additions (`apis/triton-api/src/lib.rs`)

```rust
#[channel {
    protocol = WEBSOCKETS,
    path = "/v1/k8s/relay",
    tags = ["k8s-relay"],
}]
async fn k8s_relay_register(
    rqctx: RequestContext<Self::Context>,
    upgraded: WebsocketConnection,
) -> WebsocketChannelResult;

#[channel {
    protocol = WEBSOCKETS,
    path = "/v1/k8s/relay/connect",
    tags = ["k8s-relay"],
}]
async fn k8s_relay_connect(
    rqctx: RequestContext<Self::Context>,
    upgraded: WebsocketConnection,
) -> WebsocketChannelResult;
```

Look at `cloudapi-api`'s existing `#[channel]` endpoints (e.g.
`changefeed`) for the exact shape and the
`WebsocketChannelResult`/`WebsocketConnection` imports.

### Server-side state

`ApiContext` gets a new field:

```rust
relay: Arc<RelayState>,
```

where `RelayState` (in `services/triton-api-server/src/relay.rs`) is:

```rust
pub struct RelayState {
    // Phase 1: single registered tunnel. Replace with a HashMap
    // keyed by cluster id when that's actually wired through.
    tunnel: Mutex<Option<TunnelHandle>>,
}

pub struct TunnelHandle {
    // Sender side of a channel the agent task reads. To open a new
    // stream targeted at host:port, send a request and receive back
    // a yamux::Stream from the agent's side of the connection.
    open_stream: mpsc::Sender<oneshot::Sender<Result<yamux::Stream>>>,
}
```

`k8s_relay_register`:
1. Wrap the WebSocket in AsyncRead+Write
2. Build yamux as **client** (the server-side of the application
   protocol is the yamux client; the agent is the yamux server, so
   "open stream" requests originate from the API server)
3. Register the tunnel handle in `RelayState`
4. Spawn the yamux event loop; on disconnect, clear the handle
5. Block until WebSocket closes

`k8s_relay_connect`:
1. Look up the registered tunnel; if none, close the WebSocket with
   a reason
2. Wrap the incoming WebSocket in AsyncRead+Write, build yamux as
   **server** (this side is what the bridge tool dials)
3. For each inbound stream from the bridge: ask the registered
   tunnel to open a new outbound stream to the agent, then
   `tokio::io::copy_bidirectional` between the two streams

Reuse `triton-relay-protocol::bridge` to keep the byte-pump in one
place.

### What to NOT do here

- No auth. Both endpoints are wide open in the POC.
- No cluster-id routing. First agent wins; subsequent registrations
  replace the previous handle and any in-flight bridge streams hang
  up.
- No reconnect logic on the server side. If the agent drops, the
  next agent registers and life continues.

## Phase 4: End-to-end test with `talosctl` and `kubectl`

Once the three binaries build and the server runs, drive it from a
real client.

### Setup

On the SmartOS zone:
- Drop the `triton-relay-agent` binary in the zone
- Write `/etc/triton-relay-agent.json`:
  ```json
  {
    "relay_endpoint": "ws://<triton-api-server>:8080/v1/k8s/relay"
  }
  ```
- Run the agent, watch logs

On the box that has `talosctl` / `kubectl`:
- Run `triton-relay-bridge`:
  ```
  triton-relay-bridge \
    --relay-url ws://<triton-api-server>:8080/v1/k8s/relay/connect \
    --listen 127.0.0.1:50000 \
    --target <talos-node-fabric-ip>:50000
  ```
- In another terminal, the same with `--listen 127.0.0.1:6443
  --target <k8s-api-fabric-ip>:6443`

### Exercising

```
talosctl --endpoints 127.0.0.1:50000 --talosconfig <existing-talosconfig> version
```

talosctl uses mTLS keyed to the cluster's PKI. The relay is opaque
(it doesn't terminate TLS); the talos cert names the cluster's
control plane IPs, not `127.0.0.1`, so depending on talosctl's
verifier behaviour you may need `--nodes <talos-node-ip>` to tell
talosctl which node identity to expect. The bridge → tunnel → agent
TCP pipe is transparent; TLS terminates at the actual Talos node.

```
kubectl --kubeconfig <existing-kubeconfig> --server https://127.0.0.1:6443 get nodes
```

Same TLS situation. If kubeconfig pins a hostname, override with
`--insecure-skip-tls-verify` for the POC or edit the kubeconfig.
Don't lose sleep over the cert hostname mismatch — the POC is
proving the byte pipe, not the auth UX.

### Success criteria

- Agent logs: "stream opened to 10.x.x.x:50000, 12345 bytes in,
  3210 bytes out, closed."
- Bridge logs: similar from its side.
- `talosctl version` returns the node's Talos version
- `kubectl get nodes` returns the cluster's nodes

## Verification at each phase

| Phase | What proves it works |
|-------|----------------------|
| 1 | `cargo test -p triton-api-server` green on SmartOS |
| 2 | `cargo build --workspace` green; all three new crates compile |
| 3 | Run agent + server locally with no bridge → agent reports "registered, idle" and the server holds the tunnel handle; kill agent → server clears handle |
| 4 | `talosctl version` and `kubectl get nodes` both work through their respective bridges |

## Out of scope

- Auth on either endpoint (no JWT, no shared token, no TLS pinning)
- Reconnect / backoff in the agent (just exit cleanly on disconnect
  for the POC; restart it manually)
- Multiple concurrent tunnels per server (single `Option<TunnelHandle>`)
- Cluster-id routing
- Reading config from VM metadata (mdata-get) — that's a follow-up
  whose interface is the JSON config file we already accept
- NAT zone integration
- SMF manifests / service registration
- Integration with the `feature/kelp-cluster-crud` cluster records
- Customer-facing UX: a `triton k8s relay` CLI subcommand wrapping
  the bridge

## Open decisions intentionally left to the implementing session

- Specific yamux crate version (`yamux = "0.13"` is current upstream;
  pick what compiles cleanly)
- Tokio-tungstenite vs `tokio_websockets` — go with whichever the
  workspace already pulls in (look at Cargo.lock; tokio-tungstenite
  is the safer bet, it's a dropshot dep)
- Whether to bundle bridge + agent + server starter in a single
  `cargo xtask relay-up` style harness for testing. Nice-to-have; not
  required.
- Exact shape of the AsyncRead+Write adapter over the WebSocket
  stream. There are crates (`ws_stream_tungstenite`) that do this; if
  one is already in the lock file or close to the workspace, prefer
  it over rolling your own.

## Git policy

Same as the cluster-CRUD branch:
- Branch + commits OK on `feature/kelp-relay-poc`
- No pushes without explicit user authorization
- One commit per logical phase is fine — or squash later if the user
  prefers a single commit

## What this leaves for follow-up branches

Once the byte pipe works end to end:

1. **Auth (Phase 2+)**: shared token first, then JWT delivered via
   VM metadata (mdata-set from the server, mdata-get in the agent)
2. **Reconnect + backoff**: agent reconnects with jittered
   exponential backoff
3. **Multi-tunnel**: cluster-id keyed registry on the server,
   routing logic for bridge connections
4. **NAT zone integration**: bake the agent into the NAT zone image,
   activate via metadata
5. **`triton k8s relay` CLI**: customer-facing version of the bridge
   tool, with auth and cluster lookup integrated
6. **TritonAPI's own use of the tunnel**: bootstrap, kubeconfig,
   health checks via the relay instead of direct API access
