// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Domain types shared between the storage layer and the wire surface.

use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use chrono::{DateTime, Utc};
use ipnetwork::{IpNetwork, Ipv4Network, Ipv6Network};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A tenancy boundary. Every Triton Cloud resource ultimately rolls up
/// to a silo; in production each silo is bound to its own identity
/// provider. Phase 0 carries only the bare-minimum identifying fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Silo {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    /// The tenant federated users from this silo's IdP land in by
    /// default. Created atomically with the silo. Operators can
    /// later create additional tenants in the silo and (in a future
    /// slice) re-assign users.
    pub default_tenant_id: Uuid,
    pub created_at: DateTime<Utc>,
}

/// Request body for creating a silo.
///
/// Distinct from [`Silo`] because the server assigns `id` and
/// `created_at`. `description` is optional on the wire and stored as
/// an empty string when omitted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NewSilo {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
}

/// Operator or federated-tenant account.
///
/// One `User` row covers two distinct credential models:
///
/// * **Password-auth operators** — `tenant_id = None`, `federation =
///   None`, `password_hash` is the bcrypt hash of the operator's
///   password. The bootstrap root user is the canonical example;
///   root operators are cluster-wide and have no tenant.
/// * **Federated users** — `tenant_id = Some(...)`, `federation =
///   Some(...)` carries the OIDC `(issuer, subject)` pair, and
///   `password_hash` is empty. Created just-in-time on the first
///   successful OIDC login per `(silo_id, issuer, subject)`; the
///   user lands in the silo's default tenant
///   ([`Silo::default_tenant_id`]).
///
/// `is_root` is mutually exclusive with `tenant_id`: the root
/// operator is cluster-wide; federated users are tenant-scoped.
///
/// The owning silo can be derived from `tenant_id` via a
/// [`Tenant::silo_id`] lookup when needed (e.g. by the auth layer
/// for legacy silo-scoped Cedar rules).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct User {
    pub id: Uuid,
    pub username: String,
    /// Bcrypt hash of the operator's password. Empty string for
    /// federated users (whose credential is the upstream OIDC
    /// token).
    pub password_hash: String,
    /// True for the bootstrap operator. Cedar policy uses this to
    /// short-circuit to "permit anything" until per-action policies
    /// are written.
    pub is_root: bool,
    pub created_at: DateTime<Utc>,
    /// Tenant this user belongs to. `None` for the bootstrap root
    /// operator and other cluster-wide accounts. Federated users
    /// are always tenant-scoped — they land in their silo's
    /// default tenant (see [`Silo::default_tenant_id`]) on JIT
    /// creation. The owning silo, if any, is derivable via
    /// [`Tenant::silo_id`].
    #[serde(default)]
    pub tenant_id: Option<Uuid>,
    /// External-IdP linkage when this user authenticates via OIDC.
    /// `None` for password-auth users.
    #[serde(default)]
    pub federation: Option<Federation>,
}

/// External-IdP linkage for a federated [`User`]. Combined with
/// the silo derived from [`User::tenant_id`] this is the unique
/// key the auth middleware resolves on each OIDC login.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Federation {
    /// OIDC `iss` claim — must equal the issuer URL of the silo's
    /// configured [`IdpConfig`].
    pub issuer: String,
    /// OIDC `sub` claim — the upstream IdP's stable identifier for
    /// this user.
    pub subject: String,
}

/// Per-silo OpenID Connect identity-provider configuration.
///
/// Operators write one of these per silo via
/// `POST /v2/silos/{silo_id}/idp`; tritond eagerly fetches the
/// discovery document at write time and only persists the config if
/// the IdP is reachable and well-formed. Tenant users in that silo
/// then authenticate by presenting their IdP-issued ID token as a
/// `Bearer` credential.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdpConfig {
    /// OIDC issuer URL. Must match the `iss` claim on every
    /// presented ID token.
    pub issuer_url: String,
    /// OIDC client identifier registered with the IdP.
    pub client_id: String,
    /// OIDC client secret. Stored in plaintext for Phase 0e-b;
    /// encryption at rest is a manta-storage Phase 0/1 concern.
    pub client_secret: String,
    /// Expected `aud` claim. Defaults to `client_id` when absent.
    pub audience: Option<String>,
}

/// Wire-safe view of an [`IdpConfig`] with the client secret
/// redacted. Returned by `GET /v2/silos/{silo_id}/idp`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct IdpConfigView {
    pub issuer_url: String,
    pub client_id: String,
    pub audience: Option<String>,
}

/// Sub-tenancy boundary inside a tenant. Workload resources
/// (instances, volumes, networks) nest under projects.
///
/// E-3 re-parented projects from `silo_id` to `tenant_id`: the
/// silo brand-level container owns one or more tenants; tenants
/// own projects; projects own workloads. The owning silo, if
/// needed, is derivable via [`Tenant::silo_id`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Project {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub name: String,
    pub description: String,
    pub created_at: DateTime<Utc>,
}

/// Request body for creating a project. The owning tenant comes
/// from the URL path, not the body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NewProject {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
}

/// A logical customer container inside a [`Silo`]. Tenants own
/// users, projects, and per-tenant resources; a Silo is the
/// brand-level identity / billing / catalog parent.
///
/// Phase 0 ships the structural primitive only. Per-tenant
/// billing fields, IdP overrides, and explicit user-tenant
/// membership land in follow-on slices.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Tenant {
    pub id: Uuid,
    pub silo_id: Uuid,
    pub name: String,
    pub description: String,
    pub created_at: DateTime<Utc>,
}

/// Request body for creating a tenant. The owning silo comes
/// from the URL path, not the body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NewTenant {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
}

/// Reserved-VNI ceiling. Values below this are off-limits for tenant
/// VPCs; the dataplane keeps `[0, 4096)` for system VNIs (boundary
/// services, transit, future internal traffic). 4096 matches Oxide's
/// reserved range.
pub const VPC_VNI_RESERVED_CEILING: u32 = 4096;

/// 24-bit Geneve VNI ceiling (exclusive). VNIs are drawn from
/// `[VPC_VNI_RESERVED_CEILING, VPC_VNI_MAX)`.
pub const VPC_VNI_MAX: u32 = 1 << 24;

/// Tenant VPC. Project-scoped, tenant-rooted. Mirrors the per-VPC
/// fields OPTE consumes in `oxide_vpc::api::VpcCfg`: a 24-bit Geneve
/// VNI plus an optional primary IPv4 CIDR and optional primary IPv6
/// CIDR (matching OPTE's `IpCfg` enum: Ipv4-only, Ipv6-only, or
/// dual-stack). Per-NIC concerns (guest MAC, private IPs, external
/// IPs, attached_subnets, DHCP) are *not* on the VPC record — those
/// land on subnet/instance/NIC resources in later slices.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Vpc {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub project_id: Uuid,
    /// Route table inherited by new subnets unless explicitly
    /// reassociated later. Created atomically with the VPC.
    pub main_route_table_id: Uuid,
    pub name: String,
    pub description: String,
    /// Geneve Virtual Network Identifier. Server-assigned at create
    /// time, drawn from `[VPC_VNI_RESERVED_CEILING, VPC_VNI_MAX)`,
    /// unique rack-wide.
    pub vni: u32,
    /// Primary IPv4 CIDR for the VPC overlay. `None` for IPv6-only.
    /// Wire format is the canonical CIDR string, e.g. `"10.0.0.0/24"`.
    #[schemars(with = "Option<String>")]
    pub ipv4_block: Option<Ipv4Network>,
    /// Primary IPv6 CIDR for the VPC overlay. `None` for IPv4-only.
    /// Wire format is the canonical CIDR string, e.g. `"fd00::/48"`.
    #[schemars(with = "Option<String>")]
    pub ipv6_block: Option<Ipv6Network>,
    pub created_at: DateTime<Utc>,
}

/// Request body for creating a VPC. The owning tenant + project come
/// from the URL path, not the body. The server assigns `id`, `vni`,
/// and `created_at`. At least one of `ipv4_block` / `ipv6_block` must
/// be `Some`; the API rejects requests with both `None`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NewVpc {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    #[schemars(with = "Option<String>")]
    pub ipv4_block: Option<Ipv4Network>,
    #[serde(default)]
    #[schemars(with = "Option<String>")]
    pub ipv6_block: Option<Ipv6Network>,
}

/// Layer-3 subnet inside a VPC. Each subnet carves a CIDR out of its
/// parent VPC's IPv4 and/or IPv6 block. Multiple subnets may exist
/// per VPC; their CIDRs must not overlap. NIC attach points to a
/// specific subnet at instance-launch time.
///
/// Invariants enforced at create time:
/// * Every present subnet CIDR must be a strict subnet of the parent
///   VPC's same-family CIDR (`ipv4_block ⊆ vpc.ipv4_block`,
///   `ipv6_block ⊆ vpc.ipv6_block`).
/// * No subnet CIDR (in either family) may overlap an existing
///   subnet CIDR in the same VPC.
/// * At least one of `ipv4_block` / `ipv6_block` must be `Some`, and
///   each present family must also be present on the parent VPC
///   (an IPv4-only VPC cannot host an IPv6 subnet).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Subnet {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub project_id: Uuid,
    pub vpc_id: Uuid,
    /// Active route table for this subnet. Defaults to the parent
    /// VPC's `main_route_table_id` at create time.
    pub route_table_id: Uuid,
    pub name: String,
    pub description: String,
    /// IPv4 CIDR for this subnet. Must be a subnet of the parent
    /// VPC's `ipv4_block`, and must not overlap any other subnet's
    /// IPv4 CIDR in the same VPC.
    #[schemars(with = "Option<String>")]
    pub ipv4_block: Option<Ipv4Network>,
    /// IPv6 CIDR for this subnet. Must be a subnet of the parent
    /// VPC's `ipv6_block`, and must not overlap any other subnet's
    /// IPv6 CIDR in the same VPC.
    #[schemars(with = "Option<String>")]
    pub ipv6_block: Option<Ipv6Network>,
    pub created_at: DateTime<Utc>,
}

/// Request body for creating a subnet. The owning tenant, project,
/// and VPC come from the URL path. The server assigns `id` and
/// `created_at`. At least one of `ipv4_block` / `ipv6_block` must be
/// `Some`; the API rejects requests with both `None`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NewSubnet {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    #[schemars(with = "Option<String>")]
    pub ipv4_block: Option<Ipv4Network>,
    #[serde(default)]
    #[schemars(with = "Option<String>")]
    pub ipv6_block: Option<Ipv6Network>,
}

/// Named set of routes inside a VPC. Every VPC has one auto-created
/// main table; additional tables can be created and later associated
/// to subnets when the route-table API lands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RouteTable {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub project_id: Uuid,
    pub vpc_id: Uuid,
    pub name: String,
    pub description: String,
    /// True for the table created atomically with the VPC. Main route
    /// tables cannot be deleted directly; they are removed with the
    /// parent VPC.
    pub is_main: bool,
    pub created_at: DateTime<Utc>,
}

/// Request body for creating an additional route table in a VPC.
/// Parentage is inferred from the URL path once the public API lands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NewRouteTable {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
}

/// Destination CIDR for a route. Serialized as a canonical CIDR
/// string, e.g. `"0.0.0.0/0"` or `"fd00::/48"`.
pub type IpCidr = IpNetwork;

/// One route row inside a route table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Route {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub project_id: Uuid,
    pub vpc_id: Uuid,
    pub route_table_id: Uuid,
    pub name: String,
    pub description: String,
    /// Destination CIDR. Wire format is the canonical CIDR string.
    #[schemars(with = "String")]
    pub destination: IpCidr,
    pub target: RouteTarget,
    pub created_at: DateTime<Utc>,
}

/// Request body for creating a route inside a route table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NewRoute {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// Destination CIDR. Wire format is the canonical CIDR string.
    #[schemars(with = "String")]
    pub destination: IpCidr,
    pub target: RouteTarget,
}

/// Route target. Post-v1 variants such as peering, interconnect, and
/// site-to-site VPN can be added without changing the existing wire
/// shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum RouteTarget {
    /// Drop without an ICMP response.
    Blackhole,
    /// Drop with ICMP unreachable.
    Reject,
    /// Send to the VPC virtual gateway.
    VirtualGateway,
    /// Send to a NAT gateway in the same VPC.
    NatGateway { nat_gateway_id: Uuid },
    /// Reserved for system-installed routes. Public v1 API rejects
    /// this target at the edge.
    FloatingIp { floating_ip_id: Uuid },
}

/// Project-owned VPC egress point. A NAT gateway reserves one public
/// address from the same Phase 0 public pool used by [`FloatingIp`],
/// then downstream edge realization decides where and how that
/// address is programmed.
///
/// The stored record carries `desired_generation`; [`Self::realized`]
/// is computed from the realization rows at read time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NatGateway {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub project_id: Uuid,
    pub vpc_id: Uuid,
    pub name: String,
    pub description: String,
    /// Public address family requested at create time.
    pub family: AddressFamily,
    /// Public source address reserved for egress.
    pub public_address: IpAddr,
    /// Edge cluster selected to host this NAT gateway. `None` until
    /// edge placement lands in the Agent D/E slices.
    #[serde(default)]
    pub edge_cluster_id: Option<Uuid>,
    /// Monotonic desired-state generation. Create starts at 1;
    /// future wire-affecting mutations increment it atomically.
    pub desired_generation: u64,
    /// Read-time projection of the per-realizer rows for this NAT
    /// gateway. This is not stored as a denormalized field.
    pub realized: RealizedNetworkState,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Request body for creating a [`NatGateway`]. Parentage is inferred
