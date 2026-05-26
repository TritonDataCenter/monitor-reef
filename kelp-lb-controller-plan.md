# Plan: `triton k8s lb` — Server-Side Port to New Relay Model

## Context

The `remotes/origin/k8s-expiremental-testing` branch has a working
`triton k8s lb install/status/remove` implementation, but it is
CLI-heavy and depends on:

- Local cluster state files (`~/.triton/k8s/<name>/`)
- Local kubeconfig file on disk
- User's SSH key on disk (passed via `--key-path`)
- Direct CloudAPI calls from the CLI process
- `kube` crate in the CLI binary for k8s API calls

The current branch (`kelp-cluster-crud`) stores everything server-side.
Clusters have no local state; k8s access goes through the relay tunnel;
CloudAPI calls come from the server using the operator key. The `lb`
subcommand must be ported to match this architecture.

## What the Old Implementation Does

1. Load cluster state from filesystem to get `fabric_network_id`.
2. Read local kubeconfig to create a `kube` client.
3. Call CloudAPI to discover:
   - Public network UUID
   - External CNS suffix (from public network's suffix list)
   - Internal CNS root (from fabric network's suffix list)
   - Datacenter name
   - LB VM image UUID (newest image named `cloud-load-balancer`)
   - LB VM package UUID
4. Construct `worker_cns_name`:
   `{fabric-name}.worker.svc.{account}.{datacenter}.{cns-root}`
5. Apply RBAC manifest to k8s (ClusterRole, ClusterRoleBinding,
   ServiceAccount).
6. Create `triton-credentials` Secret: `key-id` + `private-key` content.
7. Create `triton-lb-controller-config` ConfigMap with all config values.
8. Apply controller Deployment manifest (image from `--controller-image`).
9. Wait for Deployment rollout (180s timeout).

`status` reads the Deployment + pod status from k8s via `kube` client.
`remove` deletes all the above k8s resources.

## Known Bugs in the Go Controller (Separate Repo)

Before this plan is worth implementing, two controller bugs must be fixed.
Both are documented in `docs/analysis/` on the old branch.

### Bug 1: Env vars never read (`lb-controller-network-gaps.md`)

`CreateLoadBalancer` in `pkg/triton/client.go` does not use the
`PUBLIC_NETWORK`, `FABRIC_NETWORK`, or `WORKER_CNS_NAME` env vars.
As a result, LB VMs are created on the wrong network and have no CNS
record (NXDOMAIN, then 503 from HAProxy).

Fix (controller repo):
- Read `PUBLIC_NETWORK`, `FABRIC_NETWORK`, `WORKER_CNS_NAME`,
  `CLUSTER_NAME`, `EXTERNAL_CNS_SUFFIX` from env in `main.go`.
- Pass them to the Triton client config struct.
- Add `Networks` (both public + fabric) and `CNS.Services` to
  `CreateInstanceInput`.

### Bug 2: Firewall tag mismatch (`lb-vm-firewall-tag-mismatch.md`)

Worker firewall rules allow traffic from tag `k8s.cluster = <uuid>`.
The controller tags LB VMs with `k8s-cluster = <name>` — wrong name
(hyphen vs dot) and wrong value (name vs uuid). LB VM traffic is dropped
by workers' firewalls → 503.

Fix (controller repo):
- Add `CLUSTER_UUID` env var read in `main.go`.
- Add `"k8s.cluster": clusterUUID` to `CreateInstanceInput.Tags`.

The CLI already passes `cluster-uuid` in the ConfigMap + sets a
`CLUSTER_UUID` env var in `deployment.yaml`.

## New Architecture

```
triton k8s lb install <cluster>
        │
        │  POST /v1/k8s/clusters/{cluster}/lb
        ▼
triton-api-server
  ├── CloudAPI calls (operator key, user-account URL)
  │     • list networks → discover public network UUID
  │     • get public network → external CNS suffix
  │     • get fabric network → internal CNS root
  │     • list datacenters → datacenter name
  │     • list images → newest cloud-load-balancer UUID
  │     • list packages → resolve package name → UUID
  │
  ├── Build ConfigMap data (all the above)
  │
  └── Apply to k8s via relay
        • RBAC manifest
        • triton-credentials Secret  (operator key + key-id)
        • triton-lb-controller-config ConfigMap
        • Deployment manifest
        • Poll until Deployment ready (or timeout)
```

## Credential Strategy

The controller needs Triton credentials to provision LB VMs at runtime.
Two options:

**Option A (recommended)**: Store the server's operator key in the
`triton-credentials` Secret, set `TRITON_ACCOUNT` to the cluster owner's
account login. CloudAPI allows an operator key to act for any account
via the `/:account/` URL segment — the same mechanism used by
`provision_vm` and `run_add_workers` on the server today.

**Option B (fallback)**: Accept `triton_key_id` + `triton_private_key`
in the `InstallLbRequest` body. The user provides their own credentials;
server stores them in the Secret unchanged. Matches old behavior exactly
but requires the user to pass credentials.

Start with Option A. If the operator acting-as-user CloudAPI path does
not work for instance creation, fall back to Option B.

## API Changes

### New types (`apis/triton-api/src/types/k8s.rs`)

```rust
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct InstallLbRequest {
    /// Package for LoadBalancer VMs (default: "sample-1G")
    #[serde(default = "default_lb_package")]
    pub package: String,

    /// Image name or UUID for LB VMs (default: newest "cloud-load-balancer")
    pub image: Option<String>,

    /// Override external CNS suffix (auto-discovered if absent)
    pub external_cns_suffix: Option<String>,

    /// Controller container image
    #[serde(default = "default_controller_image")]
    pub controller_image: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct LbStatus {
    pub installed: bool,
    pub ready: bool,
    pub replicas: Option<i32>,
    pub available_replicas: Option<i32>,
}
```

### New endpoints (`apis/triton-api/src/lib.rs`)

```rust
/// Install the Triton LB controller into the cluster.
/// Returns 202 Accepted; poll GET to check readiness.
#[endpoint { method = POST, path = "/v1/k8s/clusters/{cluster}/lb", tags = ["k8s"] }]
async fn k8s_cluster_lb_install(
    rqctx: RequestContext<Self::Context>,
    path: Path<ClusterPath>,
    body: TypedBody<InstallLbRequest>,
) -> Result<HttpResponseAccepted<Cluster>, HttpError>;

/// Return LB controller status.
#[endpoint { method = GET, path = "/v1/k8s/clusters/{cluster}/lb", tags = ["k8s"] }]
async fn k8s_cluster_lb_status(
    rqctx: RequestContext<Self::Context>,
    path: Path<ClusterPath>,
) -> Result<HttpResponseOk<LbStatus>, HttpError>;

/// Remove the LB controller from the cluster.
#[endpoint { method = DELETE, path = "/v1/k8s/clusters/{cluster}/lb", tags = ["k8s"] }]
async fn k8s_cluster_lb_remove(
    rqctx: RequestContext<Self::Context>,
    path: Path<ClusterPath>,
) -> Result<HttpResponseDeleted, HttpError>;
```

## Server Implementation

### New file: `services/triton-api-server/src/kube_relay.rs`

A thin k8s client that routes connections through the relay tunnel
instead of a local kubeconfig file. Key function:

```rust
pub async fn kube_client_for_cluster(
    relay: &Arc<RelayState>,
    cluster: &ClusterRecord,
) -> Result<kube::Client>
```

Implementation:
- Uses the cluster's `endpoint` field (e.g. `https://192.168.128.166:6443`)
  as the API server URL.
- Opens a relay stream to the k8s API port via `relay.open_stream(target)`.
- Wraps the stream as a hyper connector (see `hyper::client::conn` or a
  custom `tower::Service`).
- Injects the cluster's kubeconfig client cert/key from
  `ControlPlaneConfig` for authentication.

**Alternative (simpler for now)**: Write the kubeconfig to a temp file
(`tempfile` crate) and use `kube::Config::from_kubeconfig`. The relay
bridge is already running as a separate process when the user runs
`relay-bridge`, so the server can temporarily start its own internal
relay connection to `127.0.0.1:6443`. This is hacky but unblocks the
implementation without a custom hyper connector.

### CloudAPI discovery helpers (`services/triton-api-server/src/lb.rs`)

Port the discovery functions from the old `install.rs`:

- `discover_public_network(cloudapi, account)` → `Uuid`
- `discover_external_cns_suffix(cloudapi, account, network_id)` → `String`
- `discover_internal_cns_root(cloudapi, account, fabric_network_id)` → `String`
- `discover_datacenter(cloudapi, account)` → `String`
- `find_newest_image_by_name(cloudapi, account, name)` → `Uuid`
- `resolve_package(cloudapi, account, name)` → `Uuid`

These use the operator CloudAPI client with the user's account in the
URL (same as `provision_vm`).

### Handler: `k8s_cluster_lb_install`

```
1. Authenticate caller, load cluster record.
2. Verify cluster state == Running.
3. Verify relay tunnel registered for cluster.
4. Verify cloudapi present in ApiContext.
5. Spawn run_lb_install(store, relay, cloudapi, record, req).
6. Return 202 with current cluster record.
```

### `run_lb_install` (async, spawned)

```
1. provision_account = record.account_id.to_string()
2. Discover: public_network, external_cns_suffix, cns_suffix, datacenter.
3. Discover: lb_image_id, lb_package_id.
4. Compute worker_cns_name from fabric network info.
5. Build ConfigMap data (same keys as old install.rs).
6. Connect kube client via relay.
7. Apply RBAC YAML (static, same as old branch).
8. Create/update triton-credentials Secret:
   - key-id: operator key fingerprint from config
   - private-key: operator private key content from config
9. Create/update triton-lb-controller-config ConfigMap.
10. Apply Deployment YAML with {{CONTROLLER_IMAGE}} replaced.
11. Poll Deployment until available_replicas >= 1 (180s timeout).
12. Update cluster record: lb_installed = true.
```

### Handler: `k8s_cluster_lb_status`

Synchronous (returns immediately):
1. Connect kube client via relay.
2. GET `triton-lb-controller` Deployment in `kube-system`.
3. Return `LbStatus`.

### Handler: `k8s_cluster_lb_remove`

1. Connect kube client via relay.
2. Delete Deployment, ConfigMap, Secret, ClusterRoleBinding,
   ClusterRole, ServiceAccount.
3. Return 204.

## CLI Changes

### New files

```
cli/triton-cli/src/commands/k8s/lb/mod.rs
cli/triton-cli/src/commands/k8s/lb/install.rs
cli/triton-cli/src/commands/k8s/lb/status.rs
cli/triton-cli/src/commands/k8s/lb/remove.rs
```

All three are thin wrappers — resolve cluster, call the endpoint, print
result. No local kubeconfig, no SSH key, no CloudAPI calls from the CLI.

### `install.rs`

```rust
pub struct InstallArgs {
    pub cluster: String,
    #[arg(long, default_value = "sample-1G")]
    pub package: String,
    #[arg(long)]
    pub image: Option<String>,
    #[arg(long)]
    pub external_cns_suffix: Option<String>,
    #[arg(long, default_value = "travispaul/triton-lb-controller:latest")]
    pub controller_image: String,
}
```

Calls `k8s_cluster_lb_install()` and prints "Installing LB controller
for cluster '{name}'. Check status with: triton k8s lb status <name>".

### `mod.rs`

```rust
pub enum LbCommand { Install, Status, Remove }
```

Add `Lb { command: LbCommand }` variant to `K8sCommand`.
Add `pub mod lb` in `k8s/mod.rs`.
Wire into `K8sCommand::run`.

## Manifest Strategy

Embed manifests directly in the server binary (same as old CLI used
`include_str!`), or store them in `services/triton-api-server/src/lb/`.
Use `include_str!` at compile time. Same RBAC and Deployment YAML as the
old branch.

## New Cluster Record Fields

Add to `ClusterRecord` in `cluster_store.rs`:

```rust
pub lb_installed: bool,
```

Add to `Cluster` API type (for status visibility):
```rust
pub lb_installed: Option<bool>,
```

Not strictly required for MVP but useful for `triton k8s get` output.

## Implementation Order

1. **Fix Go controller bugs** (separate repo, prerequisite):
   - Bug 1: Read env vars, add Networks + CNS to CreateInstanceInput
   - Bug 2: Read CLUSTER_UUID env, add `k8s.cluster` tag

2. **API types**: `InstallLbRequest`, `LbStatus` in `k8s.rs`

3. **API trait**: three new endpoints in `lib.rs`

4. **Regenerate OpenAPI spec + client**:
   `make openapi-generate && make clients-generate`

5. **Server `lb.rs`**: CloudAPI discovery helpers

6. **Server `kube_relay.rs`**: k8s client over relay

7. **Server `main.rs`**: handler stubs + `run_lb_install`

8. **Manifests**: embed RBAC + Deployment YAML in server binary

9. **CLI**: `lb/mod.rs`, `install.rs`, `status.rs`, `remove.rs`

10. **Wire up**: add `Lb` to `K8sCommand`, `pub mod lb` in `k8s/mod.rs`

11. **Build + test**: `make package-build PACKAGE=triton-api-server`

## Key Files to Touch

| File | Change |
|------|--------|
| `apis/triton-api/src/types/k8s.rs` | Add `InstallLbRequest`, `LbStatus` |
| `apis/triton-api/src/lib.rs` | Add three lb endpoints |
| `openapi-specs/generated/triton-api.json` | Regenerate |
| `openapi-specs/patched/triton-gateway-api.json` | Regenerate |
| `clients/internal/triton-gateway-client/src/generated.rs` | Regenerate |
| `services/triton-api-server/src/lb.rs` | New: discovery helpers |
| `services/triton-api-server/src/kube_relay.rs` | New: relay k8s client |
| `services/triton-api-server/src/main.rs` | Three handlers + run_lb_install |
| `services/triton-api-server/src/cluster_store.rs` | Add `lb_installed` field |
| `services/triton-api-server/Cargo.toml` | Add `kube` + `k8s-openapi` deps |
| `cli/triton-cli/src/commands/k8s/lb/` | New module (4 files) |
| `cli/triton-cli/src/commands/k8s/mod.rs` | Add `Lb` variant + `pub mod lb` |

## What This Does NOT Include

- LB VM cleanup on cluster delete (follow-up: delete LB VMs before
  deleting cluster record)
- Multiple LB controller replicas / HA
- Custom firewall rule creation for the LB VM (the controller bug fix
  handles this via the `k8s.cluster` tag)
- User-provided SSH key path (replaced by operator credentials)
- Interactive confirmation for `lb remove` (server returns 204
  immediately; CLI can add `--yes` flag if desired)
