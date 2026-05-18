# LoadBalancer Controller: Missing Network and CNS Configuration

## Problem

After the CLI manifest fixes (missing args, wrong env var names, missing
RBAC), the controller will start successfully and provision Triton VMs for
LoadBalancer services. However, those VMs will be created **without
explicit network assignments or CNS service tags**, causing two failures:

1. **HAProxy can not reach k8s worker pods** -- the LB VM lands on
   Triton's default network instead of the cluster's fabric network, so
   it has no route to the pod/node IPs. Result: **503 Service
   Unavailable**.

2. **DNS does not resolve** -- without a `triton.cns.services` tag the
   LB VM gets no CNS record, so
   `k8s-my-cluster-default-podinfo.svc.travis.ext.corp` has no A record.
   Result: **NXDOMAIN** (or stale/missing DNS).

## Root Cause

`CreateLoadBalancer` in
`triton-loadbalancer-controller/pkg/triton/client.go` (line 179) builds
a `compute.CreateInstanceInput` that omits three fields the
`triton-go/v2` library supports:

```go
createInput := &compute.CreateInstanceInput{
    Name:     params.Name,
    Package:  packageName,
    Image:    imageId,
    Metadata: metadata,
    Tags: map[string]interface{}{
        "k8s-service":  params.Name,
        "managed-by":   "triton-loadbalancer-controller",
        "loadbalancer": "true",
    },
    // Missing: Networks, CNS
}
```

### Missing field: `Networks`

Without `Networks` (or `NetworkObjects`), CloudAPI places the VM on the
account's default network. For a LoadBalancer VM to function it must be
attached to:

| Network | Why |
|---------|-----|
| **Public network** (`PUBLIC_NETWORK` env var) | Gives the VM a routable IP that external clients can reach. |
| **Fabric network** (`FABRIC_NETWORK` env var) | Gives the VM a private IP on the same subnet as the k8s worker nodes, so HAProxy backends can connect to NodePorts. |

The CLI already passes both UUIDs as environment variables to the
controller pod (via the ConfigMap), but the Go code never reads them.

### Missing field: `CNS`

Triton CNS (Container Name Service) automatically creates DNS records
for instances based on the `triton.cns.services` tag. The k8s
provisioning code in the CLI sets this tag for control plane and worker
nodes:

```rust
// provisioning.rs:361-367
let cns_services = match role {
    NodeRole::Control => "k8s,ctrl",
    NodeRole::Worker  => "k8s,worker",
};
tags.insert("triton.cns.services".to_string(),
    serde_json::Value::String(cns_services.to_string()));
```

This produces DNS names like:
- `ctrl.svc.{account}.{datacenter}.{cns-suffix}` (control plane)
- `worker.svc.{account}.{datacenter}.{cns-suffix}` (workers)

For LB VMs the controller needs an analogous tag so that each
LoadBalancer service gets a DNS record. The expected external DNS name
from the user's test script is:

```
k8s-my-cluster-default-podinfo.svc.travis.ext.corp
```

This follows the pattern:
`k8s-{cluster}-{namespace}-{service}.svc.{account}.{external-cns-suffix}`

The controller has all the necessary components to build this name
(`CLUSTER_NAME`, service namespace, service name, `EXTERNAL_CNS_SUFFIX`)
but does not construct or apply it.

## Available Environment Variables (Unused)

The CLI's deployment manifest injects these into the controller pod, but
the Go code ignores all of them:

| Env Var | Source | Intended Use |
|---------|--------|--------------|
| `PUBLIC_NETWORK` | ConfigMap `public-network` | Network UUID for LB VM's external NIC |
| `FABRIC_NETWORK` | ConfigMap `fabric-network` | Network UUID for LB VM's internal NIC |
| `WORKER_CNS_NAME` | ConfigMap `worker-cns-name` | DNS name for discovering worker node IPs (e.g. `my-fabric.worker.svc.travis.earth.cns.capsule.corp`) |
| `CLUSTER_NAME` | ConfigMap `cluster-name` | Used to construct the LB VM name and CNS service tag |
| `TRITON_DATACENTER` | ConfigMap `datacenter` | Part of the CNS hostname hierarchy |
| `TRITON_CNS_SUFFIX` | ConfigMap `cns-suffix` | Internal CNS root (e.g. `cns.capsule.corp`) |
| `EXTERNAL_CNS_SUFFIX` | ConfigMap `external-cns-suffix` | External CNS root (e.g. `ext.corp`) |