/// from the tenant/project/VPC URL path. The server assigns the id,
/// public address, generation, and timestamps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NewNatGateway {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub family: AddressFamily,
}

/// Stored form of [`NatGateway`]. Kept separate from the wire view so
/// the realization roll-up is never persisted and cannot drift.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct NatGatewayRecord {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub project_id: Uuid,
    pub vpc_id: Uuid,
    pub name: String,
    pub description: String,
    pub family: AddressFamily,
    pub public_address: IpAddr,
    #[serde(default)]
    pub edge_cluster_id: Option<Uuid>,
    pub desired_generation: u64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl NatGatewayRecord {
    #[must_use]
    pub(crate) fn resource_id(&self) -> NetworkResourceId {
        NetworkResourceId::NatGateway { id: self.id }
    }

    #[must_use]
    pub(crate) fn into_view(self, rows: Vec<Realization>) -> NatGateway {
        let realized = RealizedNetworkState::from_rows(self.desired_generation, rows);
        NatGateway {
            id: self.id,
            tenant_id: self.tenant_id,
            project_id: self.project_id,
            vpc_id: self.vpc_id,
            name: self.name,
            description: self.description,
            family: self.family,
            public_address: self.public_address,
            edge_cluster_id: self.edge_cluster_id,
            desired_generation: self.desired_generation,
            realized,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

/// Kind of dataplane responsibility an [`EdgeCluster`] owns. v1 uses
/// one `NatGateway` cluster per NAT gateway; the enum keeps the
/// durable shape open for floating-IP decap and shared edge fleets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum EdgeClusterKind {
    NatGateway,
    FloatingIpDecap,
    Shared,
}

impl EdgeClusterKind {
    /// Whether a cluster of this kind may bind to `resource`.
    #[must_use]
    pub fn accepts_resource(self, resource: EdgeClusterResource) -> bool {
        match self {
            EdgeClusterKind::NatGateway => {
                matches!(resource, EdgeClusterResource::NatGateway { .. })
            }
            EdgeClusterKind::FloatingIpDecap => {
                matches!(resource, EdgeClusterResource::FloatingIp { .. })
            }
            EdgeClusterKind::Shared => true,
        }
    }
}

/// Resource whose edge dataplane is owned by an [`EdgeCluster`].
/// Kept narrower than [`NetworkResourceId`] so only edge-placeable
/// intent records can be bound to an edge cluster.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum EdgeClusterResource {
    NatGateway { nat_gateway_id: Uuid },
    FloatingIp { floating_ip_id: Uuid },
}

impl EdgeClusterResource {
    #[must_use]
    pub fn kind_tag(self) -> &'static str {
        match self {
            EdgeClusterResource::NatGateway { .. } => "nat_gateway",
            EdgeClusterResource::FloatingIp { .. } => "floating_ip",
        }
    }

    #[must_use]
    pub fn id(self) -> Uuid {
        match self {
            EdgeClusterResource::NatGateway { nat_gateway_id } => nat_gateway_id,
            EdgeClusterResource::FloatingIp { floating_ip_id } => floating_ip_id,
        }
    }
}

/// Host-side NIC coordinate assigned to an edge instance by the
/// placer/materializer. v1 store records carry this as durable
/// placement data; the fhrun/firehyve manifest renderer translates it
/// into runtime-specific NIC fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EdgeNicCoord {
    pub nic_tag: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mac: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ip: Option<IpAddr>,
}

/// Realization state of one edge instance from tritond's
/// perspective. Agent reports still flow through [`Realization`];
/// this field is the placement/materializer lifecycle for the
/// instance record itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum EdgeClusterInstanceState {
    Pending,
    Starting,
    Running,
    Stopping,
    Stopped,
    Failed,
}

/// One concrete firehyve/fhrun edge instance selected for an
/// [`EdgeCluster`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EdgeClusterInstance {
    pub id: Uuid,
    pub cn_id: Uuid,
    pub fhrun_manifest_uri: String,
    pub north_nic: EdgeNicCoord,
    pub south_nic: EdgeNicCoord,
    pub control_socket: String,
    pub state: EdgeClusterInstanceState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Durable edge dataplane group. v1 creates one cluster per NAT
/// gateway and stores zero or more instances under the cluster so the
/// later placer can add HA without changing the parent record shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EdgeCluster {
    pub id: Uuid,
    pub name: String,
    pub kind: EdgeClusterKind,
    #[serde(default)]
    pub bound_resources: Vec<EdgeClusterResource>,
    #[serde(default)]
    pub instances: Vec<EdgeClusterInstance>,
    pub desired_generation: u64,
    pub realized: RealizedNetworkState,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Request body for creating an [`EdgeCluster`]. v1 callers create a
/// system-owned cluster and bind it to a `NatGateway`; placement and
/// instance membership land in a follow-up slice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NewEdgeCluster {
    pub name: String,
    pub kind: EdgeClusterKind,
    #[serde(default)]
    pub bound_resources: Vec<EdgeClusterResource>,
    #[serde(default)]
    pub instances: Vec<EdgeClusterInstance>,
}

/// Stored form of [`EdgeCluster`]. As with [`NatGatewayRecord`], the
/// realized view is computed from realization rows rather than
/// persisted on the record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct EdgeClusterRecord {
    pub id: Uuid,
    pub name: String,
    pub kind: EdgeClusterKind,
    pub bound_resources: Vec<EdgeClusterResource>,
    pub instances: Vec<EdgeClusterInstance>,
    pub desired_generation: u64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl EdgeClusterRecord {
    #[must_use]
    pub(crate) fn resource_id(&self) -> NetworkResourceId {
        NetworkResourceId::EdgeCluster { id: self.id }
    }

    #[must_use]
    pub(crate) fn into_view(self, rows: Vec<Realization>) -> EdgeCluster {
        let realized = RealizedNetworkState::from_rows(self.desired_generation, rows);
        EdgeCluster {
            id: self.id,
            name: self.name,
            kind: self.kind,
            bound_resources: self.bound_resources,
            instances: self.instances,
            desired_generation: self.desired_generation,
            realized,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

/// Validate a candidate subnet's CIDRs against its parent VPC and the
/// peer subnets already in the same VPC. Shared by both store
/// backends (`MemStore` and `FdbStore`) so the invariants stay in
/// lockstep.
///
/// Returns the conflict-message string on violation. The caller is
/// expected to have already validated that at least one of
/// `ipv4_block` / `ipv6_block` is `Some` — that's an API-edge concern,
/// not a per-backend one.
pub(crate) fn validate_subnet_cidrs(
    vpc: &Vpc,
    ipv4_block: Option<Ipv4Network>,
    ipv6_block: Option<Ipv6Network>,
    peers: &[Subnet],
) -> Result<(), String> {
    if let Some(v4) = ipv4_block {
        let parent = vpc.ipv4_block.ok_or_else(|| {
            format!(
                "subnet has ipv4_block but parent vpc {} has no IPv4 plan",
                vpc.id
            )
        })?;
        if !v4.is_subnet_of(parent) {
            return Err(format!(
                "subnet ipv4_block {v4} is not contained in vpc ipv4_block {parent}"
            ));
        }
    }
    if let Some(v6) = ipv6_block {
        let parent = vpc.ipv6_block.ok_or_else(|| {
            format!(
                "subnet has ipv6_block but parent vpc {} has no IPv6 plan",
                vpc.id
            )
        })?;
        if !v6.is_subnet_of(parent) {
            return Err(format!(
                "subnet ipv6_block {v6} is not contained in vpc ipv6_block {parent}"
            ));
        }
    }
    for peer in peers {
        if let (Some(v4), Some(peer_v4)) = (ipv4_block, peer.ipv4_block)
            && v4.overlaps(peer_v4)
        {
            return Err(format!(
                "subnet ipv4_block {v4} overlaps existing subnet {} ipv4_block {peer_v4}",
                peer.id
            ));
        }
        if let (Some(v6), Some(peer_v6)) = (ipv6_block, peer.ipv6_block)
            && v6.overlaps(peer_v6)
        {
            return Err(format!(
                "subnet ipv6_block {v6} overlaps existing subnet {} ipv6_block {peer_v6}",
                peer.id
            ));
        }
    }
    Ok(())
}

#[must_use]
pub(crate) fn canonical_ip_network(destination: IpNetwork) -> IpNetwork {
    match destination {
        IpNetwork::V4(v4) => Ipv4Network::new(v4.network(), v4.prefix())
            .map(IpNetwork::V4)
            .unwrap_or(destination),
        IpNetwork::V6(v6) => Ipv6Network::new(v6.network(), v6.prefix())
            .map(IpNetwork::V6)
            .unwrap_or(destination),
    }
}

#[must_use]
pub(crate) fn route_destination_family_present(vpc: &Vpc, destination: IpNetwork) -> bool {
    match destination {
        IpNetwork::V4(_) => vpc.ipv4_block.is_some(),
        IpNetwork::V6(_) => vpc.ipv6_block.is_some(),
    }
}

impl From<IdpConfig> for IdpConfigView {
    fn from(config: IdpConfig) -> Self {
        IdpConfigView {
            issuer_url: config.issuer_url,
            client_id: config.client_id,
            audience: config.audience,
        }
    }
}

/// Wire-safe view of a [`User`]: same identity, no credential material.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct UserView {
    pub id: Uuid,
    pub username: String,
    pub is_root: bool,
    pub created_at: DateTime<Utc>,
}

impl From<User> for UserView {
    fn from(user: User) -> Self {
        UserView {
            id: user.id,
            username: user.username,
            is_root: user.is_root,
            created_at: user.created_at,
        }
    }
}

/// Permission scope attached to an [`ApiKey`]. Determines which
/// Cedar actions the key may authorise *beyond* what its owning
/// user could do natively. The scope check runs at the auth layer,
/// before Cedar — see `services/tritond/src/auth.rs`. Mapping from
/// scope to allowed actions is exhaustive there so the compiler
/// flags any new [`crate::types`] action that hasn't been classified.
///
/// Records persisted before this field existed deserialise as
/// [`ApiKeyScope::Full`] thanks to `#[serde(default)]` on
/// [`ApiKey::scope`]; this preserves the pre-scope behaviour where
/// every minted key had the full permissions of the owning user.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ApiKeyScope {
    /// Full access — equivalent to authenticating as the owning user.
    /// Default when no scope is specified.
    #[default]
    Full,
    /// List/get on every resource, plus audit chain reads. Cannot
    /// mutate state, mint or delete API keys, or change IdP config.
    /// Useful for monitoring agents and read replicas.
    ReadOnly,
    /// Audit chain reads only (`audit_list`, `audit_fetch`,
    /// `audit_verify`). Useful for compliance pipelines that should
    /// see "who did what when" but never the resources themselves.
    AuditOnly,
    /// Provisioning-agent scope: `agent_claim` and `agent_complete`
    /// only. A key with this scope cannot read tenant resources or
    /// audit events; it can only pull jobs from the queue and
    /// report outcomes. Used by the per-CN `tritonagent` to
    /// authenticate to tritond's `/v2/agent/*` surface.
    Agent,
}

/// API key record. Storage carries the bcrypt hash of the secret
/// segment plus the non-secret `lookup_id` used to find the record
/// in O(1). Plaintext is shown to the operator exactly once at
/// creation time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiKey {
    pub id: Uuid,
    /// User this key authenticates as.
    pub user_id: Uuid,
    /// Operator-supplied free-text label (e.g. "ci-pipeline").
    pub description: String,
    /// Non-secret 12-character lookup identifier — the prefix half of
    /// the wire-form secret. Indexed by `Store` for O(1) lookup.
    pub lookup_id: String,
    /// Bcrypt hash of the secret segment of the wire-form key.
    pub hash: String,
    /// Permission scope. Records written before this field existed
    /// deserialise as [`ApiKeyScope::Full`].
    #[serde(default)]
    pub scope: ApiKeyScope,
    /// When set, this key is bound to a specific compute node and the
    /// auth layer rejects any request whose `claimed_by` (or future
    /// per-CN selector) doesn't match. Minted by the CN approval
    /// flow; manually-minted operator keys leave this `None`.
    #[serde(default)]
    pub bound_to_cn: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

/// Wire-safe view of an [`ApiKey`]: identifying metadata, no hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ApiKeyView {
    pub id: Uuid,
    pub user_id: Uuid,
    pub description: String,
    pub scope: ApiKeyScope,
    #[serde(default)]
    pub bound_to_cn: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

impl From<ApiKey> for ApiKeyView {
    fn from(key: ApiKey) -> Self {
        ApiKeyView {
            id: key.id,
            user_id: key.user_id,
            description: key.description,
            scope: key.scope,
            bound_to_cn: key.bound_to_cn,
            created_at: key.created_at,
        }
    }
}

/// User-supplied SSH public key, registered into one of five
/// possible scopes (see [`SshKeyScope`]) so instance launches can
/// pick keys to inject into authorized_keys. The User scope is the
/// load-bearing one in practice (every user has their own personal
/// keys); the other scopes mirror [`Image`] for symmetry and to
/// support shared deployment keys.
///
/// The server validates the openssh wire format at create time and
/// computes the SHA-256 fingerprint. The raw `public_key` string is
/// stored verbatim so it can be handed to cloud-init without
/// reformatting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SshKey {
    pub id: Uuid,
    /// Visibility scope. The variant carries the parent identity
    /// (silo_id / tenant_id / project_id / user_id) for the
    /// non-Public scopes; visibility checks resolve up the
    /// project → tenant → silo chain when needed.
    pub scope: SshKeyScope,
    pub name: String,
    pub description: String,
    /// OpenSSH-formatted public key — `<algo> <base64> [comment]`.
    /// Server-validated at create time; rejected with 400 if the
    /// `ssh-key` crate refuses to parse it.
    pub public_key: String,
    /// SHA-256 fingerprint, e.g. `SHA256:abcd...`. Server-computed
    /// at create time and stored alongside the key for cheap
    /// display / lookup. Never accepted on the wire.
    pub fingerprint: String,
    pub created_at: DateTime<Utc>,
}

