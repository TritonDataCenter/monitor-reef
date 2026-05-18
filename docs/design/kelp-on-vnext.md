# Kelp on Triton Cloud vnext — requirements and mapping

> Status: requirements + mapping document. Not an implementation
> plan, not a v1 deliverable, not a Kelp service spec.
>
> Last updated: 2026-05-05.
>
> This document translates the Kelp design (managed Kubernetes /
> Talos for classic Triton DataCenter) onto Triton Cloud vnext
> primitives. Its job is to make sure v1 ships a primitive set that
> Kelp can land on later without breaking-change migrations, and to
> give Kelp a defensible shape as a v1.x / v2 product candidate.
>
> Kelp itself is not built as part of v1. The v1 work that this
> document touches is already owned by other agents:
>
> - VPCs, subnets, NAT, FIPs, security groups, route tables — Agent A
>   (`vpc-control-plane-v1.md`).
> - Proteus dataplane — `PROTEUS_PLAN.md` and `proteus/TODO.md`.
> - firehyve / fhrun edge runtime — Agent D.
> - NAT realization and north/south path — Agent E.
> - Image catalog and guest-config delivery — currently unassigned;
>   this doc records the requirements Kelp imposes.
> - DNS / service discovery — currently unassigned; this doc records
>   the requirements Kelp imposes.

## 1. Summary

Kelp is a managed Kubernetes service. Each Kelp cluster is a set of
dedicated Triton VMs running Talos Linux on isolated tenant network
fabric, with isolated PKI and isolated kubeconfigs. The classic Kelp
design names four client surfaces (HTTP API, `triton k8s` CLI,
Terraform provider, web UI) and three in-cluster components
(LoadBalancer controller, CSI driver, autoscaler). Triton owns
cluster lifecycle: create, bootstrap, list, get, kubeconfig, health,
add workers, add controllers, upgrade, delete.

For Triton Cloud vnext, Kelp is a v1.x / v2 product. v1 only needs to
ship VM, VPC, NAT, FIP, image, security-group, and lifecycle
primitives clean enough that someone can stand up a Talos cluster on
v1 manually. Kelp's job in v1 is to be the loudest acceptance
workload, not a deliverable.

The v1 Kelp acceptance bar is therefore:

> An operator can use only v1 primitives — no Kelp service, no
> Terraform provider, no CCM, no in-cluster controllers — to launch a
> 1-node or 3-node Talos cluster, attach a floating IP to the
> Kubernetes API, and reach `kubectl get nodes` from outside the
> cloud.

If that test passes, v1 has not painted Kelp into a corner.

## 2. Why this is a requirements doc

Three reasons Kelp belongs upstream of v1, not inside v1:

1. **Compliance pressure.** Talos surfaces problems classic Triton
   could hide: there is no SSH, no shell, and no in-image
   customization. Talos boots from a single machine-config document
   delivered through a guest-config channel. vnext now ships that
   channel — NoCloud cidata via `smartos-live` af99d6a1 — but only
   bhyve images that opt in via the
   `org.smartos:cloudinit_datasource=nocloud` manifest tag get a
   cidata disk, so the v1 image catalog must surface the tag.
2. **Network pressure.** Kubernetes assumes routed pod CIDRs, IPAM
   that survives node replacement, in-cluster service load
   balancing, and a clear north/south termination model. v1's
   `NatGateway` + `FloatingIp` + `RouteTable` primitives can carry
   most of that, but only if the data model is extensible without
   wire breaks.
3. **Service-discovery pressure.** Kubernetes `Service type=LoadBalancer`
   wants to register a stable name. Classic Kelp uses CNS for
   round-robin DNS over LB instances. vnext has not yet committed to
   a DNS surface; Kelp formalizes the requirement so DNS does not
   become a post-v1 emergency.

The deliverable from this doc is a checklist for v1 owners and a
post-v1 workstream queue for whoever later builds Kelp.

## 3. Classic-Kelp → vnext primitive mapping

Classic Kelp depended on seven Triton DataCenter services. vnext
replaces them with the small set in `monitor-reef`/`proteus`. The
mapping is:

| Classic service | Classic role | vnext replacement | v1 status |
|---|---|---|---|
| **CloudAPI / VMAPI** | VM lifecycle, NIC management, metadata, firewall hooks | `tritond` `/v2/tenants/{t}/projects/{p}/instances` (create/get/list/start/stop/restart/delete), agent jobs, instance NICs/disks/FIPs | shipping in v1 (Agent A surface, plus existing `Instance`/`Nic`/`Disk` types in `tritond-store`) |
| **NAPI** | Fabric networks, subnets, IP allocation | `tritond` `Vpc` + `Subnet` + Proteus `triton-vpc` plugin | shipping in v1 (Agent A; Proteus Phase 8 in `proteus/TODO.md`) |
| **FWAPI** | Firewall rules at the VM | `SecurityGroup` + `SecurityGroupRule` + NIC↔SG attachment, compiled to Proteus distributed firewall | shipping in v1 (Agent A §4.4–§4.6) |
| **IMGAPI** | Image catalog, name/version resolution | `tritond` images (silo / tenant / project / public scopes; content-addressed via `derive_image_id`) | shipping in v1 — but image manifest needs Talos-specific fields (§5) |
| **PAPI** | Package / flavor catalog | vnext flavor / package model | open question (`triton-cloud.md` §11 #10); v1 candidate |
| **CNS** | DNS for clusters and LBs | vnext DNS / service discovery (CNS-shaped, health-aware, split-horizon, FDB-backed; anycast on every CN per saved design preference) | open question (`triton-cloud.md` §11 #8); v1 may defer |
| **Moray** | Metadata persistence | FoundationDB via `tritond-store` `FdbStore` | shipping in v1 |

Two vnext items fall out of this table:

1. **Guest machine-config delivery** is decided. `smartos-live`
   af99d6a1 ships NoCloud cidata for bhyve guests, triggered by the
   `org.smartos:cloudinit_datasource=nocloud` image-manifest tag and
   driven from `customer_metadata['cloud-init:user-data' | 'cloud-init:network-config' | …]`.
   Kelp's only ask is that v1 surface the tag on `Image` records and
   forward `customer_metadata` opaquely through instance create.
   Detail in §5.2. This closes `triton-cloud.md` §11 open
   question #6.
2. **DNS / service discovery** is open. Kelp can wait, but only if
   v1 commits to the data model now so post-v1 service discovery
   doesn't break Kelp LBs.

No new tritond resource types are needed for v1 to support a
manual Kelp acceptance test. A Kelp service later will introduce
its own resources (cluster, node pool, Kubernetes version), but
none of those land in v1.

## 4. Minimum vnext primitives Kelp needs

Kelp's minimum surface is small because Talos is opinionated. v1
only needs to ship the rows marked **v1**; rows marked **post-v1**
are deferred.

| # | Primitive | Why Kelp needs it | Status |
|---|---|---|---|
| 1 | bhyve-branded VM lifecycle (create/start/stop/restart/delete) | Talos control-plane and worker nodes are bhyve VMs in v1 | v1 |
| 2 | VPC + subnet | per-cluster network isolation | v1 |
| 3 | Private NIC attachment with MAC + IPv4 (and optional IPv6) | Talos node addressing | v1 |
| 4 | Security group + SG rule + NIC↔SG attachment | Talos API (50000/tcp), Kubernetes API (6443/tcp), etcd (2379–2380/tcp), kubelet (10250/tcp), CNI / overlay ports | v1 (Agent A §4.4–§4.6) |
| 5 | NAT gateway | egress for nodes and pods (routed-pod-CIDR mode) | v1 (Agent A §4.3) |
| 6 | Floating IP (1:1 NAT, with attach/detach to a NIC) | Kubernetes API and Talos API ingress for the operator | v1 |
| 7 | Image catalog with content-addressed images and a Talos compatibility flag | Talos image must be importable, fingerprintable, and selectable by name+version | v1, with manifest extension (§5) |
| 8 | Guest machine-config delivery via NoCloud cidata + `customer_metadata['cloud-init:*']` | Talos cannot boot without machine-config | shipping (smartos-live af99d6a1); v1 only needs to surface the image tag and forward customer_metadata — see §5.2 |
| 9 | Realized-state visibility for instance and NIC | Kelp must know when a node has applied its NIC config and is reachable | v1 (Agent A `RealizedNetworkState` slice) |
| 10 | Route table with per-prefix static routes | post-v1 routed-pod-CIDR mode points pod CIDR at a node NIC | v1 model must be extensible (Agent A §4.1–§4.2) |
| 11 | DNS / service discovery, VPC-scoped private zones, health-aware records | Kelp `LoadBalancer` Services and per-cluster service-discovery records | post-v1, but data model decided in v1 |
| 12 | Cluster, node pool, Kubernetes version resources | Kelp service itself | post-v1 |
| 13 | Cilium policy backend driven from a tritond policy IR | unified VM/pod policy | post-v1 (Proteus `TODO.md` Phase 10 items 11–13) |
| 14 | Triton Cloud Controller Manager | node routes, LB integration, Gateway API | post-v1 (Proteus `TODO.md` Phase 10 item 13) |
| 15 | Routed pod CIDR allocator + per-node lease | one route per node-pod-CIDR in VPC route table | post-v1 (Proteus `TODO.md` Phase 10 item 14) |
| 16 | CSI driver (NFS volumes via vnext volume API) | persistent storage | post-v1 — also blocked on a vnext volume API |
| 17 | Cluster autoscaler integration | node-pool scaling | post-v1 |
| 18 | LoadBalancer Service path (in-cluster Triton VMs first, edge-gateway L4 later) | Kubernetes LB workloads | post-v1 |
| 19 | Gateway API integration | future ingress shape | post-v1 |

The most important v1 line items are #7 and #8: without a Talos-aware
image catalog and a real guest-config channel, even a manual Kelp
acceptance test on v1 fails.

## 5. Talos image and machine-config requirements

### 5.1 Image manifest

Kelp requires the v1 image catalog to model:

- **Architecture**: `amd64` minimum. arm64 is post-v1.
- **Brand compatibility**: explicit `bhyve_compatible` flag. Talos
  v1 testing runs as `brand=bhyve` per `triton-cloud.md`.
- **Boot mode**: UEFI is preferred. Talos AMD64 ships UEFI by
  default; direct-kernel boot is a workaround if UEFI is not
  available in v1 bhyve.
- **Disk format**: raw or ZFS-streamed Talos image.
- **Talos version**: declared in the image manifest, not implied by
  filename. Kelp must be able to map a Kubernetes version to a
  Talos version at cluster create time.
- **Integrity**: SHA-256 (already in `tritond-store::Image`).
- **Metadata-channel hint**: which guest-config channel the image
  expects (`nocloud-iso`, `metadata-server`, `kernel-cmdline`).
  Without this hint, Talos cannot find its config.

### 5.2 Guest machine-config channel — NoCloud via SmartOS

**Decided.** Triton vnext uses the **NoCloud** cloud-init datasource on
bhyve, landed upstream in `smartos-live` commit
`af99d6a1` ("OS-8711 Support cloud-init NoCloud datasource for Bhyve
guests", 2026-03-04). Talos's own `nocloud` platform reads `user-data`
as the machine config, which fits this channel directly.

How it works in `smartos-live` af99d6a1:

- **Image opt-in.** The image manifest carries an
  `org.smartos:cloudinit_datasource` tag. When set to `nocloud`,
  `VM.js` propagates it to `internal_metadata.cloudinit_datasource =
  'nocloud'` at provision time.
- **Disk generation.** During `createVM`, if the brand is `bhyve` and
  `cloudinit_datasource === 'nocloud'`, `cloudinit.nocloud.updatePayloadDisks`
  appends a FAT16 cidata volume to `payload.add_disks` and stores its
  path in `internal_metadata.cloudinit_nocloud_path`. The FS is built
  by `cloudinit/lofs-fat16.js` at instance start by
  `cloudinit.nocloud.createPCFS`.
- **Payload contents.** The cidata FS contains the standard NoCloud
  files (`meta-data`, `user-data`, `network-config`, `vendor-data`).
  Their contents come from `customer_metadata`:
  - `cloud-init:user-data` — full machine-config blob (Talos YAML).
  - `cloud-init:network-config` — optional override; if absent, VM.js
    generates network-config v2 automatically from `payload.add_nics`,
    matching by MAC address and respecting `dhcp` / `addrconf`.
  - `cloud-init:meta-data` — instance-id and hostname.
  - `cloud-init:vendor-data` — optional.
- **Refresh on change.** A SHA-256 hash of the rendered NoCloud
  payload is stored in `internal_metadata.cloudinit_nocloud_hash`.
  On VM start, the FS is rebuilt only if the hash changes, so
  pushing a new machine-config is just a `customer_metadata` update
  followed by a restart.

What v1 needs to do on top of this:

1. **Image catalog.** `tritond` must surface the
   `org.smartos:cloudinit_datasource` tag on import, so the
   `Image` record can declare `metadata_channel = "nocloud"` and
   Kelp can refuse to use a non-NoCloud image for a Talos cluster.
2. **Per-instance config.** `tritond`'s instance-create path must
   forward an opaque `customer_metadata` map to `tritonagent`, with
   an authorization model and audit-redaction story for the
   `cloud-init:*` keys (they contain bootstrap secrets — Talos PKI
   bootstrap material, kubelet bootstrap tokens, tenant SSH-equivalent
   identity). Reading these keys back must be project-scoped at
   minimum, with redaction in audit logs.
3. **Network-config behavior.** v1 should default to letting
   `VM.js` auto-render `network-config` from NICs. Kelp can override
   per-cluster if Talos networking needs (e.g. VIP on the control
   plane) require it; the override path is just a
   `cloud-init:network-config` key.
4. **No `tritonagent` work to build the cidata disk.** The platform
   builds it. The agent only has to forward `customer_metadata`
   correctly and not strip the `cloud-init:*` keys.

Talos `metal` (kernel-cmdline `talos.config=…`) and a hypothetical
Talos `triton` platform remain options for the future, but **vnext
v1 commits to NoCloud**. This closes `triton-cloud.md` §11 open
question #6 ("minimum guest metadata/config mechanism") for v1.

### 5.3 PKI and kubeconfig

Talos clusters require their own PKI. v1 does not need to know
anything about it; PKI generation, storage, and rotation will live
inside Kelp later. The only v1 touchpoint is that the machine-config
blob can contain Talos PKI material and must therefore be treated as
a secret at the tritond control-plane and audit layers.

### 5.4 Open image questions

- Does v1 ship a curated public Talos image, or does the operator
  publish their own?
- Does the image catalog allow per-image declared minimums (vCPU,
  RAM, root disk size) so Kelp can refuse an under-spec'd flavor?
- Where does the Talos→Kubernetes version map live: in the image
  manifest, in a tritond-side map, or in Kelp's own resource model
  later?

## 6. Control-plane endpoint access

Kelp clusters expose two TCP services that callers outside the
cluster need:

- **Talos API** on port 50000/tcp (gRPC, mTLS) — used during
  bootstrap, upgrade, reset, kubeconfig retrieval, and health
  probes.
- **Kubernetes API** on port 6443/tcp (HTTPS, mTLS) — used for all
  normal `kubectl` traffic.

A v1.x Kelp service runs in the operator silo and needs reachability
to both ports for every cluster. Manual v1 acceptance only needs the
operator's workstation to reach those ports.

Four reachability options, in increasing isolation:

1. **FIP per cluster** (simplest, v1-compatible).
   - Allocate one FIP per cluster control plane and attach it to
     either a single control-plane node (1-node test) or a small
     in-VPC HAProxy / kube-vip VM (3-node HA test).
   - SG rule restricts 6443/tcp and 50000/tcp to a known operator
     CIDR or to "any" only for short-lived testing.
   - This works on the v1 primitive set as-shipped.
2. **Private endpoint in an operator VPC**.
   - Both the operator silo's Kelp service and the customer cluster
     attach to the same `EdgeCluster` or shared services VPC, with
     SG rules instead of FIPs.
   - Requires post-v1 inter-VPC connectivity / `EdgeGateway`
     primitives (out of v1 scope per `triton-cloud.md` §6 / `proteus/TODO.md`
     Phase 10 items 1, 42–45).
3. **Bastion / gateway VM**.
   - A small SSH/HTTPS bastion VM in the cluster's VPC fronts both
     APIs.
   - Works on v1 primitives but adds an operational liability
     (bastion VM lifecycle, image, patching) that Kelp should
     prefer not to own.
4. **Management reachability via a dedicated management VPC**.
   - Reuse classic Triton's "admin network" pattern via a
     post-v1 management `EdgeGateway`.

For v1 acceptance: option 1. Kelp later should converge on option
2, with a documented `EdgeGateway`-side termination of the K8s
API. Triton-Talos-Bridge (a metadata-channel side door from Talos
to TritonAPI) is **not** required and Kelp should not assume it; if
a bridge becomes desirable later, it should re-use the same
guest-config / metadata channel that v1 ships in §5.2.

## 7. NAT and egress

v1 covers the ordinary case. Pod egress (post-v1) requires
care to avoid wire breaks.

### 7.1 Node egress

Each Talos node is a normal VM with a private NIC on the cluster's
VPC subnet. Egress goes through the project's `NatGateway` (Agent A
§4.3). No new contract needed.

Kelp should request that the Agent A `NatGateway` carry an optional
identity/tag so that, post-v1, pod-egress flows from a worker pool
can be distinguished from node-self egress for flow logs and
billing. Adding it later is a small wire change; deciding the
field shape now is free.

### 7.2 Pod egress (post-v1)

Two modes:

- **Cilium overlay** — pods are NAT'd to the node's primary IP,
  egress flows from the node, all node-egress rules still apply.
  Acceptable bootstrap. Routed-pod-CIDR is preferred long-term per
  RFD 00001/07.
- **Routed pod CIDR** — each node owns a /24 (or sized) pod CIDR
  inside the VPC; the VPC route table has one route per node,
  pointing the pod CIDR at the node's NIC. Egress applies SG rules
  and NAT GW the same way node egress does.

Routed pod CIDR mode imposes one v1-shape requirement on Agent A:
the `RouteTable` model must support per-prefix static routes that
target a NIC ID (not just a NAT GW or an internet gateway). Agent
A's current §4.2 covers this; Kelp simply records that the v1
shape must keep that `Route::next_hop` variant intact.

### 7.3 Pod ingress

`Service type=LoadBalancer` ingress is post-v1. Two compatible
realizations exist:

- **In-cluster LB VMs (Moirai pattern)**. A small Triton-side
  controller provisions HAProxy VMs per Service, attaches a FIP,
  registers a CNS (or vnext-DNS) name. Works on v1 primitives
  manually; productized in Kelp post-v1.
- **Edge-gateway L4**. The eventual product mode: tritond allocates
  a VIP from the edge cluster's pool, programs the Proteus edge
  blueprint, and updates the Service status. Requires post-v1
  `EdgeGateway` + `EdgeCluster` plumbing.

Kelp should use the in-cluster LB VM pattern for the first Kelp
release and migrate to edge-gateway L4 when `EdgeCluster` is in
production.

## 8. Kubernetes networking requirements

Per RFD 00001/07 and `PROTEUS_PLAN.md` §11.9:

- **Cilium is the first Kubernetes pod dataplane.** Kelp should not
  build a Triton-native pod dataplane. Cilium policy is rendered
  from a Triton policy IR (post-v1 — Proteus `TODO.md` Phase 10
  items 11–15).
- **Distributed firewall stays at the VM port.** Cilium enforces
  pod-to-pod policy; Proteus enforces VM/VPC/edge policy. Both
  layers can deny traffic. Traceflow must show both.
- **Routed pod CIDR is the long-term mode.** Cilium overlay is
  acceptable for the first Kelp release.
- **kube-proxy replacement** (Cilium) is acceptable. Whether it is
  default depends on Talos profile choice; out of vnext scope.
- **Hubble** lives inside the cluster. Triton flow-log
  correlation with Hubble is post-v1.
- **Gateway API** is post-v1 and depends on edge-gateway L4
  termination.

Kelp must, at a minimum, document the chosen Cilium mode and the
chosen IPAM mode at create time, and refuse to mix modes within a
cluster.

## 9. What Kelp can use to test v1 without becoming a v1 blocker

The following tests can be run with **only v1 primitives** and
**no Kelp service code**. They prove v1 has not painted Kelp into a
corner. None of these are required for v1 acceptance; they are
proposed acceptance-time experiments.

### 9.1 Single-node Talos stunt cluster

Goal: prove machine-config delivery, FIP, SG, and bhyve image flow
work end-to-end.

Steps:

1. `tcadm bootstrap` and `tcadm cn approve` (already required for
   v1).
2. Operator imports a Talos AMD64 bhyve-compatible image into the
   project image scope.
3. Operator creates a project, a VPC, and a `/24` subnet.
4. Operator allocates a `NatGateway` for the VPC's default route.
5. Operator allocates a `FloatingIp` and reserves it.
6. Operator creates a SG with two ingress rules: 50000/tcp from
   their own IP, 6443/tcp from their own IP. Default deny inbound,
   allow outbound.
7. Operator creates a single `brand=bhyve` instance from the Talos
   image, attaches it to the subnet, attaches the SG to its NIC,
   and supplies the rendered Talos machine-config in
   `customer_metadata['cloud-init:user-data']`. VM.js (smartos-live
   af99d6a1) builds a FAT16 cidata disk from that metadata at start;
   no extra agent work required. Network-config v2 is auto-rendered
   from the NIC payload, so DHCP / addrconf "just works" for typical
   subnets.
8. Operator attaches the FIP to the instance NIC.
9. Operator runs `talosctl --nodes <FIP> bootstrap`.
10. Operator runs `talosctl --nodes <FIP> kubeconfig`.
11. Operator runs `kubectl get nodes`. Expect `Ready`.

Step 7 is unblocked at the platform layer by smartos-live af99d6a1.
The remaining v1 risk is that `tritond` strips or misroutes the
`cloud-init:*` keys, or that the image catalog does not propagate
the `org.smartos:cloudinit_datasource` tag. Both are small and
covered by the v1 image-manifest and instance-create test plans.

### 9.2 3-node HA Talos stunt cluster

Same as §9.1, but with three control-plane bhyve VMs and a small
HAProxy / kube-vip bhyve VM (or in-cluster `keepalived`) holding
the FIP. This proves SG semantics across three nodes (etcd peer
ports 2379–2380/tcp), `NatGateway` egress under load, and FIP
attach-detach behavior under failover.

### 9.3 In-cluster pod test

Cilium overlay is sufficient. Once nodes are `Ready`, deploy the
Kubernetes e2e conformance suite and run a smoke profile. This
flushes out:

- DHCP / network-config issues at node boot;
- MTU mismatches between bhyve virtio and the VPC overlay;
- SG rule completeness (kubelet 10250, Cilium VXLAN/Geneve, etc.).

### 9.4 Per-Service LoadBalancer manual

Manually provision a HAProxy bhyve VM per `LoadBalancer` Service
the Kubernetes test profile creates. Reuse `NatGateway` egress for
return traffic. This proves the v1 primitive set can mimic the
Moirai LB pattern manually — and therefore that Kelp's eventual LB
controller will land cleanly on top.

### 9.5 Things v1 acceptance does *not* need

- Cluster, node pool, Kubernetes version resources.
- A `triton k8s` CLI.
- A Terraform provider extension.
- CCM, CSI, autoscaler in-cluster.
- Cilium policy compilation from Triton policy IR.
- Routed pod CIDRs.
- Gateway API.

If any of those slip into v1 to make a stunt cluster work, Kelp has
become a v1 blocker. They belong post-v1.

## 10. Explicit deferrals

Stated for clarity, so v1 owners do not feel pressured to land
these:

- Kelp service in tritond (no `Cluster`, `NodePool`, `KubernetesVersion`
  resources in v1).
- Kubernetes cluster lifecycle automation (bootstrap, scale,
  upgrade, delete).
- `triton k8s` CLI subcommand.
- Terraform provider integration.
- Triton Cloud Controller Manager.
- Cilium installation, configuration, and policy compilation.
- CSI driver against vnext volumes (also blocked on a vnext volume
  API surface).
- Cluster autoscaler external gRPC provider.
- LoadBalancer Service controller (Moirai-equivalent).
- Gateway API integration.
- Web UI for cluster management.
- triton-talos-bridge (out-of-band metadata channel from Talos to
  tritond). NoCloud cidata (§5.2) covers the inbound config
  direction. If outbound Talos→tritond signalling is wanted later,
  it should reuse the cidata path or a future vnext metadata server
  rather than introduce a new side door.
- `fhyve`-branded Talos VMs. v1 stays on `bhyve`; `fhyve` for tenant
  Talos is post-v1.

## 11. Proposed post-v1 workstream sequence

This is the order Kelp work should land **after** v1 is shipping a
reliable VM + VPC + NAT path. Each line is one workstream, not one
commit.

1. **Kelp service skeleton in `tritond`** —
   `Cluster`, `NodePool`, `KubernetesVersion`, `TalosImageMap`
   resources; create / get / list / delete; storage in
   `tritond-store`; audit; tenant-scoped Cedar rules.
2. **Talos image catalog hardening** —
   formalize manifest fields chosen in §5.1; curated public Talos
   image; per-image declared CPU/RAM/disk minimums; image→Talos
   version→Kubernetes version map.
3. **Machine-config UX** — Kelp's `Cluster` create/upgrade flows
   render Talos machine-config and write it to
   `customer_metadata['cloud-init:user-data']` via the existing
   instance-create/update path. Add `tcadm`/CLI exposure for the
   cluster-side rendering, talosctl-equivalent kubeconfig
   retrieval, and rotation. The cidata channel itself is already
   shipping (§5.2) and does not need productization.
4. **Cluster bootstrap saga** — generate Talos PKI; render
   machine-config; provision control-plane VMs; attach SGs;
   allocate FIP; bootstrap etcd; record state in `tritond-store`.
5. **Worker pool lifecycle** — provision worker VMs; join cluster;
   manual scale up/down; manual delete with drain.
6. **Kubeconfig retrieval** — minimal API endpoint that wraps
   `talosctl kubeconfig`; scoped credentials with explicit lifetime.
7. **Cluster health endpoint** — aggregate Talos service health and
   Kubernetes node readiness.
8. **Rolling Kubernetes / Talos upgrade** — talosctl-driven node
   upgrade with health verification between steps.
9. **In-cluster LoadBalancer controller** (Moirai-equivalent) —
   per-Service Triton VMs, FIP attachment, vnext-DNS round-robin.
10. **Triton Cloud Controller Manager** — node identity, node
    addresses, route publication for pod CIDRs, LoadBalancer
    integration (initially against the controller from step 9, later
    against edge-gateway L4).
11. **Routed pod CIDR allocator** — per-cluster pod CIDR; per-node
    /24 lease; route publication into `RouteTable`; release on
    node delete.
12. **Cilium policy backend** — render Cilium NetworkPolicy /
    CiliumNetworkPolicy from Triton policy IR; correlate with
    Proteus distributed-firewall decisions.
13. **Cilium installation / Talos profile integration** — install
    Cilium with the chosen routing mode (overlay first, routed
    long-term); kube-proxy replacement decision.
14. **Edge-gateway L4 LoadBalancer path** — VIP allocation from
    `EdgeCluster`, Proteus edge blueprint programming, Service
    status updates.
15. **Gateway API integration** — Gateway listeners → Triton VIPs/FIPs.
16. **CSI driver** — depends on a vnext volume API; out of Kelp's
    direct path until volumes ship.
17. **Cluster autoscaler external gRPC provider** — per-pool;
    scale-up first; scale-down after drain support.
18. **Hubble / flow-log correlation** — link Cilium pod identities
    to Proteus port identities for unified traceflow.
19. **`triton k8s` CLI** — wraps the cluster API.
20. **Terraform provider** — `triton_k8s_cluster`, etc.
21. **Web UI** — cluster management surface.

Items 1–8 are the natural Kelp "v1.x" cut. Items 9–13 are "v2"
when routed pod CIDRs and Cilium policy compilation become
foundational. Items 14–21 are mature-product work.

## 12. Open questions for the manager

These are decisions that should be made for v1 even if the
implementation slips into v1.x:

1. **Guest-config channel for v1** — *closed.* NoCloud cidata via
   smartos-live af99d6a1, driven by the
   `org.smartos:cloudinit_datasource=nocloud` image-manifest tag
   and `customer_metadata['cloud-init:*']`. v1 work: surface the
   tag on `Image`, forward `customer_metadata` opaquely, redact
   `cloud-init:*` keys in audit/unauth reads. Detail in §5.2.
2. **Image manifest extensions** — is `bhyve_compatible`, `arch`,
   `boot_mode`, `talos_version`, and the
   `org.smartos:cloudinit_datasource` tag passthrough v1, or is a
   richer `compatibility` blob deferred? Recommended: minimum v1
   field set is `arch`, `bhyve_compatible`, and the cloud-init
   datasource tag passthrough; the rest can be deferred.
3. **DNS / service discovery commitment** — does v1 reserve the
   data-model decision (CNS-shaped, FDB-backed, anycast on every
   CN) even if the surface is post-v1? Recommended: yes. Reserving
   the model in v1 prevents a post-v1 wire break.
4. **`NatGateway` worker-pool identity field** — does Agent A's v1
   `NatGateway` carry an optional `tag`/`identity` field so post-v1
   pod-egress flow logs can distinguish worker pools? Recommended:
   add the field as `Option<String>` now to avoid a wire break later.
5. **Image-scope choice for Talos images** — does Kelp expect
   public, silo, tenant, or project image scope? Recommended:
   `silo`-scope (operator-curated, tenant-readable) as the default,
   with `project`-scope override for tenants that build their own.
6. **PKI custody** — does v1 commit that machine-config blobs are
   secrets in tritond, with redaction in audit and unauthenticated
   reads? Recommended: yes. Required for any Talos work.
7. **Talos → Kubernetes version mapping location** — image
   manifest, tritond-side map, or Kelp resource model? Recommended:
   tritond-side map in v1.x once Kelp lands; image manifest
   in v1 if a single curated image is shipped.
8. **Cilium mode default for Kelp v1.x** — overlay or routed? If
   routed: routed-pod-CIDR allocator must land before Kelp v1.x.
   Recommended: overlay default for Kelp v1.x; routed in v2.

## 13. Cross-agent contracts

This document does **not** propose any tritond contract changes.
It records soft requirements that, if accommodated in v1, prevent a
breaking change later:

- **Agent A (`vpc-control-plane-v1.md`)**:
  - `RouteTable` / `Route` `next_hop` variant must include "NIC ID"
    (already present in §4.2). Required for routed pod CIDRs.
  - `NatGateway` should carry an optional `identity` / `tag` field
    (recommendation §12 #4). Cheap to add now; expensive later.
  - `SecurityGroupRule` must support port-range + protocol +
    CIDR predicates (already present in §4.5). Required for Talos
    API and Kubernetes API rules.
  - `RealizedNetworkState` realized-vs-desired generation model
    (Agent A H-1) is what Kelp later uses to know when nodes are
    actually network-reachable.
- **Agent B (Proteus blueprint compiler)**: no Kelp-specific
  blueprint requirement; Kelp uses normal VPC blueprints in v1.
  Cilium policy compilation is post-v1 (Proteus `TODO.md` Phase
  10 items 11–13).
- **Agent C (`tritonagent` realized-state)**: existing instance /
  NIC realized-state suffices for the §9.1 stunt cluster.
- **Agent D (firehyve / fhrun edge runtime)**: no Kelp dependency
  in v1. Post-v1 edge-gateway L4 LoadBalancer path will land on top
  of Agent D's edge-cluster work.
- **Agent E (NAT and north/south)**: no Kelp dependency in v1
  beyond the `NatGateway` realization that Kelp also uses for node
  and pod egress.
- **Image catalog owner (unassigned)**: Kelp asks that v1 surface
  the `org.smartos:cloudinit_datasource` image-manifest tag on the
  `Image` record (read-through from imgadm/IMGAPI manifest tags),
  plus the manifest fields in §5.1.
- **Instance-create surface (Agent F / `tritond`)**: Kelp asks
  that the v1 `NewInstance`/instance-create path forward
  `customer_metadata` opaquely to `tritonagent`, with
  project-scoped authorization on the `cloud-init:*` keys and
  audit redaction on those keys (they carry Talos PKI bootstrap
  material and kubelet bootstrap tokens). No new resource
  required, just the metadata field on instance create + an
  audit-redaction list. Closes the v1 work for §5.2.
- **DNS / service discovery owner (unassigned)**: Kelp asks for
  the data-model commitment in §12 #3.

## 14. Where this doc fits

- `triton-cloud.md` §"Kelp Relationship" — describes Kelp as a
  downstream acceptance workload. This doc is the concrete shape of
  that relationship.
- `rfd/00001/07-kubernetes-talos-networking.md` — describes
  Kubernetes/Talos networking architecture. This doc consumes that
  RFD detail and translates the network-side requirements into v1
  scope decisions.
- `monitor-reef/docs/design/vpc-control-plane-v1.md` — Agent A's v1
  VPC desired-state model. This doc references that doc's resources
  but proposes no changes to them.
- `PROTEUS_PLAN.md` and `proteus/TODO.md` — Proteus dataplane plan
  and tracker. This doc references their post-v1 Cilium / CCM /
  routed-pod-CIDR work as Kelp's natural home post-v1.
