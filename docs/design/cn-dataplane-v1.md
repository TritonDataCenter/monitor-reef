<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

Copyright 2026 Edgecast Cloud LLC.
-->

# CN Dataplane v1 - bhyve VM and Proteus realization

> Owner: Agent C (CN dataplane).
> Status: design, not yet implemented.
> Scope: `tritonagent` on SmartOS compute nodes.
> Out of scope: VPC intent APIs, Proteus blueprint compiler,
> firehyve/fhrun edge runtime, NAT/FIP packet behavior, UI, Kelp.

This document defines the v1 compute-node actuation contract for
tenant VMs. For v1, tenant VMs are bhyve-branded VMs created through
the normal SmartOS `vmadm` lifecycle. firehyve/fhrun is reserved for
north/south edge services and is not the tenant VM runtime.

## 1. Goals

v1 `tritonagent` must turn `tritond` desired state into realized
SmartOS host state:

* create bhyve VMs with the CPU, memory, disk, image, and metadata
  shape requested by `tritond`;
* create and attach one Proteus-backed port per tenant NIC;
* apply the compiled Proteus `PortBlueprint` for every NIC;
* start, stop, restart, pause, and delete host-side VM and port state;
* report applied generations and useful failures back to `tritond`;
* clean up partial VM, NIC, and port state after delete or failed
  provisioning.

The first implementation slice after this design must be narrow and
side-effect-free. The recommended first code slice is a pure bhyve
`vmadm` payload builder with unit tests, without executing `vmadm`
and without adding Proteus side effects.

## 2. Current capabilities

The current `tritonagent` already has these working pieces:

* first-boot self-registration through `/v2/agent/register`, claim-code
  approval, and per-CN API-key persistence at
  `/var/lib/tritonagent/credentials`;
* authenticated job claim, job blueprint fetch, and job complete calls;
* per-CN binding: a CN-bound key must claim, fetch, and complete jobs
  as the bound SmartOS `server_uuid`;
* background heartbeat and opaque CN status posts with VM, zpool,
  memory, disk-usage, boot-time, and timestamp data;
* image materialization through direct ZFS receive, sha256 verification,
  idempotent `zones/<image_id>@final` detection, and synthetic imgadm
  manifest publication;
* image platform compatibility checking via SmartOS buildstamp;
* `vmadm` create, stop, reboot, and idempotent delete for current
  Phase 0 jobs;
* a harvested `tritond-cn-platform` crate that provides SmartOS
  wrappers for `vmadm`, ZFS, kstat, sysinfo, and CN status collection.

`tritond` already provides:

* `ProvisioningJob` records for `Provision`, `Stop`, `Restart`, and
  best-effort `Delete`;
* `ProvisioningBlueprint` with `instance`, `image`, `nics`, `disks`,
  and `ssh_public_keys` for `Provision`;
* lifecycle advancement from `Pending` to `Provisioning` on claim and
  to `Running`, `Stopped`, or `Failed` on complete;
* instance create that atomically creates the instance, its NIC records,
  and boot disk records;
* instance delete that clears the control-plane record, cascades NICs,
  disks, and FIP attachments in the store, then enqueues a best-effort
  host delete job.

## 2.1 CN Placement Role

Every registered CN carries an operator-controlled placement role:
`tenant`, `edge`, or `both`. New registrations default to `tenant` so the
existing lab fleet remains tenant-workload capable until an operator opts a
node into edge placement.

Operators set the role through `POST /v2/cns/{server_uuid}/role`, exposed by
`tcadm cn label set <server_uuid> --role <tenant|edge|both>`. The role is a
placement admission signal only; it does not change agent authentication,
heartbeats, or job-claim binding. The north/south edge placer consumes
`edge` and `both` records when assigning firehyve/fhrun edge instances.

Tenant VM placement for M1 is intentionally small:

* `tritond` considers only approved CNs with a recent registration or
  heartbeat timestamp and role `tenant` or `both`;
* create chooses the eligible CN with the fewest already assigned
  instances, breaking ties by SmartOS `server_uuid` for deterministic
  tests and repeatable lab behavior;
* the selected CN is persisted on `Instance.host_cn_uuid`;
* every subsequent Provision, Stop, Restart, and Delete job for that
  instance uses `target_cn_uuid = Instance.host_cn_uuid`, so only the
  bound `tritonagent` on that SmartOS host can claim it;