/// Request body for registering a new SSH key in any scope's
/// catalog. The owning scope (Public / Silo / Tenant / Project /
/// User) is inferred from the URL path the request hit, *not*
/// from the body. The server assigns `id`, `fingerprint`, and
/// `created_at`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NewSshKey {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub public_key: String,
}

/// Visibility scope of an [`SshKey`]. Same shape as
/// [`ImageScope`] — see Slice F. User scope is the most common
/// in practice (every user owns their own personal keys); Public
/// is for operator-distributed emergency-access keys; Silo /
/// Tenant / Project are for shared deployment keys.
///
/// The variant carries everything the visibility predicate needs
/// — there are no denormalised silo_id / tenant_id fields on
/// [`SshKey`]. For `Project`, the resolver looks up the project
/// to derive its tenant + silo when needed; for `Tenant`, the
/// resolver looks up the tenant for its silo when needed. Cold
/// path; correctness > one extra read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum SshKeyScope {
    Public,
    Silo { silo_id: Uuid },
    Tenant { tenant_id: Uuid },
    Project { project_id: Uuid },
    User { user_id: Uuid },
}

impl SshKeyScope {
    /// "Namespace key" used by [`derive_ssh_key_id`] so the same
    /// fingerprint in different scopes produces different ssh-key
    /// ids (no cross-scope collisions). Returns:
    /// * `Uuid::nil()` for `Public`.
    /// * `silo_id` for `Silo`.
    /// * `tenant_id` for `Tenant`.
    /// * `project_id` for `Project`.
    /// * `user_id` for `User`.
    #[must_use]
    pub fn namespace_id(&self) -> Uuid {
        match self {
            SshKeyScope::Public => Uuid::nil(),
            SshKeyScope::Silo { silo_id } => *silo_id,
            SshKeyScope::Tenant { tenant_id } => *tenant_id,
            SshKeyScope::Project { project_id } => *project_id,
            SshKeyScope::User { user_id } => *user_id,
        }
    }

    /// Stable short tag used as a discriminator inside
    /// [`derive_ssh_key_id`] so two scopes whose namespace UUIDs
    /// happen to collide (vanishingly unlikely, but possible)
    /// still produce distinct ssh-key ids.
    #[must_use]
    pub fn namespace_tag(&self) -> &'static str {
        match self {
            SshKeyScope::Public => "public",
            SshKeyScope::Silo { .. } => "silo",
            SshKeyScope::Tenant { .. } => "tenant",
            SshKeyScope::Project { .. } => "project",
            SshKeyScope::User { .. } => "user",
        }
    }
}

/// Visibility scope of an [`Image`]. The variant determines who
/// can see and use the image: `Public` is everyone (including
/// anonymous probes on the public listing endpoint); `Silo` is
/// every member of any tenant under that silo; `Tenant` is
/// every member of that tenant; `Project` is every tenant
/// member with project access (Phase 0: every tenant member);
/// `User` is one specific user.
///
/// The variant carries everything the visibility predicate
/// needs — there are no denormalised silo_id / tenant_id
/// fields on `Image`. For `Project`, the resolver looks up the
/// project to derive its tenant + silo when needed; for
/// `Tenant`, the resolver looks up the tenant for its silo
/// when needed. Cold path; correctness > one extra read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ImageScope {
    Public,
    Silo { silo_id: Uuid },
    Tenant { tenant_id: Uuid },
    Project { project_id: Uuid },
    User { user_id: Uuid },
}

impl ImageScope {
    /// "Namespace key" used by [`derive_image_id`] so the same
    /// content sha256 in different scopes produces different
    /// image ids (no cross-scope collisions). Returns:
    /// * `Uuid::nil()` for `Public`.
    /// * `silo_id` for `Silo`.
    /// * `tenant_id` for `Tenant`.
    /// * `project_id` for `Project`.
    /// * `user_id` for `User`.
    #[must_use]
    pub fn namespace_id(&self) -> Uuid {
        match self {
            ImageScope::Public => Uuid::nil(),
            ImageScope::Silo { silo_id } => *silo_id,
            ImageScope::Tenant { tenant_id } => *tenant_id,
            ImageScope::Project { project_id } => *project_id,
            ImageScope::User { user_id } => *user_id,
        }
    }

    /// Stable short tag used as a discriminator inside
    /// [`derive_image_id`] so two scopes whose namespace UUIDs
    /// happen to collide (vanishingly unlikely, but possible)
    /// still produce distinct image ids.
    #[must_use]
    pub fn namespace_tag(&self) -> &'static str {
        match self {
            ImageScope::Public => "public",
            ImageScope::Silo { .. } => "silo",
            ImageScope::Tenant { .. } => "tenant",
            ImageScope::Project { .. } => "project",
            ImageScope::User { .. } => "user",
        }
    }
}

/// Image catalog entry. Multi-scope as of Slice F: see
/// [`ImageScope`] for the variants. Phase 0 ships only the
/// metadata record — image content lives in mantafs / object
/// storage and is not modelled here. Operators register images
/// by URL + sha256 and trust the caller for the content match;
/// an eventual import pipeline will pre-stage content in storage
/// and verify the digest before the record is persisted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Image {
    pub id: Uuid,
    /// Visibility scope. The variant carries the parent identity
    /// (silo_id / tenant_id / project_id / user_id) for the
    /// non-Public scopes; visibility checks resolve up the
    /// project → tenant → silo chain when needed.
    pub scope: ImageScope,
    pub name: String,
    pub description: String,
    /// OS family identifier (e.g. `linux`, `windows`, `smartos`).
    /// Stringly-typed in Phase 0; will tighten to an enum once the
    /// instance brand model lands.
    pub os: String,
    /// OS version / distro tag (e.g. `ubuntu-22.04`,
    /// `windows-server-2022`). Free-form for Phase 0.
    pub version: String,
    /// Total content size, in bytes.
    pub size_bytes: u64,
    /// Lowercase hex SHA-256 of the image content. Server-validated
    /// for length (64 chars) and charset at create time.
    pub sha256: String,
    /// Optional URL where the image content can be fetched. `None`
    /// means the content is registered out-of-band (e.g. already in
    /// mantafs at a known path resolved by image_id).
    pub source_url: Option<String>,
    /// Optional host-compatibility constraints, populated when
    /// the image was registered via the bundle path (image-create
    /// with `bundle_url`). When `Some`, the per-CN agent
    /// rejects a Provision before vmadm if the instance brand
    /// or host platform fails the constraints. `None` skips the
    /// gate (legacy / explicit-fields image-create path); a
    /// future slice migrates every Image record to carry
    /// compatibility metadata.
    #[serde(default)]
    pub compatibility: Option<ImageCompatibility>,
    pub created_at: DateTime<Utc>,
}

/// Host-compatibility constraints on an [`Image`]. Mirrors
/// `tritond_image_manifest::Compatibility` exactly — the bundle
/// ingest path copies the manifest's compatibility block in
/// without translation. The per-CN agent enforces these gates
/// at provision time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ImageCompatibility {
    /// SmartOS brand the image is built for (e.g.
    /// `joyent-minimal`, `lx`). Compared against the
    /// instance's requested brand.
    pub brand: String,
    /// CPU architecture (e.g. `x86_64`).
    pub arch: String,
    /// SmartOS platform buildstamp (`YYYYMMDDTHHMMSSZ`); the
    /// host's platform buildstamp must be lexicographically
    /// `>=` this value. `None` means "any platform."
    #[serde(default)]
    pub min_smartos_platform: Option<String>,
}

/// M1 default boot disk size for bhyve instances.
///
/// Triton image `size_bytes` tracks the imported image content, which
/// can be smaller than the host-side zvol's used/reserved size. Until
/// the package/SKU model lets callers request an explicit disk size,
/// bhyve boot disks need a real VM-sized floor so SmartOS does not try
/// to shrink an imported image clone during `vmadm create`.
pub const BHYVE_M1_MIN_BOOT_DISK_BYTES: u64 = 20 * 1024 * 1024 * 1024;

/// Default the auto-created boot disk size for an instance image.
pub fn default_boot_disk_size_bytes(image: &Image) -> u64 {
    if image
        .compatibility
        .as_ref()
        .is_some_and(|compat| compat.brand == "bhyve")
    {
        image.size_bytes.max(BHYVE_M1_MIN_BOOT_DISK_BYTES)
    } else {
        image.size_bytes
    }
}

/// Request body for registering an image in any scope's catalog.
/// The owning scope (Public / Silo / Tenant / Project / User) is
/// inferred from the URL path the request hit, *not* from the
/// body. The server assigns `id` and `created_at`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NewImage {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub os: String,
    pub version: String,
    pub size_bytes: u64,
    pub sha256: String,
    #[serde(default)]
    pub source_url: Option<String>,
    /// Optional UUID to pin for the new image. When `None` (the
    /// usual case), the server derives the UUID deterministically
    /// from `(scope, sha256)` via [`derive_image_id`] — same
    /// content in the same scope always yields the same id across
    /// hosts and replays, so the per-CN agent's content-addressed
    /// ZFS dataset (`zones/<image_id>`) collapses identical bytes
    /// into one import. Different scopes registering the same
    /// content yield distinct ids (no cross-scope collisions).
    /// `Some(...)` is only useful for cross-cluster mirroring
    /// scenarios where the operator wants tritond's id to match a
    /// UUID minted elsewhere; the store rejects the create with
    /// [`StoreError::Conflict`] if the id is already in use.
    #[serde(default)]
    pub id: Option<Uuid>,
    /// Optional host-compatibility constraints. Populated by
    /// the server when an image is registered via the bundle
    /// ingest path; absent when the operator passed individual
    /// fields by hand. The per-CN agent enforces these when
    /// `Some`.
    #[serde(default)]
    pub compatibility: Option<ImageCompatibility>,
}

/// Stable namespace for [`derive_image_id`]. Picked once on
/// 2026-05-02; **never change this value** — it would
/// retroactively re-key every persisted image. Generated via
/// `python3 -c 'import uuid; print(uuid.uuid4())'`.
pub const TRITOND_IMAGE_NAMESPACE: Uuid = Uuid::from_bytes([
    0xb5, 0xb5, 0x0a, 0x2c, 0xf0, 0x6c, 0x49, 0x09, 0x94, 0x52, 0x11, 0xe2, 0xef, 0xd7, 0xcd, 0x67,
]);

/// Derive an image UUID from its owning scope + content sha256.
///
/// Uses UUID v5 (SHA-1-based) over a fixed tritond namespace so
/// the mapping `(scope, sha256) → uuid` is stable across hosts
/// and cluster replays. The same image content registered in
/// the same scope under two different names yields the same id,
/// which makes per-CN `zones/<image_id>` content-addressed
/// storage work without a separate lookup table.
///
/// **Why scope-keyed and not content-only.** Image records
/// carry `scope` as part of their identity (same name in two
/// different silos / tenants / projects / users must coexist as
/// separate records). The scope's `namespace_tag()` and
/// `namespace_id()` are folded into the v5 input so two scopes
/// can never produce the same id even if their parent UUIDs
/// somehow collide.
///
/// `sha256` is expected to be lowercase hex; case is normalised
/// here so trivial input differences don't desync the mapping.
#[must_use]
pub fn derive_image_id(scope: &ImageScope, sha256: &str) -> Uuid {
    let normalised = sha256.to_ascii_lowercase();
    let tag = scope.namespace_tag();
    let ns_id = scope.namespace_id();
    let mut input = Vec::with_capacity(tag.len() + 1 + 16 + 1 + normalised.len());
    input.extend_from_slice(tag.as_bytes());
    input.push(b':');
    input.extend_from_slice(ns_id.as_bytes());
    input.push(b':');
    input.extend_from_slice(normalised.as_bytes());
    Uuid::new_v5(&TRITOND_IMAGE_NAMESPACE, &input)
}

#[cfg(test)]
mod derive_image_id_tests {
    use super::*;

    fn fixture_silo_scope() -> ImageScope {
        ImageScope::Silo {
            silo_id: Uuid::from_bytes([0xab; 16]),
        }
    }

    #[test]
    fn deterministic_for_same_inputs() {
        let s = fixture_silo_scope();
        assert_eq!(
            derive_image_id(&s, "abc123"),
            derive_image_id(&s, "abc123"),
            "same (scope, sha256) must yield same UUID",
        );
    }

    #[test]
    fn case_insensitive_on_sha256() {
        let s = fixture_silo_scope();
        assert_eq!(
            derive_image_id(&s, "ABCDEF1234"),
            derive_image_id(&s, "abcdef1234"),
            "case differences must not desync the mapping",
        );
    }

