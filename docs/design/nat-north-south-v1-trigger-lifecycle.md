# N/S Edge Trigger and Lifecycle (v1)

Companion brief to `nat-north-south-v1.md`. This doc captures **how
firehyve north/south edge VMs are provisioned, materialized, and reaped**,
modelled on the way Triton SmartOS-1g auto-spawns per-fabric NAT zones
today, with the parts of that design we are keeping and the parts we are
deliberately replacing.

Hand this to Agent E (NAT and North/South), Agent A (VPC control plane),
Agent C (CN actuation), and Agent D (firehyve/fhrun edge runtime). It is
deliberately concrete. Anything not stated here is open and should be
escalated to the coordinator.

---

## 1. Purpose

We have an existing, working pattern in Triton SmartOS-1g for "create a
fabric NAT data plane lazily, on first need, and reap it when nobody is
left." That pattern is correct in shape but wrong in implementation
choice (per-fabric SmartOS zones running ipfilter+ipnat). For vnext we
want the **same trigger and lifecycle semantics** with a different
data plane:

- the data plane is one or more **firehyve/fhrun edge microVMs** (Agent
  D's runtime), programmed by **proteus** on the host or by an in-edge
  nftables/AF_XDP backend (per the v1 dataplane decision Agent E owns);
- placement is on **edge CNs**, selected by tritond, not on the same CN
  as the tenant VM;
- realization (rules, FIP rewrites, SNAT pools) is driven from
  **tritond's desired state** in FDB, with the on-CN datapath following
  generations the way Agent C/B already do for tenant ports.

The user-visible knob is unchanged in spirit: a project creates a VPC,
attaches a `NatGateway`, the system materializes the necessary edge
capacity automatically. No operator step in the per-tenant path.

## 2. Today's model in SmartOS-1g (the reference)

A precise read of the legacy code; treat this section as the spec we are
matching, not aspirational.

### 2.1 Triggering primitive

A **fabric network** in NAPI carries two booleans:

- `internet_nat` (intent): this fabric should have outbound internet
  via NAT.
- `gateway_provisioned` (state): the NAT data plane currently exists
  and owns the gateway IP.

Source: `~/workspace/triton/sdc-napi/lib/models/network.js:179, 225,
1256, 1313, 1339, 1395, 1439, 1507`. A NIC owning the fabric gateway
flips the bool atomically as part of the NIC create/delete batch:
`~/workspace/triton/sdc-napi/lib/models/nic/obj.js:317, 389`.

### 2.2 Lazy provision on first VM

The trigger is **VM provision into a fabric whose `internet_nat=true`
and `gateway_provisioned=false`**. It is not "fabric created". It is
not "first NIC". It is "first VM that needs egress."

The provision workflow (VMAPI) executes three tasks from
`~/workspace/triton/sdc-vmapi/lib/workflows/fabric-common.js`:

1. `getFabricNatNics` (line 349). Walk the new VM's NICs, ask NAPI for
   each, keep those where `nic.fabric && nic.internet_nat &&
   !nic.gateway_provisioned && nic.ip !== nic.gateway` (line 417).
2. `acquireFabricTickets` (line 73). For each candidate network, take a
   **CNAPI waitlist ticket** scoped `fabric_nat`, `id =
   network_uuid`, expiry 600s. This is the per-network mutex against
   concurrent provisions racing the NAT creation.
3. `provisionFabricNats` (line 145). Inside the ticket: re-check
   `gateway_provisioned` (someone holding the ticket before us may have
   just done it; line 203). If still false, call SAPI
   `createInstanceAsync` on the `nat` service with two NICs (line 210):
   - primary: external NIC from `sdc_nat_pool`, `allow_ip_spoofing:
     true`;
   - secondary: the fabric NIC at the gateway IP,
     `allow_ip_spoofing: true`;
   - `metadata['com.joyent:ipnat_subnet']` carries the fabric CIDR.

Then `waitForFabricNatProvisions` (line 295) blocks the parent
provision until the NAT zone is `running`, because the guest VM's
cloud-init may need internet on first boot. Timeout 600s.

Ticket-release is split: the NAT-provision job releases its own ticket
on success; the parent releases on failure. That split is itself a
lesson (see §6.4).

### 2.3 The data plane

The NAT zone is a SmartOS zone (image name `nat`, package `sdc_128`)
whose first-boot script is the entirety of the data plane:
`~/workspace/triton/sdc-nat/bin/setup-nat`.

It does five things, total:

1. Reads `com.joyent:ipnat_subnet` from mdata (line 56).
2. Picks the "primary" NIC via `mdata-get sdc:nics | json -gac
   'this.primary === true'` (line 60). Aborts if more than one primary.
3. Enables IPv4 forwarding (line 31).
4. Writes `/etc/ipf/ipnat.conf` with two `map` rules: TCP/UDP portmap
   `1025:65535`, and a bare map for ICMP (line 76).
5. `ipf -E && ipnat -CF && ipnat -f` (line 36).

There is no in-zone control plane. The zone's only configuration input
is `ipnat_subnet`. There is no API, no health endpoint, no metrics
beyond per-VM zone stats, no rule mutation after first boot, no DNAT,
no inbound floating-IP capability.

### 2.4 Operator setup

`sdcadm post-setup fabrics`:
`~/workspace/triton/sdcadm/lib/post-setup/fabrics.js:373` `setupNat`
creates the SAPI `nat` service. Notable shape:

- no `networks` field (line 384). Networks are picked per-instance by
  the workflow above.
- `pass_vmapi_metadata_keys: ['com.joyent:ipnat_subnet']` (line 398) is
  the trick that lets per-instance metadata reach the zone without
  config-agent running inside it.
- `fabric_cfg` (with `sdc_nat_pool`) is stored on the `sdc` SAPI
  application (line 748).

State for "is there a NAT zone for fabric X" lives in **NAPI** (the
`gateway_provisioned` bool). The actual VM is in **VMAPI**, looked up
by alias `nat-<network_uuid>` (fabric-common.js:215, 463). There is no
durable foreign key between them; the alias is the only link.

### 2.5 Lazy reap

When a fabric NIC is deleted, the symmetrical chain
`destroyFabricNats` (fabric-common.js:441) runs inside the same
waitlist ticket. It lists remaining NICs on the network; if only the
gateway NIC is left, it `sapi.deleteInstance` on the zone found by
alias. That flips `gateway_provisioned` back to false via the NIC
delete batch on the gateway NIC.

## 3. What we are keeping for vnext

These properties of the legacy design are correct; we copy them.

1. **Lazy materialization.** No N/S edge capacity is provisioned for a
   `NatGateway` until at least one tenant NIC needs egress through it.
   Cheap NatGateways stay free; an empty VPC does not cost a public IP
   (the IP is reserved at create time, but no edge VM, no proteus
   rules, no host CN load).
2. **Per-resource distributed lock during materialization.** A
   waitlist-shaped lock keyed on **the desired-state generation of the
   `NatGateway`** prevents concurrent first-tenant provisions from
   racing the edge spin-up. Granularity: one lock per `NatGateway.id`,
   not per VPC, not per CN.
3. **Reverse trigger from tenant NIC delete.** When the last NIC
   referencing a `NatGateway` (via its subnet's route table) goes away,
   the edge capacity is reaped. Same lock, opposite direction.
4. **Service in the catalogue, instances picked at materialization
   time.** Today: SAPI `nat` service, no `networks` field, networks
   picked per call. Tomorrow: a tritond-known **edge cluster service
   template** (image, packaging, manifest schema) whose concrete
   instances are picked when first needed: which CN, which underlay
   address, which public address.
5. **Block tenant readiness on edge readiness when egress is on the
   critical path.** Today: `waitForFabricNatProvisions` blocks the
   parent VM provision until the NAT zone is `running`. Tomorrow: the
   tenant port's "ready" signal must wait for the edge generation to
   reach `applied` if and only if that port's route table actually
   targets a `NatGateway` whose realization is not yet `applied`.
   Otherwise we are free to return ready immediately.

## 4. What we are deliberately changing

These are the failure modes of the legacy design we will not
reproduce.

1. **Drop "one VM per fabric per tenant".** The legacy model spins one
   SmartOS zone per `(tenant, fabric)` with NAT, even if the tenant has
   50 fabrics each with one busy VM. v1: one **edge cluster** per
   `NatGateway`, sized to load. Multiple `NatGateway` records may
   share an edge cluster (Agent E owns the placement policy; default is
   "one cluster per NatGateway, scale instances within the cluster").
2. **Move from alias-as-foreign-key to durable IDs.** The legacy
   `nat-<network_uuid>` alias is the only link between the network and
   the zone. Rename or out-of-band delete and the system silently
   believes a gateway exists. v1: the `NatGateway` row in FDB carries
   `edge_cluster_id`, and the edge cluster row carries the set of
   firehyve/fhrun instance IDs. Reconciliation is by ID, not by name.
3. **Replace per-instance first-boot config with control protocol.**
   The legacy zone is configured exactly once at first boot from
   `ipnat_subnet`. Cannot mutate. v1: the edge agent inside the
   firehyve VM speaks `triton.edge.control.v1` over the host Unix
   socket already specified in
   `firehyve/docs/edge-control-protocol-v1.md`. Rules, SNAT pools, FIP
   bindings are pushed by tritonagent on the host CN as the desired
   generation advances, the same way tenant ports advance through
   proteus generations. The edge VM is not stateless, but it is
   **stateless across restarts**: tritonagent re-pushes the current
   generation on edge agent reconnect. This matches the proteus
   contract for tenant ports.
4. **Drop the brittle "primary NIC by mdata" heuristic.** The legacy
   `setup-nat` greps mdata for the one nic with `primary==true`. The
   edge manifest in fhrun already names the north (public) and south
   (fabric/underlay) NIC roles explicitly; the edge agent reads the
   manifest, never guesses.
5. **Split provision-success ticket-release.** Today the NAT-provision
   job releases its own ticket on success; the parent releases on
   failure. Two owners, hard to reason about, 600s expiry as a safety
   net. v1: a single owner of the lock for the whole materialization
   (the workflow that took it). On failure, the lock is released; on
   success, the lock is released after the realized state is durable.
   No "release from inside the child."
6. **Stop coupling the public IP allocator to the fabric.** Today the
   external NIC is allocated from `sdc_nat_pool` at NAT-zone create
   time. v1: the public IP is allocated **at `NatGateway` create
   time**, from `FLOATING_IP_V{4,6}_POOL`, and stored on the
   `NatGateway` row (`public_address`,
   `vpc-control-plane-v1.md:215`). Edge materialization consumes the
   already-allocated address. This makes "reserve a public IP for this
   project" a separate, observable, billable step from "stand up data
   plane to use it."
7. **Stop scattering state across services.** Today the answer to "is
   there a NAT for fabric X" is a NAPI bool, the VM is in VMAPI, the
   service definition is in SAPI, the lock is in CNAPI, the package is
   in PAPI. v1: **FDB is source of truth for desired state**;
   tritond-store carries `NatGateway`, `EdgeCluster`,
   `EdgeClusterInstance` rows; tenant-side realized state is the same
   `RealizedNetworkState` Agent A already specified for tenant ports
   (`vpc-control-plane-v1.md:240`).

## 5. Proposed v1 trigger and lifecycle

### 5.1 Resources

Three desired-state shapes are involved. Two already exist; one is new.

- `NatGateway` (Agent A, exists, see `vpc-control-plane-v1.md:215`).
  Public-side identity. Has `public_address`, `edge_cluster_id`
  (filled in by tritond at create or first need), `desired_generation`,
  `realized: RealizedNetworkState`.
- `RouteTable` / `Route { target: RouteTarget::NatGateway { .. } }`
  (Agent A, exists, see `vpc-control-plane-v1.md:179`). The trigger
  for actually needing edge capacity is: "some subnet's active route
  table contains a route whose target is this `NatGateway`, AND that
  subnet currently has at least one NIC, AND that NIC's port is in a
  state that needs egress (running or starting)."
- `EdgeCluster` (Agent E, **new**, the analog of the SAPI `nat`
  service plus its instances). Schema sketch:

  ```rust
  pub struct EdgeCluster {
      pub id: Uuid,
      pub tenant_id: Uuid,                  // or system-owned
      pub kind: EdgeClusterKind,            // NatGateway | FloatingIpDecap | shared
      pub bound_resources: Vec<Uuid>,       // NatGateway ids, FIP ids
      pub instances: Vec<EdgeClusterInstance>,
      pub desired_generation: u64,
      pub realized: RealizedNetworkState,
      pub created_at: DateTime<Utc>,
      pub updated_at: DateTime<Utc>,
  }

  pub struct EdgeClusterInstance {
      pub id: Uuid,
      pub cn_id: Uuid,                      // edge CN this VM lives on
      pub fhrun_manifest_uri: String,       // or inline manifest hash
      pub north_nic: NicCoord,              // public-side
      pub south_nic: NicCoord,              // underlay/VPC-side
      pub control_socket: String,
      pub realized: RealizedInstanceState,
  }
  ```

  Agent E owns the precise shape; the manifest contract is in their
  `nat-north-south-v1.md` deliverable.

### 5.2 The trigger function

A pure function in tritond:

```text
needs_edge(nat_gateway) =
    exists(subnet s) such that:
      s.vpc_id == nat_gateway.vpc_id AND
      s.active_route_table contains a Route with
        target = NatGateway { id = nat_gateway.id } AND
      exists(nic n) such that:
        n.subnet_id == s.id AND
        n.port.desired_state in { Running, Starting }
```

This is the v1 analog of the legacy
`nic.fabric && nic.internet_nat && !nic.gateway_provisioned`. It is
recomputed on any of the three input changes (route table mutation,
NIC create/delete, port state change). Implementation: tritond keeps a
per-`NatGateway` watcher; the watcher fires
`materialize_or_reap(nat_gateway)`.

### 5.3 Materialize

```text
materialize(nat_gateway):
    lock = fdb.acquire("edge_lock", nat_gateway.id, ttl=600s)
    refetch nat_gateway from fdb
    if nat_gateway.realized.is_applied_for(desired_generation):
        return  // someone else did it under the lock
    if nat_gateway.edge_cluster_id is None:
        ec = pick_or_create_edge_cluster(nat_gateway)
        atomically: nat_gateway.edge_cluster_id = ec.id
    advance_generation_and_apply(ec)        // pushes manifest to fhrun, edge agent
    wait until ec.realized.applied_generation >= ec.desired_generation
        OR fail with structured error (no_capacity | launch_failed |
                                       dataplane_failed | unhealthy)
    release lock
```

Notes:

- `fdb.acquire` is the v1 analog of CNAPI waitlist tickets. Single
  owner. TTL is the safety net, not the contract.
- The lock is per `NatGateway.id`, not per VPC and not per edge
  cluster. Two NatGateways that happen to share a cluster proceed
  independently.
- `pick_or_create_edge_cluster` is Agent E's placement policy. v1
  default: one cluster per NatGateway, two instances minimum (see
  §5.7), placed on edge CNs with capacity.
- The blocking wait is what `waitForFabricNatProvisions` does today.
  We keep that semantics (and accept the latency) only on the **first
  tenant whose port needs this gateway**. Subsequent tenants see
  `realized.is_applied` and return immediately.

### 5.4 Reap

```text
reap_if_idle(nat_gateway):
    lock = fdb.acquire("edge_lock", nat_gateway.id, ttl=600s)
    if needs_edge(nat_gateway):
        return  // re-armed
    teardown(nat_gateway.edge_cluster_id)
        // delete fhrun instances, free public IP back to pool? NO.
        // see §5.5 for the public-IP question.
    atomically: nat_gateway.edge_cluster_id = None
    release lock
```

`teardown` waits for all instances to leave `running`, the same way
the legacy `destroyNatZone` (fabric-common.js:461) waits on
`sapi.deleteInstance`. Failure to stop a single edge instance does
**not** delete the `NatGateway`; the NatGateway lives until the user
deletes it.

### 5.5 Public IP lifecycle (open question Q1)

In the legacy design, the public IP is bound to the existence of the
NAT zone: zone created → IP from pool, zone destroyed → IP returned.

In v1 we have two choices:

- (A) IP is bound to the `NatGateway` (allocated at create, freed at
  delete). Reaping the edge cluster does NOT free the IP. **This is
  the recommended default.** It matches FloatingIp semantics and means
  a project's egress IP is stable across periods of inactivity.
- (B) IP is bound to edge materialization (allocated when first
  needed, freed when reaped). Cheaper for the operator, worse UX (the
  egress IP can change after a quiet period). Rejected for v1 unless
  Agent A overrides.

Default is (A). Locked unless escalated.

### 5.6 What blocks on what during VM provision

Today, the **VM provision workflow** drives NAT zone creation
inline. We do not want vnext tenant provisions to be coupled to edge
materialization beyond the minimum.

Proposed:

1. Tenant port create / start fires `needs_edge` recompute on each
   `NatGateway` reachable from the port's subnet's route table.
2. If `needs_edge` flips false→true and `realized.applied`
   < `desired`, the tenant port's `ready` predicate gains a dependency
   on the NatGateway's `realized`.
3. The CN actuator (Agent C) does not block. The CN reports the port
   as `applied` when its proteus generation is realized. The
   **user-facing readiness** (e.g., the CloudAPI machine state) is the
   join across port, edge, and any FIP attachments. tritond computes
   that join.

This decouples CN actuation from edge actuation. They proceed in
parallel; the user-facing "machine is ready" gate is computed.

### 5.7 HA stance

The legacy design is single-VM-per-fabric. No HA. We pay one VM's
worth of failure domain per fabric.

v1 minimum:

- two firehyve edge instances per `EdgeCluster`,
- on **distinct edge CNs**,
- public address advertised by the edge subsystem (Agent D defines
  whether that is anycast/BGP, ECMP next-hops, or a leader/follower
  model. Recommended for v1 nftables backend: leader/follower with a
  shared public address moved by the edge agents on health change.
  Recommended for v1 AF_XDP backend: ECMP equal-cost routes if the
  underlay supports it; else leader/follower.).

Failure domains:

- single edge instance failure: surviving instance picks up; brief
  flow disruption is acceptable; conntrack is **not** synced in v1.
- edge CN failure: same as above.
- both edge instances down: NatGateway `realized.state = Unhealthy`,
  user-visible. tritond attempts replacement on a third edge CN.
- no edge capacity in the cluster's edge pool: NatGateway
  `realized.state = NoCapacity`, user-visible, surfaced through Agent
  G. `materialize` returns the structured error from §5.3.

### 5.8 Reaper cadence

The legacy reap is triggered exactly when the last NIC leaves. v1
should keep that synchronous trigger but ALSO run a periodic
reconciler:

- on every tritond leadership transition;
- on every `NatGateway` watcher fire;
- on a slow timer (default 60s) that recomputes `needs_edge` for every
  NatGateway with `edge_cluster_id != None`.

The slow timer is the safety net for missed events. Cheap because
`needs_edge` is a watcher recompute, not a poll across CNs.

## 6. Component split

This maps directly onto the existing agent split.

### 6.1 Agent A (VPC control plane, FDB schema)

Owns:

- `NatGateway`, `RouteTable`, `Route` (already done).
- Deciding whether to add `EdgeCluster` as a first-class FDB resource
  (recommended: yes) or to model it as an internal tritond detail. If
  first-class: schema, indexes, watch keys.
- The `ready_for_egress` predicate that joins port, NatGateway, and
  any FIPs.

Does not own:

- placement policy;
- manifest shape;
- the edge lock semantics (lives in tritond);
- the dataplane backend choice.

### 6.2 Agent B (proteus blueprint compilation)

Owns:

- `RouteTarget::NatGateway` compilation. Today blocked on Agent E
  (`tritond-integration-v1.md:153`); this brief unblocks it by
  specifying that the compile target is the **underlay coordinate of
  the active leader of `ec.instances`** (or the anycast/ECMP set, per
  §5.7). The compiler emits an edge route-target referencing the
  cluster, and the realized layer at apply time resolves to current
  leader / member set.
- FloatingIp `EdgeTerminated` compilation similarly.

Does not own:

- the fhrun manifest;
- which CN hosts the edge VM;
- the lock.

### 6.3 Agent C (CN actuator)

Owns:

- on **edge CNs**, supervising the firehyve/fhrun edge instance
  lifecycle the same way it supervises tenant zones today;
- pushing edge generations to the fhrun control socket;
- reporting `RealizedInstanceState` back through tritond;
- relaying `triton.edge.control.v1` traffic between tritond and the
  in-edge agent.

Does not own:

- the edge dataplane internals (those run inside the fhrun VM, owned
  by Agent D).

### 6.4 Agent D (firehyve / fhrun edge runtime)

Owns:

- fhrun manifest schema for edge VMs (`north_nic`, `south_nic`,
  `control_listen`, dataplane backend selector, health endpoint);
- the `triton.edge.control.v1` server inside the edge VM (already
  drafted in `firehyve/docs/edge-control-protocol-v1.md`);
- HA mechanism (leader/follower, anycast, ECMP) for the chosen
  backend.

Does not own:

- when the edge VM is launched (that is the trigger in §5.2).

### 6.5 Agent E (NAT and N/S)

Owns:

- the `EdgeCluster` placement policy (which edge CN, how many
  instances, how to scale);
- the rendering function from `(NatGateway, FloatingIps in cluster,
  bindings)` to fhrun manifests;
- the failure taxonomy from §5.7 surfaced in `RealizedNetworkState`;
- the user-facing `NatGateway.realized` state model.

Should write the precise lock contract (§5.3 to §5.4) into
`nat-north-south-v1.md` after reading this brief.

### 6.6 Agent G (operator visibility)

Surface:

- per-NatGateway: `state, applied_generation, public_address,
  edge_cluster_id, instance_count, last_error`;
- per-EdgeCluster: instance health, leader, packet/flow counters;
- per-tenant view: "which NatGateway is this VM using, is it healthy,
  is its public IP advertised."

## 7. State machine for one `NatGateway`

```
              create
                │
                ▼
         Allocated (public_address bound, no edge yet)
                │  needs_edge becomes true
                ▼
         Materializing  ──fail──▶  Failed { reason }
                │  realized.applied
                ▼
            Active   ◀──reconcile──┐
                │                  │
                │  needs_edge      │  scale / repair / reattach
                │  becomes false   │
                ▼                  │
              Idle ────────────────┘
                │  user delete
                ▼
            Deleting
                │
                ▼
           Deleted (public_address returned)
```

`Failed { reason }` is one of `NoCapacity`, `LaunchFailed`,
`DataplaneFailed`, `Unhealthy`, mirroring `nat-north-south-v1.md`'s
required failure taxonomy.

## 8. Open questions

1. **(Q1, §5.5) Is the public IP bound to NatGateway or to edge
   materialization?** Recommendation: NatGateway. Confirm with Agent A.
2. **(Q2) Is `EdgeCluster` a first-class FDB resource exposed to
   operators, or a tritond-internal record?** Recommendation:
   first-class but read-only at the user CLI in v1; operator CLI
   (`tcadm`) sees full shape. Confirm with Agent A and Agent G.
3. **(Q3) Do we share an `EdgeCluster` across multiple `NatGateway`s
   or default to 1:1?** Recommendation: 1:1 in v1, share-by-policy in
   v2. Confirm with Agent E.
4. **(Q4) Conntrack sync between edge instances?** Recommendation: no
   in v1; document the brief flow disruption on failover.
5. **(Q5) HA mechanism for v1 nftables backend.** Recommendation:
   leader/follower with public IP failover. Anycast/ECMP deferred to
   AF_XDP backend.
6. **(Q6) Does the tenant port `ready` signal block on
   NatGateway.realized?** Recommendation: only when the port's active
   route table actually targets this NatGateway and `realized.applied
   < desired`. Confirm with Agent C and Agent A.
7. **(Q7) Edge CN pool sizing and admission.** Out of scope for this
   brief; tracked under Agent E placement policy.

## 9. What the next agent should produce

Reading order: this brief, then `vpc-control-plane-v1.md` §4.3 and
§4.7, then `firehyve/docs/edge-control-protocol-v1.md`, then
`proteus/docs/tritond-integration-v1.md` §"Edge Assumptions From
Agent D/E".

Concrete first deliverables, atomic:

1. (Agent A) `EdgeCluster` schema PR or rejection note. Decision on
   Q2.
2. (Agent E) Update `nat-north-south-v1.md` to reference this brief,
   adopt §5 verbatim or with annotated diffs, resolve Q3 and Q5,
   produce the manifest renderer signature (input: NatGateway + FIPs +
   bindings + placement; output: fhrun manifest JSON; pure function).
3. (Agent B) Lift the "skip `RouteTarget::NatGateway`" gate in the
   compiler; emit edge-target referencing `EdgeCluster`. Test with a
   fixture that materializes a NatGateway plus one tenant port with a
   default route through it.
4. (Agent D) Confirm `north_nic` / `south_nic` / `control_listen`
   manifest fields and the dataplane backend selector keys. If
   diverging, write `edge-runtime-v1.md` and supersede this brief's
   §6.4.
5. (Agent C) Edge-CN supervision shape: same protocol as tenant zone
   supervision, with the addition of relaying `triton.edge.control.v1`
   to tritond. Document in cn-dataplane-v1.md.

## 10. Reference material

Legacy code, kept as the canonical study:

- `~/workspace/triton/sdc-vmapi/lib/workflows/fabric-common.js` —
  trigger, lock, materialize, reap.
- `~/workspace/triton/sdc-nat/bin/setup-nat` — first-boot data plane.
- `~/workspace/triton/sdc-napi/lib/models/network.js` and
  `.../nic/obj.js` — `internet_nat` / `gateway_provisioned`
  invariants.
- `~/workspace/triton/sdcadm/lib/post-setup/fabrics.js:373` —
  operator-side service installation (`setupNat`).

Vnext context:

- `monitor-reef/docs/design/vpc-control-plane-v1.md` — `NatGateway`,
  `FloatingIp`, route-table model, realized-state pattern.
- `monitor-reef/docs/design/nat-north-south-v1.md` — Agent E's
  primary deliverable; this brief feeds it.
- `firehyve/docs/edge-control-protocol-v1.md` — host↔edge agent
  protocol.
- `proteus/docs/tritond-integration-v1.md` §"Edge Assumptions From
  Agent D/E" — what Agent B is waiting for.
- `AGENT_E_PROMPT.md` — Agent E charter and required deliverable
  list.