* in-process test deployments that have no registered CNs keep using
  unrouted jobs so the stub provisioner remains useful during local
  development. Real-agent deployments should run `tritond` with
  `TRITOND_DISABLE_INPROCESS_PROVISIONER=1`, which makes missing tenant
  capacity fail instance create instead of leaving a job without an
  external consumer.

North/south edge placement for M1 uses the same durable job queue rather
than a separate pub-sub channel:

* the first tenant port blueprint whose active route table targets a
  `NatGateway` materializes one `EdgeClusterKind::NatGateway` cluster
  if that gateway is not already bound;
* the M1 placer considers only approved CNs with role `edge` or `both`,
  an activity timestamp, and an IPv6 edge-underlay hint in sysinfo or
  status;
* the cluster gets one `EdgeClusterInstance` in M1. The record shape
  already supports additional instances for HA after the single-edge MVP
  is running;
* tritond renders the fhrun manifest with dataplane
  `backend = "nftables"`, persists the edge-control socket path in the
  instance record, and enqueues `JobKind::EdgeApply` with
  `target_cn_uuid` set to the selected edge CN;
* tenant CN port blueprints include `EdgeClusterIntentV1` underlay
  coordinates so `RouteTarget::NatGateway` can lower to a Proteus edge
  target.

The queue remains a poll/claim loop for M1 because it is restart-safe and
matches SmartOS service behavior. A later long-poll, SSE, WebSocket, or
NATS-style pub-sub transport can reduce idle latency, but it should carry
the same durable job ids and completion semantics rather than replacing
the queue as source of truth.

`tritonagent` applies edge jobs on the selected edge CN:

* `TRITONAGENT_EDGE_ROOT` defaults to `/var/lib/tritonagent/edge`.
  Each edge instance uses `<edge_root>/<edge_instance_id>/manifest.json`,
  `fhrun.pid`, stdout/stderr logs, and `edge-control.sock`;
* `TRITONAGENT_FHRUN_BIN` defaults to `/opt/firehyve/bin/fhrun`;
* `JobKind::EdgeApply` parses and validates the shared fhrun manifest,
  rejects any v1 dataplane backend other than `nftables`, writes the
  manifest atomically, runs `fhrun --check`, and starts fhrun as a
  supervised host process;
* re-applying an unchanged manifest is idempotent when the recorded fhrun
  pid is still running. A changed manifest restarts the local fhrun
  process because live `dataplane.replace` is reserved for a later slice;
* `JobKind::EdgeReap` best-effort terminates the recorded fhrun pid and
  removes the runtime directory, so orphan cleanup can be driven through
  the same durable job queue.

## 3. Current gaps

The host realization path is still Phase 0 shaped:

* the create payload is for a `joyent-minimal` zone, not `brand=bhyve`;
* CPU and memory are mapped to zone controls, not bhyve `vcpus` and
  `ram`;
* disk realization is reduced to zone quota/image clone behavior, not
  boot zvol creation plus bhyve disk devices;
* package/SKU semantics do not yet exist in the vnext `Instance` shape;
* guest configuration is limited to `root_authorized_keys`; there is no
  explicit cloud-init/no-cloud/metadata ISO contract for Linux guests;
* NICs are attached to the flat `admin` nic tag with hardcoded netmask,
  resolver, VLAN, and MTU;
* there is no Proteus port id, no Proteus port create/apply/start path,
  and no port cleanup;
* no applied generation is reported for VPC, subnet, route, security
  group, NAT, FIP, or NIC-related desired state;
* job failures are free-form strings; they are useful, but not yet
  phase-structured enough for `tcadm doctor`;
* Delete jobs are best-effort after the control-plane record is already
  gone, so host leaks remain possible if enqueue or agent execution fails.

## 4. v1 provisioning inputs

`tritonagent` should continue fetching one job blueprint after claim, but
the `ProvisioningBlueprint` needs an additive network extension once
Agent A and Agent B stabilize their contracts.

Required v1 fields, grouped by owner:

* Existing `tritond` fields: `Instance`, `Image`, `Vec<Nic>`,
  `Vec<Disk>`, and raw SSH public keys.
* Agent A desired/applied state: stable resource ids and monotonic
  desired generations for the intent records that affected each NIC's
  compiled blueprint.
* Agent B compiler output: one Proteus `PortBlueprint` per NIC, including
  `PortBlueprint.port_id`, `network_id`, `generation`, `ClientLinkConfig`,
  and postcard-encoded `TritonVpcBlueprint`.