    #[test]
    fn different_content_yields_different_id() {
        let s = fixture_silo_scope();
        assert_ne!(
            derive_image_id(&s, &"a".repeat(64)),
            derive_image_id(&s, &"b".repeat(64)),
            "distinct sha256 inputs must not collide",
        );
    }

    #[test]
    fn same_content_in_different_silos_does_not_collide() {
        let a = ImageScope::Silo {
            silo_id: Uuid::from_bytes([0x01; 16]),
        };
        let b = ImageScope::Silo {
            silo_id: Uuid::from_bytes([0x02; 16]),
        };
        assert_ne!(
            derive_image_id(&a, "abc123"),
            derive_image_id(&b, "abc123"),
            "same content in different silos must yield distinct ids",
        );
    }

    #[test]
    fn same_content_in_different_scope_kinds_does_not_collide() {
        // The scope tag prefix in the v5 input means two scopes
        // whose namespace_ids happen to match still produce
        // distinct image ids.
        let id = Uuid::from_bytes([0x07; 16]);
        let silo = ImageScope::Silo { silo_id: id };
        let tenant = ImageScope::Tenant { tenant_id: id };
        let project = ImageScope::Project { project_id: id };
        let user = ImageScope::User { user_id: id };
        let public = ImageScope::Public;
        let ids = [
            derive_image_id(&silo, "abc"),
            derive_image_id(&tenant, "abc"),
            derive_image_id(&project, "abc"),
            derive_image_id(&user, "abc"),
            derive_image_id(&public, "abc"),
        ];
        for i in 0..ids.len() {
            for j in (i + 1)..ids.len() {
                assert_ne!(
                    ids[i], ids[j],
                    "scope variants {i} and {j} must yield distinct image ids",
                );
            }
        }
    }

    #[test]
    fn namespace_pinned() {
        // Locks the namespace constant: regenerating the namespace
        // would re-key every persisted image, so a future change
        // here is a wire break and must be deliberate.
        assert_eq!(
            TRITOND_IMAGE_NAMESPACE.to_string(),
            "b5b50a2c-f06c-4909-9452-11e2efd7cd67",
        );
    }
}

/// Stable namespace for [`derive_ssh_key_id`]. Picked once on
/// 2026-05-03; **never change this value** — it would
/// retroactively re-key every persisted SSH key. Generated via
/// `python3 -c 'import uuid; print(uuid.uuid4())'`.
pub const TRITOND_SSH_KEY_NAMESPACE: Uuid = Uuid::from_bytes([
    0x6a, 0x4f, 0x9e, 0x12, 0x7d, 0x3b, 0x4e, 0x55, 0x90, 0x21, 0x4d, 0x82, 0x1f, 0x6c, 0xa1, 0x09,
]);

/// Derive an SSH-key UUID from its owning scope + openssh
/// fingerprint.
///
/// Uses UUID v5 (SHA-1-based) over a fixed tritond namespace so
/// the mapping `(scope, fingerprint) → uuid` is stable across
/// hosts and cluster replays. The same key registered in the
/// same scope under two different names yields the same id,
/// which makes idempotent re-create safe.
///
/// **Why scope-keyed and not fingerprint-only.** SSH-key records
/// carry `scope` as part of their identity (the same fingerprint
/// in two different silos / tenants / projects / users must
/// coexist as separate records). The scope's `namespace_tag()`
/// and `namespace_id()` are folded into the v5 input so two
/// scopes can never produce the same id even if their parent
/// UUIDs somehow collide.
///
/// `fingerprint` is folded in verbatim — it's already a stable,
/// canonical SHA-256 string produced by the `ssh-key` crate's
/// `PublicKey::fingerprint(...)`, so no normalisation is needed.
#[must_use]
pub fn derive_ssh_key_id(scope: &SshKeyScope, fingerprint: &str) -> Uuid {
    let tag = scope.namespace_tag();
    let ns_id = scope.namespace_id();
    let mut input = Vec::with_capacity(tag.len() + 1 + 16 + 1 + fingerprint.len());
    input.extend_from_slice(tag.as_bytes());
    input.push(b':');
    input.extend_from_slice(ns_id.as_bytes());
    input.push(b':');
    input.extend_from_slice(fingerprint.as_bytes());
    Uuid::new_v5(&TRITOND_SSH_KEY_NAMESPACE, &input)
}

#[cfg(test)]
mod derive_ssh_key_id_tests {
    use super::*;

    fn fixture_silo_scope() -> SshKeyScope {
        SshKeyScope::Silo {
            silo_id: Uuid::from_bytes([0xab; 16]),
        }
    }

    #[test]
    fn deterministic_for_same_inputs() {
        let s = fixture_silo_scope();
        assert_eq!(
            derive_ssh_key_id(&s, "SHA256:abc123"),
            derive_ssh_key_id(&s, "SHA256:abc123"),
            "same (scope, fingerprint) must yield same UUID",
        );
    }

    #[test]
    fn different_fingerprint_yields_different_id() {
        let s = fixture_silo_scope();
        assert_ne!(
            derive_ssh_key_id(&s, "SHA256:aaaa"),
            derive_ssh_key_id(&s, "SHA256:bbbb"),
            "distinct fingerprints must not collide",
        );
    }

    #[test]
    fn same_fingerprint_in_different_silos_does_not_collide() {
        let a = SshKeyScope::Silo {
            silo_id: Uuid::from_bytes([0x01; 16]),
        };
        let b = SshKeyScope::Silo {
            silo_id: Uuid::from_bytes([0x02; 16]),
        };
        assert_ne!(
            derive_ssh_key_id(&a, "SHA256:abc"),
            derive_ssh_key_id(&b, "SHA256:abc"),
            "same fingerprint in different silos must yield distinct ids",
        );
    }

    #[test]
    fn same_fingerprint_in_different_scope_kinds_does_not_collide() {
        let id = Uuid::from_bytes([0x07; 16]);
        let silo = SshKeyScope::Silo { silo_id: id };
        let tenant = SshKeyScope::Tenant { tenant_id: id };
        let project = SshKeyScope::Project { project_id: id };
        let user = SshKeyScope::User { user_id: id };
        let public = SshKeyScope::Public;
        let ids = [
            derive_ssh_key_id(&silo, "SHA256:x"),
            derive_ssh_key_id(&tenant, "SHA256:x"),
            derive_ssh_key_id(&project, "SHA256:x"),
            derive_ssh_key_id(&user, "SHA256:x"),
            derive_ssh_key_id(&public, "SHA256:x"),
        ];
        for i in 0..ids.len() {
            for j in (i + 1)..ids.len() {
                assert_ne!(
                    ids[i], ids[j],
                    "scope variants {i} and {j} must yield distinct ssh-key ids",
                );
            }
        }
    }

    #[test]
    fn namespace_pinned() {
        // Locks the namespace constant: regenerating the namespace
        // would re-key every persisted SSH key, so a future change
        // here is a wire break and must be deliberate.
        assert_eq!(
            TRITOND_SSH_KEY_NAMESPACE.to_string(),
            "6a4f9e12-7d3b-4e55-9021-4d821f6ca109",
        );
    }
}

/// Per-project resource quota. Singleton: each project has at most
/// one quota record. Set with PUT, read with GET, remove with DELETE
/// (project becomes "unlimited" — no record means no enforcement).
///
/// Enforcement is *not* shipped in Phase 0 — these limits are
/// stored and readable but no instance-create flow consults them
/// yet. The shape is fixed now so the enforcement layer (Tier 3)
/// has a stable contract to build against.
///
/// Limits are absolute caps, not reservations: `cpu_limit = 8`
/// means "this project may have up to 8 vCPUs across all running
/// instances." Storage and memory are bytes; cpu and instance
/// counts are simple `u32`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Quota {
    pub tenant_id: Uuid,
    pub project_id: Uuid,
    /// Maximum total vCPUs across all instances in the project.
    pub cpu_limit: u32,
    /// Maximum total memory across all instances in the project,
    /// in bytes.
    pub memory_bytes: u64,
    /// Maximum total disk across all instances + volumes in the
    /// project, in bytes.
    pub disk_bytes: u64,
    /// Maximum number of running instances in the project.
    pub instance_limit: u32,
    pub updated_at: DateTime<Utc>,
}

/// Request body for setting a project's quota. The owning tenant
/// and project come from the URL path. The server assigns `updated_at`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NewQuota {
    pub cpu_limit: u32,
    pub memory_bytes: u64,
    pub disk_bytes: u64,
    pub instance_limit: u32,
}

/// Lifecycle state of a tenant instance. The state machine:
///
/// ```text
///   create
///      ↓
///   Pending ──→ Provisioning ──┬→ Running ←──┬── Stopped
///                              │             │      ↑
///                              │             │      │
///                              ↓             ↓      │
///                            Failed        Stopping─┘
/// ```
///
/// Phase 0 collapses Pending → Provisioning → Running into a
/// synchronous transition inside the create handler (there is no
/// agent yet). The full async path lands when the provisioning
/// intent queue + stub executor slice ships.
///
/// Operator-driven transitions: start (Stopped → Pending), stop
/// (Running → Stopping), restart (Running → Stopping → Pending).
/// Agent-driven transitions: Pending → Provisioning → Running,
/// Provisioning → Failed, Stopping → Stopped.
///
/// Delete is allowed only from Stopped or Failed; deleting a
/// Running instance returns 409.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
#[non_exhaustive]
pub enum LifecycleState {
    /// Newly created; not yet picked up by an agent.
    Pending,
    /// An agent has claimed the provisioning job and is working.
    Provisioning,
    /// Up and running.
    Running,
    /// Stop requested; agent is winding down.
    Stopping,
    /// Fully stopped; safe to delete or restart.
    Stopped,
    /// Unrecoverable error. Inspect `reason`; the instance must be
    /// deleted (no in-place recovery in Phase 0).
    Failed { reason: String },
}

impl LifecycleState {
    /// True if a `delete` request should be accepted from this
    /// state. Phase 0 requires the instance to be fully terminal
    /// (Stopped or Failed); a future slice may add a force-delete
    /// path that drives Running → Stopping → Stopped → deleted in
    /// one operator-visible step.
    #[must_use]
    pub fn is_deletable(&self) -> bool {
        matches!(
            self,
            LifecycleState::Stopped | LifecycleState::Failed { .. }
        )
    }

    /// Discriminant-only view of the state; useful for CAS
    /// transitions where the caller doesn't want to construct a
    /// fake `Failed { reason: "" }` just to match.
    #[must_use]
    pub fn kind(&self) -> LifecycleStateKind {
        match self {
            LifecycleState::Pending => LifecycleStateKind::Pending,
            LifecycleState::Provisioning => LifecycleStateKind::Provisioning,
            LifecycleState::Running => LifecycleStateKind::Running,
            LifecycleState::Stopping => LifecycleStateKind::Stopping,
            LifecycleState::Stopped => LifecycleStateKind::Stopped,
            LifecycleState::Failed { .. } => LifecycleStateKind::Failed,
        }
    }
}

/// Discriminant-only companion to [`LifecycleState`]. Used by
/// [`crate::Store::transition_instance_lifecycle`]'s
/// `expected_from` parameter so callers can name "any Failed state"
/// without committing to a specific `reason` string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum LifecycleStateKind {
    Pending,
    Provisioning,
    Running,
    Stopping,
    Stopped,
    Failed,
}

/// Tenant compute instance. Project-scoped; references one image
/// (boot media), one subnet (network attach point), and zero-or-more
/// SSH keys (injected into authorized_keys at provisioning time).
///
/// Phase 0 ships only the metadata + lifecycle state machine. The
/// actual provisioning is faked synchronously inside the create
/// handler; a future slice introduces the intent queue and stub
/// executor that will become the swap-out point for a real
/// `tritonagent`.
///
/// Several fields that real cloud instances carry are deliberately
/// omitted in v0: cloud-init userdata, tags/labels, brand
/// (zone/hvm/lx/bhyve), affinity rules, console URL, migration
/// history. Each will land as the consuming use case ships. The
/// hosting CN is recorded once placement lands so subsequent
/// lifecycle jobs return to the same SmartOS host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Instance {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    pub description: String,
    /// Boot image. As of Slice F images are multi-scope; the
    /// instance-create handler enforces that the principal can
    /// see this image (via the [`crate`]-level visibility
    /// predicate). Cross-scope references that the principal
    /// cannot see surface as `NotFound`.
    pub image_id: Uuid,
    /// Subnet the instance's primary NIC attaches to. Phase 0
    /// auto-creates a NIC at provisioning time; a future slice
    /// adds explicit NIC records that operators can manage
    /// separately. Subnet must live in a VPC inside this project.
    pub primary_subnet_id: Uuid,
    /// SSH keys to inject into the instance's authorized_keys at
    /// first boot. Each key id must live in the same silo as the
    /// instance. May be empty (no key injection).
    pub ssh_key_ids: Vec<Uuid>,
    /// Number of vCPUs.
    pub cpu: u32,
    /// Memory budget in bytes.
    pub memory_bytes: u64,
    /// SmartOS compute node that owns this instance's host-side VM.
    ///
    /// `None` is retained for in-process test/stub jobs and legacy
    /// records created before tenant placement existed. Real M1
    /// deployments assign this before the first Provision job is
    /// enqueued.
    #[serde(default)]
    pub host_cn_uuid: Option<Uuid>,
    pub lifecycle: LifecycleState,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Request body for creating an instance. The owning tenant +
