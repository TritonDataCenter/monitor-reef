<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

Copyright 2026 Edgecast Cloud LLC.
-->

# VPC Control Plane v1 — desired-state model

> Owner: Agent A (VPC control plane).
> Status: design baseline approved by manager 2026-05-05;
> implementation in progress (H-1 through H-6 and H-13 committed).
> Last updated: 2026-05-06 (H-13 realization report endpoint).
> Scope: `tritond` source-of-truth for tenant network intent.
> Out of scope: Proteus blueprint compilation, edge dataplane, packet
> path, UI work — those are owned by Agents B/C/D/E.

This document defines the v1 desired-state network resources owned
by `tritond`, the API path shape, the store/index requirements, the
auth model, the CLI surface, and the explicit handoff contracts to
the agents downstream of `tritond`. It is the contract Agent A
implements; everything else reads it.

## 1. Goals and non-goals

### v1 goals

* `tritond` is the single source of truth for VPC network intent.
* Every concept the v1 product exposes (`VPC`, subnet, route table,
  route, security group, security-group rule, NAT gateway, floating
  IP, floating-IP attachment) is a first-class, addressable
  resource with stable id, parent linkage, and atomic CRUD.
* Every concept that the dataplane needs to realize (Proteus port
  blueprint, edge manifests) is a *derived* view, not a separately
  edited resource. Operators and end-users never write a blueprint
  by hand.
* Realized state is recorded per resource and exposed via the same
  API surface, so "what does intent say?" and "what is actually
  programmed on the wire?" are both observable.
* Cross-tenant isolation is enforced the same way every existing
  workload resource is enforced: tenant-scoped Cedar gating + the
  defence-in-depth tenant_id/project_id rechecks in the handler.

### v1 non-goals

* Proteus blueprint generation (Agent B).
* `tritonagent`-side application of blueprints or edge manifests
  (Agent C).
* `fhrun` / firehyve edge runtime (Agent D).
* Per-flow NAT realization, port allocation across HA edge peers,
  BGP / Maglev (Agent E).
* Dynamic groups, gateway firewall attachments, VPC peering,
  cloud-interconnect / site-to-site attachments, multicast — these
  are reserved fields on the Proteus blueprint side and explicitly
  deferred from the v1 intent surface (see §11).

## 2. What exists today

These resources are already implemented end-to-end (store +
handlers + tests + CLI):

* `Silo`, `Tenant`, `Project` — identity / billing / isolation
  hierarchy. Project is the workload boundary.
* `Vpc` — project-scoped, server-assigned VNI in the user range,
  optional IPv4/IPv6 primary CIDRs (`libs/tritond-store/src/types.rs:206`).
* `Subnet` — VPC-scoped, contained-in-VPC CIDRs validated against
  parent + peers in the same FDB transaction (`libs/tritond-store/src/types.rs:259`,
  `validate_subnet_cidrs`).
* `Instance`, `Nic`, `Disk` — instance create allocates a primary
  NIC + boot disk atomically; multi-NIC at create supported via
  `NewInstance.extra_nics`.
* `FloatingIp` — project-owned, family-symmetric, atomic
  attach-replace, instance-delete cascade-detach. Address allocated
  from hardcoded `FLOATING_IP_V{4,6}_POOL` constants
  (`libs/tritond-store/src/types.rs:1617`).
* `tritonagent` registration / approval / heartbeat / job-claim
  loop is real; vmadm-driven Provision/Stop/Restart already wires
  through `JobKind` (`libs/tritond-store/src/types.rs:1229`).

## 3. Network gaps (the v1 problem)

| Concept | Today | Gap |
|---|---|---|
| Route table | none | no resource; subnets have no associated routing intent |
| Route | none | cannot express "default → NAT GW", "10.0.0.0/8 → blackhole", peering routes |
| Security group | none | no per-port distributed firewall intent |
| Security-group rule | none | no rule grammar; firewall policy lives nowhere |
| NIC↔SG attachment | none | no way to scope SGs onto NICs / subnets |
| NAT gateway | none | egress for private subnets cannot be expressed |
| Floating IP termination | implicit (CN-terminated) | no edge-terminated path; no edge cluster ref |
| `IpPool` (operator BYO range) | hardcoded TEST-NET-3 / 2001:db8::/48 | a real install needs operator-supplied pools |
| Edge cluster | none | NAT/FIP have no place to land "which edge serves this VPC" |
| Realized network state | partial (instance lifecycle only) | no per-resource generation tracking; no programmed/applied/failed view |
| `tritond` → blueprint compile | none | covered by Agent B; this doc just nails the intent contract Agent B reads |
| `tritonagent` realized-state report | partial (vmadm lifecycle) | network-side reporting absent |

The v1 plan closes every row except the blueprint compiler and the
realized-state report; those are handed off explicitly in §10.

## 4. v1 desired-state resources

All new resources are tenant-scoped (the tenant boundary is the
load-bearing security property; see locked decision #30 in
`STATUS.md`). All v1 resources nest under
`/v2/tenants/{tenant_id}/projects/{project_id}/...` except where
called out.

### 4.1 Route table

A *route table* is a named bag of routes attached to one or more
subnets in a single VPC. v1 ships:

* an automatic "main" route table per VPC, created atomically with
  the VPC (so existing tests keep working — VPCs without routes
  still resolve "send to local subnet" / "send to virtual gateway")