* Agent C local state: host-side mapping from `instance_id` and `nic_id`
  to Proteus port state, used only for idempotence and cleanup.

The blueprint extension should be additive so old Stop/Restart/Delete
jobs remain valid:

```rust
pub struct ProvisioningBlueprint {
    pub job_id: Uuid,
    pub kind: JobKind,
    pub instance: Option<Instance>,
    pub image: Option<Image>,
    pub nics: Vec<Nic>,
    pub disks: Vec<Disk>,
    pub ssh_public_keys: Vec<String>,
    pub network_ports: Vec<NetworkPortRealization>, // new
}

pub struct NetworkPortRealization {
    pub nic_id: Uuid,
    pub port_id: Uuid,
    pub desired_generation: u64,
    pub port_blueprint: PortBlueprint,
    pub affected_resources: Vec<NetworkResourceGeneration>,
}
```

`affected_resources` is optional for the first code slice, but v1 needs it
for precise realized-state reporting. Agent B owns the mapping between a
compiled port blueprint and the intent records it represents.

## 5. bhyve `vmadm` payload requirements

The v1 payload builder should be a typed Rust builder that renders JSON for
`VmadmTool::create`. It should be testable without a SmartOS host.

The target payload shape is:

```json
{
  "uuid": "<instance_id>",
  "brand": "bhyve",
  "alias": "<instance.name or tritond-<id>>",
  "hostname": "<same>",
  "ram": 4096,
  "vcpus": 2,
  "disks": [
    {
      "boot": true,
      "model": "virtio",
      "image_uuid": "<image_id>",
      "size": 42949672960
    }
  ],
  "nics": [
    {
      "interface": "net0",
      "mac": "<nic.mac>",
      "model": "virtio",
      "mtu": 1500,
      "primary": true
    }
  ],
  "customer_metadata": {
    "root_authorized_keys": "..."
  },
  "internal_metadata": {
    "tritond.instance_id": "<instance_id>",
    "tritond.tenant_id": "<tenant_id>",
    "tritond.project_id": "<project_id>",
    "tritond.image_sha256": "<sha256>",
    "tritond.vm_brand": "bhyve"
  },
  "tags": {
    "tritond.instance_id": "<instance_id>",
    "tritond.tenant_id": "<tenant_id>",
    "tritond.project_id": "<project_id>"
  }
}
```

Exact SmartOS field names for bhyve disk and NIC attachment must be checked
against the lab host before live execution is enabled. The first builder
slice should encode the intended contract and unit-test it, then a later
SmartOS slice can adjust field names from observed `vmadm` behavior before
turning on execution.

### 5.1 CPU, RAM, and package

Current vnext instances carry `cpu` and `memory_bytes`. For bhyve:

* `cpu` maps to `vcpus`;
* `memory_bytes` maps to `ram` in MiB, clamped to a sane minimum;
* package/SKU defaults are not yet modeled in vnext. Until Agent A or a
  scheduler slice adds a package resource, the builder uses the explicit
  instance fields and records "package unresolved" as a non-fatal
  planning gap.

The builder should reject zero CPU and zero/too-small memory before calling
`vmadm`.

### 5.2 Image and boot disk

For v1 the agent still materializes image content first. The bhyve builder
then creates a boot disk device from the boot `Disk` record:

* choose the `DiskKind::Boot` disk whose `source_image_id` equals the
  blueprint image id;
* set the boot disk size from `Disk.size_bytes`, not from the image alone;
* carry `image_uuid` where SmartOS `vmadm` can clone the imported image;
* use virtio block unless a future image compatibility record requires a
  different model;
* fail before `vmadm create` if no boot disk or no image is present.

Longer term, v1 can support additional `DiskKind::Data` records by adding
more `disks` entries. That is not part of the first code slice.

### 5.3 Primary NIC

Every `Nic` in the blueprint maps to one guest virtio NIC:

* interface names are stable by sorted input order, with primary first;
* MAC comes from `Nic.mac`;
* MTU comes from Agent B's `PortBlueprint.link.mtu` when present;
* no flat admin-network `ip` or hardcoded `netmask` appears in the bhyve
  payload for Proteus-backed tenant NICs;
* guest IPs, gateway, DHCP, DNS, routes, and firewall policy are delivered
  by Proteus, not by a flat `vmadm` admin NIC.

The open SmartOS detail is how the bhyve NIC is attached to the Proteus
client link. The preferred split is:

1. Agent C creates the Proteus port with a link id/name usable by SmartOS.
2. Agent C references that link in the `vmadm` NIC entry.
3. `vmadm` creates the bhyve guest with a virtio NIC backed by that link.

If SmartOS requires a VNIC created by `dladm create-vnic -l <proteus-link>`,
that local VNIC creation belongs to Agent C's platform adapter, not to
Agent B's compiler.

### 5.4 SSH and configuration injection

The minimum v1 path preserves current behavior for zone payloads:

* concatenate visible SSH public keys into
  `customer_metadata.root_authorized_keys`;
* carry tritond identity and image digest in `internal_metadata`;
* use metadata/config drive only when the image declares it needs one.

For bhyve MVP payloads, `tritonagent` dispatches from
`Image.compatibility.brand == "bhyve"` until `Instance` grows an explicit
brand field. Those payloads include NoCloud seed data in
`customer_metadata["cloud-init:user-data"]` and
`customer_metadata["cloud-init:meta-data"]`, plus the SmartOS
`org.smartos:cloudinit_datasource = "nocloud"` marker. That matches the
SmartOS NoCloud support now expected for v1 bhyve guests.

The builder should expose a single `GuestConfig` input enum later:

```rust
pub enum GuestConfig {
    MetadataOnly,
    NoCloudIso { user_data: String, meta_data: String },
}
```

Until that type lands, the v1 design claims only the NoCloud shape above;
generic cloud-init provider support remains out of scope.

### 5.5 Proteus-backed attachment

Proteus attachment is a separate phase from `vmadm` JSON construction. The
VM payload references only the host-side client link. The port blueprint
carries all tenant network semantics:

* VPC identity, VNI, subnet, NIC, instance, and port ids;
* guest MAC and addresses;
* virtual gateway;
* DHCP/DNS defaults;
* security-group rules;
* route table and propagated routes;
* NAT/FIP bindings and edge target refs;
* desired generation.

Agent C must not compile these fields. It consumes Agent B's
`PortBlueprint` and reports whether the local dataplane accepted it.

The v1 control-plane endpoint is `GET /v2/agent/blueprints/{port_id}`.
It is available only to a CN-bound Agent key that currently owns an
in-progress claim for the port's instance. The response carries
`port_id`, `generation`, and `blueprint_postcard_base64`, where the
base64 payload is a postcard-encoded
`proteus_api::blueprint::PortBlueprint`. M1 uses generation `1` for the
initial provision apply; later network-update slices must store and
increment a per-port desired generation.

The first tritonagent integration consumes this endpoint per NIC during
Provision jobs. It treats `Nic.id` as the v1 Proteus `PortId`, decodes the
opaque `PortBlueprint`, opens the configured Proteus device
(`/dev/proteus` by default), and calls create/apply/start before `vmadm
create`. Until Agent B returns the precise affected-resource list, the
agent reports the applied port generation against the enclosing VPC as a
coarse CN realization row; later slices replace that with the compiler's
resource mapping.

## 6. Proteus port lifecycle

The current Proteus API exposes the needed primitive lifecycle:

* `CreatePort` reserves a port slot and starts in `Ready`;
* `ApplyBlueprint` installs a `PortBlueprint` and advances applied
  generation;
* `StartPort` begins packet processing;
* `PausePort` stops packet processing while preserving state;
* `DeletePort` releases the port;
* `GetPortSummary` and `GetGenerationStatus` return realized state.

Agent C's lifecycle should be deterministic and idempotent.

### 6.1 Provision create

For each NIC in the job blueprint:

1. Validate one `NetworkPortRealization` exists for the NIC.
2. Create or verify the Proteus port.
3. Apply the port blueprint.
4. Query generation status.
5. Create the bhyve VM once all required ports are ready or created.
6. Start the Proteus ports.
7. Complete the job only after `vmadm create` and port start succeed.

The ordering favors cleanup: a failed `vmadm create` can delete ports before
any guest runs. If SmartOS requires the VM to exist before a VNIC can bind to
the port, the live implementation may split step 5 earlier, but cleanup
must still unwind both sides.

### 6.2 Apply blueprint

On initial provision and later network changes:

* call `ApplyBlueprint` with the exact `generation` Agent B supplied;
* treat `StaleGeneration` as a convergence signal only if
  `GetGenerationStatus` proves `applied_generation >= desired_generation`;
* otherwise report failure and leave the port paused or unchanged;
* never synthesize a generation locally.

### 6.3 Start