/// project come from the URL path. The server assigns `id`,
/// initial `lifecycle`, `created_at`, and `updated_at`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NewInstance {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub image_id: Uuid,
    pub primary_subnet_id: Uuid,
    #[serde(default)]
    pub ssh_key_ids: Vec<Uuid>,
    pub cpu: u32,
    pub memory_bytes: u64,
    /// Additional NICs beyond the primary one on
    /// `primary_subnet_id`. Each entry causes the store to
    /// allocate one NIC + IP from the named subnet at create
    /// time. The `InstanceCreateResult.nics` Vec returns all
    /// of them in declaration order (primary at index 0). The
    /// agent's vmadm payload iterates over the Vec and attaches
    /// `net0`, `net1`, …
    #[serde(default)]
    pub extra_nics: Vec<NewInstanceNic>,
}

/// One additional NIC requested at instance create time. A
/// future slice will let operators attach more after create
/// via a dedicated POST endpoint; for v0 the only way to add
/// a non-primary NIC is to declare it here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NewInstanceNic {
    /// Subnet to draw this NIC's IP from. Must live in a VPC
    /// inside the same project as the instance.
    pub subnet_id: Uuid,
    /// Operator-friendly NIC label (`primary`, `db-tier`, …).
    /// Must be unique within the instance.
    pub name: String,
}

/// What a control-plane job asks an agent to do.
///
/// Instance variants carry a target `instance_id` so an agent can
/// look up VM state and drive the lifecycle forward. Edge variants
/// carry an `edge_instance_id`; `manifest_bytes` is the exact fhrun
/// manifest payload the target CN should apply.
///
/// This enum is `#[non_exhaustive]` so adding post-v1 variants (for
/// example Migrate, Resize, or AF_XDP-specific edge work) is not a
/// breaking change for downstream matchers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum JobKind {
    /// Drive a Pending instance through Provisioning → Running.
    /// Used both for first-time create and for `start` (which
    /// transitions Stopped → Pending and then enqueues a Provision).
    Provision { instance_id: Uuid },
    /// Drive a Running instance through Stopping → Stopped.
    Stop { instance_id: Uuid },
    /// Drive a Running instance through Stopping → Pending →
    /// Provisioning → Running. The agent is responsible for the
    /// whole cycle; the operator never sees Pending in between.
    Restart { instance_id: Uuid },
    /// Best-effort `vmadm delete` follow-up, enqueued by the
    /// `instance_delete` handler *after* the tritond record is
    /// already cleared. The agent's vmadm-delete must be
    /// idempotent — on a host where the zone never existed
    /// (e.g. agent crashed before its prior Provision), the
    /// agent reports `Completed` rather than `Failed`. v0
    /// trade-off: if the agent is unreachable, the zone leaks
    /// until an operator runs `vmadm delete` manually. A future
    /// slice promotes this to a Deleting → Deleted lifecycle
    /// gated on agent ack.
    Delete { instance_id: Uuid },
    /// Apply or update a firehyve/fhrun edge instance on the
    /// target CN. The manifest is JSON bytes rendered by tritond
    /// from the EdgeCluster's desired state; the agent persists it
    /// to the host runtime path, asks fhrun/firehyve to converge, and
    /// reports realization for the carried EdgeCluster generation.
    EdgeApply {
        edge_cluster_id: Uuid,
        edge_instance_id: Uuid,
        desired_generation: u64,
        manifest_bytes: Vec<u8>,
    },
    /// Reap a firehyve/fhrun edge instance that no longer has a
    /// desired EdgeCluster binding.
    EdgeReap { edge_instance_id: Uuid },
}

impl JobKind {
    /// Extract the target VM instance id when this is an
    /// instance-lifecycle job.
    #[must_use]
    pub fn instance_id(&self) -> Option<Uuid> {
        match self {
            JobKind::Provision { instance_id }
            | JobKind::Stop { instance_id }
            | JobKind::Restart { instance_id }
            | JobKind::Delete { instance_id } => Some(*instance_id),
            JobKind::EdgeApply { .. } | JobKind::EdgeReap { .. } => None,
        }
    }

    /// Extract the target edge instance id when this is edge work.
    #[must_use]
    pub fn edge_instance_id(&self) -> Option<Uuid> {
        match self {
            JobKind::EdgeApply {
                edge_instance_id, ..
            }
            | JobKind::EdgeReap { edge_instance_id } => Some(*edge_instance_id),
            JobKind::Provision { .. }
            | JobKind::Stop { .. }
            | JobKind::Restart { .. }
            | JobKind::Delete { .. } => None,
        }
    }

    /// Stable target id used for logs and queue diagnostics.
    #[must_use]
    pub fn target_id(&self) -> Uuid {
        self.instance_id()
            .or_else(|| self.edge_instance_id())
            .expect("every JobKind variant carries a target uuid")
    }
}

#[cfg(test)]
mod job_kind_tests {
    use super::*;

    #[test]
    fn instance_id_only_applies_to_instance_jobs() {
        let instance_id = Uuid::new_v4();
        let edge_instance_id = Uuid::new_v4();

        assert_eq!(
            JobKind::Provision { instance_id }.instance_id(),
            Some(instance_id)
        );
        assert_eq!(JobKind::EdgeReap { edge_instance_id }.instance_id(), None);
    }

    #[test]
    fn edge_apply_manifest_bytes_round_trip() {
        let edge_cluster_id = Uuid::new_v4();
        let edge_instance_id = Uuid::new_v4();
        let desired_generation = 7;
        let manifest_bytes = br#"{"dataplane":{"backend":"nftables"}}"#.to_vec();
        let kind = JobKind::EdgeApply {
            edge_cluster_id,
            edge_instance_id,
            desired_generation,
            manifest_bytes: manifest_bytes.clone(),
        };

        assert_eq!(kind.instance_id(), None);
        assert_eq!(kind.edge_instance_id(), Some(edge_instance_id));
        assert_eq!(kind.target_id(), edge_instance_id);

        let json = serde_json::to_value(&kind).unwrap();
        assert_eq!(json["kind"].as_str(), Some("edge_apply"));
        assert_eq!(
            json["edge_instance_id"],
            serde_json::Value::String(edge_instance_id.to_string())
        );
        assert_eq!(
            json["edge_cluster_id"],
            serde_json::Value::String(edge_cluster_id.to_string())
        );
        assert_eq!(
            json["desired_generation"],
            serde_json::Value::Number(desired_generation.into())
        );
        assert_eq!(
            json["manifest_bytes"],
            serde_json::Value::Array(
                manifest_bytes
                    .iter()
                    .map(|byte| serde_json::Value::from(*byte))
                    .collect()
            )
        );

        let decoded: JobKind = serde_json::from_value(json).unwrap();
        assert_eq!(decoded, kind);
    }

    #[test]
    fn edge_reap_target_round_trip() {
        let edge_instance_id = Uuid::new_v4();
        let kind = JobKind::EdgeReap { edge_instance_id };

        assert_eq!(kind.instance_id(), None);
        assert_eq!(kind.edge_instance_id(), Some(edge_instance_id));
        assert_eq!(kind.target_id(), edge_instance_id);

        let json = serde_json::to_value(&kind).unwrap();
        assert_eq!(json["kind"].as_str(), Some("edge_reap"));
        assert_eq!(
            json["edge_instance_id"],
            serde_json::Value::String(edge_instance_id.to_string())
        );

        let decoded: JobKind = serde_json::from_value(json).unwrap();
        assert_eq!(decoded, kind);
    }
}

/// Lifecycle of a single provisioning job.
///
/// `Pending` → claimable by the next agent that polls. `InProgress`
/// → an agent has claimed it; the agent is responsible for driving
/// to `Completed` or `Failed`. Terminal states (`Completed`,
/// `Failed`) are not re-queued automatically — operators retry by
/// issuing the originating action again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
#[non_exhaustive]
pub enum JobStatus {
    Pending,
    InProgress,
    Completed,
    Failed { reason: String },
}

/// Discriminant-only companion to [`JobStatus`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum JobStatusKind {
    Pending,
    InProgress,
    Completed,
    Failed,
}

impl JobStatus {
    #[must_use]
    pub fn kind(&self) -> JobStatusKind {
        match self {
            JobStatus::Pending => JobStatusKind::Pending,
            JobStatus::InProgress => JobStatusKind::InProgress,
            JobStatus::Completed => JobStatusKind::Completed,
            JobStatus::Failed { .. } => JobStatusKind::Failed,
        }
    }
}

/// A unit of work for a provisioning agent. Created by the tritond
/// instance handlers; consumed by an agent (the in-process stub
/// today; a real `tritonagent` per CN in the future).
///
/// The wire shape is stable across Phase 0 and the eventual real
/// agent — the only thing that changes is *who* claims and
/// completes jobs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProvisioningJob {
    pub id: Uuid,
    pub kind: JobKind,
    pub status: JobStatus,
    /// Monotonically-increasing sequence number that determines
    /// the queue order. Older jobs (lower seq) are claimed first.
    /// Server-assigned at enqueue time.
    pub seq: u64,
    pub created_at: DateTime<Utc>,
    /// Set when the job was first claimed by an agent.
    #[serde(default)]
    pub claimed_at: Option<DateTime<Utc>>,
    /// Identifier of the agent that claimed the job. In Phase 0
    /// this is the in-process stub's name (e.g. `"stub-provisioner"`).
    #[serde(default)]
    pub claimed_by: Option<String>,
    /// Set when the job reached a terminal status (Completed or
    /// Failed).
    #[serde(default)]
    pub completed_at: Option<DateTime<Utc>>,
    /// Optional placement: when `Some(server_uuid)`, only an
    /// agent bound to that CN will claim the job. When `None`,
    /// any claimer (the in-process stub or any bound agent) can
    /// claim. Tenant instance handlers populate this from
    /// [`Instance::host_cn_uuid`] once placement has selected a
    /// SmartOS host; tests and legacy records may remain unrouted.
    #[serde(default)]
    pub target_cn_uuid: Option<Uuid>,
}

/// Request body for enqueuing a new job. Server assigns `id`,
/// `seq`, `created_at`, and starts in `JobStatus::Pending`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NewJob {
    pub kind: JobKind,
    /// Pin this job to a specific CN. See
    /// [`ProvisioningJob::target_cn_uuid`].
    #[serde(default)]
    pub target_cn_uuid: Option<Uuid>,
}

/// Outcome a worker reports when finishing a job.
///
/// Wire-stable: rides as the body of `POST /v2/agent/jobs/{id}/complete`
/// when a real `tritonagent` is reporting back. The serde tag is
/// `kind` and the case is snake_case — matches every other tagged
/// enum on the wire (e.g. [`JobStatus`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum JobOutcome {
    Completed,
    Failed { reason: String },
}

/// Per-instance network interface. Auto-created at instance create
/// time with a single "primary" NIC; multi-NIC attach/detach is a
/// future slice. Mirrors what the dataplane (OPTE per-NIC config)
/// will eventually consume — guest MAC, primary IPv4/IPv6 in the
/// subnet's CIDR.
///
/// Phase 0 stores only the per-NIC fields the agent needs to open
/// the overlay attachment for an instance:
///
/// * `mac` — guest MAC address. Locally-administered (`02:` prefix)
///   + 5 random bytes; uniqueness is rack-wide.
/// * `primary_ipv4` / `primary_ipv6` — addresses allocated from the
///   parent subnet's CIDR. Each family is `Some` only if the subnet
///   has a CIDR for that family.
///
/// External IPs, attached_subnets (other-subnet routing), and
/// firewall rules are *not* on the NIC record — they live on
/// future External-IP / Route / FirewallRule resources.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Nic {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub project_id: Uuid,
    pub instance_id: Uuid,
    pub vpc_id: Uuid,
    pub subnet_id: Uuid,
    pub name: String,
    /// Guest MAC, formatted as 6 lowercase hex bytes separated by
    /// colons (e.g. `02:1a:b3:cd:ef:42`).
    pub mac: String,
    /// Primary IPv4 from `subnet.ipv4_block`. `None` if the parent
    /// subnet is IPv6-only.
    pub primary_ipv4: Option<Ipv4Addr>,
    /// Primary IPv6 from `subnet.ipv6_block`. `None` if the parent
    /// subnet is IPv4-only.
    pub primary_ipv6: Option<Ipv6Addr>,
    pub created_at: DateTime<Utc>,
}

/// Allocate the lowest unused IPv4 address inside `cidr`, given the
/// set of `already_allocated` addresses. Skips:
///
/// * the network address (`cidr.network()`),
/// * the gateway (network + 1, conventionally `.1`),
/// * the broadcast address (`cidr.broadcast()`).
///
/// Returns `None` if the subnet is exhausted.
#[must_use]
pub fn allocate_ipv4(cidr: Ipv4Network, already_allocated: &HashSet<Ipv4Addr>) -> Option<Ipv4Addr> {
    let network = cidr.network();
    let broadcast = cidr.broadcast();
    let gateway = next_ipv4(network)?;
    for candidate in cidr.iter() {
        if candidate == network || candidate == gateway || candidate == broadcast {
            continue;
        }
        if !already_allocated.contains(&candidate) {
            return Some(candidate);
        }
    }
    None
}

