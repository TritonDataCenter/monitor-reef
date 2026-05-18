# LoadBalancer VM Firewall Tag Mismatch

## Problem

After all CLI manifest and CNS suffix fixes, the LB controller
successfully provisions an HAProxy VM, configures it with the correct
portmap and backend CNS name, and DNS resolves correctly. However,
`curl` still returns **503 Service Unavailable** because the HAProxy VM
cannot reach worker NodePorts -- the workers' firewall rules block the
traffic.

## Root Cause

The k8s bootstrap creates Triton firewall rules that use the tag
`k8s.cluster` (with a dot) set to the **cluster UUID** to allow
intra-cluster communication:

```
FROM tag "k8s.cluster" = "b34a3d95-a175-4f8c-9352-88bb5349ae6d"
  TO tag "k8s.cluster" = "b34a3d95-a175-4f8c-9352-88bb5349ae6d"
  ALLOW tcp PORT all
```

Control plane and worker nodes are provisioned with this tag by
`provisioning.rs` (line 348-351):

```rust
tags.insert(
    "k8s.cluster".to_string(),
    serde_json::Value::String(cluster_id.to_string()),  // UUID
);
```

But the LB controller sets a **different** tag on LB VMs. From the
observed instance metadata:

```json
{
    "k8s-cluster": "my-cluster",
    "k8s-namespace": "default",
    "k8s-service": "podinfo",
    "role": "k8s-loadbalancer"
}
```

Two mismatches:
1. **Tag name**: `k8s-cluster` (hyphen) vs `k8s.cluster` (dot)
2. **Tag value**: `"my-cluster"` (human name) vs `"b34a3d95-..."` (UUID)

Because the LB VM does not carry `k8s.cluster = <uuid>`, the workers'
firewall rules do not match it, and all TCP traffic from the LB VM to
the workers is dropped. HAProxy's health checks fail, all backends are
marked down, and it returns 503.

## Evidence

LB VM instance details showing the tag mismatch:

```
LB VM tags:
  "k8s-cluster": "my-cluster"           <-- wrong name, wrong value

Worker VM tags:
  "k8s.cluster": "b34a3d95-..."         <-- what firewall rules match on
```

Firewall rules for the cluster:

```
ad8a1b26  FROM tag "k8s.cluster" = "b34a3d95-..." TO tag "k8s.cluster" = "b34a3d95-..." ALLOW tcp PORT all
3d1b88c3  FROM tag "k8s.cluster" = "b34a3d95-..." TO tag "k8s.cluster" = "b34a3d95-..." ALLOW udp PORT all
```

The LB VM has `firewall_enabled: false`, so it is not itself blocking
inbound traffic. The problem is that the **workers** have firewalls
enabled and only accept traffic from instances with the matching
`k8s.cluster` tag.

## Required Fix

The controller needs to set `k8s.cluster` (with a dot) to the **cluster
UUID** on every LB VM it provisions. The cluster UUID is not currently
available to the controller.

### Option A: Pass cluster UUID as config (recommended)

The CLI already stores the cluster UUID in `ClusterState` and could pass
it to the controller via the ConfigMap. The controller would then set
the correct tag when creating LB instances.

**CLI changes** (`install.rs`):
- Add `cluster-uuid` to the ConfigMap data in `build_configmap_data()`
- Add a `CLUSTER_UUID` env var mapping in `deployment.yaml`

**Controller changes** (`client.go`):
- Read `CLUSTER_UUID` from environment
- Set `"k8s.cluster": clusterUUID` in the `Tags` map of
  `CreateInstanceInput` (alongside the existing tags)

The tag on LB VMs should then be:
```json
{
    "k8s.cluster": "b34a3d95-a175-4f8c-9352-88bb5349ae6d",
    "k8s-cluster": "my-cluster",
    "k8s-namespace": "default",
    "k8s-service": "podinfo",
    "role": "k8s-loadbalancer"
}
```

The existing `k8s-cluster` (hyphen) tag can be kept for the controller's
own instance discovery; the new `k8s.cluster` (dot) tag is solely for
firewall rule matching.

### Option B: Create a dedicated firewall rule for LB VMs

Instead of adding the cluster tag, create a firewall rule that allows
traffic from the LB VM's existing tags to the cluster workers:

```
FROM tag "role" = "k8s-loadbalancer"
  TO tag "k8s.cluster" = "<uuid>"
  ALLOW tcp PORT all
```

This could be added by the CLI during `triton k8s lb install`. It would
not require controller changes but is less clean -- the rule would need
the cluster UUID hardcoded and would not automatically apply to LB VMs
for new services.

### Option C: Also enable firewall on LB VMs

Currently LB VMs are created with `firewall_enabled: false`. If the
controller enabled firewalls and set the `k8s.cluster` tag, the LB VMs
would also be protected by the intra-cluster rules. This is the most
secure option.

## Recommendation

**Option A** is the simplest and most correct fix:

1. CLI side (this repo): add `cluster-uuid` to ConfigMap + env var
2. Controller side: read env var, add `k8s.cluster` tag to LB VMs

This requires changes in both repos but is a small, well-scoped fix.
The CLI change is ~10 lines; the controller change is ~5 lines.

## Scope

| Repo | File | Change |
|------|------|--------|
| `monitor-reef` | `cli/triton-cli/src/commands/k8s/lb/install.rs` | Add `cluster-uuid` to `build_configmap_data()` |
| `monitor-reef` | `cli/triton-cli/src/commands/k8s/lb/manifests/deployment.yaml` | Add `CLUSTER_UUID` env var from ConfigMap |
| `triton-loadbalancer-controller` | `pkg/triton/client.go` | Read `CLUSTER_UUID` env, add `k8s.cluster` tag to `CreateInstanceInput.Tags` |

## Workaround

Until the fix is deployed, manually add the tag to existing LB VMs:

```bash
triton instance tag set lb-my-cluster-default-podinfo-0 \
    "k8s.cluster=b34a3d95-a175-4f8c-9352-88bb5349ae6d"
```

Or create a one-off firewall rule:

```bash
triton fwrule create \
    'FROM tag "role" = "k8s-loadbalancer" TO tag "k8s.cluster" = "b34a3d95-a175-4f8c-9352-88bb5349ae6d" ALLOW tcp PORT all'
```