Start is allowed only after a successful apply. A `StartPort` failure is
terminal for the current job because traffic would not flow even if the VM
boots.

### 6.4 Pause and delete

Stop should pause Proteus ports before or with `vmadm stop`, then report the
VM stop outcome. Restart should keep the same port ids and apply any pending
blueprints before `StartPort`.

Delete should:

1. pause ports if they exist;
2. stop/delete the VM idempotently;
3. delete every known port id;
4. report cleanup failures separately from VM-not-found idempotence.

`UnknownPort` is success for delete cleanup. It is not success for provision.

### 6.5 Generation report

After every successful apply, and after every observed degraded/failure
status, Agent C reports realized state to `tritond`. The Agent A draft
proposes:

```rust
POST /v2/agent/network-realization
{
  "resource": { "kind": "...", "id": "..." },
  "realizer": { "kind": "cn", "id": "<server_uuid>" },
  "generation": 17,
  "status": "applied",
  "message": null
}
```

Agent C should report the generation for each affected resource Agent B
names in `affected_resources`. If Agent B initially provides only per-port
generation, Agent C can report a port-scoped placeholder only after Agent A
adds a resource kind for it.

## 7. Desired vs applied contract

`tritond` remains the desired-state authority. `tritonagent` is a
realizer, not an intent writer.

Rules:

* desired generation is assigned by `tritond` or Agent B's compiler input,
  not by Agent C;
* applied generation comes from Proteus `GetGenerationStatus` or
  `PortSummary.applied_generation`;
* Agent C reports `Applied` only after Proteus reports the generation
  applied;
* Agent C reports `Failed` when a phase cannot converge and includes a
  concise phase-coded diagnostic;
* Agent C may report `Pending` or `Compiling` only if Proteus exposes a
  stable in-progress status and `tritond` accepts non-terminal reports;
* reports must be idempotent and monotonic. Re-reporting the same applied
  generation is safe. Reporting an older generation should be ignored or
  rejected by `tritond`.

The status payload should keep free-form diagnostics short. Detailed command
stderr belongs in local logs and future support bundles, not unbounded
control-plane rows.

## 8. Failure taxonomy and retry behavior

Failures should be structured by phase before they are flattened into
`JobOutcome::Failed`.

Proposed taxonomy:

| Phase | Examples | Retry behavior |
|---|---|---|
| `blueprint_fetch` | HTTP failure, missing instance/image/NIC | transient HTTP retries inside poll loop; missing required records fail job |
| `image_fetch` | missing source URL, HTTP error, sha mismatch | HTTP may retry in image helper; sha mismatch is terminal |
| `image_install` | gzip/zfs receive/snapshot/manifest failure | cleanup partial dataset; retry only by new job/operator action |
| `vm_payload` | zero CPU, no boot disk, invalid NIC/port mapping | terminal programmer/control-plane contract failure |
| `proteus_create` | duplicate port, driver unavailable | duplicate can be idempotent if summary matches; driver unavailable fails job |
| `proteus_apply` | stale generation, compile failure, schema mismatch | stale may converge after status check; schema/compile failure is terminal |
| `proteus_start` | unknown port, driver transition failure | terminal for provision/start |
| `vmadm_create` | payload rejected, image not in imgadm, capacity | terminal for job; cleanup ports and partial VM |
| `vmadm_stop` | not running, not found | idempotent success where target state is already reached |
| `cleanup` | port delete failed, VM delete failed | best-effort retries locally, then report leak risk |
| `report` | network-realization endpoint failure | retry bounded times; do not mark job complete before required reports if endpoint is part of contract |

Current job claiming does not requeue failed jobs. That should remain true
for v1 until the queue has explicit retry counters. Agent-side retry is for
short transient local operations only; user/operator retry is a new
start/provision/delete action.

## 9. Cleanup behavior

Provision cleanup must work from any partial point:

* image install failure destroys partial `zones/<image_id>` receive output
  before returning failure;
* Proteus create/apply failure deletes any ports created by this job;
* `vmadm create` failure deletes any VM with the target UUID and deletes
  created ports;
* post-create port start failure pauses/deletes ports and deletes the VM if
  the job has not been reported complete;
* delete jobs treat missing VM and missing ports as success;
* delete jobs surface non-idempotent failures as cleanup failures with enough
  context for `tcadm doctor`.

Agent C needs a small host-local reconciliation function before live v1:

* list VM by instance UUID;
* list Proteus ports with Triton tags or port ids from the blueprint;
* compare local state with the current job's desired action;
* converge idempotently instead of blindly reissuing creates.

This can be pure tests against the fake Proteus backend first.

## 10. Integration points

### 10.1 Agent A - VPC control plane

Agent C depends on Agent A for:

* final `RealizedNetworkState` type;
* final `NetworkResourceId` enum;
* `POST /v2/agent/network-realization` endpoint and auth rules;
* monotonic desired-generation rules;
* the set of resource kinds Agent C is expected to report for a port
  apply.

Agent C should not implement broad network reporting until Agent A's
`RealizedNetworkState` slice lands.

### 10.2 Agent B - Proteus integration

Agent C depends on Agent B for:

* compiled `PortBlueprint` per NIC;
* stable port id derivation or allocation ownership;
* `affected_resources` mapping from port generation to intent resources;
* Proteus userspace adapter choice for `tritonagent`;
* schema/version mismatch handling;
* expectations for `StaleGeneration` and degraded apply status.

`proteus/docs/tritond-integration-v1.md` was not present at planning time,
so this doc treats the current Proteus `CreatePort`/`ApplyBlueprint`/
`StartPort`/`PausePort`/`DeletePort` API as available primitives, not as the
final cross-agent contract.

### 10.3 Agent D - firehyve/fhrun edge runtime

Agent D owns the edge runtime. Agent C provides only CN-side VM and port
realization. If an edge manifest needs to know whether a tenant VM's port is
running, it should read realized state from `tritond`; Agent D should not
reach into Agent C host-local state.

### 10.4 Agent E - NAT/FIP behavior

Agent E owns NAT/FIP behavior and edge manifests. Agent C only applies the
per-port NAT/FIP policy that Agent B compiled into a Proteus blueprint and
reports the applied generation. Edge-side NAT/FIP realization uses the same
Agent A reporting endpoint with an edge realizer, not Agent C's CN realizer.

## 11. Proposed atomic commit queue

Each row is intended to be one commit with docs and tests in the same
commit. Build and audit run per `monitor-reef/AGENTS.md`.

| # | Slice | Touches | Tests |
|---|---|---|---|
| C-1 | Add this design doc | `docs/design/cn-dataplane-v1.md` | docs-only; build/audit run after commit |
| C-2 | Pure bhyve `vmadm` payload builder | `services/tritonagent/src/vmadm.rs` plus docs note | unit tests for CPU/RAM, boot disk, NIC order, NoCloud/SSH metadata, missing fields |
| C-3 | Structured provisioning phase errors | `services/tritonagent/src/lib.rs`, `vmadm.rs`, docs | unit tests for phase labels and `JobOutcome::Failed` message formatting |
| C-4 | Use `tritond-cn-platform::VmadmTool` in agent vmadm path | `services/tritonagent/src/vmadm.rs` | mock-script tests for create/stop/reboot/delete idempotence |
| C-5 | Add Proteus userspace adapter with fake backend tests | `services/tritonagent/src/proteus.rs` | fake lifecycle tests for create/apply/start/pause/delete/status |
| C-6 | Extend blueprint model with network ports after Agent B contract | API/client regeneration, agent consume path | serialization tests and agent-side validation tests |
| C-7 | Report network realization after apply after Agent A endpoint | agent report path | handler/client integration test once endpoint exists |
| C-8 | Provision ordering: image, Proteus ports, vmadm create, port start | agent job driver | fake end-to-end provision success and cleanup-on-failure tests |
| C-9 | Delete cleanup for VM and Proteus ports | agent job driver | idempotent missing VM/port tests; partial failure tests |
| C-10 | CN status includes concise Proteus port summaries | `tritond-cn-platform`/agent status | collector tests with fake port summaries |

The first implementation commit should be C-2. It does not require Agent A
or Agent B because it is side-effect-free and only shapes the bhyve `vmadm`
payload.

## 12. Stop rules

Stop and report before implementation if:

* the change requires final Agent A realized-state endpoint semantics;
* the change requires Agent B's compiled blueprint contract;
* the change changes public `tritond` API/resource shapes outside Agent C's
  ownership;
* the dirty worktree overlaps the planned write set;
* SmartOS lab validation contradicts the proposed bhyve `vmadm` field names;
* build, tests, or audit cannot run.

Until Agent A/B contracts land, Agent C can safely continue with pure
payload builders, structured local errors, platform-adapter cleanup, and fake
Proteus adapter tests.