/// Allocate the lowest unused IPv6 address inside `cidr`, skipping
/// the network address and the gateway (`network + 1`,
/// conventionally `::1` in `fdXX::/N` subnets).
///
/// Returns `None` if the subnet is exhausted (operationally
/// unreachable for v0 with /48 or /64 subnets and a few NICs).
#[must_use]
pub fn allocate_ipv6(cidr: Ipv6Network, already_allocated: &HashSet<Ipv6Addr>) -> Option<Ipv6Addr> {
    let network = cidr.network();
    let gateway = next_ipv6(network)?;
    // For IPv6 we don't iterate the full address space; we walk
    // from `gateway + 1` outward and stop on the first free
    // address. /64 has 2^64 addresses; we won't realistically hit
    // the end. The cidr.contains() check guards a wraparound.
    let mut candidate = next_ipv6(gateway)?;
    loop {
        if !cidr.contains(candidate) {
            return None;
        }
        if !already_allocated.contains(&candidate) {
            return Some(candidate);
        }
        candidate = next_ipv6(candidate)?;
    }
}

fn next_ipv4(ip: Ipv4Addr) -> Option<Ipv4Addr> {
    let bits = u32::from(ip);
    bits.checked_add(1).map(Ipv4Addr::from)
}

fn next_ipv6(ip: Ipv6Addr) -> Option<Ipv6Addr> {
    let bits = u128::from(ip);
    bits.checked_add(1).map(Ipv6Addr::from)
}

/// Generate a random locally-administered MAC address.
///
/// Formats as 6 lowercase hex bytes separated by colons. The first
/// byte is set to `0x02` (locally-administered, unicast — the
/// universal vendor-free prefix); the remaining 5 bytes are drawn
/// from `rng.random()` so the MAC is rack-unique with vanishing
/// collision probability for any realistic deployment.
#[must_use]
pub fn generate_mac<R: rand::Rng>(rng: &mut R) -> String {
    let mut bytes = [0u8; 6];
    rng.fill(&mut bytes[..]);
    bytes[0] = 0x02; // locally-administered, unicast
    format!(
        "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5]
    )
}

/// What a disk is for. Phase 0 ships only `Boot` disks (auto-created
/// from the instance's image at instance create time). `Data` is
/// reserved for the future multi-disk attach slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum DiskKind {
    /// Bootable; sourced from an Image. The instance boots from this
    /// disk on first start.
    Boot,
    /// Blank persistent storage attached to the instance. Reserved
    /// for the future multi-disk slice; not yet exercised.
    Data,
}

/// Bundled result of a successful [`crate::Store::create_instance`].
/// Surfaces every record the store creates atomically with the
/// instance.
///
/// Phase 0 always returns exactly one NIC (the auto-created
/// `"primary"`) and one Disk (the auto-created `"boot"`). The
/// vectors are additive — when multi-NIC / multi-disk attach lands
/// as a follow-on slice, callers don't need to change shape; they
/// just observe more entries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceCreateResult {
    pub instance: Instance,
    pub nics: Vec<Nic>,
    pub disks: Vec<Disk>,
}

/// Per-instance persistent storage. Phase 0 auto-creates a single
/// boot disk per instance, sized from the source image and tagged
/// with that image's id. Bhyve images are clamped to the M1 default
/// VM boot-disk floor until packages/SKUs expose an explicit size.
/// The disk record is the metadata view; the actual content (zfs
/// dataset, mantafs object, etc.) is materialized by the agent at
/// provisioning time.
///
/// Multi-disk attach (data disks beyond boot) lands as a follow-on
/// slice. The wire shape here is stable across that change — a
/// `kind: Data` disk is a strict superset of what `Boot` carries
/// once `source_image_id` is allowed to be `None`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Disk {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub project_id: Uuid,
    pub instance_id: Uuid,
    pub name: String,
    pub description: String,
    pub kind: DiskKind,
    /// Total size in bytes. Boot disks default via
    /// [`default_boot_disk_size_bytes`]; future data disks accept an
    /// explicit operator-supplied size.
    pub size_bytes: u64,
    /// Image the boot disk was sourced from. `Some` for `Boot` disks
    /// that came from a registered image; `None` for blank `Data`
    /// disks (Phase 0 has no callers that produce `None`, but the
    /// type allows it for the multi-disk slice).
    #[serde(default)]
    pub source_image_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

/// Hardcoded Phase 0 IPv4 floating-IP pool. Drawn from
/// **TEST-NET-3** (`203.0.113.0/24`, RFC 5737), the canonical
/// "documentation" range — explicitly chosen so the pool addresses
/// look obviously fake on the wire, won't collide with anyone's
/// real RFC1918 / public space, and surface immediately if a
/// future Phase ships these without replacing the constant.
///
/// A future slice promotes this to an operator-managed `IpPool`
/// resource so silos / projects can BYO public ranges.
pub const FLOATING_IP_V4_POOL: Ipv4Network =
    match Ipv4Network::new_checked(Ipv4Addr::new(203, 0, 113, 0), 24) {
        Some(net) => net,
        None => panic!("FLOATING_IP_V4_POOL is a compile-time constant CIDR"),
    };

/// Hardcoded Phase 0 IPv6 floating-IP pool. Drawn from the
/// **documentation prefix** `2001:db8::/48` (RFC 3849).
pub const FLOATING_IP_V6_POOL: Ipv6Network =
    match Ipv6Network::new_checked(Ipv6Addr::new(0x2001, 0x0db8, 0, 0, 0, 0, 0, 0), 48) {
        Some(net) => net,
        None => panic!("FLOATING_IP_V6_POOL is a compile-time constant CIDR"),
    };

/// Address family selector used by [`NewFloatingIp`] to ask the
/// server to allocate from one or the other Phase 0 pool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AddressFamily {
    V4,
    V6,
}

/// Where a [`FloatingIp`] is currently attached. `None` (i.e. the
/// `FloatingIp::attached_to` field is `None`) means the IP is
/// allocated to the project but not bound to any NIC — it persists
/// across attach/detach cycles and across instance deletes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FloatingIpAttachment {
    pub instance_id: Uuid,
    pub nic_id: Uuid,
    pub attached_at: DateTime<Utc>,
}

/// Tenant-managed external IP, allocated from a fleet pool and
/// attachable to any NIC in the same project. Persists independent
/// of any single instance: when the attached instance is deleted,
/// the `attached_to` field clears but the FloatingIp itself stays
/// owned by the project and reusable.
///
/// Each FloatingIp represents *one* address — the wire shape does
/// not bifurcate IPv4 and IPv6. The address family is implicit in
/// the `address` bits.
///
/// Phase 0 invariants vs other clouds:
///
/// * **Project-owned, not instance-owned.** Instance delete →
///   auto-detach, never auto-release. (AWS detaches but leaves the
///   IP at the account level, which loses the project-scoping
///   story; we keep the project envelope.)
/// * **Symmetric IPv4 + IPv6.** No "v6 floating IPs are a separate
///   type" wart.
/// * **Atomic attach replaces.** A second attach with a different
///   NIC swaps the binding in one transaction; no detach-then-attach
///   window in the control plane.
/// * **Delete is explicit.** Detaching does not auto-release; the
///   FloatingIp persists and is visible in `tcadm` listings until
///   the operator deletes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FloatingIp {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    pub description: String,
    /// The actual external address. Allocated at create time from
    /// the requested family's pool. Immutable for the life of the
    /// record.
    pub address: IpAddr,
    /// Currently-bound NIC, or `None` if floating. Replaced
    /// atomically by `attach`; cleared by `detach` and by the
    /// instance-delete cascade.
    #[serde(default)]
    pub attached_to: Option<FloatingIpAttachment>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Request body for allocating a new FloatingIp. The server picks
/// the actual address from the family-specific Phase 0 pool; the
/// caller asks for `V4` or `V6` and gets the lowest free address.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NewFloatingIp {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub family: AddressFamily,
}

/// Cluster-level system keys. Phase 0 has exactly one
/// (`SystemKey::JwtSigning`); future entries will include the
/// transit-engine master key and any per-silo OIDC client secrets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SystemKey {
    /// 32-byte HS256 secret used to sign and validate operator JWTs.
    JwtSigning,
}

impl SystemKey {
    /// Stable storage tag, used as the FDB key suffix.
    pub fn tag(self) -> &'static str {
        match self {
            SystemKey::JwtSigning => "jwt_signing",
        }
    }
}

// ---------------------------------------------------------------------
// Compute nodes (`Cn`)
// ---------------------------------------------------------------------

/// Lifecycle state of a compute-node registration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum CnState {
    /// Self-registered; awaiting operator approval (or auto-approve
    /// window). Carries an active `claim_code` until approval or
    /// expiry.
    Pending,
    /// Approved and active. The per-CN API key bound via
    /// [`ApiKey::bound_to_cn`] is the agent's credential for the
    /// rest of the `/v2/agent/*` surface.
    Approved,
    /// Explicitly disabled by an operator. The bound API key is
    /// revoked. The record stays for audit visibility; a fresh
    /// registration from the same `server_uuid` is rejected until
    /// the operator removes the disabled record (Phase 1 surface)
    /// — for Phase 0, "disable" is the terminal state.
    Disabled,
}

/// Placement role for a compute node.
///
/// M1 starts every CN as tenant-capable. Operators can mark a CN as
/// edge-only, or both tenant and edge, before the edge placer starts
/// assigning firehyve/fhrun north/south instances.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum CnRole {
    /// Eligible for tenant workload placement.
    #[default]
    Tenant,
    /// Eligible for north/south edge placement.
    Edge,
    /// Eligible for tenant workload and north/south edge placement.
    Both,
}

/// A compute node registered with the control plane.
///
/// Identity is the SmartOS `server_uuid` (read from
/// `/usr/bin/sysinfo`), not a tritond-generated id, so re-registration
/// after a reboot or reimage of the same physical host idempotently
/// maps to the same record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cn {
    /// SmartOS server UUID. Stable across reboots; unique per
    /// physical compute node.
    pub server_uuid: Uuid,
    pub hostname: String,
    /// Admin-network IPv4 address last reported during registration.
    /// Stored for operator visibility only ("this CN claims to be at
    /// IP X"); not used as an authentication signal.
    pub admin_ip: Option<Ipv4Addr>,
    pub state: CnState,
    /// Operator-controlled placement role. Defaults to tenant so
    /// existing lab CNs keep accepting tenant workloads until
    /// explicitly marked as edge-capable.
    #[serde(default)]
    pub role: CnRole,
    pub registered_at: DateTime<Utc>,
    /// When the Pending → Approved transition happened. `None`
    /// while still Pending.
    pub approved_at: Option<DateTime<Utc>>,
    /// Most recent agent-side activity. Updated by Slice D's
    /// heartbeater; `None` until then.
    pub last_seen: Option<DateTime<Utc>>,
    /// Raw sysinfo blob from the most recent registration. Opaque
    /// to tritond; surfaced via `tcadm cn show`.
    pub sysinfo: serde_json::Value,
    /// Active claim code in normalized form (six chars of Crockford
    /// base32, no hyphens). `None` once approved or expired. Indexed
    /// for O(1) lookup by code.
    pub claim_code: Option<String>,
    pub claim_code_expires_at: Option<DateTime<Utc>>,
    /// Opaque token the agent presents to long-poll
    /// `/v2/agent/register/status`. Rotated on every (re-)registration
    /// so a stale agent never accidentally retrieves a credential
    /// belonging to a fresh registration. Holding the token is *not*
    /// sufficient to approve; operators approve via `claim_code`.
    pub poll_token: String,
    /// API key id minted at approval, bound to this CN. `None`
    /// while Pending. The plaintext is delivered to the agent via
    /// [`Cn::pending_credential`] on the next long-poll, then
    /// cleared.
    pub bound_api_key_id: Option<Uuid>,
    /// Plaintext API key, stored transiently between approval and
    /// the agent's first long-poll-after-approval. Cleared on
    /// retrieval. Never serialized into wire-level views.
    ///
    /// Storing plaintext at rest is a known Phase 0 cost: the
    /// window is typically sub-second (long-poll triggers
    /// immediately on state flip) and capped at the claim code's
    /// retention TTL. A future slice will encrypt this field
    /// against the manta-storage Phase 0 secrets engine.
    #[serde(default)]
    pub pending_credential: Option<String>,
    /// Most recent status payload posted by the agent's
    /// heartbeater (Slice D). Opaque to tritond — the agent
    /// chooses the shape — but typically `{ vms, zpools,
    /// meminfo, diskinfo, boot_time, timestamp }`. `None` until
    /// the first status post lands.
    #[serde(default)]
    pub last_status: Option<serde_json::Value>,
}

/// Wire-safe view of a [`Cn`]: strips the transient plaintext
/// credential field and any other secrets from operator-facing JSON.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CnView {
    pub server_uuid: Uuid,
    pub hostname: String,
    pub admin_ip: Option<Ipv4Addr>,
    pub state: CnState,
    pub role: CnRole,
    pub registered_at: DateTime<Utc>,
    pub approved_at: Option<DateTime<Utc>>,
    pub last_seen: Option<DateTime<Utc>>,
    pub sysinfo: serde_json::Value,
    /// Displayed in the `XXX-XXX` format for operator readability.
    /// `None` once approved or expired.
    pub claim_code: Option<String>,
    pub claim_code_expires_at: Option<DateTime<Utc>>,
    pub bound_api_key_id: Option<Uuid>,
    /// Last full status payload published by the agent
    /// (vms / zpools / meminfo / disk_usage / boot_time blob).
    pub last_status: Option<serde_json::Value>,
}