* operator-created additional route tables in the same VPC
* explicit subnet→route-table association (one route table per
  subnet at any time; default is the VPC's main route table)

```rust
pub struct RouteTable {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub project_id: Uuid,
    pub vpc_id: Uuid,
    pub name: String,
    pub description: String,
    /// True for the auto-created VPC main table. Deleting the main
    /// route table returns 409 Conflict (manager ruling 2026-05-05);
    /// it goes away when the VPC itself is deleted.
    pub is_main: bool,
    pub created_at: DateTime<Utc>,
}

pub struct NewRouteTable {
    pub name: String,
    pub description: Option<String>,
}
```

Subnet associations are stored on the `Subnet` record (see §5
schema notes); a separate `SubnetRouteTableAssociation` record is
not warranted in v1 because at most one route table is active per
subnet at a time.

### 4.2 Route

```rust
pub struct Route {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub project_id: Uuid,
    pub vpc_id: Uuid,
    pub route_table_id: Uuid,
    pub name: String,
    pub description: String,
    /// Destination CIDR. Wire format is the canonical CIDR string.
    pub destination: IpCidr,
    pub target: RouteTarget,
    pub created_at: DateTime<Utc>,
}

pub enum IpCidr {
    V4(Ipv4Network),
    V6(Ipv6Network),
}

#[non_exhaustive]
pub enum RouteTarget {
    /// Drop with no ICMP. Useful for blackholing test prefixes.
    Blackhole,
    /// Drop with ICMP unreachable.
    Reject,
    /// Send to the VPC's virtual gateway. Implicit on the main
    /// table for the VPC's primary CIDRs; explicit anywhere else.
    VirtualGateway,
    /// Send to the named NAT gateway.
    NatGateway { nat_gateway_id: Uuid },
    /// Send to the named floating-IP attachment's NIC. Reserved;
    /// not user-creatable in v1 (only the system installs these).
    FloatingIp { floating_ip_id: Uuid },
}
```

The `RouteTarget` enum is `#[non_exhaustive]` so peering /
interconnect / site-to-site targets can be added in later slices
without a wire break. v1 explicitly rejects `FloatingIp` route
targets at the API edge (the variant exists so the store record can
hold system-installed entries the tritond → blueprint compiler
emits during attach).

Invariants enforced in the create transaction:

* `route_table.vpc_id == path.vpc_id`
* `destination` is a valid CIDR for some family present on the
  parent VPC (an IPv4 destination requires `vpc.ipv4_block` to be
  `Some`; same for IPv6)
* per-table uniqueness on `destination` (CIDR equality, not
  containment — overlapping CIDRs are allowed and resolved by the
  longest-prefix-match rule downstream)
* `NatGateway { nat_gateway_id }` references a `NatGateway` in the
  same VPC (cross-VPC route target → 400)
* `FloatingIp` is rejected at the API edge (400) — system-installed
  only

### 4.3 NAT gateway

A NAT gateway is a project-scoped, VPC-attached egress object. v1
keeps the API minimal and stable; the backing implementation is
chosen by Agent E (firehyve/fhrun edge microVM with nftables for
v0, AF_XDP/Maglev/BGP later) and is invisible at this layer.

```rust
pub struct NatGateway {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub project_id: Uuid,
    pub vpc_id: Uuid,
    pub name: String,
    pub description: String,
    /// Family of the public side. v1 ships V4 first; V6 follows the
    /// same shape because every record carries a single family
    /// (mirrors FloatingIp, locked decision #27).
    pub family: AddressFamily,
    /// Public address allocated at create time from the same Phase 0
    /// pool FloatingIp uses (FLOATING_IP_V{4,6}_POOL). When operator
    /// IpPools land, the wire shape stays the same and the pool
    /// reference moves into NewNatGateway.
    pub public_address: IpAddr,
    /// Edge cluster currently selected to host this NAT. None until
    /// Agent E lands edge-cluster placement; once Agent E ships,
    /// tritond fills this in atomically with create. Operator does
    /// not pick the edge cluster directly in v1.
    pub edge_cluster_id: Option<Uuid>,
    /// Monotonic desired-state generation. Create starts at 1;
    /// future wire-affecting mutations increment it atomically.
    pub desired_generation: u64,
    /// Realized-state pointer; see §6.
    pub realized: RealizedNetworkState,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub struct NewNatGateway {
    pub name: String,
    pub description: Option<String>,
    pub family: AddressFamily,
}
```

Invariants:

* one NAT gateway per `(project, vpc, name)` (within-VPC name
  uniqueness, mirrors VPC/Subnet)
* deleting a NAT gateway with referencing routes returns 409 (no
  cascading deletes; see locked decision #17)
* `family == V4` allocates from `FLOATING_IP_V4_POOL`; `V6` from
  `FLOATING_IP_V6_POOL`. Same allocator the FloatingIp path uses.
  This is a deliberate design choice: NAT gateway public IPs and
  floating IPs are the same kind of resource (a public address
  reserved by the project), so they share the pool. The shared
  index makes "is this address already in use" trivially correct.

### 4.4 Security group

```rust
pub struct SecurityGroup {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub project_id: Uuid,
    pub vpc_id: Uuid,
    pub name: String,
    pub description: String,
    pub created_at: DateTime<Utc>,
}

pub struct NewSecurityGroup {
    pub name: String,
    pub description: Option<String>,
}
```

Per-VPC name-uniqueness, mirrors Subnet.

### 4.5 Security-group rule

```rust
pub struct SecurityGroupRule {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub project_id: Uuid,
    pub vpc_id: Uuid,
    pub security_group_id: Uuid,
    pub description: String,
    pub direction: Direction,
    pub action: RuleAction,
    pub protocol: L4Protocol,
    pub source: HostMatcher,
    pub destination: HostMatcher,
    pub source_ports: PortRange,
    pub destination_ports: PortRange,
    pub icmp_type_code: Option<(u8, u8)>,
    pub created_at: DateTime<Utc>,
}

pub enum Direction { Inbound, Outbound }
pub enum RuleAction { Allow, Deny }
pub enum L4Protocol { Any, Tcp, Udp, Icmp4, Icmp6 }

#[non_exhaustive]
pub enum HostMatcher {
    Any,
    Cidr(IpCidr),
    /// Same-VPC peer SG. Cross-VPC peering matchers are deferred.
    SecurityGroup { security_group_id: Uuid },
}

pub struct PortRange {
    /// Inclusive bounds. (0, 0xffff) for "any".
    pub start: u16,
    pub end: u16,
}
```

The shape deliberately mirrors `proteus::triton_vpc::blueprint::SecurityGroupRule`
so Agent B's compiler is a near-direct projection. Fields the
Proteus blueprint carries that v1 intent does not yet expose
(`DynamicGroup`, `Vpc(VpcId)` matcher) are omitted from the intent
enum and added later when a customer needs them.

Invariants:

* every CIDR in a `Cidr` matcher is canonicalized (`network()` form)
  before storage; arbitrary host bits round-trip but normalize
* `HostMatcher::SecurityGroup` references an SG in the same VPC
* port ranges use `start <= end`; for non-Tcp/Udp the field is
  ignored downstream but stored verbatim
* per-SG rule order is *not* exposed; rules are evaluated as a set.
  **Conflict semantics (manager ruling 2026-05-05): Deny wins over
  Allow; most-specific-match is evaluated *after* Deny precedence.**
  This makes `RuleAction::Deny` meaningful — a Deny rule cannot be
  shadowed by a more-specific Allow. Phase 0 picks set semantics
  deliberately to avoid the AWS-style "rule numbering" footgun.

### 4.6 NIC↔SG attachment

A NIC carries 0..N security groups. The relationship lives on its
own join record so attach/detach are atomic and Cedar gates can
target the action distinctly:

```rust
pub struct NicSecurityGroupAttachment {
    pub nic_id: Uuid,
    pub security_group_id: Uuid,
    pub attached_at: DateTime<Utc>,
}
```

Default behavior when a NIC has no attached SG is "deny all
inbound, allow all outbound" — matches Oxide. **Attached SG policy
must be explicit (manager ruling 2026-05-05); the first Agent A
slices do not auto-create comfort SGs.** Operators and end users
attach SGs as discrete intent.

### 4.7 Floating-IP termination shape

The existing `FloatingIp` keeps its wire shape (locked decision
#27). v1 adds two fields without breaking it. **The widening lands
in its own slice (H-11) that intentionally includes OpenAPI regen,
client regen, and updated tests — no silent widening of the
existing public wire shape (manager ruling 2026-05-05).**

```rust
pub struct FloatingIp {
    // ... existing fields unchanged ...

    /// CN-terminated (decap on the host running the attached
    /// instance) vs Edge-terminated (decap on the named edge
    /// cluster). v1 allows operators to read this; the system sets
    /// it based on the project's edge configuration (Agent E).
    pub termination: FipTermination,
    /// When `termination = EdgeTerminated`, the cluster currently
    /// hosting the FIP. None for CnTerminated.
    pub edge_cluster_id: Option<Uuid>,
    /// Realized-state pointer; see §6.
    pub realized: RealizedNetworkState,
}

#[non_exhaustive]
pub enum FipTermination { CnTerminated, EdgeTerminated }
```

`FipTermination` deliberately matches Proteus
`FipTermination::CnTerminated|EdgeTerminated`. v1 attach-policy:
`POST .../floating-ips/{id}/attach` records the desired NIC and
sets termination based on a per-VPC default (CN-terminated until
edge clusters land, edge-terminated thereafter). The user CLI
contract does *not* expose termination — it's a system attribute.

### 4.8 Edge cluster records (store shape first)

The Proteus blueprint has `AttachmentId edge_cluster` references
and the v1 NAT/FIP intent records carry `edge_cluster_id`. The v1
runtime contract is firehyve/fhrun with an nftables backend; AF_XDP
remains a later empirical optimization if nftables misses the v1
throughput, latency, connection-count, or update targets.

S10 starts with durable store records and no tenant-facing CRUD. The
placer/materializer will create one `NatGateway` edge cluster per NAT
gateway in v1, while the schema already supports more than one
instance per cluster and future floating-IP decap clusters.

```rust
pub enum EdgeClusterKind {
    NatGateway,
    FloatingIpDecap,
    Shared,
}

pub enum EdgeClusterResource {
    NatGateway { nat_gateway_id: Uuid },
    FloatingIp { floating_ip_id: Uuid },
}

pub struct EdgeCluster {
    pub id: Uuid,
    pub name: String,
    pub kind: EdgeClusterKind,
    pub bound_resources: Vec<EdgeClusterResource>,
    pub instances: Vec<EdgeClusterInstance>,
    pub desired_generation: u64,
    pub realized: RealizedNetworkState,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
```

Store invariants:

* `name` is globally unique across edge clusters.
* every `bound_resources` entry exists before the cluster is stored.
* `EdgeClusterKind::NatGateway` binds only NAT gateways;
  `FloatingIpDecap` binds only floating IPs; `Shared` may bind either.
* duplicate bound resources in one create request return 409.
* `desired_generation` starts at 1 and `realized` is computed from
  network-realization rows at read time.

Tenant-facing create/update/delete remains absent for v1. Later S10
slices add the NAT materializer that creates these records and fills
`NatGateway.edge_cluster_id`, then the operator read surface can list
clusters for debugging.

## 5. Schema changes to existing resources

* `Subnet`: add `route_table_id: Uuid`. Set to the parent VPC's
  main route table at create. Operator can later swap via
  `PUT .../subnets/{id}/route-table` (idempotent, no detach window).
* `Vpc`: add `main_route_table_id: Uuid`. Set atomically when the
  VPC is created.
* `FloatingIp`: add `termination` and `edge_cluster_id` (see §4.7).

Migrations: there is no real migration — Phase 0 has not shipped
to a customer. The store types add the fields with `#[serde(default)]`
where reasonable so existing in-process tests keep round-tripping
through the wire.

**No silent widening (manager ruling 2026-05-05).** Each
modification of an existing public API struct (`Vpc`, `Subnet`,
`FloatingIp`) is its own atomic slice that intentionally runs
`make openapi-generate`, `make clients-generate`, and updates the
affected tests. The slice queue (§13) calls this out explicitly on
H-4 (Vpc + Subnet) and H-11 (FloatingIp).

## 6. Desired vs realized state

The dataplane is asynchronous. The control plane records *intent*;
realizers (`tritonagent` per CN, future fhrun-managed edge runtime
per VPC) report what they actually programmed.

Every new v1 network resource record carries a single
`desired_generation: u64` field. The store keeps per-realizer
realization rows in their own keyspace
(`network_realization/<kind>/<id>/<realizer_kind>/<realizer_id>`).
The wire-visible `RealizedNetworkState` field on a resource
response is **computed at read time** from
`(desired_generation, list_network_realizations(resource))` — it is
not stored as a denormalization on the resource record. A
denormalized copy would silently drift from the per-realizer rows
on every realizer report.

```rust
pub struct RealizedNetworkState {
    /// Mirrors the resource record's `desired_generation` field.
    /// Monotonically increased by tritond on every wire-affecting
    /// mutation.
    pub desired_generation: u64,
    /// Highest generation any realizer has reported with
    /// `RealizationStatus::Applied`. None until any realizer
    /// applies.
    pub applied_generation: Option<u64>,
    /// Per-realizer rows, sorted by `(realizer.kind_tag(),
    /// realizer.id())`.
    pub realizations: Vec<Realization>,
}

pub struct Realization {
    pub realizer: RealizerId,
    pub generation: u64,
    pub status: RealizationStatus,
    pub last_reported_at: DateTime<Utc>,
    /// Short free-form diagnostic. Detailed stderr stays in agent
    /// logs / future support bundles, not in unbounded
    /// control-plane rows.
    pub message: Option<String>,
}

#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum RealizerId {
    Cn { id: Uuid },
    EdgeCluster { id: Uuid },
}

#[non_exhaustive]
pub enum RealizationStatus {
    /// Realizer accepted the work and handed the blueprint to its
    /// dataplane. Aligns with Agent B's `accepted_generation`
    /// concept (see `proteus/docs/tritond-integration-v1.md`).
    Accepted,
    /// Dataplane confirmed the generation active.
    Applied,
    /// Realizer failed to converge. `message` describes the phase.
    Failed,
}

impl RealizedNetworkState {
    /// Canonical projection. Sort + roll up per-realizer rows.
    pub fn from_rows(desired_generation: u64, rows: Vec<Realization>) -> Self;
}
```

The variant set is deliberately small. `#[non_exhaustive]` allows
post-v1 additions (e.g. `Compiling`, `Pending`) when downstream
needs them; Agent C calls out that they may want to report a
non-terminal state if Proteus exposes a stable in-progress
status, but v1 does not require it.

Generation rules:

* every store mutation that changes wire-affecting fields (CIDRs,
  rules, route targets, attachments) increments the resource
  record's `desired_generation: u64` atomically with the write.
* `record_network_realization(resource, realizer, generation,
  status, message)` upserts the per-realizer row. A write with
  `generation < existing.generation` is rejected with
  `StoreError::Conflict` (the API edge surfaces this as 409 — the
  "backward report" case the Agent C contract calls out).
  Idempotent at the same generation; status downgrades at the same
  generation are allowed (the dataplane could fail at a previously
  applied generation due to a transient issue).
* `applied_generation` is computed by `from_rows` as the max
  generation across rows whose status is `Applied`.

Rule of thumb (manager-amended 2026-05-05):

* **New resources** introduced by v1 (`RouteTable`, `Route`,
  `NatGateway`, `SecurityGroup`, `SecurityGroupRule`,
  `NicSecurityGroupAttachment`) carry `desired_generation: u64`
  from inception and project a `realized: RealizedNetworkState`
  view at read time.
* **Existing resources** (`Vpc`, `Subnet`, `FloatingIp`) gain
  `desired_generation` only via dedicated slices (H-4 for Vpc +
  Subnet, H-11 for FloatingIp) that intentionally include OpenAPI
  regen, client regen, and updated tests.
* **H-1 ships only the `RealizedNetworkState` types, the helper
  (`from_rows`), and the two store trait methods
  (`record_network_realization`, `list_network_realizations`),
  with focused unit tests. H-1 does not retrofit any existing
  struct and does not ship the HTTP endpoint** (the endpoint lands
  in H-13 once at least one realized resource exists).
* The single realization report endpoint accepts
  `(resource_id, realizer, generation, status, message)` and
  writes into `record_network_realization`. This keeps the
  realized surface small (one endpoint) while the intent surface
  is broad.

## 7. API path shape

All paths nest under the existing tenant/project hierarchy. v1
adds:

```
GET    /v2/tenants/{t}/projects/{p}/vpcs/{v}/route-tables
POST   /v2/tenants/{t}/projects/{p}/vpcs/{v}/route-tables
GET    /v2/tenants/{t}/projects/{p}/vpcs/{v}/route-tables/{rt}
DELETE /v2/tenants/{t}/projects/{p}/vpcs/{v}/route-tables/{rt}

GET    /v2/tenants/{t}/projects/{p}/vpcs/{v}/route-tables/{rt}/routes
POST   /v2/tenants/{t}/projects/{p}/vpcs/{v}/route-tables/{rt}/routes
GET    /v2/tenants/{t}/projects/{p}/vpcs/{v}/route-tables/{rt}/routes/{r}
DELETE /v2/tenants/{t}/projects/{p}/vpcs/{v}/route-tables/{rt}/routes/{r}

PUT    /v2/tenants/{t}/projects/{p}/vpcs/{v}/subnets/{s}/route-table

GET    /v2/tenants/{t}/projects/{p}/vpcs/{v}/nat-gateways
POST   /v2/tenants/{t}/projects/{p}/vpcs/{v}/nat-gateways
GET    /v2/tenants/{t}/projects/{p}/vpcs/{v}/nat-gateways/{n}
DELETE /v2/tenants/{t}/projects/{p}/vpcs/{v}/nat-gateways/{n}

GET    /v2/tenants/{t}/projects/{p}/vpcs/{v}/security-groups
POST   /v2/tenants/{t}/projects/{p}/vpcs/{v}/security-groups
GET    /v2/tenants/{t}/projects/{p}/vpcs/{v}/security-groups/{sg}
DELETE /v2/tenants/{t}/projects/{p}/vpcs/{v}/security-groups/{sg}

GET    /v2/tenants/{t}/projects/{p}/vpcs/{v}/security-groups/{sg}/rules
POST   /v2/tenants/{t}/projects/{p}/vpcs/{v}/security-groups/{sg}/rules
GET    /v2/tenants/{t}/projects/{p}/vpcs/{v}/security-groups/{sg}/rules/{r}
DELETE /v2/tenants/{t}/projects/{p}/vpcs/{v}/security-groups/{sg}/rules/{r}

POST   /v2/tenants/{t}/projects/{p}/instances/{i}/nics/{n}/security-groups
DELETE /v2/tenants/{t}/projects/{p}/instances/{i}/nics/{n}/security-groups/{sg}

POST   /v2/agent/network-realization      (agent-only — Agent C ships
                                           the handler; Agent A ships
                                           the schema and dispatch
                                           into the matching record)

GET    /v2/edge-clusters                  (deferred to Agent D, Agent A
                                           commits only the read shape)
```

`tcadm net` ergonomics (§9) drive the user-facing paths; the
operator-only edge-cluster surface is fleet-scoped (the existing
operator pattern; mirrors `/v2/cns`).

## 8. Store / indexing requirements

The new resources follow the existing `tritond-store` pattern: one
trait method per CRUD operation, structural invariants enforced
inside the same FDB transaction as the write, MemStore mirrors
behavior 1:1 for tests.

### 8.1 New trait methods (sketch)

```rust
trait Store {
    // route tables
    async fn create_route_table(
        &self, tenant_id: Uuid, project_id: Uuid, vpc_id: Uuid, req: NewRouteTable,
    ) -> Result<RouteTable, StoreError>;
    async fn get_route_table(&self, id: Uuid) -> Result<RouteTable, StoreError>;
    async fn list_route_tables_in_vpc(&self, vpc_id: Uuid) -> Result<Vec<RouteTable>, StoreError>;
    async fn delete_route_table(&self, id: Uuid) -> Result<(), StoreError>;

    // routes
    async fn create_route(
        &self, tenant_id: Uuid, project_id: Uuid, vpc_id: Uuid,
        route_table_id: Uuid, req: NewRoute,
    ) -> Result<Route, StoreError>;
    async fn get_route(&self, id: Uuid) -> Result<Route, StoreError>;
    async fn list_routes_in_table(&self, route_table_id: Uuid) -> Result<Vec<Route>, StoreError>;
    async fn delete_route(&self, id: Uuid) -> Result<(), StoreError>;

    // NAT gateways
    async fn create_nat_gateway(
        &self, tenant_id: Uuid, project_id: Uuid, vpc_id: Uuid, req: NewNatGateway,
    ) -> Result<NatGateway, StoreError>;
    async fn get_nat_gateway(&self, id: Uuid) -> Result<NatGateway, StoreError>;
    async fn list_nat_gateways_in_vpc(&self, vpc_id: Uuid) -> Result<Vec<NatGateway>, StoreError>;
    async fn delete_nat_gateway(&self, id: Uuid) -> Result<(), StoreError>;

    // security groups
    async fn create_security_group(...) -> Result<SecurityGroup, StoreError>;
    async fn get_security_group(...) -> ...;
    async fn list_security_groups_in_vpc(...) -> ...;
    async fn delete_security_group(...) -> ...;

    // SG rules
    async fn create_sg_rule(...) -> Result<SecurityGroupRule, StoreError>;
    async fn list_sg_rules(&self, sg_id: Uuid) -> Result<Vec<SecurityGroupRule>, StoreError>;
    async fn get_sg_rule(...) -> ...;
    async fn delete_sg_rule(...) -> ...;

    // NIC ↔ SG attachments
    async fn attach_nic_security_group(&self, nic_id: Uuid, sg_id: Uuid)
        -> Result<NicSecurityGroupAttachment, StoreError>;
    async fn detach_nic_security_group(&self, nic_id: Uuid, sg_id: Uuid)
        -> Result<(), StoreError>;
    async fn list_nic_security_groups(&self, nic_id: Uuid)
        -> Result<Vec<NicSecurityGroupAttachment>, StoreError>;

    // subnet → route table reassociation
    async fn set_subnet_route_table(&self, subnet_id: Uuid, route_table_id: Uuid)
        -> Result<Subnet, StoreError>;

    // realized-state reporting (Slice H-1)
    async fn record_network_realization(
        &self, resource: NetworkResourceId, realizer: RealizerId,
        generation: u64, status: RealizationStatus, message: Option<String>,
    ) -> Result<(), StoreError>;
    async fn list_network_realizations(
        &self, resource: NetworkResourceId,
    ) -> Result<Vec<Realization>, StoreError>;
}
```

`NetworkResourceId` is an enum tagged with the resource kind so a
single endpoint can dispatch into the matching record (e.g.
`{ kind: "nat_gateway", id: <uuid> }`). Callers project the rows
returned by `list_network_realizations` into a
`RealizedNetworkState` view via the canonical helper
`RealizedNetworkState::from_rows(desired_generation, rows)`.

### 8.2 FDB key prefixes

Mirrors the existing layout (`vpc/by_id/<uuid>`,
`vpc/by_project/<proj>/<name>`, `vpc/in_project/<proj>/<vpc>`):

```
route_table/by_id/<uuid>
route_table/by_vpc/<vpc>/<name>
route_table/in_vpc/<vpc>/<rt>
route_table/main/<vpc>                    -> rt_id (singleton-per-vpc)

route/by_id/<uuid>
route/by_table/<rt>/<destination_canon>   (uniqueness: dest CIDR per table)
route/in_table/<rt>/<route>
route/by_nat_gateway/<nat>/<route>        (reverse target index for delete guards)

nat_gateway/by_id/<uuid>
nat_gateway/by_vpc/<vpc>/<name>
nat_gateway/in_vpc/<vpc>/<nat>
nat_gateway/by_address/<addr>             (uniqueness: address singleton —
                                           shared with floating_ip pool index)

edge_cluster/by_id/<uuid>
edge_cluster/by_name/<name>
edge_cluster/all/<uuid>
edge_cluster/by_resource/<kind>/<resource>/<edge>

security_group/by_id/<uuid>
security_group/by_vpc/<vpc>/<name>
security_group/in_vpc/<vpc>/<sg>

sg_rule/by_id/<uuid>
sg_rule/in_sg/<sg>/<rule>

nic_sg/<nic>/<sg>                         (membership, double-key for
                                           list-by-nic and list-by-sg)
nic_sg_by_sg/<sg>/<nic>

network_realization/<resource_kind>/<resource>/<realizer>
                                          (single key per (resource,
                                           realizer); applied generation
                                           lives in the row)
```

Pool index for public addresses (NAT GW + FIP share the same v1
pool) reuses the existing `floating_ip/ip_alloc/v{4,6}/<addr>` key
namespace; the value distinguishes which resource holds it.

### 8.3 MemStore mirroring

Every new field in `Inner` is a `HashMap` matching the FDB indexes
1:1. Tests in `libs/tritond-store/src/mem.rs` cover round-trip,
within-VPC name uniqueness, cross-VPC same-name OK, parent-not-found
404, route-table-cannot-delete-when-routes-exist 409, NAT-cannot-
delete-when-routes-target-it 409, SG-cannot-delete-when-rules-exist
409 (or cascade — see §11 open question), NIC-not-found 404.

## 9. Auth / Cedar additions

Add to the `Action` enum in `services/tritond/src/auth.rs`:

```
RouteTableList, RouteTableCreate, RouteTableGet, RouteTableDelete,
RouteList, RouteCreate, RouteGet, RouteDelete,
NatGatewayList, NatGatewayCreate, NatGatewayGet, NatGatewayDelete,
SecurityGroupList, SecurityGroupCreate, SecurityGroupGet, SecurityGroupDelete,
SgRuleList, SgRuleCreate, SgRuleGet, SgRuleDelete,
NicSgAttach, NicSgDetach, NicSgList,
SubnetSetRouteTable,
EdgeClusterList,                          // operator-only fleet-scoped
NetworkRealizationReport,                 // agent-only
```

Add to the Cedar `tenant-member-allows-tenant-scoped-actions` rule
the action list above (except `EdgeClusterList`, which stays root-
only, and `NetworkRealizationReport`, which is gated under the
existing Agent scope — locked decision #29).

`is_read_action` adds `RouteTableList | RouteTableGet | RouteList |
RouteGet | NatGatewayList | NatGatewayGet | SecurityGroupList |
SecurityGroupGet | SgRuleList | SgRuleGet | NicSgList`. This is
load-bearing: ReadOnly-scope API keys must continue to work.

## 10. CLI shape

`tcadm net` (operator) and `triton net` (end-user, future) follow
the existing `tcadm tenant project vpc` pattern. v1 ships every
subcommand under `tcadm` to unblock operator workflows; the
end-user `triton` CLI will be a thin alias.

```
tcadm net route-table list      T P V
tcadm net route-table create    T P V --name NAME
tcadm net route-table get       T P V RT
tcadm net route-table delete    T P V RT

tcadm net route list            T P V RT
tcadm net route create          T P V RT \
                                --name NAME --destination CIDR \
                                --target nat-gateway:<ID>|virtual-gateway|blackhole|reject
tcadm net route get             T P V RT R
tcadm net route delete          T P V RT R

tcadm net nat-gw list           T P V
tcadm net nat-gw create         T P V --name NAME --family v4|v6
tcadm net nat-gw get            --tenant T --project P --vpc V <ID>
tcadm net nat-gw delete         --tenant T --project P --vpc V <ID>

tcadm net sg list               --tenant T --project P --vpc V
tcadm net sg create             --tenant T --project P --vpc V --name NAME
tcadm net sg get                --tenant T --project P --vpc V <ID>
tcadm net sg delete             --tenant T --project P --vpc V <ID>

tcadm net sg rule list          ... --sg SG
tcadm net sg rule create        ... --sg SG --direction inbound|outbound \
                                --action allow|deny --protocol tcp|udp|icmp4|icmp6|any \
                                --source any|cidr:CIDR|sg:<ID> \
                                --destination ... \
                                --source-ports start-end \
                                --destination-ports start-end \
                                --icmp type/code
tcadm net sg rule get           ... --sg SG <ID>
tcadm net sg rule delete        ... --sg SG <ID>

tcadm net nic sg attach         --tenant T --project P --instance I --nic N --sg SG
tcadm net nic sg detach         --tenant T --project P --instance I --nic N --sg SG
tcadm net nic sg list           --tenant T --project P --instance I --nic N

tcadm net subnet route-table set --tenant T --project P --vpc V --subnet S --route-table RT

tcadm net realized              --resource <kind>:<id>     # show realization rows
```

Helper UX: `tcadm net realized --watch` polls until applied
generation == desired generation, with a per-realizer status. This
is the tool operators use to answer "did the dataplane converge?"
without reading FDB.

## 11. Out-of-scope (deferred from v1)

* **Dynamic groups.** Reserved field on Proteus blueprint;
  intent shape adds `HostMatcher::DynamicGroup { id }` later when a
  customer needs tag-based SG membership.
* **Gateway firewall.** Per-gateway policy attachment. Reserved.
* **VPC peering, transit gateways, cloud interconnect, site-to-site.**
  Proteus blueprint reserves the variants; intent surface ships
  none in v1.
* **Multicast.** Proteus blueprint reserves `MulticastPolicy`;
  intent surface ships nothing.
* **Operator `IpPool` resource.** v1 keeps `FLOATING_IP_V{4,6}_POOL`
  hardcoded constants. NAT GW and FIP both draw from those. The
  IpPool resource is a one-slice rip-and-replace later; the wire
  shape on `NatGateway` / `FloatingIp` does not change.
* **Ephemeral IPs.** AWS-style "auto-public-IP at instance launch"
  is deferred. v1 requires explicit FIP allocate + attach.
* **DHCP / DNS configuration.** Per-VPC DhcpCfg / DNS resolvers are
  on Proteus blueprint but v1 ships compiler-default values
  (Agent B picks). The intent surface adds them when first
  customer requires non-default DNS / DHCP options.
* **Per-rule SG ordering / numbering.** v1 uses set semantics
  (Allow > Deny, longest-prefix). Numbered NACLs land later if a
  customer asks.

### Manager rulings (2026-05-05)

All five open design questions are resolved. Decisions live
verbatim in `AGENT_A_MANAGER_DECISIONS.md` at the umbrella root;
this section restates them so the design doc is self-contained.

* **No cascading deletes — approved.** SG-with-rules → 409;
  SG-attached-to-NICs → 409; RT-with-routes-or-subnet-associations
  → 409; NAT-GW-referenced-by-routes → 409. Operators must clear
  children before deleting parents.
* **NAT GW shares the FloatingIp public pool — approved.** The
  shared allocator records the resource holder
  (`{kind, id}` value on the pool index key) so FIP and NAT
  allocations cannot collide. Delete/release frees the address
  cleanly.
* **`HostMatcher::SecurityGroup` resolved at Proteus compile time —
  approved.** Intent stores the SG id verbatim; Agent B resolves
  membership when compiling the per-port blueprint. Recompile
  triggers are listed in §12.1.
* **NIC default policy — approved with clarification.** No attached
  SG ⇒ deny inbound, allow outbound. Attached SG policy must be
  explicit. Agent A's first slices do not auto-create comfort SGs.
* **Security-rule conflict semantics — amended.** Deny wins over
  Allow. Most-specific-match is evaluated *after* Deny precedence.
  `RuleAction::Deny` is meaningful and cannot be shadowed by a
  more-specific Allow. (See §4.5 for the full rule.)
* **EdgeCluster ownership — amended again for S10.** Agent D still
  owns the firehyve/fhrun runtime contract, but the control-plane
  store now carries first-class `EdgeCluster` records (§4.8) so the
  NAT materializer has durable placement state to write. Tenant-facing
  EdgeCluster CRUD remains out of v1.
* **`RealizedNetworkState` sequencing — amended.** H-1 ships only
  the types, helpers, and store reporting machinery with focused
  tests; it does *not* retrofit the field onto any existing public
  API struct. Existing-struct widening (Vpc, Subnet, FloatingIp)
  lands in dedicated slices that intentionally include OpenAPI
  regen, client regen, and test updates. (See §6 rule of thumb.)
* **Route table main delete — amended.** Deleting the main route
  table returns 409 Conflict (not 404). The main RT is removed
  only when the parent VPC itself is deleted.

## 12. Handoff contracts

### 12.1 Agent B — Proteus blueprint compiler

**Reads (intent records owned by Agent A):**

* `Vpc { id, vni, ipv4_block, ipv6_block, main_route_table_id }`
* `Subnet { id, vpc_id, ipv4_block, ipv6_block, route_table_id }`
* `RouteTable { id, vpc_id, is_main }`
* `Route { route_table_id, destination, target }`
* `SecurityGroup { id, vpc_id }`, `SecurityGroupRule` (list per SG)
* `NicSecurityGroupAttachment { nic_id, security_group_id }`
* `NatGateway { id, vpc_id, public_address, edge_cluster_id, family }`
* `FloatingIp { id, address, attached_to, termination, edge_cluster_id }`
* `Instance`, `Nic` (existing)

**Writes (none):** Agent B is read-only against `tritond` intent.
Compiled blueprints are sent to `tritonagent` via the existing
`/v2/agent/jobs/{id}/blueprint` mechanism.

**Contract guarantees from Agent A:**

* every record has stable `id: Uuid`, immutable parent linkage,
  monotonic `desired_generation`
* `desired_generation` increments atomically with any field
  Agent B observes
* Agent A never mutates a record without bumping the generation
* `RouteTarget::SecurityGroup` (HostMatcher variant) refers to an
  SG in the same VPC; cross-VPC matchers will land in a future
  slice with a separate variant — Agent B can rely on
  same-VPC-only resolution

**Required from Agent B:**

* a `compile(per_port_inputs) -> TritonVpcBlueprint` function. The
  per-port input bundle is "an Instance NIC plus everything reachable
  from it via the intent graph"; no further roll-up into
  per-CN blueprints is needed at this layer.
* Agent B does not need to know about `RealizedNetworkState` —
  realized-state is reported by the realizer (Agent C / Agent E),
  not by the compiler.

**Recompile triggers (manager ruling 2026-05-05).** Because
`HostMatcher::SecurityGroup` is resolved at compile time into
`Cidr` matchers, Agent B must recompile any per-port blueprint
that names an SG when *any* of the following changes:

* a `SecurityGroupRule` is created or deleted in any SG attached
  to the port's NIC, or in any SG referenced by a rule's
  `HostMatcher::SecurityGroup` matcher;
* a `NicSecurityGroupAttachment` is created or deleted on any NIC
  whose membership feeds an SG referenced by the port's blueprint
  (this includes both the port's own NIC and every NIC that is a
  member of an SG that another rule resolves);
* a `Nic` primary IP changes (rare in v1 — only on instance
  recreate — but possible) and that NIC is a member of an SG
  whose membership is resolved into the port's blueprint;
* a `Subnet` route-table association changes
  (`PUT .../subnets/{id}/route-table`), affecting every port in
  that subnet;
* a `Route` is created, deleted, or has its `target` mutated in
  the route table the port's subnet is associated with.

Agent A guarantees that every triggering mutation atomically
bumps the affected resource's `desired_generation`, so Agent B's
"is the blueprint stale?" check reduces to a generation
comparison; Agent B does not have to reason about the trigger
graph at the source.

### 12.2 Agent C — `tritonagent` realized-state reporting

**Reads (from Agent A):**

* the per-job `ProvisioningBlueprint` extension surface so the
  agent can pick up the network realization metadata it needs to
  ack.

**Writes (into Agent A):**

* `POST /v2/agent/network-realization` with body
  `{ resource: NetworkResourceId, realizer: RealizerId::Cn(server_uuid),
     generation: u64, status: RealizationStatus, message: Option<String> }`.

**Contract guarantees:**

* the endpoint is gated by Agent scope + per-CN binding
  (locked decision #36); for `RealizerId::Cn`, the reported CN must
  match the bound API key. The future blueprint ownership check
  ("this CN received this resource in a blueprint") lands with the
  blueprint/apply slices that introduce that mapping.
* `applied_generation` only moves forward; backward reports are
  rejected (409) — this is the canonicalization Agent A enforces
  in the store transaction
* Agent C does not need to know about route table / SG topology —
  it only echoes the generation the dataplane (Proteus) reported
  applying. The mapping "this Proteus port is governed by these
  intent records" is Agent B's concern at compile time and rides
  along in the blueprint Agent C receives.

### 12.3 Agent D — firehyve / fhrun edge runtime

**Reads (from Agent A):**

* `EdgeCluster` records created by the NAT materializer.
* `NatGateway` records that name an `edge_cluster_id` matching
  this edge cluster.
* `FloatingIp` records with `termination = EdgeTerminated` and
  matching `edge_cluster_id`.

**Writes (into Agent A):**

* same `POST /v2/agent/network-realization` endpoint, with
  `realizer: RealizerId::EdgeCluster(edge_cluster_id)`.

**Contract:**

* Agent A renders the stable edge manifest contract and stores
  cluster placement state. Agent D/E materializes that contract via
  firehyve/fhrun and reports realization against the edge cluster.
* Agent A guarantees that NAT GW / FIP records carry an
  `edge_cluster_id` whenever termination is edge-side, so Agent E
  has a concrete cluster to render against.

### 12.4 Agent E — NAT and north/south realization

Agent E's per-flow / per-rule mechanics are out of Agent A's view.
What Agent A guarantees:

* NAT gateway is a stable, atomically-created object with a stable
  public address and a (possibly-late-bound) `edge_cluster_id`.
* Routes targeting `RouteTarget::NatGateway` are resolvable end-to-
  end without further indirection.
* The shared address pool with FloatingIp means Agent E never has
  to chase address-collision races between NAT public IPs and
  FIPs.
* Realization reports flow through the same single endpoint as
  Agent C's, so Agent E does not need a parallel reporting
  surface.

## 13. Proposed atomic commit queue

Each entry is a single commit per the AGENTS.md rule (one logical
change). Tests + docs ride with the code in the same commit.

| # | Slice | Touches | Tests |
|---|-------|---------|-------|
| H-1 | `RealizedNetworkState`, `Realization`, `RealizerId`, `RealizationStatus`, `NetworkResourceId` types + `RealizedNetworkState::from_rows` helper + `record_network_realization` and `list_network_realizations` store trait methods + MemStore impl + FdbStore impl. **No retrofit onto `Vpc`/`Subnet`/`FloatingIp`** (manager ruling). | `libs/tritond-store/src/{types,lib,mem,fdb}.rs` | unit: helper rollup; round-trip; backward-generation rejection (StoreError::Conflict); same-generation status downgrade allowed; idempotent re-report; multi-realizer rows; distinct resources isolated; pre-realization list returns empty (not 404); kind-tag stability for both enums |
| H-2 | `NatGateway` record (store layer only): types, trait, MemStore, FdbStore. The stored row carries `desired_generation` from inception; the public view returns a computed `realized: RealizedNetworkState` without storing that denormalization. Address allocator extends the existing FIP pool to record `{kind, id}` so FIP+NAT cannot collide. | `libs/tritond-store/src/{types,lib,mem,fdb}.rs` | unit: create/get/list/delete, within-VPC name uniqueness, cross-VPC same name OK, shared-pool collision impossible, delete frees address |
| H-3 | `NatGateway` API surface + handlers + `tcadm net nat-gw` + integration tests. Copies the `tests/vpcs.rs` template. **Includes `make openapi-generate` + `make clients-generate`** (new endpoints). | `apis/tritond-api/src/lib.rs`, `services/tritond/src/{lib,auth}.rs`, `cli/tcadm/src/{main,commands}.rs`, `clients/internal/tritond-client` (regen), `services/tritond/tests/nat_gateways.rs` | integration: cross-tenant 404, within-VPC name unique, address from pool, anonymous → 404. **Does not** include the delete-when-route-references → 409 test (deferred to H-6 when `Route` exists; manager ruling). |
| H-4 | `RouteTable` record + `Vpc.main_route_table_id` (atomic with VPC create) + `Subnet.route_table_id` (defaults to parent VPC's main RT). **Widens `Vpc` and `Subnet` — slice intentionally runs `make openapi-generate` + `make clients-generate` and updates affected tests** (manager ruling: no silent widening). | `libs/tritond-store/src/{types,lib,mem,fdb}.rs`, `apis/tritond-api/src/lib.rs`, `clients/internal/tritond-client` (regen), `services/tritond/tests/{vpcs,subnets}.rs` | unit: VPC create produces main RT, subnet create defaults to main RT; integration: existing VPC + subnet tests round-trip new fields |
| H-5 | `RouteTable` API surface + handlers + `tcadm net route-table` | API + service + CLI + tests | integration: list/create/get/delete; main RT delete → 409 (not 404); cross-VPC 404; anonymous denied; tenant member allowed only in own tenant. RT-with-routes delete → 409 lands with H-6 when `Route` exists. Non-main RT-with-subnet-associations delete → 409 lands with H-10 when public reassociation exists; store protection is already present from H-4. |
| H-6 | `Route` store + API + CLI + tests; `RouteTarget::NatGateway` references resolve in same VPC. **Adds the NAT-GW-with-referencing-routes → 409 test deferred from H-3.** | `libs/tritond-store/src/{types,lib,mem,fdb}.rs`, `apis/tritond-api/src/{lib,types}.rs`, `services/tritond/src/{lib,auth}.rs`, `cli/tcadm/src/{main,commands}.rs`, `clients/internal/tritond-client` (regen), `services/tritond/tests/routes.rs` | integration: route create/list/get/delete; route create with NAT target across VPC → 400; per-table destination uniqueness; NAT delete with referencing route → 409; route-table delete with routes → 409; FloatingIp variant rejected at API edge → 400; anonymous denied. |
| H-7 | `SecurityGroup` store + API + CLI + tests | API + service + CLI + tests | integration: within-VPC name uniqueness, delete-when-rules-exist → 409, delete-when-NICs-attached → 409 |
| H-8 | `SecurityGroupRule` store + API + CLI + tests. Conflict semantics: Deny > Allow, then most-specific (manager ruling); enforced at Agent B compile, but the rule grammar is exercised here. | API + service + CLI + tests | integration: rule grammar serialization round-trip; CIDR canonicalization; cross-SG matcher resolution within VPC only |
| H-9 | `NicSecurityGroupAttachment` store + API + CLI + tests. Default-deny-inbound applies when zero SGs attached. | API + service + CLI + tests | integration: attach/detach atomic; instance delete cascades detach (matches FloatingIp pattern); delete-attached SG → 409 |
| H-10 | `Subnet` route-table reassociation (`PUT .../subnets/{id}/route-table`); bumps subnet `desired_generation`. | API + service + CLI + tests | integration: reassoc bumps generation; idempotent set-to-current is a no-op |
| H-11 | `FloatingIp` adds `termination` + `edge_cluster_id` + `realized: RealizedNetworkState`. **Widens an existing public API struct — slice intentionally runs `make openapi-generate` + `make clients-generate` and updates `tests/floating_ips.rs`** (manager ruling: no silent widening). | types + handlers + API + client (regen) + tests | integration: existing FIP attach/detach tests still pass; defaults are `CnTerminated` + `edge_cluster_id = None` + `realized` = empty |
| H-12 / S10 | `EdgeCluster` store records: durable cluster, bound-resource index, computed realized view, and store-only CRUD for the NAT materializer. No tenant-facing CRUD yet. | `libs/tritond-store/src/{types,lib,mem,fdb}.rs` | unit: create/get/list/list-by-resource/delete; duplicate name; missing resource; kind/resource mismatch; duplicate bound resource |
| H-13 | **Done.** `POST /v2/agent/network-realization` handler + Agent-scope auth gating. Requires a CN-bound Agent key, validates the reported resource exists, enforces `RealizerId::Cn` matches the bound CN, and stores the row through `record_network_realization`. | `apis/tritond-api/src/{lib,types}.rs`, `services/tritond/src/{lib,auth}.rs`, `clients/internal/tritond-client` (regen), `services/tritond/tests/agent.rs` | integration: per-CN bound key required; unbound Agent key denied; backward-generation report → 409; multiple CN realizers produce distinct rows on the NatGateway realized view |
| H-14 | `tcadm net realized` UX (read-only across realized resource kinds) | CLI + tests | smoke: `tcadm net realized --resource nat_gateway:<id>` shows desired vs applied; `--watch` polls until applied == desired |

H-1 to H-3 are the **first integration milestone** Agent A reports
back to the coordinator: NAT GW exists end-to-end and Agent B can
start consuming it. H-4 starts the routing loop by making every VPC
carry an atomically-created main route table and every new subnet
inherit it. H-5 to H-6 expose and populate those route tables. H-7
to H-10 close the firewall loop. H-11 to H-14 close the edge /
realized loop.

## 14. Recommended first slice

Implement **H-1 + H-2 + H-3 as a tight three-commit cluster**, in
that order (manager green-light 2026-05-05):

* H-1 because every *new* network resource (starting with NAT GW
  in H-2) carries a desired generation and returns a computed
  `RealizedNetworkState` view from inception; landing the type,
  helpers, and store reporting machinery first avoids reshuffling
  them across H-2..H-9. **H-1 explicitly does not
  retrofit `realized` onto `Vpc` / `Subnet` / `FloatingIp`** —
  those land in dedicated slices (H-4, H-11) per manager ruling.
* H-2 + H-3 because NAT gateway is the smallest reasonable
  end-to-end resource (no children, one parent, one address
  allocation, trivial CLI) and is the resource that proves the
  "intent-only, no realization" contract works without Agent B or
  Agent E being online yet.