## Required Changes

### 1. Read network config from environment

In `main.go` or a new config struct, read and validate:

```go
publicNetwork  := os.Getenv("PUBLIC_NETWORK")
fabricNetwork  := os.Getenv("FABRIC_NETWORK")
clusterName    := os.Getenv("CLUSTER_NAME")
workerCNSName  := os.Getenv("WORKER_CNS_NAME")
externalSuffix := os.Getenv("EXTERNAL_CNS_SUFFIX")
```

Pass these to the Triton client (either via a config struct or by
extending `NewClient`).

### 2. Add networks to `CreateInstanceInput`

In `CreateLoadBalancer` (`client.go` ~line 179), add both networks:

```go
createInput := &compute.CreateInstanceInput{
    Name:     params.Name,
    Package:  packageName,
    Image:    imageId,
    Networks: []string{publicNetwork, fabricNetwork},
    Metadata: metadata,
    Tags:     tags,
    CNS:      cns,
}
```

Or using `NetworkObjects` for finer control (e.g. if specific fabric IPs
are needed):

```go
NetworkObjects: []compute.NetworkObject{
    {IPv4UUID: publicNetwork},
    {IPv4UUID: fabricNetwork},
},
```

### 3. Set CNS service tag

Build a CNS service name that follows the established pattern and set it
on the instance:

```go
// Produces e.g. "k8s-my-cluster-default-podinfo"
cnsServiceName := fmt.Sprintf("k8s-%s-%s-%s",
    clusterName,
    service.Namespace,
    service.Name)

createInput.CNS = compute.InstanceCNS{
    Services: []string{cnsServiceName},
}
```

This will cause Triton CNS to create:
- **Internal**: `k8s-my-cluster-default-podinfo.svc.{account}.{datacenter}.{cns-suffix}`
- **External**: `k8s-my-cluster-default-podinfo.svc.{account}.{external-cns-suffix}`

The `triton-go/v2` library handles the tag conversion automatically --
`InstanceCNS.Services` is serialized as
`tag.triton.cns.services=k8s-my-cluster-default-podinfo` in the
CloudAPI request.

### 4. Pass service namespace to `LoadBalancerParams`

Currently `extractLoadBalancerParams` only captures `service.Name` (line
237). The namespace is needed for the CNS service name. Add a
`Namespace` field to `LoadBalancerParams`:

```go
type LoadBalancerParams struct {
    Name            string
    Namespace       string  // new
    PortMappings    []PortMapping
    MaxBackends     int
    CertificateName string
    MetricsACL      []string
}
```

And populate it in the controller:

```go
params := triton.LoadBalancerParams{
    Name:      service.Name,
    Namespace: service.Namespace,
}
```

### 5. Use `WORKER_CNS_NAME` for backend resolution

The HAProxy image on the LB VM needs to know where to send traffic. The
`WORKER_CNS_NAME` (e.g.
`my-fabric.worker.svc.travis.earth.cns.capsule.corp`) resolves to
all worker node IPs on the fabric network. This should be passed as
instance metadata so the HAProxy configuration can use it as the backend
server address:

```go
metadata["cloud.tritoncompute:backend_host"] = workerCNSName
```

The exact metadata key depends on what the `cloud-load-balancer` HAProxy
image expects. The portmap already carries
`BackendName: service.Name` but this is just a label -- the actual
backend address that HAProxy connects to is likely derived from
`backend_host` metadata or a similar convention in the image.

## Impact

Without these changes, even with the CLI manifest fixes applied:

- The controller **starts** and **watches** services correctly.
- It **provisions** an LB VM, but on the wrong network(s).
- The LB VM **cannot reach** k8s worker nodes (no fabric connectivity).
- The LB VM **has no DNS record** (no CNS tag).
- Users see either NXDOMAIN (DNS) or 503 (if they curl the raw IP).

## Scope

These changes are entirely within the
`triton-loadbalancer-controller` repository:

- `cmd/manager/main.go` -- read env vars, pass config to client
- `pkg/triton/client.go` -- add Networks and CNS to CreateInstanceInput
- `pkg/controller/loadbalancer_controller.go` -- pass Namespace in params

The CLI (`monitor-reef`) already passes all the necessary configuration.
No CLI changes are needed beyond the manifest fixes already applied.