impl From<Cn> for CnView {
    fn from(cn: Cn) -> Self {
        CnView {
            server_uuid: cn.server_uuid,
            hostname: cn.hostname,
            admin_ip: cn.admin_ip,
            state: cn.state,
            role: cn.role,
            registered_at: cn.registered_at,
            approved_at: cn.approved_at,
            last_seen: cn.last_seen,
            sysinfo: cn.sysinfo,
            claim_code: cn.claim_code.as_deref().map(format_claim_code),
            claim_code_expires_at: cn.claim_code_expires_at,
            bound_api_key_id: cn.bound_api_key_id,
            last_status: cn.last_status,
        }
    }
}

/// Auto-approve window: while open, new Pending CN registrations are
/// promoted to Approved without operator action. Bounded by both wall
/// time (`expires_at`) and a remaining-count budget so an operator
/// can't accidentally leave the window open forever.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AutoApproveWindow {
    pub opened_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    /// Remaining auto-approvals before the window closes. `None`
    /// means "unlimited count, time-bound only".
    pub remaining_count: Option<u64>,
    /// Operator who opened the window (free-text, captured from the
    /// authenticated principal at open time).
    pub opened_by: String,
}

/// Hard upper bound on auto-approve window duration: 24 hours. Bigger
/// than this and the window is too useful to an attacker who notices
/// it's open.
pub const AUTO_APPROVE_WINDOW_MAX: std::time::Duration =
    std::time::Duration::from_secs(24 * 60 * 60);

/// How long a freshly-issued claim code remains valid. After expiry,
/// the agent's next registration attempt rotates it.
pub const CLAIM_CODE_TTL: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// Crockford base32 alphabet (omits `0/O/1/I/L/U` to avoid
/// console-misread ambiguity). `U` is omitted by Crockford to
/// reduce the chance of accidental swear words in random codes.
pub const CLAIM_CODE_ALPHABET: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// Length of the normalized claim code (no separator). Six
/// characters of base32 = 30 bits of entropy, ample given the 1h
/// TTL and per-IP rate limit on the approve endpoint.
pub const CLAIM_CODE_LEN: usize = 6;

/// Generate a fresh claim code in normalized form (six characters,
/// no hyphen) using the supplied RNG. Caller is responsible for
/// the FDB-side collision check; this function does not consult
/// any index.
pub fn generate_claim_code<R: rand::Rng + ?Sized>(rng: &mut R) -> String {
    let mut out = String::with_capacity(CLAIM_CODE_LEN);
    for _ in 0..CLAIM_CODE_LEN {
        let idx = rng.random_range(0..CLAIM_CODE_ALPHABET.len());
        out.push(CLAIM_CODE_ALPHABET[idx] as char);
    }
    out
}

/// Format a normalized claim code for display: `XXX-XXX`.
pub fn format_claim_code(normalized: &str) -> String {
    if normalized.len() == CLAIM_CODE_LEN {
        format!("{}-{}", &normalized[..3], &normalized[3..])
    } else {
        normalized.to_string()
    }
}

/// Normalize a user-typed claim code: strip the hyphen, uppercase,
/// reject anything that isn't six characters of the Crockford
/// alphabet. Returns `None` for invalid inputs so the caller can
/// 400 cleanly.
pub fn normalize_claim_code(input: &str) -> Option<String> {
    let stripped: String = input
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .collect();
    if stripped.len() != CLAIM_CODE_LEN {
        return None;
    }
    let upper = stripped.to_ascii_uppercase();
    if !upper.bytes().all(|b| CLAIM_CODE_ALPHABET.contains(&b)) {
        return None;
    }
    Some(upper)
}

/// Generate a fresh poll token: 32 hex chars (128 bits of entropy).
/// This is the long-poll bearer; uniqueness is checked at the FDB
/// layer.
pub fn generate_poll_token<R: rand::Rng + ?Sized>(rng: &mut R) -> String {
    let mut bytes = [0u8; 16];
    rng.fill_bytes(&mut bytes);
    let mut hex = String::with_capacity(32);
    for b in bytes {
        hex.push_str(&format!("{b:02x}"));
    }
    hex
}

#[cfg(test)]
mod cn_tests {
    use super::*;

    #[test]
    fn claim_code_round_trip() {
        let mut rng = rand::rng();
        let code = generate_claim_code(&mut rng);
        assert_eq!(code.len(), CLAIM_CODE_LEN);
        assert!(code.bytes().all(|b| CLAIM_CODE_ALPHABET.contains(&b)));
        let display = format_claim_code(&code);
        assert_eq!(display.len(), CLAIM_CODE_LEN + 1); // hyphen
        let normalized = normalize_claim_code(&display).expect("normalize");
        assert_eq!(normalized, code);
    }

    #[test]
    fn normalize_rejects_lookalike_chars() {
        // Crockford drops 0/O/1/I/L/U. A code containing those is invalid.
        assert!(normalize_claim_code("ABCDEU").is_none());
        assert!(normalize_claim_code("ABCDEL").is_none());
        assert!(normalize_claim_code("ABCDEI").is_none());
        assert!(normalize_claim_code("ABCDEO").is_none());
    }

    #[test]
    fn normalize_accepts_lowercase_and_hyphen() {
        let normalized = normalize_claim_code("k7p-9x2").expect("normalize");
        assert_eq!(normalized, "K7P9X2");
    }

    #[test]
    fn normalize_rejects_wrong_length() {
        assert!(normalize_claim_code("ABC").is_none());
        assert!(normalize_claim_code("ABCDEFG").is_none());
    }

    #[test]
    fn poll_token_is_32_hex_chars() {
        let mut rng = rand::rng();
        let token = generate_poll_token(&mut rng);
        assert_eq!(token.len(), 32);
        assert!(token.bytes().all(|b| b.is_ascii_hexdigit()));
    }
}

// ---------------------------------------------------------------------
// Realized network state (Agent A, Slice H-1)
// ---------------------------------------------------------------------
//
// `tritond` is the desired-state authority. Realizers
// (`tritonagent` per CN, the future firehyve/fhrun-managed edge
// runtime per VPC) report what they actually programmed via
// `POST /v2/agent/network-realization` (Slice H-13). The control
// plane stores one row per `(resource, realizer)` tuple; the
// `RealizedNetworkState` view rolls those rows up at read time.
//
// `desired_generation` lives directly on each network resource
// record (every new v1 resource carries `desired_generation: u64`
// and bumps it atomically with every wire-affecting mutation).
// The `RealizedNetworkState` field on a wire response is computed
// at serialize time from `(desired_generation, list_network_realizations(resource))`,
// not stored as a denormalization — a denormalized copy on the
// resource record would silently drift from the per-realizer rows
// every time a realizer reported.
//
// Existing public structs (`Vpc`, `Subnet`, `FloatingIp`) gain
// `desired_generation` only via dedicated slices that intentionally
// include OpenAPI/client regen + tests (manager ruling 2026-05-05).

/// Outcome a realizer reports at a given generation. The variant
/// set is deliberately small in v1: `Accepted` for "agent received
/// the blueprint and handed it to its dataplane" (Agent B's
/// `accepted_generation`); `Applied` for "dataplane confirmed the
/// generation active" (the canonical terminal report); `Failed` for
/// terminal apply failure with a short message. The enum is
/// `#[non_exhaustive]` so post-v1 additions (e.g. `Compiling`,
/// `Pending`) can land without breaking downstream matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum RealizationStatus {
    /// The realizer accepted the work and handed the blueprint to
    /// its backing dataplane. Aligns with Agent B's
    /// `accepted_generation` concept (see
    /// `proteus/docs/tritond-integration-v1.md`).
    Accepted,
    /// The dataplane confirmed the generation active (e.g. Proteus
    /// `GetGenerationStatus` returned `applied_generation >=
    /// generation` on the realizer's port).
    Applied,
    /// The realizer failed to converge to this generation. The
    /// associated message should describe the phase (image fetch,
    /// blueprint apply, port start, ...).
    Failed,
}

/// Identity of a realizer reporting a [`Realization`]. v1 ships
/// `Cn` (per-server `tritonagent`, identified by SmartOS
/// `server_uuid`) and `EdgeCluster` (firehyve/fhrun-managed edge
/// microVMs, populated when Agent E begins reporting).
///
/// Wire shape: `{ "kind": "cn", "id": "<uuid>" }` /
/// `{ "kind": "edge_cluster", "id": "<uuid>" }`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum RealizerId {
    /// SmartOS compute node identified by its `server_uuid`.
    Cn { id: Uuid },
    /// Firehyve / fhrun-managed edge cluster identified by its
    /// `EdgeCluster.id`. Reserved for Agent E reporting; the
    /// reference type lands in H-12.
    EdgeCluster { id: Uuid },
}

impl RealizerId {
    /// Stable wire-format kind tag. Matches the
    /// `#[serde(tag = "kind", rename_all = "snake_case")]` shape
    /// so the FDB key segment and the JSON wire tag are the same
    /// string.
    #[must_use]
    pub fn kind_tag(self) -> &'static str {
        match self {
            RealizerId::Cn { .. } => "cn",
            RealizerId::EdgeCluster { .. } => "edge_cluster",
            // No catch-all: adding a `#[non_exhaustive]` variant is
            // a deliberate code change that should also extend the
            // wire tag map below in the same commit.
        }
    }

    /// The realizer id, regardless of variant.
    #[must_use]
    pub fn id(self) -> Uuid {
        match self {
            RealizerId::Cn { id } | RealizerId::EdgeCluster { id } => id,
        }
    }
}

/// Tagged identity of a network resource that may have realization
/// rows. The realization endpoint (Slice H-13) dispatches into the
/// matching record by `(kind, id)`. v1 ships variants for every
/// resource the design doc names (§6); the `#[non_exhaustive]`
/// posture allows post-v1 additions.
///
/// Wire shape: `{ "kind": "nat_gateway", "id": "<uuid>" }`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum NetworkResourceId {
    /// A `Vpc`. Populated after H-4 widens `Vpc` with
    /// `desired_generation`.
    Vpc { id: Uuid },
    /// A `Subnet`. Populated after H-4 widens `Subnet` with
    /// `desired_generation`.
    Subnet { id: Uuid },
    /// A `RouteTable`. Populated when H-5 lands.
    RouteTable { id: Uuid },
    /// A `Route`. Populated when H-6 lands.
    Route { id: Uuid },
    /// A `SecurityGroup`. Populated when H-7 lands.
    SecurityGroup { id: Uuid },
    /// A `SecurityGroupRule`. Populated when H-8 lands.
    SecurityGroupRule { id: Uuid },
    /// A NIC↔SG attachment. Populated when H-9 lands.
    NicSecurityGroupAttachment { id: Uuid },
    /// A `NatGateway`. Populated when H-2 lands. This is the first
    /// realized resource the codebase will exercise end-to-end
    /// (intent + realization) in the H-1..H-3 cluster.
    NatGateway { id: Uuid },
    /// A `FloatingIp`. Populated when H-11 widens the existing
    /// `FloatingIp` struct with `desired_generation` + termination
    /// fields.
    FloatingIp { id: Uuid },
    /// An `EdgeCluster` hosting system-owned edge dataplane work.
    EdgeCluster { id: Uuid },
}

impl NetworkResourceId {
    /// Stable wire-format kind tag. Matches the
    /// `#[serde(tag = "kind", rename_all = "snake_case")]` shape
    /// so the FDB key segment and the JSON wire tag are the same
    /// string. Used by `FdbStore` to compose
    /// `network_realization/<kind>/<id>/<realizer_kind>/<realizer_id>`
    /// keys.
    #[must_use]
    pub fn kind_tag(self) -> &'static str {
        match self {
            NetworkResourceId::Vpc { .. } => "vpc",
            NetworkResourceId::Subnet { .. } => "subnet",
            NetworkResourceId::RouteTable { .. } => "route_table",
            NetworkResourceId::Route { .. } => "route",
            NetworkResourceId::SecurityGroup { .. } => "security_group",
            NetworkResourceId::SecurityGroupRule { .. } => "security_group_rule",
            NetworkResourceId::NicSecurityGroupAttachment { .. } => "nic_security_group_attachment",
            NetworkResourceId::NatGateway { .. } => "nat_gateway",
            NetworkResourceId::FloatingIp { .. } => "floating_ip",
            NetworkResourceId::EdgeCluster { .. } => "edge_cluster",
        }
    }

    /// The resource id, regardless of variant.
    #[must_use]
    pub fn id(self) -> Uuid {
        match self {
            NetworkResourceId::Vpc { id }
            | NetworkResourceId::Subnet { id }
            | NetworkResourceId::RouteTable { id }
            | NetworkResourceId::Route { id }
            | NetworkResourceId::SecurityGroup { id }
            | NetworkResourceId::SecurityGroupRule { id }
            | NetworkResourceId::NicSecurityGroupAttachment { id }
            | NetworkResourceId::NatGateway { id }
            | NetworkResourceId::FloatingIp { id }
            | NetworkResourceId::EdgeCluster { id } => id,
        }
    }
}

/// Per-realizer realization row. One per `(resource, realizer)`
/// tuple; written by [`crate::Store::record_network_realization`]
/// and read back by [`crate::Store::list_network_realizations`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Realization {
    /// Which CN or edge cluster reported this row.
    pub realizer: RealizerId,
    /// Generation the realizer reports for the resource. Monotonic
    /// per `(resource, realizer)`: a write with `generation <
    /// existing.generation` is rejected with
    /// [`crate::StoreError::Conflict`] (the "backward report" case
    /// the Agent C contract calls out).
    pub generation: u64,
    /// Realizer-side outcome at this generation.
    pub status: RealizationStatus,
    /// Wall-clock time the row was last upserted by the realizer.
    pub last_reported_at: DateTime<Utc>,
    /// Free-form short diagnostic from the realizer. Surfaced
    /// verbatim in `tcadm net realized`. Kept short — detailed
    /// stderr belongs in agent logs and future support bundles, not
    /// in unbounded control-plane rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Rolled-up realization view of a single network resource. Computed
/// at read time from the resource's `desired_generation` field plus
/// the per-realizer rows in the `network_realization/...` keyspace.
///
/// **Not a denormalization.** The resource record stores
/// `desired_generation: u64` directly; this view is synthesized by
/// [`RealizedNetworkState::from_rows`] when the resource is
/// serialized for a wire response. Storing the rolled-up view would
/// drift from the per-realizer rows on every realizer report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RealizedNetworkState {
    /// Desired generation of the resource. Monotonically increased
    /// by `tritond` on every wire-affecting mutation.
    pub desired_generation: u64,
    /// Highest generation any realizer has reported with
    /// [`RealizationStatus::Applied`]. `None` if no realizer has
    /// applied anything yet. Useful as a coarse "did anything take?"
    /// signal; "did the dataplane converge everywhere?" requires
    /// inspecting [`Self::realizations`] against the expected
    /// realizer set.
    #[serde(default)]
    pub applied_generation: Option<u64>,
    /// Per-realizer rows, sorted by `(realizer.kind_tag(),
    /// realizer.id())` for deterministic output.
    pub realizations: Vec<Realization>,
}

impl RealizedNetworkState {
    /// Roll `(desired_generation, rows)` up into a
    /// [`RealizedNetworkState`]. Sorts `rows` deterministically and
    /// computes `applied_generation` as the max generation across
    /// rows whose status is [`RealizationStatus::Applied`].
    ///
    /// This is the canonical projection callers should use; both
    /// MemStore and FdbStore return rows in the same sorted order,
    /// so re-running this helper on an already-sorted vec is
    /// idempotent.
    #[must_use]
    pub fn from_rows(desired_generation: u64, mut rows: Vec<Realization>) -> Self {
        rows.sort_by(|a, b| {
            a.realizer
                .kind_tag()
                .cmp(b.realizer.kind_tag())
                .then_with(|| a.realizer.id().cmp(&b.realizer.id()))
        });
        let applied_generation = rows
            .iter()
            .filter(|r| matches!(r.status, RealizationStatus::Applied))
            .map(|r| r.generation)
            .max();
        Self {
            desired_generation,
            applied_generation,
            realizations: rows,
        }
    }
}

// ============================================================
// Storage clusters (operator-only)
// ============================================================
//
// `StorageCluster` registers an external manta-storage daemon
// (mantad for the S3 surface today; mantafs / manta-block in
// follow-ups) so tritond can broker operator-driven admin calls
// to it without leaking the bearer token to admin-backend.

/// Discriminator for the storage surface a registered cluster serves.
///
/// Each surface has its own forwarder endpoint family
/// (`/v2/storage/clusters/{id}/s3/*` etc.); attempting to call a
/// surface's endpoint family on a cluster registered under a
/// different surface returns a `409 Conflict`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum StorageClusterSurface {
    /// S3-compatible object surface (mantad / mantas3-cluster
    /// `/admin/v1/*`).
    S3,
    /// Filesystem surface (mantafs / tritonfs). Registration is
    /// accepted; forwarder endpoints are not yet implemented.
    Fs,
    /// Block-volume surface (manta-block). Registration is accepted;
    /// forwarder endpoints are not yet implemented.
    Block,
}

impl StorageClusterSurface {
    /// Stable string label used in URL path segments and FDB indexes.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::S3 => "s3",
            Self::Fs => "fs",
            Self::Block => "block",
        }
    }
}

/// Operator-observed health of a registered storage cluster.
///
/// Populated by the `/v2/storage/clusters/{id}/health` endpoint, which
/// runs a probe against the cluster's `/admin/v1/cluster` summary.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, Default,
)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum StorageClusterStatus {
    /// Last probe succeeded and all nodes were alive.
    Healthy,
    /// Last probe succeeded but at least one node was missing.
    Degraded,
    /// Last probe failed (connection refused, timeout, 5xx).
    Unreachable,
    /// No probe has run yet, or the last probe was too long ago to
    /// trust.
    #[default]
    Unknown,
}

/// A registered manta-storage cluster.
///
/// Operator-only resource (root-allows-all in Cedar). Holds the bearer
/// token used to authenticate against the cluster's `/admin/v1/*`
/// surface; admin-backend never sees the raw token because every
/// admin operation goes through tritond's typed forwarder endpoints
/// at `/v2/storage/clusters/{id}/...`.
///
/// `admin_token` is stored in plaintext for Phase 0 — same precedent
/// as `IdpConfig.client_secret` (see `STATUS.md` deferred-work table:
/// "Encryption-at-rest for `IdpConfig.client_secret` → manta-storage
/// Phase 0 secrets engine ships"). The `StorageClusterView` wire shape
/// redacts the token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageCluster {
    pub id: Uuid,
    /// Operator-chosen short name. Unique cluster-wide. Used as the
    /// secondary FDB index.
    pub name: String,
    /// Surface the cluster serves.
    pub surface: StorageClusterSurface,
    /// HTTP base URL for the cluster's admin API
    /// (e.g. `http://10.199.199.250:7101`).
    pub endpoint: String,
    /// Bearer token tritond presents on `/admin/v1/*` calls.
    pub admin_token: String,
    /// Default region the surface uses for SigV4 signing
    /// (informational; admin-backend echoes it to clients).
    pub default_region: String,
    /// Optional human-friendly label.
    pub display_name: Option<String>,
    /// Most recent health probe outcome.
    #[serde(default)]
    pub status: StorageClusterStatus,
    pub created_at: DateTime<Utc>,
    /// When `status` was last refreshed by a health probe.
    pub last_observed_at: Option<DateTime<Utc>>,
}

/// Body of `POST /v2/storage/clusters`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NewStorageCluster {
    pub name: String,
    pub surface: StorageClusterSurface,
    pub endpoint: String,
    pub admin_token: String,
    #[serde(default = "default_storage_cluster_region")]
    pub default_region: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

fn default_storage_cluster_region() -> String {
    "us-east-1".to_string()
}

/// Wire-side projection of [`StorageCluster`] that **redacts the
/// bearer token**. This is what `GET /v2/storage/clusters` and
/// `GET /v2/storage/clusters/{id}` return.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StorageClusterView {
    pub id: Uuid,
    pub name: String,
    pub surface: StorageClusterSurface,
    pub endpoint: String,
    pub default_region: String,
    pub display_name: Option<String>,
    pub status: StorageClusterStatus,
    pub created_at: DateTime<Utc>,
    pub last_observed_at: Option<DateTime<Utc>>,
}

impl From<StorageCluster> for StorageClusterView {
    fn from(c: StorageCluster) -> Self {
        Self {
            id: c.id,
            name: c.name,
            surface: c.surface,
            endpoint: c.endpoint,
            default_region: c.default_region,
            display_name: c.display_name,
            status: c.status,
            created_at: c.created_at,
            last_observed_at: c.last_observed_at,
        }
    }
}

#[cfg(test)]
mod realized_state_tests {
    use super::*;

    fn cn(uuid: &str) -> RealizerId {
        RealizerId::Cn {
            id: Uuid::parse_str(uuid).unwrap(),
        }
    }

    fn row(realizer: RealizerId, generation: u64, status: RealizationStatus) -> Realization {
        Realization {
            realizer,
            generation,
            status,
            last_reported_at: Utc::now(),
            message: None,
        }
    }

    #[test]
    fn from_rows_sets_applied_to_max_applied_generation() {
        let r1 = cn("11111111-1111-1111-1111-111111111111");
        let r2 = cn("22222222-2222-2222-2222-222222222222");
        let view = RealizedNetworkState::from_rows(
            10,
            vec![
                row(r1, 7, RealizationStatus::Applied),
                row(r2, 9, RealizationStatus::Applied),
                row(
                    RealizerId::EdgeCluster {
                        id: Uuid::parse_str("33333333-3333-3333-3333-333333333333").unwrap(),
                    },
                    11,
                    RealizationStatus::Failed,
                ),
            ],
        );
        assert_eq!(view.desired_generation, 10);
        // Applied 9 wins; the Failed-at-11 row is ignored for the
        // applied summary.
        assert_eq!(view.applied_generation, Some(9));
        assert_eq!(view.realizations.len(), 3);
    }

    #[test]
    fn from_rows_no_applied_yields_none() {
        let view = RealizedNetworkState::from_rows(
            5,
            vec![row(
                cn("11111111-1111-1111-1111-111111111111"),
                3,
                RealizationStatus::Accepted,
            )],
        );
        assert_eq!(view.applied_generation, None);
    }

    #[test]
    fn from_rows_sorts_deterministically() {
        let r1 = cn("11111111-1111-1111-1111-111111111111");
        let r2 = cn("22222222-2222-2222-2222-222222222222");
        let edge = RealizerId::EdgeCluster {
            id: Uuid::parse_str("33333333-3333-3333-3333-333333333333").unwrap(),
        };
        // Feed in unsorted: edge first (kind_tag "edge_cluster" >
        // "cn" alphabetically), then r2, then r1.
        let view = RealizedNetworkState::from_rows(
            1,
            vec![
                row(edge, 1, RealizationStatus::Applied),
                row(r2, 1, RealizationStatus::Applied),
                row(r1, 1, RealizationStatus::Applied),
            ],
        );
        // Expect: cn rows first (by id ascending), edge_cluster
        // last.
        assert_eq!(view.realizations[0].realizer, r1);
        assert_eq!(view.realizations[1].realizer, r2);
        assert_eq!(view.realizations[2].realizer, edge);
    }

    #[test]
    fn empty_rows_yields_empty_view() {
        let view = RealizedNetworkState::from_rows(0, vec![]);
        assert_eq!(view.desired_generation, 0);
        assert_eq!(view.applied_generation, None);
        assert!(view.realizations.is_empty());
    }

    #[test]
    fn network_resource_id_kind_tags_are_stable() {
        // Spot-check that the kind_tag matches the serde wire tag
        // exactly. If a future refactor renames a tag, the
        // FDB-keyspace layout breaks silently — this test catches
        // the drift before it ships.
        let id = Uuid::nil();
        let cases: &[(NetworkResourceId, &str)] = &[
            (NetworkResourceId::Vpc { id }, "vpc"),
            (NetworkResourceId::Subnet { id }, "subnet"),
            (NetworkResourceId::RouteTable { id }, "route_table"),
            (NetworkResourceId::Route { id }, "route"),
            (NetworkResourceId::SecurityGroup { id }, "security_group"),
            (
                NetworkResourceId::SecurityGroupRule { id },
                "security_group_rule",
            ),
            (
                NetworkResourceId::NicSecurityGroupAttachment { id },
                "nic_security_group_attachment",
            ),
            (NetworkResourceId::NatGateway { id }, "nat_gateway"),
            (NetworkResourceId::FloatingIp { id }, "floating_ip"),
            (NetworkResourceId::EdgeCluster { id }, "edge_cluster"),
        ];
        for (resource, want) in cases {
            assert_eq!(resource.kind_tag(), *want);
            // Round-trip through serde and check the wire tag.
            let json = serde_json::to_value(resource).unwrap();
            assert_eq!(json["kind"].as_str().unwrap(), *want);
        }
    }

    #[test]
    fn edge_cluster_resource_tags_and_acceptance_are_stable() {
        let id = Uuid::nil();
        let nat = EdgeClusterResource::NatGateway { nat_gateway_id: id };
        let fip = EdgeClusterResource::FloatingIp { floating_ip_id: id };

        assert_eq!(nat.kind_tag(), "nat_gateway");
        assert_eq!(fip.kind_tag(), "floating_ip");
        assert_eq!(nat.id(), id);
        assert_eq!(fip.id(), id);
        assert!(EdgeClusterKind::NatGateway.accepts_resource(nat));
        assert!(!EdgeClusterKind::NatGateway.accepts_resource(fip));
        assert!(EdgeClusterKind::FloatingIpDecap.accepts_resource(fip));
        assert!(!EdgeClusterKind::FloatingIpDecap.accepts_resource(nat));
        assert!(EdgeClusterKind::Shared.accepts_resource(nat));
        assert!(EdgeClusterKind::Shared.accepts_resource(fip));
    }

    #[test]
    fn realizer_id_kind_tags_are_stable() {
        let id = Uuid::nil();
        let cases: &[(RealizerId, &str)] = &[
            (RealizerId::Cn { id }, "cn"),
            (RealizerId::EdgeCluster { id }, "edge_cluster"),
        ];
        for (realizer, want) in cases {
            assert_eq!(realizer.kind_tag(), *want);
            let json = serde_json::to_value(realizer).unwrap();
            assert_eq!(json["kind"].as_str().unwrap(), *want);
        }
    }
}
