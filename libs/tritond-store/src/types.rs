// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Domain types shared between the storage layer and the wire surface.

use std::collections::{BTreeMap, BTreeSet, HashSet};
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
    /// True for operators with the fleet-admin role (the
    /// `fleet-admin-allows-fleet-actions` Cedar rule). Distinct from
    /// `is_root`: a fleet-admin can list/inspect cross-silo CN +
    /// legacy-VM state but cannot perform tenant-scoped writes.
    /// `false` for federated users; today only operators with `is_root`
    /// also implicitly have fleet-admin (via the root-allows-all rule).
    /// Defaults to `false` so existing persisted user records
    /// round-trip without churn.
    #[serde(default)]
    pub fleet_admin: bool,
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
    /// Operator capabilities granted to this user. Gates the
    /// `/v1/system/` operator surface per RFD 00007 D-Ap-13.
    /// `is_root` users bypass capability checks (they implicitly carry
    /// every capability); for non-root users the auth layer checks
    /// `capabilities.contains(&required)` before serving any
    /// `/v1/system/` endpoint. `fleet_admin == true` users migrate to
    /// `{SystemRead, SystemOperate}` at AP-1; `SystemConfigWrite` and
    /// `StorageAdmin` are granted explicitly per user.
    ///
    /// Defaults to the empty set so existing persisted user records
    /// round-trip without churn; the AP-1 migration in
    /// [`crate::Store::migrate_user_capabilities`] populates the set
    /// from `fleet_admin` once at deploy time.
    #[serde(default)]
    pub capabilities: BTreeSet<Capability>,
}

/// Operator capability that gates access to `/v1/system/` endpoints.
///
/// Per RFD 00007 D-Ap-13, the v1 capability set is intentionally
/// small. Each `/v1/system/` endpoint declares the capability it
/// requires; the auth layer's `require_capability` helper checks
/// `Principal::Operator.capabilities` against the requirement and
/// returns `404 NotFound` on a mismatch (the same shape as
/// cross-tenant deny; an attacker cannot distinguish "no access"
/// from "no such resource").
///
/// Adding a new capability is non-breaking on deserialise
/// (`#[serde(other)]` would mask the unknown variant, but we
/// deliberately do *not* use it here — an unknown capability in a
/// persisted row is a fail-loud signal that the cluster is reading
/// data from a newer writer). Adding a variant is breaking on the
/// auth-layer match (compile error if not classified), which is the
/// fail-loud check.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum Capability {
    /// Read any `/v1/system/` resource: fleet inventories (instances,
    /// disks, NICs, CNs), sagas, migrations, audit log, cluster
    /// config reads, storage cluster inventory reads, silo +
    /// utilization views. Includes drain-preview (dry-run).
    SystemRead,
    /// Mutate fleet resources at the per-resource level: CN approve,
    /// CN disable, CN role change, CN auto-approve window open/close,
    /// saga abandon, capability grant/revoke. Does NOT cover cluster
    /// config writes or storage cluster administration.
    SystemOperate,
    /// Write cluster-wide settings (`PUT`/`DELETE /v1/system/config/{key}`).
    /// Distinct from `SystemOperate` because changing cluster-wide
    /// behaviour is a different blast radius than per-resource ops.
    SystemConfigWrite,
    /// Administer storage clusters: register, drain, reweight, remove
    /// nodes; manage IAM users / access keys / policies on a cluster;
    /// set presigners. Read access is covered by `SystemRead`.
    StorageAdmin,
}

impl Capability {
    /// Every variant, in declaration order. Used by the AP-1 root
    /// migration to populate `User.capabilities` for the bootstrap
    /// operator (root carries every capability) and by tests that
    /// need to assert exhaustive coverage.
    pub fn all() -> &'static [Capability] {
        &[
            Capability::SystemRead,
            Capability::SystemOperate,
            Capability::SystemConfigWrite,
            Capability::StorageAdmin,
        ]
    }
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
/// `POST /v1/silos/{silo_id}/idp`; tritond eagerly fetches the
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
/// redacted. Returned by `GET /v1/silos/{silo_id}/idp`.
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
    /// What kind of network this subnet is. `External` subnets carry a
    /// FlatL2 nic_tag + VLAN and are the source of public / floating
    /// IPs; `Internal` / `Fabric` are VPC overlay subnets. Defaults to
    /// `Internal` for wire back-compat with pre-C-1 subnet rows.
    #[serde(default)]
    pub kind: NetworkKind,
    /// FK to the [`NicTag`] this subnet's traffic egresses on. Set for
    /// `External` subnets; `None` for overlay subnets.
    #[serde(default)]
    pub nic_tag: Option<Uuid>,
    /// 802.1Q VLAN id for an `External` subnet's nic_tag link
    /// (`None` = untagged). The agent's `EnsureExternalLink` creates the
    /// matching VLAN-tagged VNIC on the nic_tag's physical link.
    #[serde(default)]
    pub vlan_id: Option<u16>,
    /// Lowest IPv4 the allocator may hand out (inclusive); `None` falls
    /// back to the block's first usable host. Scopes a pool to a
    /// sub-range of a larger upstream block.
    #[serde(default)]
    pub provision_start_ipv4: Option<Ipv4Addr>,
    /// Highest IPv4 the allocator may hand out (inclusive); `None` falls
    /// back to the block's last usable host.
    #[serde(default)]
    pub provision_end_ipv4: Option<Ipv4Addr>,
    #[serde(default)]
    pub provision_start_ipv6: Option<Ipv6Addr>,
    #[serde(default)]
    pub provision_end_ipv6: Option<Ipv6Addr>,
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

/// Network kind. `External` is FlatL2 public space (floating-IP and
/// NAT-gateway source); `Internal` / `Fabric` are VPC overlay subnets.
/// An unrecognized wire value decodes to `Unknown`, which the external
/// dataplane path treats as "not external" (fail closed) rather than
/// silently behaving as `Internal`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema, clap::ValueEnum,
)]
#[serde(rename_all = "snake_case")]
pub enum NetworkKind {
    #[default]
    Internal,
    External,
    Fabric,
    #[serde(other)]
    Unknown,
}

/// A named L2 segment a CN can attach VNICs to (e.g. `external`,
/// `internal`, `sdc_underlay`). External subnets reference one by id;
/// per-CN provisioning lives in [`CnNicTagInventory`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NicTag {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    /// Effective MTU of the tag's link; a subnet's MTU must not exceed
    /// this.
    pub mtu: u32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Request body for registering a nic_tag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NewNicTag {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub mtu: u32,
}

/// One nic_tag a CN's hardware provides, with the physical link, VLAN,
/// and MTU it lands on. Published by `tritonagent` from sysinfo /
/// nictagadm; read by placement and the FIP allocator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NicTagProvision {
    pub nic_tag: Uuid,
    pub physical_nic: String,
    pub vlan_id: u16,
    pub mtu: u32,
}

/// The per-CN list of nic_tags this CN provides. Single-writer: only
/// the owning CN's agent writes its row (`cn-nic-tags/<cn>`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CnNicTagInventory {
    pub cn: Uuid,
    pub provides: Vec<NicTagProvision>,
    pub published_at: DateTime<Utc>,
}

/// An ordered list of external networks (subnets) the allocator walks
/// in order when handing out public addresses. Carried from NAPI; v1
/// defaults allocation to a single network.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NetworkPool {
    pub id: Uuid,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// Ordered external-subnet ids; allocation walks them in order.
    pub networks: Vec<Uuid>,
    /// Silos allowed to allocate from this pool; empty = all silos.
    #[serde(default)]
    pub owner_silos: Vec<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Request body for creating a network pool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NewNetworkPool {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub networks: Vec<Uuid>,
    #[serde(default)]
    pub owner_silos: Vec<Uuid>,
}

/// Request body for creating an operator-scoped External subnet.
///
/// External subnets are FlatL2 public space: they carry a nic_tag +
/// VLAN and are the source of public / floating IPs, allocated on the
/// single global public-IP index (invariant D5). They are *not*
/// tenant VPC subnets — there is no parent VPC — so the store stamps
/// reserved nil ids for `tenant_id` / `project_id` / `vpc_id` /
/// `route_table_id`. At least one of `ipv4_block` / `ipv6_block` must
/// be `Some`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NewExternalSubnet {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    #[schemars(with = "Option<String>")]
    pub ipv4_block: Option<Ipv4Network>,
    #[serde(default)]
    #[schemars(with = "Option<String>")]
    pub ipv6_block: Option<Ipv6Network>,
    /// The nic_tag this external subnet egresses on. Must resolve to a
    /// registered [`NicTag`].
    pub nic_tag: Uuid,
    #[serde(default)]
    pub vlan_id: Option<u16>,
    #[serde(default)]
    pub provision_start_ipv4: Option<Ipv4Addr>,
    #[serde(default)]
    pub provision_end_ipv4: Option<Ipv4Addr>,
    #[serde(default)]
    pub provision_start_ipv6: Option<Ipv6Addr>,
    #[serde(default)]
    pub provision_end_ipv6: Option<Ipv6Addr>,
    /// Silos allowed to allocate from this subnet; empty = all silos.
    /// Not enforced until C-3.
    #[serde(default)]
    pub owner_silos: Vec<Uuid>,
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

// =============================================================================
// DHCP / IPAM (Phase γ.1 + γ.4: per-VPC pool, reservations, leases)
// =============================================================================
//
// The kmod plugin already synthesizes DHCP OFFER/ACK/NAK from each port's
// pre-assigned IP (per α). This block adds the operator-facing IPAM
// surface: per-VPC pool tuning, sticky reservations, and a lease record
// that's written when an instance gets an IP. Once tritonagent forwards
// kmod DhcpRequest events (β.2) into tritond, those events will update
// `last_renewed_at` on the existing lease record — the schema below is
// shaped for that follow-up.

/// Per-VPC DHCPv4 pool config + segment-wide DHCP options. Singleton
/// per VPC; absence means "use the subnet CIDR directly with defaults."
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DhcpPool {
    pub vpc_id: Uuid,
    /// Renewal-cadence hint emitted in DHCP option 51.
    pub lease_seconds_default: u32,
    /// Operator-specified IPv4 addresses to skip during allocation.
    /// Useful for reserving the gateway, broadcast, or external
    /// services living on the subnet.
    #[serde(default)]
    #[schemars(with = "Vec<String>")]
    pub excluded_ipv4: Vec<Ipv4Addr>,
    /// VPC-wide DHCP options merged into every per-port response
    /// (after `Dhcpv4Options.additional_options` from the port's own
    /// blueprint). Example: an NTP server, a vendor-specific PXE
    /// pointer, etc.
    #[serde(default)]
    pub additional_options: Vec<DhcpOptionRaw>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Wire shape of one raw DHCP option. Matches proteus's `DhcpOption`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DhcpOptionRaw {
    pub code: u8,
    /// Up to 250 bytes (option-length byte caps at 255 minus 2 for
    /// code+length, conservatively 250). Serde encodes as a JSON
    /// array of integers; the wire-cap check lives in the create
    /// handler, not here.
    pub value: Vec<u8>,
}

/// Request body for `PUT /v1/.../vpcs/{vpc_id}/dhcp/pool`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NewDhcpPool {
    pub lease_seconds_default: u32,
    #[serde(default)]
    #[schemars(with = "Vec<String>")]
    pub excluded_ipv4: Vec<Ipv4Addr>,
    #[serde(default)]
    pub additional_options: Vec<DhcpOptionRaw>,
}

/// Operator-pinned MAC→IP mapping. Survives instance delete; an
/// instance booting later with a matching MAC reuses this IP.
///
/// Per-MAC additional options are merged on top of the per-VPC pool's
/// `additional_options` and the per-port blueprint's options at
/// response synthesis time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DhcpReservation {
    pub vpc_id: Uuid,
    /// Canonical lowercase colon-separated form, e.g. `"02:08:20:ab:cd:ef"`.
    pub mac: String,
    pub ipv4: Ipv4Addr,
    #[serde(default)]
    pub hostname: Option<String>,
    #[serde(default)]
    pub per_mac_options: Vec<DhcpOptionRaw>,
    pub created_at: DateTime<Utc>,
}

/// Request body for `POST /v1/.../vpcs/{vpc_id}/dhcp/reservations`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NewDhcpReservation {
    pub mac: String,
    pub ipv4: Ipv4Addr,
    #[serde(default)]
    pub hostname: Option<String>,
    #[serde(default)]
    pub per_mac_options: Vec<DhcpOptionRaw>,
}

/// One lease the IPAM has handed out. Written when tritond pre-assigns
/// an IP (γ.4 hook on `create_instance`); updated by the lease-renewal
/// event consumer (δ slice, not yet wired).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DhcpLease {
    pub vpc_id: Uuid,
    pub mac: String,
    pub ipv4: Ipv4Addr,
    /// The instance the lease was assigned to at create time. Stays
    /// pinned through instance delete so sticky-by-MAC keeps working
    /// when the operator re-creates with the same MAC; cleared when
    /// the operator explicitly releases the lease.
    pub instance_id: Uuid,
    /// NIC the lease is bound to.
    pub nic_id: Uuid,
    /// Last DHCP message type observed by the kmod for this MAC
    /// (DISCOVER, REQUEST, RELEASE, …). `None` until tritonagent
    /// starts forwarding events (δ slice).
    #[serde(default)]
    pub last_msg_type: Option<u8>,
    #[serde(default)]
    pub last_xid: Option<u32>,
    #[serde(default)]
    pub last_renewed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

// =============================================================================
// Firewall rules (Slice 1: per-VPC flat rule list)
// =============================================================================

/// Per-VPC firewall rule. Slice 1 ships a flat per-VPC list — every NIC in
/// the VPC inherits every rule (no security-group attachment yet). Tritond
/// translates this directly into proteus
/// `triton_vpc::tritond_intent_v1::FirewallRuleIntentV1` when computing the
/// per-port blueprint, which the dataplane compiles into one
/// `SecurityGroupRule` per record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FirewallRule {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub project_id: Uuid,
    pub vpc_id: Uuid,
    pub name: String,
    pub description: String,
    /// Higher numbers evaluate first inside a layer.
    pub priority: u16,
    pub direction: FirewallDirection,
    pub action: FirewallAction,
    pub protocol: FirewallProtocol,
    /// `None` ⇒ match any source.
    #[serde(default)]
    #[schemars(with = "Option<String>")]
    pub source_cidr: Option<IpNetwork>,
    /// `None` ⇒ match any destination.
    #[serde(default)]
    #[schemars(with = "Option<String>")]
    pub destination_cidr: Option<IpNetwork>,
    /// Inclusive port range. `None` ⇒ any port. Ignored for non-TCP/UDP.
    #[serde(default)]
    pub source_ports: Option<FirewallPortRange>,
    #[serde(default)]
    pub destination_ports: Option<FirewallPortRange>,
    /// Optional ICMP type/code filter. Only valid when `protocol` is
    /// `Icmp4` or `Icmp6`; the API rejects mismatches at create time.
    #[serde(default)]
    pub icmp_type_code: Option<FirewallIcmpFilter>,
    pub created_at: DateTime<Utc>,
}

/// Request body for creating a [`FirewallRule`]. The owning tenant /
/// project / VPC come from the URL path; the server assigns `id` and
/// `created_at`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NewFirewallRule {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub priority: u16,
    pub direction: FirewallDirection,
    pub action: FirewallAction,
    pub protocol: FirewallProtocol,
    #[serde(default)]
    #[schemars(with = "Option<String>")]
    pub source_cidr: Option<IpNetwork>,
    #[serde(default)]
    #[schemars(with = "Option<String>")]
    pub destination_cidr: Option<IpNetwork>,
    #[serde(default)]
    pub source_ports: Option<FirewallPortRange>,
    #[serde(default)]
    pub destination_ports: Option<FirewallPortRange>,
    #[serde(default)]
    pub icmp_type_code: Option<FirewallIcmpFilter>,
}

/// ICMP type + code pair carried as a struct (instead of `(u8, u8)`)
/// so the JSON schema is OpenAPI v3.0–compatible (tuple arrays are
/// not representable until v3.1).
#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FirewallIcmpFilter {
    #[serde(rename = "type")]
    pub kind: u8,
    pub code: u8,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FirewallDirection {
    Inbound,
    Outbound,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FirewallAction {
    Allow,
    Deny,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FirewallProtocol {
    Any,
    Tcp,
    Udp,
    Icmp4,
    Icmp6,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FirewallPortRange {
    pub low: u16,
    pub high: u16,
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

/// True if `ip` is contained in the IPv4 block of `network` (or the
/// network is V6, in which case the answer is trivially false).
#[must_use]
pub(crate) fn cidr_contains_ipv4(network: IpNetwork, ip: Ipv4Addr) -> bool {
    network.contains(IpAddr::V4(ip))
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
    /// The capability set this user carries. Surfaced on `tcadm whoami`
    /// and `tcadm system user show` per RFD 00007. Empty set means no
    /// `/v1/system/` access; `is_root: true` users bypass the check
    /// regardless of the set.
    #[serde(default)]
    pub capabilities: BTreeSet<Capability>,
}

impl From<User> for UserView {
    fn from(user: User) -> Self {
        UserView {
            id: user.id,
            username: user.username,
            is_root: user.is_root,
            created_at: user.created_at,
            capabilities: user.capabilities,
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
    /// authenticate to tritond's `/v1/agent/*` surface.
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

/// Hard ceiling on an operator-requested boot disk (16 TiB). A bound this
/// high never gets in a real caller's way; it exists so a bogus or hostile
/// `disk_bytes` can't ask the agent to reserve an absurd zvol.
pub const MAX_BOOT_DISK_BYTES: u64 = 16 * 1024 * 1024 * 1024 * 1024;

/// Resolve the boot-disk size for a new instance. An explicit
/// operator request is honored but floored at the image content size
/// (a zvol smaller than the image can't hold it); `None` falls back to
/// the brand default. Callers that accept `requested` from the wire must
/// also bound it by [`MAX_BOOT_DISK_BYTES`] at the API edge.
pub fn resolve_boot_disk_size_bytes(image: &Image, requested: Option<u64>) -> u64 {
    match requested {
        Some(bytes) => bytes.max(image.size_bytes),
        None => default_boot_disk_size_bytes(image),
    }
}

#[cfg(test)]
mod boot_disk_size_tests {
    use super::*;

    fn bhyve_image(size_bytes: u64) -> Image {
        Image {
            id: Uuid::nil(),
            scope: ImageScope::Public,
            name: "img".to_string(),
            description: String::new(),
            os: "linux".to_string(),
            version: "ubuntu-24.04".to_string(),
            size_bytes,
            sha256: "a".repeat(64),
            source_url: None,
            compatibility: Some(ImageCompatibility {
                brand: "bhyve".to_string(),
                arch: "x86_64".to_string(),
                min_smartos_platform: None,
            }),
            created_at: Utc::now(),
        }
    }

    #[test]
    fn none_falls_back_to_brand_default() {
        // A small bhyve image still floors at the 20 GiB brand default.
        let img = bhyve_image(3 * 1024 * 1024 * 1024);
        assert_eq!(
            resolve_boot_disk_size_bytes(&img, None),
            BHYVE_M1_MIN_BOOT_DISK_BYTES
        );
    }

    #[test]
    fn explicit_request_is_honored() {
        let img = bhyve_image(3 * 1024 * 1024 * 1024);
        let want = 40 * 1024 * 1024 * 1024;
        assert_eq!(resolve_boot_disk_size_bytes(&img, Some(want)), want);
    }

    #[test]
    fn explicit_request_floors_at_image_content() {
        // A request smaller than the image can't hold it; floor to the
        // image size rather than truncate.
        let img = bhyve_image(8 * 1024 * 1024 * 1024);
        let too_small = 2 * 1024 * 1024 * 1024;
        assert_eq!(
            resolve_boot_disk_size_bytes(&img, Some(too_small)),
            img.size_bytes
        );
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

/// SmartOS brand the instance's host-side VM runs as.
///
/// Derived at create time from the boot [`Image`]'s
/// [`ImageCompatibility::brand`] (the only place tritond currently
/// learns it). `NotApplicable` is the default — it covers both
/// records created before this field existed and images registered
/// via the explicit-fields path that carries no compatibility block.
///
/// Used by the console surface (VNC framebuffer is only meaningful
/// for `Bhyve` / `Kvm`) and by the UI to label instances. It is
/// `#[serde(other)]` on `NotApplicable` so an unrecognised future
/// brand string round-trips harmlessly rather than failing
/// deserialization.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum InstanceBrand {
    /// `kvm` HVM zone (legacy hypervisor; serial + VNC consoles).
    Kvm,
    /// `bhyve` HVM zone (serial via the zone console; VNC framebuffer).
    Bhyve,
    /// `lx` Linux-syscall zone (serial via the zone console only).
    Lx,
    /// `joyent-minimal` native SmartOS zone (serial via the zone
    /// console only).
    JoyentMinimal,
    /// Brand not recorded / not recognised. The console surface
    /// treats this as "serial only".
    #[default]
    #[serde(other)]
    NotApplicable,
}

impl InstanceBrand {
    /// Map an [`ImageCompatibility::brand`] string onto a brand.
    /// Unknown strings (and the empty string) map to
    /// [`InstanceBrand::NotApplicable`].
    #[must_use]
    pub fn from_compat_brand(brand: &str) -> Self {
        match brand {
            "kvm" => InstanceBrand::Kvm,
            "bhyve" => InstanceBrand::Bhyve,
            "lx" => InstanceBrand::Lx,
            "joyent-minimal" => InstanceBrand::JoyentMinimal,
            _ => InstanceBrand::NotApplicable,
        }
    }

    /// Derive the brand from an image's compatibility block, if any.
    #[must_use]
    pub fn from_image(image: &Image) -> Self {
        image
            .compatibility
            .as_ref()
            .map_or(InstanceBrand::NotApplicable, |c| {
                Self::from_compat_brand(&c.brand)
            })
    }

    /// Stable lowercase wire name (e.g. `"joyent-minimal"`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            InstanceBrand::Kvm => "kvm",
            InstanceBrand::Bhyve => "bhyve",
            InstanceBrand::Lx => "lx",
            InstanceBrand::JoyentMinimal => "joyent-minimal",
            InstanceBrand::NotApplicable => "not-applicable",
        }
    }

    /// Whether a VNC / framebuffer console is meaningful for this
    /// brand. Only the two HVM brands have a graphics device.
    #[must_use]
    pub fn supports_vnc(self) -> bool {
        matches!(self, InstanceBrand::Kvm | InstanceBrand::Bhyve)
    }
}

impl std::fmt::Display for InstanceBrand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod instance_brand_tests {
    use super::*;

    #[test]
    fn from_compat_brand_maps_known_values() {
        assert_eq!(InstanceBrand::from_compat_brand("kvm"), InstanceBrand::Kvm);
        assert_eq!(
            InstanceBrand::from_compat_brand("bhyve"),
            InstanceBrand::Bhyve
        );
        assert_eq!(InstanceBrand::from_compat_brand("lx"), InstanceBrand::Lx);
        assert_eq!(
            InstanceBrand::from_compat_brand("joyent-minimal"),
            InstanceBrand::JoyentMinimal
        );
    }

    #[test]
    fn from_compat_brand_unknown_is_not_applicable() {
        assert_eq!(
            InstanceBrand::from_compat_brand(""),
            InstanceBrand::NotApplicable
        );
        assert_eq!(
            InstanceBrand::from_compat_brand("docker"),
            InstanceBrand::NotApplicable
        );
        assert_eq!(InstanceBrand::default(), InstanceBrand::NotApplicable);
    }

    #[test]
    fn wire_names_and_vnc_support() {
        assert_eq!(InstanceBrand::JoyentMinimal.as_str(), "joyent-minimal");
        assert_eq!(InstanceBrand::Bhyve.to_string(), "bhyve");
        assert_eq!(
            serde_json::to_string(&InstanceBrand::JoyentMinimal).unwrap(),
            "\"joyent-minimal\""
        );
        assert!(InstanceBrand::Bhyve.supports_vnc());
        assert!(InstanceBrand::Kvm.supports_vnc());
        assert!(!InstanceBrand::Lx.supports_vnc());
        assert!(!InstanceBrand::JoyentMinimal.supports_vnc());
        assert!(!InstanceBrand::NotApplicable.supports_vnc());
    }

    #[test]
    fn unknown_string_round_trips_via_serde_other() {
        // A future brand string we don't know yet must deserialize to
        // NotApplicable rather than erroring (Type Safety Rule #5).
        let b: InstanceBrand = serde_json::from_str("\"some-future-brand\"").unwrap();
        assert_eq!(b, InstanceBrand::NotApplicable);
    }
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
/// omitted in v0: cloud-init userdata, tags/labels, affinity rules,
/// console URL, migration history. Each will land as the consuming
/// use case ships. The hosting CN is recorded once placement lands
/// so subsequent lifecycle jobs return to the same SmartOS host.
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
    /// SmartOS brand this instance's host-side VM runs as. Derived
    /// from the boot image's compatibility block at create time;
    /// `NotApplicable` for older records and explicit-fields images.
    #[serde(default)]
    pub brand: InstanceBrand,
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
    /// Optional boot-disk size in bytes. `None` (the default) sizes the
    /// boot disk via [`default_boot_disk_size_bytes`]; an explicit value
    /// is honored down to the image content size (see
    /// [`resolve_boot_disk_size_bytes`]) and bounded at the API edge by
    /// [`MAX_BOOT_DISK_BYTES`].
    #[serde(default)]
    pub disk_bytes: Option<u64>,
    /// Optional MAC address to pin on the primary NIC. Accepted in
    /// any case + any of the usual separator styles
    /// (`02:08:20:ab:cd:ef`, `02-08-20-AB-CD-EF`, `0208.20ab.cdef`).
    /// Used to opt into sticky-by-MAC IPAM (γ.2): if a reservation
    /// or prior lease in the parent VPC matches this MAC and that
    /// address is free, the instance is allocated that IP. None
    /// (the default) keeps the legacy auto-generated MAC behaviour.
    #[serde(default)]
    pub mac: Option<String>,
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
    /// First-time create only: the agent runs `vmadm create`.
    /// Powering on an already-provisioned (Stopped) instance uses
    /// [`JobKind::Start`] instead — re-running `vmadm create` on an
    /// existing zone fails with "VM already exists".
    Provision { instance_id: Uuid },
    /// Power on an already-provisioned instance that is Stopped:
    /// the agent runs `vmadm start <uuid>`. The zone and its Proteus
    /// ports already exist and persist across a power cycle (the same
    /// reason `Restart` re-realizes nothing), so the agent only boots
    /// the zone — no zone or port re-create. Drives the lifecycle
    /// Pending → Running.
    Start { instance_id: Uuid },
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
    /// Re-apply a single running port's compiled blueprint at its
    /// current (bumped) generation. Unlike `Provision`, the port
    /// already exists and is started, so the agent applies only -- no
    /// zone or port re-create. tritond owns the blueprint and the
    /// monotonic generation (`Store::get_port_generation`); the agent
    /// fetches and applies, never invents. Enqueued for every
    /// blueprint-affecting mutation on a running VM (FIP attach/detach,
    /// firewall, routes).
    ApplyPortBlueprint { instance_id: Uuid, nic_id: Uuid },
    /// Realize a CN-terminated 1:1 NAT floating IP on the hosting CN.
    /// Enqueued by the attach saga, pinned to `hosted_cn`. The agent
    /// applies the recomputed port blueprint (which lands the SetSrc /
    /// SetDst rules + the `hosted_fips` delta), ensures the external
    /// link, adds the `<fip>/32` ipadm alias, and fires a gratuitous
    /// ARP burst. `instance_id` pins the job to the VM the FIP attaches
    /// to so the per-VM job view surfaces it; `nic_id` is the OPTE port
    /// to recompute; `generation` is the bumped port generation the
    /// blueprint carries so a stale re-apply is fenced.
    FipClaim {
        floating_ip_id: Uuid,
        nic_id: Uuid,
        instance_id: Uuid,
        fip_addr: String,
        external_nic_tag: Option<String>,
        /// VLAN of the FIP's external subnet. The nic_tag is the
        /// physical-link identity (resolved to a link on the CN); the
        /// VLAN lives on the network (legacy SDC model). The agent
        /// creates/reuses the per-(link,vlan) `fipN` vnic over the
        /// nic_tag's link. `None` = untagged.
        vlan_id: Option<u16>,
        generation: u64,
    },
    /// Withdraw a CN-terminated floating IP from a CN. Enqueued by the
    /// attach saga's undo, the detach saga, the FIP-delete-while-hosted
    /// path, and the instance-delete cascade — always pinned to the CN
    /// that was hosting the termination. The agent invalidates the
    /// `hosted_fips` entry, removes the ipadm alias, and (for a
    /// surviving port) re-applies the withdrawn blueprint. Carries no
    /// `instance_id`: by release time the attachment / instance may
    /// already be gone, so `target_id()` falls back to
    /// `floating_ip_id`.
    FipRelease {
        floating_ip_id: Uuid,
        fip_addr: String,
        external_nic_tag: Option<String>,
        /// VLAN of the FIP's external subnet, so the agent finds the
        /// same `fipN` vnic the alias was added to on claim.
        vlan_id: Option<u16>,
        hosted_cn: Uuid,
    },
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

    // ----- LM-5 live-migration jobs -----
    //
    // The migration saga enqueues these on the source agent +
    // target agent (the `role` discriminates) so the existing
    // claim/complete mechanism drives the heavy lifting. The
    // saga awaits each job's terminal status via the standard
    // `wait_for_provisioning_job_terminal` pattern; the agents
    // perform the actual zfs / bhyve / proteus work.
    /// One pass of `zfs send`/`zfs recv` between source and target
    /// agents (uses the LM-4 dataset-stream WebSocket). The saga
    /// enqueues two of these (one on source, one on target) per
    /// snapshot round. `dataset` lets the receiver pick the
    /// destination dataset name; `from_snap`/`to_snap` shape the
    /// `zfs send -i from to` command on the source side.
    MigrateZfsSend {
        migration_id: Uuid,
        instance_id: Uuid,
        role: MigrationJobRole,
        dataset: String,
        /// `None` on the initial send; `Some(@migration-prev)` on
        /// incremental rounds.
        #[serde(default)]
        from_snap: Option<String>,
        to_snap: String,
        /// Source-only: `wss://<target_admin_ip>:<port>` base for
        /// the dial. The target side of the pair leaves this
        /// `None` — its handler listens, doesn't dial. The saga
        /// reads `Cn.admin_ip` + `migrate_listen_port` for the
        /// target CN and writes this when enqueuing the Source
        /// job.
        #[serde(default)]
        peer_endpoint: Option<String>,
        /// Source-only: lowercase-hex SHA-256 of the target's
        /// migrate-listener leaf-cert SPKI. The saga reads
        /// `Cn.console_tls_spki_sha256` (the migrate listener
        /// reuses the same cert as the console listener). The
        /// dialer pins it so an admin-IP hijack can't MITM.
        #[serde(default)]
        peer_spki_sha256_hex: Option<String>,
        /// Source-only: HS256 migrate-ticket minted by the saga
        /// with `MigrateRole::ZfsSource` using the *target* CN's
        /// `migrate_ticket_key`. The target's listener verifies
        /// the ticket on the WS upgrade; a bad / expired ticket
        /// surfaces as a 401 the dial reports as
        /// `ConnectionRefused`. ~10 min TTL.
        #[serde(default)]
        ticket: Option<String>,
    },
    /// One run of the bhyve memory-stream state machine
    /// (`OutboundMigration` on source, `InboundMigration` on
    /// target) over the LM-2/LM-3 memory-channel WebSocket.
    MigrateVmmStream {
        migration_id: Uuid,
        instance_id: Uuid,
        role: MigrationJobRole,
        /// Source-only: `wss://<target_admin_ip>:<port>` base for
        /// the dial. Same contract as the
        /// [`JobKind::MigrateZfsSend`] trio: the target side of the
        /// pair listens, so it leaves all three `None`.
        #[serde(default)]
        peer_endpoint: Option<String>,
        /// Source-only: lowercase-hex SHA-256 of the target's
        /// migrate-listener leaf-cert SPKI, pinned by the dialer.
        #[serde(default)]
        peer_spki_sha256_hex: Option<String>,
        /// Source-only: HS256 migrate-ticket minted with the
        /// *target* CN's `migrate_ticket_key`
        /// (`MigrateRole::Outbound`). ~10 min TTL.
        #[serde(default)]
        ticket: Option<String>,
    },
    /// Create the target-side zone shell for a migration: `vmadm
    /// create` with `autoboot=false`, no image ensure, Proteus ports
    /// paused, then destroy the vmadm-created datasets so the first
    /// `zfs recv` lands on a clean slate. Distinct from
    /// [`JobKind::Provision`] because a normal provision boots the
    /// guest and realizes the network — both forbidden while the
    /// source instance still owns the identity.
    MigrationProvisionTarget {
        migration_id: Uuid,
        instance_id: Uuid,
    },
    /// One leg of the legacy-compat quota dance: `zfs send` of a
    /// dataset whose `quota`/`refreservation` are set can fail at
    /// recv time, so the saga clears them on the source up front
    /// (`SaveAndClear`, which reports the original values back via
    /// the job `result`) and re-applies them on whichever side ends
    /// up owning the dataset (`Restore`).
    MigrateQuotaDance {
        migration_id: Uuid,
        instance_id: Uuid,
        /// Dataset the dance applies to (`zones/<instance>`).
        dataset: String,
        op: QuotaDanceOp,
    },
    /// Live-path source quiesce: bhyve.sock `pause-devices` →
    /// `pause-vm` → `drain-devices`, leaving the guest paused so
    /// the final ZFS increment and the RAM stream see a frozen
    /// machine. The job `result` carries `pause_complete_ts`.
    MigratePauseSource {
        migration_id: Uuid,
        instance_id: Uuid,
    },
    /// Undo of [`JobKind::MigratePauseSource`]: resume the paused
    /// source guest after a pre-switch failure. Never enqueued
    /// after `SwitchComplete` — the target owns the guest then.
    MigrateResumeSource {
        migration_id: Uuid,
        instance_id: Uuid,
    },
    /// Boot the target-side zone in bhyve listen mode so the
    /// inbound memory stream has a vmm to import into. The agent
    /// polls the zone's bhyve.sock until the listener is up.
    MigrateTargetListen {
        migration_id: Uuid,
        instance_id: Uuid,
    },
    /// Proteus port activation on the target (`start_port`) after
    /// the cutover fence completes. Source-side equivalent is
    /// [`ProteusDeactivate`].
    ProteusActivate {
        migration_id: Uuid,
        instance_id: Uuid,
        nic_ids: Vec<Uuid>,
    },
    /// Proteus port deactivation on the source (`pause_port` →
    /// `delete_port`). Runs in parallel with target-side
    /// [`ProteusActivate`] after the FDB ownership flip; loss of
    /// the deactivation surfaces as an audit warning, not a
    /// migration failure, because the target is already canonical.
    ProteusDeactivate {
        migration_id: Uuid,
        instance_id: Uuid,
        nic_ids: Vec<Uuid>,
    },
    /// Tear down the target-side zone + dataset on saga unwind
    /// (pre-switch abort). Best-effort: a target CN that's
    /// unreachable surfaces as an audit warning so an operator
    /// can clean up by hand.
    MigrationCleanupTarget {
        migration_id: Uuid,
        instance_id: Uuid,
    },
    /// Tear down the source-side zone + dataset after a successful
    /// cutover (post-switch cleanup). Same best-effort semantics
    /// as [`MigrationCleanupTarget`].
    MigrationCleanupSource {
        migration_id: Uuid,
        instance_id: Uuid,
    },
    /// Grow a disk's backing zvol on the hosting CN and enlarge the
    /// VM's flexible-disk pool to cover it. Enqueued by the disk-resize
    /// handler *after* it has grown the durable `Disk` record, pinned to
    /// the instance's `host_cn_uuid`. `size_bytes` is the new total disk
    /// size; the agent grows the pool then the boot zvol to match. A
    /// running guest realizes the new capacity on its next reboot.
    /// `disk_id` identifies the record for audit and idempotency.
    ResizeDisk {
        instance_id: Uuid,
        disk_id: Uuid,
        size_bytes: u64,
    },
}

/// Which side of a migration a [`JobKind`] runs on. Distinct from
/// [`crate::MigrationPhase`] / [`tritond_auth::MigrateRole`]: the
/// phase + ticket-role describe state-machine + auth scope, while
/// this just tells the agent's job dispatcher which arm to take
/// (e.g. spawn `zfs send` vs `zfs recv`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum MigrationJobRole {
    Source,
    Target,
}

/// Which leg of the quota dance a [`JobKind::MigrateQuotaDance`]
/// job performs. `Restore` carries the values to re-apply rather
/// than re-reading them on the agent because the restore can run
/// on the *target* CN, where the original source-side properties
/// were never visible (`zfs recv -x quota -x refreservation`
/// strips them from the stream).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum QuotaDanceOp {
    /// Read the dataset's current `quota`/`refreservation`, report
    /// them in the job `result` (shape:
    /// [`QuotaDanceSaveResult`]), then clear both.
    SaveAndClear,
    /// Re-apply previously saved values. `None` means the property
    /// was unset on the source and stays unset.
    Restore {
        #[serde(default)]
        quota_bytes: Option<u64>,
        #[serde(default)]
        refreservation_bytes: Option<u64>,
    },
}

/// Job `result` payload contract for a successful
/// [`QuotaDanceOp::SaveAndClear`]. The agent serializes this; the
/// migration saga deserializes it to fill
/// [`SourceFilesystemDetails::original_quota_bytes`] /
/// `original_refreservation_bytes`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct QuotaDanceSaveResult {
    /// Original `quota` in bytes; `None`/missing when unset.
    #[serde(default)]
    pub quota_bytes: Option<u64>,
    /// Original `refreservation` in bytes; `None`/missing when unset.
    #[serde(default)]
    pub refreservation_bytes: Option<u64>,
}

/// Job `result` payload contract for a successful source-side
/// [`JobKind::MigrateZfsSend`]. The saga's sync-convergence loop
/// reads `bytes_streamed` to decide whether another incremental
/// round is worth it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ZfsSendResult {
    /// Bytes the `zfs send` stream produced for this pass.
    #[serde(default)]
    pub bytes_streamed: u64,
}

impl JobKind {
    /// Extract the target VM instance id when this is an
    /// instance-lifecycle job. Migration jobs also carry an
    /// instance_id (the VM being migrated) and surface it here so
    /// the existing per-VM job listings include them without a
    /// JobKind-specific match.
    #[must_use]
    pub fn instance_id(&self) -> Option<Uuid> {
        match self {
            JobKind::Provision { instance_id }
            | JobKind::Start { instance_id }
            | JobKind::Stop { instance_id }
            | JobKind::Restart { instance_id }
            | JobKind::Delete { instance_id } => Some(*instance_id),
            JobKind::ApplyPortBlueprint { instance_id, .. }
            | JobKind::FipClaim { instance_id, .. }
            | JobKind::MigrateZfsSend { instance_id, .. }
            | JobKind::MigrateVmmStream { instance_id, .. }
            | JobKind::MigrationProvisionTarget { instance_id, .. }
            | JobKind::MigrateQuotaDance { instance_id, .. }
            | JobKind::MigratePauseSource { instance_id, .. }
            | JobKind::MigrateResumeSource { instance_id, .. }
            | JobKind::MigrateTargetListen { instance_id, .. }
            | JobKind::ProteusActivate { instance_id, .. }
            | JobKind::ProteusDeactivate { instance_id, .. }
            | JobKind::MigrationCleanupTarget { instance_id, .. }
            | JobKind::MigrationCleanupSource { instance_id, .. }
            | JobKind::ResizeDisk { instance_id, .. } => Some(*instance_id),
            // FipRelease carries no instance_id (the attachment may be
            // gone by release time); see `target_id` for its fallback.
            JobKind::FipRelease { .. } | JobKind::EdgeApply { .. } | JobKind::EdgeReap { .. } => {
                None
            }
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
            | JobKind::Start { .. }
            | JobKind::Stop { .. }
            | JobKind::Restart { .. }
            | JobKind::Delete { .. }
            | JobKind::ApplyPortBlueprint { .. }
            | JobKind::FipClaim { .. }
            | JobKind::FipRelease { .. }
            | JobKind::MigrateZfsSend { .. }
            | JobKind::MigrateVmmStream { .. }
            | JobKind::MigrationProvisionTarget { .. }
            | JobKind::MigrateQuotaDance { .. }
            | JobKind::MigratePauseSource { .. }
            | JobKind::MigrateResumeSource { .. }
            | JobKind::MigrateTargetListen { .. }
            | JobKind::ProteusActivate { .. }
            | JobKind::ProteusDeactivate { .. }
            | JobKind::MigrationCleanupTarget { .. }
            | JobKind::MigrationCleanupSource { .. }
            | JobKind::ResizeDisk { .. } => None,
        }
    }

    /// Extract the migration record id for migration jobs.
    /// Returns `None` for non-migration jobs. Used by the
    /// `migration/by_id/<uuid>/progress` event-log writer to
    /// surface job-lifecycle events on the migration record's
    /// timeline.
    #[must_use]
    pub fn migration_id(&self) -> Option<Uuid> {
        match self {
            JobKind::MigrateZfsSend { migration_id, .. }
            | JobKind::MigrateVmmStream { migration_id, .. }
            | JobKind::MigrationProvisionTarget { migration_id, .. }
            | JobKind::MigrateQuotaDance { migration_id, .. }
            | JobKind::MigratePauseSource { migration_id, .. }
            | JobKind::MigrateResumeSource { migration_id, .. }
            | JobKind::MigrateTargetListen { migration_id, .. }
            | JobKind::ProteusActivate { migration_id, .. }
            | JobKind::ProteusDeactivate { migration_id, .. }
            | JobKind::MigrationCleanupTarget { migration_id, .. }
            | JobKind::MigrationCleanupSource { migration_id, .. } => Some(*migration_id),
            _ => None,
        }
    }

    /// Stable target id used for logs and queue diagnostics. Total
    /// over every variant: instance jobs surface their `instance_id`,
    /// edge jobs their `edge_instance_id`, and `FipRelease` (which
    /// carries neither, because its attachment may be gone) falls back
    /// to its `floating_ip_id`. The historical `.expect(...)` panicked
    /// for any variant with no instance/edge id, so this fallback keeps
    /// `target_id` from ever aborting the process.
    #[must_use]
    pub fn target_id(&self) -> Uuid {
        self.instance_id()
            .or_else(|| self.edge_instance_id())
            .or_else(|| match self {
                JobKind::FipRelease { floating_ip_id, .. } => Some(*floating_ip_id),
                _ => None,
            })
            .unwrap_or_else(Uuid::nil)
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
        assert_eq!(
            JobKind::Start { instance_id }.instance_id(),
            Some(instance_id)
        );
        assert_eq!(JobKind::EdgeReap { edge_instance_id }.instance_id(), None);
    }

    #[test]
    fn migration_jobs_carry_instance_and_migration_ids() {
        let migration_id = Uuid::new_v4();
        let instance_id = Uuid::new_v4();
        let kind = JobKind::MigrateVmmStream {
            migration_id,
            instance_id,
            role: MigrationJobRole::Source,
            peer_endpoint: None,
            peer_spki_sha256_hex: None,
            ticket: None,
        };
        assert_eq!(kind.instance_id(), Some(instance_id));
        assert_eq!(kind.migration_id(), Some(migration_id));
        assert_eq!(kind.edge_instance_id(), None);
        // Non-migration jobs return None for migration_id.
        assert_eq!(JobKind::Provision { instance_id }.migration_id(), None);
    }

    #[test]
    fn migrate_vmm_stream_decodes_without_peer_fields() {
        // Wire-compat: a MigrateVmmStream enqueued by a pre-peer-trio
        // tritond must still decode (all three fields default None).
        let json = serde_json::json!({
            "kind": "migrate_vmm_stream",
            "migration_id": Uuid::new_v4(),
            "instance_id": Uuid::new_v4(),
            "role": "target",
        });
        let decoded: JobKind = serde_json::from_value(json).unwrap();
        assert!(matches!(
            decoded,
            JobKind::MigrateVmmStream {
                peer_endpoint: None,
                peer_spki_sha256_hex: None,
                ticket: None,
                ..
            }
        ));
    }

    #[test]
    fn migrate_quota_dance_round_trips_both_ops() {
        let save = JobKind::MigrateQuotaDance {
            migration_id: Uuid::new_v4(),
            instance_id: Uuid::new_v4(),
            dataset: "zones/abcd".to_string(),
            op: QuotaDanceOp::SaveAndClear,
        };
        let json = serde_json::to_value(&save).unwrap();
        assert_eq!(json["kind"].as_str(), Some("migrate_quota_dance"));
        assert_eq!(json["op"]["kind"].as_str(), Some("save_and_clear"));
        let decoded: JobKind = serde_json::from_value(json).unwrap();
        assert_eq!(decoded, save);

        let restore = JobKind::MigrateQuotaDance {
            migration_id: Uuid::new_v4(),
            instance_id: Uuid::new_v4(),
            dataset: "zones/abcd".to_string(),
            op: QuotaDanceOp::Restore {
                quota_bytes: Some(10 * 1024 * 1024 * 1024),
                refreservation_bytes: None,
            },
        };
        let json = serde_json::to_value(&restore).unwrap();
        assert_eq!(json["op"]["kind"].as_str(), Some("restore"));
        let decoded: JobKind = serde_json::from_value(json).unwrap();
        assert_eq!(decoded, restore);
    }

    #[test]
    fn new_migration_job_kinds_carry_ids() {
        let migration_id = Uuid::new_v4();
        let instance_id = Uuid::new_v4();
        for kind in [
            JobKind::MigrationProvisionTarget {
                migration_id,
                instance_id,
            },
            JobKind::MigratePauseSource {
                migration_id,
                instance_id,
            },
            JobKind::MigrateResumeSource {
                migration_id,
                instance_id,
            },
            JobKind::MigrateTargetListen {
                migration_id,
                instance_id,
            },
        ] {
            assert_eq!(kind.instance_id(), Some(instance_id));
            assert_eq!(kind.migration_id(), Some(migration_id));
            let json = serde_json::to_value(&kind).unwrap();
            let decoded: JobKind = serde_json::from_value(json).unwrap();
            assert_eq!(decoded, kind);
        }
    }

    #[test]
    fn migrate_zfs_send_round_trips_with_increment_fields() {
        let kind = JobKind::MigrateZfsSend {
            migration_id: Uuid::new_v4(),
            instance_id: Uuid::new_v4(),
            role: MigrationJobRole::Source,
            dataset: "zones/abcd".to_string(),
            from_snap: Some("zones/abcd@migration-base".to_string()),
            to_snap: "zones/abcd@migration-final".to_string(),
            peer_endpoint: Some("wss://10.0.0.40:4568".to_string()),
            peer_spki_sha256_hex: Some("ab".repeat(32)),
            ticket: Some("jwt-stub".to_string()),
        };
        let json = serde_json::to_value(&kind).unwrap();
        assert_eq!(json["kind"].as_str(), Some("migrate_zfs_send"));
        assert_eq!(json["role"].as_str(), Some("source"));
        let decoded: JobKind = serde_json::from_value(json).unwrap();
        assert_eq!(decoded, kind);
    }

    #[test]
    fn migrate_zfs_send_target_round_trips_without_peer_fields() {
        // Target side: `peer_endpoint`, `peer_spki_sha256_hex`,
        // `ticket` all None. The agent's target arm doesn't dial;
        // it just waits for the inbound connection to its own
        // listener.
        let kind = JobKind::MigrateZfsSend {
            migration_id: Uuid::new_v4(),
            instance_id: Uuid::new_v4(),
            role: MigrationJobRole::Target,
            dataset: "zones/abcd".to_string(),
            from_snap: None,
            to_snap: "zones/abcd@migration-base".to_string(),
            peer_endpoint: None,
            peer_spki_sha256_hex: None,
            ticket: None,
        };
        let json = serde_json::to_value(&kind).unwrap();
        let decoded: JobKind = serde_json::from_value(json).unwrap();
        assert_eq!(decoded, kind);
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

    #[test]
    fn fip_claim_carries_instance_and_round_trips() {
        let floating_ip_id = Uuid::new_v4();
        let nic_id = Uuid::new_v4();
        let instance_id = Uuid::new_v4();
        let kind = JobKind::FipClaim {
            floating_ip_id,
            nic_id,
            instance_id,
            fip_addr: "192.0.2.10".to_string(),
            external_nic_tag: Some("external".to_string()),
            vlan_id: Some(2003),
            generation: 4,
        };
        assert_eq!(kind.instance_id(), Some(instance_id));
        assert_eq!(kind.edge_instance_id(), None);
        assert_eq!(kind.migration_id(), None);
        // FipClaim resolves its target to the pinned instance.
        assert_eq!(kind.target_id(), instance_id);

        let json = serde_json::to_value(&kind).unwrap();
        assert_eq!(json["kind"].as_str(), Some("fip_claim"));
        let decoded: JobKind = serde_json::from_value(json).unwrap();
        assert_eq!(decoded, kind);
    }

    #[test]
    fn fip_release_target_id_does_not_panic() {
        // FipRelease carries no instance/edge id; `target_id` must
        // fall back to the floating_ip_id rather than panic on the
        // historical `.expect(...)`.
        let floating_ip_id = Uuid::new_v4();
        let hosted_cn = Uuid::new_v4();
        let kind = JobKind::FipRelease {
            floating_ip_id,
            fip_addr: "192.0.2.10".to_string(),
            external_nic_tag: None,
            vlan_id: None,
            hosted_cn,
        };
        assert_eq!(kind.instance_id(), None);
        assert_eq!(kind.edge_instance_id(), None);
        assert_eq!(kind.migration_id(), None);
        // The load-bearing assertion: total, no panic.
        assert_eq!(kind.target_id(), floating_ip_id);

        let json = serde_json::to_value(&kind).unwrap();
        assert_eq!(json["kind"].as_str(), Some("fip_release"));
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
    /// Free-form result payload the completing agent attached.
    /// Opaque to the queue; the enqueuing orchestrator defines the
    /// per-kind contract (e.g. [`QuotaDanceSaveResult`],
    /// [`ZfsSendResult`]). `None` for kinds with nothing to report
    /// and for completions from agents predating the field.
    #[serde(default)]
    pub result: Option<serde_json::Value>,
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
/// Wire-stable: rides as the body of `POST /v1/agent/jobs/{id}/complete`
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

/// SmartOS `internal_metadata` key carrying the canonical tritond
/// instance UUID for a managed zone.
pub const TRITOND_METADATA_INSTANCE_ID: &str = "tritond:instance_id";
/// SmartOS `internal_metadata` key carrying the tenant UUID a
/// managed zone belongs to.
pub const TRITOND_METADATA_TENANT_ID: &str = "tritond:tenant_id";
/// SmartOS `internal_metadata` key carrying the project UUID a
/// managed zone belongs to.
pub const TRITOND_METADATA_PROJECT_ID: &str = "tritond:project_id";
/// SmartOS `internal_metadata` key carrying the lowercase-hex
/// HMAC-SHA256 tag over `(instance_id, tenant_id, project_id)`,
/// signed with the per-deployment identity HMAC key.
pub const TRITOND_METADATA_IDENTITY_HMAC: &str = "tritond:identity_hmac";

/// Tamper-evident identity for a tritond-managed zone.
///
/// Minted by tritond at blueprint-fetch time using the per-deployment
/// HMAC key (see `tritond_auth::IdentityHmacKey`). Tritonagent stamps
/// the four fields verbatim into the zone's SmartOS
/// `internal_metadata` (the four `TRITOND_METADATA_*` keys above)
/// inside the same `vmadm create` payload that brings the zone into
/// existence -- so a status report cannot fire between zone creation
/// and identity write.
///
/// On a later status report, the classifier reads the four fields
/// out of the report's `internal_metadata`, recomputes the HMAC
/// from the reported triple, and compares constant-time. A
/// mismatch (or missing tag) means the metadata was tampered with
/// in-zone via `mdata-put` or copied from another deployment, and
/// the zone is quarantined as `StaleFingerprint` rather than
/// treated as managed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ManagedIdentity {
    /// Tritond `Instance.id`. Reused as the SmartOS zone UUID at
    /// `vmadm create` time.
    pub instance_id: Uuid,
    pub tenant_id: Uuid,
    pub project_id: Uuid,
    /// Lowercase hex of HMAC-SHA256 over the canonical
    /// `(instance_id, tenant_id, project_id)` triple. Signed with
    /// the per-deployment `IdentityHmac` system key.
    pub identity_hmac: String,
}

impl ManagedIdentity {
    /// Render this identity as the four `internal_metadata` entries
    /// tritonagent must fold into the `vmadm create` payload.
    /// Returned as `(key, value)` pairs in a stable order so test
    /// assertions can compare exact JSON.
    #[must_use]
    pub fn as_internal_metadata(&self) -> [(&'static str, String); 4] {
        [
            (TRITOND_METADATA_INSTANCE_ID, self.instance_id.to_string()),
            (TRITOND_METADATA_TENANT_ID, self.tenant_id.to_string()),
            (TRITOND_METADATA_PROJECT_ID, self.project_id.to_string()),
            (TRITOND_METADATA_IDENTITY_HMAC, self.identity_hmac.clone()),
        ]
    }
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

/// Sticky-aware variant: if `prefer` is `Some(ip)`, the IP lives
/// inside `cidr`, isn't a reserved network/gateway/broadcast slot,
/// and isn't already allocated, return it. Otherwise fall back to
/// [`allocate_ipv4`]'s linear scan. Used by γ.2 to honor DHCP
/// reservations and prior leases at instance-create time.
#[must_use]
pub fn allocate_ipv4_sticky(
    cidr: Ipv4Network,
    already_allocated: &HashSet<Ipv4Addr>,
    prefer: Option<Ipv4Addr>,
) -> Option<Ipv4Addr> {
    if let Some(want) = prefer {
        if cidr.contains(want) {
            let network = cidr.network();
            let broadcast = cidr.broadcast();
            let gateway = next_ipv4(network);
            let is_reserved =
                want == network || want == broadcast || gateway.map(|g| g == want).unwrap_or(false);
            if !is_reserved && !already_allocated.contains(&want) {
                return Some(want);
            }
        }
    }
    allocate_ipv4(cidr, already_allocated)
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

/// Allocate the lowest unused IPv4 inside `cidr` restricted to the
/// inclusive `[start, end]` provision window. `None` bounds fall back
/// to the block's first / last usable host. Skips the reserved
/// network / gateway / broadcast slots like [`allocate_ipv4`] and
/// checks the same `already_allocated` set so external allocation
/// stays collision-free with floating IPs and NAT gateways on the
/// single global index. Returns `None` if every address in the window
/// is taken.
#[must_use]
pub fn allocate_ipv4_in_range(
    cidr: Ipv4Network,
    start: Option<Ipv4Addr>,
    end: Option<Ipv4Addr>,
    already_allocated: &HashSet<Ipv4Addr>,
) -> Option<Ipv4Addr> {
    let network = cidr.network();
    let broadcast = cidr.broadcast();
    let gateway = next_ipv4(network)?;
    let lo = start.unwrap_or(network);
    let hi = end.unwrap_or(broadcast);
    if u32::from(lo) > u32::from(hi) {
        return None;
    }
    for candidate in cidr.iter() {
        if candidate < lo || candidate > hi {
            continue;
        }
        if candidate == network || candidate == gateway || candidate == broadcast {
            continue;
        }
        if !already_allocated.contains(&candidate) {
            return Some(candidate);
        }
    }
    None
}

/// IPv6 sibling of [`allocate_ipv4_in_range`]. Walks from the window's
/// lower bound (or the block gateway + 1) upward and stops at the
/// first free address that is still inside `cidr` and `<= end`.
#[must_use]
pub fn allocate_ipv6_in_range(
    cidr: Ipv6Network,
    start: Option<Ipv6Addr>,
    end: Option<Ipv6Addr>,
    already_allocated: &HashSet<Ipv6Addr>,
) -> Option<Ipv6Addr> {
    let network = cidr.network();
    let gateway = next_ipv6(network)?;
    // Lower bound: the requested start, or the first host after the
    // gateway. Never hand out the network or gateway slot.
    let mut candidate = match start {
        Some(s) if s > gateway => s,
        _ => next_ipv6(gateway)?,
    };
    loop {
        if !cidr.contains(candidate) {
            return None;
        }
        if let Some(hi) = end
            && candidate > hi
        {
            return None;
        }
        if candidate != network && candidate != gateway && !already_allocated.contains(&candidate) {
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

/// Normalise a MAC address to canonical lowercase colon-separated form
/// (e.g. `02:08:20:ab:cd:ef`). Accepts colon, hyphen, dot, or
/// no-separator input. Rejects anything that isn't 12 hex digits.
///
/// Shared between [`crate::MemStore`] and [`crate::FdbStore`] so MAC
/// canonicalisation is byte-identical across backends — important for
/// the DHCP keyspace where the canonical form is part of the key.
pub fn canonical_mac(s: &str) -> Result<String, crate::StoreError> {
    let cleaned: String = s
        .chars()
        .filter(|c| !matches!(*c, ':' | '-' | '.' | ' '))
        .collect();
    if cleaned.len() != 12 || !cleaned.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(crate::StoreError::Conflict(format!(
            "mac {s:?} is not 12 hex digits"
        )));
    }
    let lower = cleaned.to_ascii_lowercase();
    let mut out = String::with_capacity(17);
    for (i, c) in lower.chars().enumerate() {
        if i > 0 && i % 2 == 0 {
            out.push(':');
        }
        out.push(c);
    }
    Ok(out)
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
    /// External Subnet this address was allocated from, when the FIP
    /// was created via the pool/network path (C-3). `None` for legacy
    /// `family`-allocated records. Drives `external_nic_tag`.
    #[serde(default)]
    pub network_id: Option<Uuid>,
    /// External nic_tag the dataplane egresses/ingresses this FIP on.
    /// Derived read-only from `network_id`'s subnet `nic_tag` at create
    /// time — never client-set, so a FIP can never carry a nic_tag
    /// inconsistent with its subnet (invariant 17).
    #[serde(default)]
    pub external_nic_tag: Option<Uuid>,
    /// CN currently hosting this FIP's 1:1 NAT termination. Stamped on
    /// `attach` from the target instance's `host_cn_uuid`, cleared on
    /// `detach` and by the instance-delete cascade. Pins the dataplane
    /// claim/release jobs to a concrete CN.
    #[serde(default)]
    pub hosted_cn: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Request body for allocating a new FloatingIp. The address source
/// is one of three mutually exclusive selectors:
///
/// * `network_id` — allocate the lowest-free address from a specific
///   External subnet (C-3, preferred);
/// * `pool_id` — walk a [`NetworkPool`]'s ordered networks and take
///   the first free address (C-3);
/// * `family` — the legacy Phase-0 path that draws from the hardcoded
///   `FLOATING_IP_V*_POOL`.
///
/// At most one may be set; the server rejects a request with more
/// than one selector ([`StoreError::Conflict`]). All three are
/// `#[serde(default)]` so the additive `network_id` / `pool_id`
/// fields preserve wire back-compat with pre-C-3 `family`-only bodies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NewFloatingIp {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// Legacy Phase-0 family selector. Mutually exclusive with
    /// `network_id` / `pool_id`. `None` only when one of those is set.
    #[serde(default)]
    pub family: Option<AddressFamily>,
    /// External subnet to allocate from. Stamps `FloatingIp::network_id`
    /// and derives `external_nic_tag`. Mutually exclusive with the
    /// other two selectors.
    #[serde(default)]
    pub network_id: Option<Uuid>,
    /// Network pool to allocate from (walks its ordered networks).
    /// Mutually exclusive with the other two selectors.
    #[serde(default)]
    pub pool_id: Option<Uuid>,
}

/// Which address source a [`NewFloatingIp`] selected. The three
/// selectors are mutually exclusive; [`floating_ip_source`] enforces
/// that and projects them into this enum. Shared by MemStore and
/// FdbStore so the reject and dispatch stay byte-identical. `Copy` so
/// the FdbStore retry closure (an `Fn`) can match it each attempt.
#[derive(Clone, Copy)]
pub(crate) enum FloatingIpSource {
    Family(AddressFamily),
    Network(Uuid),
    Pool(Uuid),
}

/// Validate the mutually-exclusive `family` / `network_id` / `pool_id`
/// selectors on a [`NewFloatingIp`]. Exactly one must be set; zero or
/// more than one is a [`StoreError::Conflict`].
pub(crate) fn floating_ip_source(
    req: &NewFloatingIp,
) -> Result<FloatingIpSource, crate::StoreError> {
    let set = usize::from(req.family.is_some())
        + usize::from(req.network_id.is_some())
        + usize::from(req.pool_id.is_some());
    match set {
        1 => {
            if let Some(family) = req.family {
                Ok(FloatingIpSource::Family(family))
            } else if let Some(network_id) = req.network_id {
                Ok(FloatingIpSource::Network(network_id))
            } else {
                Ok(FloatingIpSource::Pool(req.pool_id.expect("set == 1")))
            }
        }
        0 => Err(crate::StoreError::Conflict(
            "floating ip request must set exactly one of family / network_id / pool_id".to_string(),
        )),
        _ => Err(crate::StoreError::Conflict(
            "floating ip family / network_id / pool_id are mutually exclusive".to_string(),
        )),
    }
}

/// The address family an External subnet hands out, preferring IPv4
/// when the subnet carries both blocks. `None` if it carries neither.
#[must_use]
pub(crate) fn subnet_family(subnet: &Subnet) -> Option<AddressFamily> {
    if subnet.ipv4_block.is_some() {
        Some(AddressFamily::V4)
    } else if subnet.ipv6_block.is_some() {
        Some(AddressFamily::V6)
    } else {
        None
    }
}

/// Cluster-level system keys. Phase 0 ships two: the JWT signing
/// key, and the per-deployment HMAC key used to stamp tritond
/// identity into managed-zone metadata. Future entries will include
/// the transit-engine master key and any per-silo OIDC client
/// secrets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum SystemKey {
    /// 32-byte HS256 secret used to sign and validate operator JWTs.
    JwtSigning,
    /// 32-byte HMAC-SHA256 secret used to sign the
    /// `(instance_id, tenant_id, project_id)` triple stamped into
    /// SmartOS `internal_metadata` for managed zones, and to verify
    /// that triple in CN status reports.
    IdentityHmac,
}

impl SystemKey {
    /// Stable storage tag, used as the FDB key suffix.
    pub fn tag(self) -> &'static str {
        match self {
            SystemKey::JwtSigning => "jwt_signing",
            SystemKey::IdentityHmac => "identity_hmac",
        }
    }
}

// ---------------------------------------------------------------------
// Cluster settings (`Settings`)
// ---------------------------------------------------------------------

/// Cluster-default `config/imds/enabled` when no scope in the
/// silo/tenant/project/instance chain pins one. Mirrors the value
/// [`Settings::imds_enabled_default`] starts at; `tcadm config set
/// imds.enabled_default false` flips it cluster-wide without
/// touching any per-scope key.
pub const DEFAULT_IMDS_ENABLED: bool = true;
/// Cluster-default `config/imds/hop-limit` (1) — same role as
/// [`DEFAULT_IMDS_ENABLED`], surfaced through
/// [`Settings::imds_hop_limit_default`].
pub const DEFAULT_IMDS_HOP_LIMIT: u64 = 1;
/// Cluster-default for whether tritonagent manages the bhyve memory
/// reservoir (RFD 0185), surfaced through
/// [`Settings::reservoir_enabled_default`]. A per-CN
/// [`CnPlacement::reservoir_enabled`] overrides it.
pub const DEFAULT_RESERVOIR_ENABLED: bool = true;
/// Cluster-default fraction of physical RAM the reservoir floor targets,
/// surfaced through [`Settings::reservoir_percent_default`]. A per-CN
/// [`CnPlacement::reservoir_percent`] overrides it. tritonagent clamps
/// the effective value to `[0.0, 1.0]` and the kernel reservoir limit.
pub const DEFAULT_RESERVOIR_PERCENT: f32 = 0.80;

/// Default cadence of the stale-claim sweeper, in seconds.
pub const DEFAULT_SWEEPER_INTERVAL_SECS: u64 = 60;
/// Default age (seconds) a job claim must reach before the sweeper reaps it.
pub const DEFAULT_STALE_CLAIM_THRESHOLD_SECS: u64 = 600;
/// Default cadence of the DHCP lease reconciler, in seconds.
pub const DEFAULT_DHCP_RECONCILE_INTERVAL_SECS: u64 = 300;
/// Default `now - last_activity` (seconds) before a DHCP lease is GC-eligible.
pub const DEFAULT_DHCP_LEASE_GC_THRESHOLD_SECS: u64 = 7 * 24 * 60 * 60;

/// Default retention (seconds) for terminal sagas in FDB before the
/// sweeper's retention pass prunes them. 30 days; tunable per
/// [`Settings::saga_retention_secs`] / `TRITOND_SAGA_RETENTION_SECS`.
/// RFD 00004 SG-4.
pub const DEFAULT_SAGA_RETENTION_SECS: u64 = 30 * 24 * 60 * 60;

/// Default convergence threshold for the migration saga's
/// incremental-sync loop, in bytes. When a sync round streams less
/// than this, the next round is unlikely to shrink the final
/// (guest-down) increment meaningfully, so the saga moves to
/// quiesce. 50 MiB matches the legacy sdc-migrate heuristic.
pub const DEFAULT_MIGRATION_SYNC_DELTA_THRESHOLD_BYTES: u64 = 50 * 1024 * 1024;
/// Default cap on migration sync rounds. A guest that dirties data
/// faster than the link can drain never converges; the cap bounds
/// the pre-cutover phase instead of looping forever.
pub const DEFAULT_MIGRATION_MAX_SYNC_ROUNDS: u64 = 10;

/// Default cadence of the placement load materializer, in seconds
/// (RFD 00005 PL-6).
pub const DEFAULT_PLACEMENT_LOAD_MATERIALIZER_INTERVAL_SECS: u64 = 60;
/// Default number of materializer ticks a `cn-load-summary` row may go
/// un-refreshed before the row is treated as stale.
pub const DEFAULT_PLACEMENT_LOAD_MATERIALIZER_STALENESS_TICKS: u64 = 3;

/// Which metrics backend `tritond` stores timeseries in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum MetricsBackend {
    /// In-memory ring buffer (default). Metrics do not survive a restart.
    Memory,
    /// ClickHouse over HTTP. Requires [`Settings::metrics_clickhouse_url`].
    Clickhouse,
}

/// Cluster-wide tunables that live in FoundationDB rather than in the
/// bootstrap config file. Most are read once at startup; the
/// placement keys flagged by [`ConfigKey::restart_required`] are read
/// live on every pick. The `tcadm config` subcommand and the admin
/// console read and write them.
///
/// Every field is serialized under its dotted wire name (see
/// [`ConfigKey`]) and the struct is `#[serde(default)]`, so a blob
/// written by an older binary still deserializes under a newer one
/// (missing fields take their default) and a blob written by a newer
/// binary still deserializes under an older one (unknown fields are
/// ignored).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct Settings {
    /// When `true`, `tritond` does not spawn the in-process stub
    /// provisioner; an external `tritonagent` is expected to drain the
    /// job queue. Replaces `TRITOND_DISABLE_INPROCESS_PROVISIONER`.
    #[serde(rename = "provisioner.inprocess_disabled")]
    pub provisioner_inprocess_disabled: bool,
    /// Cadence, in seconds, of the stale-claim sweeper.
    #[serde(rename = "sweeper.interval_secs")]
    pub sweeper_interval_secs: u64,
    /// How old (seconds) a job claim must be before the sweeper reaps it.
    #[serde(rename = "sweeper.stale_claim_threshold_secs")]
    pub stale_claim_threshold_secs: u64,
    /// Cadence, in seconds, of the DHCP lease reconciler.
    #[serde(rename = "dhcp.reconcile_interval_secs")]
    pub dhcp_reconcile_interval_secs: u64,
    /// Minimum `now - last_activity` (seconds) before a DHCP lease is
    /// garbage-collection eligible.
    #[serde(rename = "dhcp.lease_gc_threshold_secs")]
    pub dhcp_lease_gc_threshold_secs: u64,
    /// Which metrics backend `tritond` uses.
    #[serde(rename = "metrics.backend")]
    pub metrics_backend: MetricsBackend,
    /// Base URL of the ClickHouse HTTP endpoint (e.g.
    /// `http://10.0.0.5:8123`). Only consulted when `metrics.backend`
    /// is `clickhouse`.
    #[serde(rename = "metrics.clickhouse_url")]
    pub metrics_clickhouse_url: Option<String>,
    /// Cluster-default for `config/imds/enabled` when no scope pins a
    /// value. The layered metadata system's fallback used to be the
    /// hardcoded constant [`DEFAULT_IMDS_ENABLED`]; this knob lets an
    /// operator flip it cluster-wide via `tcadm config set
    /// imds.enabled_default false` (e.g. to default-deny IMDS) without
    /// touching every silo's `config/imds/enabled`. The compiled-in
    /// constant is the bootstrap default (the value [`Default::default`]
    /// returns when nothing's in FDB yet).
    #[serde(rename = "imds.enabled_default")]
    pub imds_enabled_default: bool,
    /// Cluster-default for `config/imds/hop-limit`. Same role as
    /// `imds_enabled_default`; defaults to [`DEFAULT_IMDS_HOP_LIMIT`]
    /// (1) for SSRF-relay safety.
    #[serde(rename = "imds.hop_limit_default")]
    pub imds_hop_limit_default: u64,
    /// Cluster-default for whether tritonagent manages the bhyve memory
    /// reservoir when a CN has no [`CnPlacement::reservoir_enabled`]
    /// override. Flip cluster-wide with `tcadm config set
    /// reservoir.enabled_default false`. Bootstrap default:
    /// [`DEFAULT_RESERVOIR_ENABLED`].
    #[serde(rename = "reservoir.enabled_default")]
    pub reservoir_enabled_default: bool,
    /// Cluster-default reservoir floor as a fraction of physical RAM when
    /// a CN has no [`CnPlacement::reservoir_percent`] override. Set with
    /// `tcadm config set reservoir.percent_default 0.75`. tritonagent
    /// clamps to `[0.0, 1.0]` and the kernel limit. Bootstrap default:
    /// [`DEFAULT_RESERVOIR_PERCENT`].
    #[serde(rename = "reservoir.percent_default")]
    pub reservoir_percent_default: f32,
    /// How long terminal sagas are kept in FDB before the sweeper's
    /// retention pass deletes the record + event log. Default 30
    /// days (`30 * 86400 = 2_592_000`). Stuck sagas are exempt and
    /// stay until human cleanup. RFD 00004 SG-4.
    #[serde(rename = "saga.retention_secs")]
    pub saga_retention_secs: u64,
    /// Realm issuer URL of an external `identityd` whose RS256 access
    /// tokens `tritond` should accept (RFD 00004 IS-3). When `None`
    /// (the default), the identityd verify path is skipped entirely and
    /// authentication behaves exactly as before. No `ConfigKey` is wired
    /// for this yet; it is set at boot via `TRITOND_IDENTITYD_ISSUER_URL`.
    #[serde(rename = "identityd.issuer_url")]
    pub identityd_issuer_url: Option<String>,
    /// Cluster-wide placement strategy profiles + the active one. The
    /// placement engine resolves the active profile's scorer weights at
    /// pick time. Surfaced as the `placement.profiles` config key and
    /// managed from the admin console's Placement surface (RFD 00005).
    #[serde(rename = "placement.profiles", default)]
    pub placement_profiles: PlacementProfiles,
    /// Cadence, in seconds, of the placement load materializer
    /// (RFD 00005 PL-6). The materializer rolls per-CN ClickHouse load
    /// metrics into `cn-load-summary` rows for the load-history
    /// scorers. Default 60s.
    #[serde(
        rename = "placement.load_materializer.interval_secs",
        default = "default_placement_load_materializer_interval_secs"
    )]
    pub placement_load_materializer_interval_secs: u64,
    /// Number of materializer ticks a `cn-load-summary` row may go
    /// un-refreshed before the placement engine treats it as stale at
    /// pick time (`staleness_ticks × interval_secs` seconds). The gate
    /// lives on the read side so a dead materializer cannot leave
    /// frozen `stale = false` rows scoring as fresh. Default 3.
    #[serde(
        rename = "placement.load_materializer.staleness_ticks",
        default = "default_placement_load_materializer_staleness_ticks"
    )]
    pub placement_load_materializer_staleness_ticks: u64,
    /// ClickHouse HTTP base URL the materializer queries. When `None`
    /// (the default) the materializer falls back to
    /// [`Settings::metrics_clickhouse_url`]; if that is also `None` the
    /// materializer task is not spawned.
    #[serde(rename = "placement.load_materializer.clickhouse_url", default)]
    pub placement_load_materializer_clickhouse_url: Option<String>,
    /// Convergence threshold for the migration saga's incremental
    /// sync loop: stop syncing once a round streams fewer bytes than
    /// this. Read live from FDB by each saga run, so changing it
    /// affects in-flight and future migrations without a restart.
    #[serde(
        rename = "migration.sync_delta_threshold_bytes",
        default = "default_migration_sync_delta_threshold_bytes"
    )]
    pub migration_sync_delta_threshold_bytes: u64,
    /// Upper bound on incremental sync rounds per migration. Read
    /// live, same as `migration.sync_delta_threshold_bytes`.
    #[serde(
        rename = "migration.max_sync_rounds",
        default = "default_migration_max_sync_rounds"
    )]
    pub migration_max_sync_rounds: u64,
}

fn default_placement_load_materializer_interval_secs() -> u64 {
    DEFAULT_PLACEMENT_LOAD_MATERIALIZER_INTERVAL_SECS
}

fn default_migration_sync_delta_threshold_bytes() -> u64 {
    DEFAULT_MIGRATION_SYNC_DELTA_THRESHOLD_BYTES
}

fn default_migration_max_sync_rounds() -> u64 {
    DEFAULT_MIGRATION_MAX_SYNC_ROUNDS
}

fn default_placement_load_materializer_staleness_ticks() -> u64 {
    DEFAULT_PLACEMENT_LOAD_MATERIALIZER_STALENESS_TICKS
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            provisioner_inprocess_disabled: false,
            sweeper_interval_secs: DEFAULT_SWEEPER_INTERVAL_SECS,
            stale_claim_threshold_secs: DEFAULT_STALE_CLAIM_THRESHOLD_SECS,
            dhcp_reconcile_interval_secs: DEFAULT_DHCP_RECONCILE_INTERVAL_SECS,
            dhcp_lease_gc_threshold_secs: DEFAULT_DHCP_LEASE_GC_THRESHOLD_SECS,
            metrics_backend: MetricsBackend::Memory,
            metrics_clickhouse_url: None,
            imds_enabled_default: DEFAULT_IMDS_ENABLED,
            imds_hop_limit_default: DEFAULT_IMDS_HOP_LIMIT,
            reservoir_enabled_default: DEFAULT_RESERVOIR_ENABLED,
            reservoir_percent_default: DEFAULT_RESERVOIR_PERCENT,
            saga_retention_secs: DEFAULT_SAGA_RETENTION_SECS,
            identityd_issuer_url: None,
            placement_profiles: PlacementProfiles::default(),
            placement_load_materializer_interval_secs:
                DEFAULT_PLACEMENT_LOAD_MATERIALIZER_INTERVAL_SECS,
            placement_load_materializer_staleness_ticks:
                DEFAULT_PLACEMENT_LOAD_MATERIALIZER_STALENESS_TICKS,
            placement_load_materializer_clickhouse_url: None,
            migration_sync_delta_threshold_bytes: DEFAULT_MIGRATION_SYNC_DELTA_THRESHOLD_BYTES,
            migration_max_sync_rounds: DEFAULT_MIGRATION_MAX_SYNC_ROUNDS,
        }
    }
}

/// One named placement strategy profile: a per-scorer weight map plus
/// metadata. The placement engine resolves the *active* profile's
/// weights at pick time (RFD 00005). `builtin` profiles ship with
/// tritond and can be cloned/edited but not deleted; operator-created
/// profiles are `builtin = false`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PlacementProfile {
    /// Stable lowercase identifier, e.g. `spread`, `consolidate`.
    pub name: String,
    /// One-line operator-facing intent.
    pub description: String,
    /// Ships with tritond (cannot be deleted; can be cloned/edited).
    #[serde(default)]
    pub builtin: bool,
    /// Scorer name → weight. Names match the `tritond-placement`
    /// scorer roster; unknown names are ignored by the engine and a
    /// missing scorer falls back to its built-in default weight.
    pub weights: std::collections::BTreeMap<String, f32>,
}

/// The cluster-wide set of placement strategy profiles plus which one
/// is active. Stored as the `placement.profiles` config key.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PlacementProfiles {
    /// `name` of the profile the engine uses for every pick.
    pub active: String,
    pub profiles: Vec<PlacementProfile>,
}

impl Default for PlacementProfiles {
    fn default() -> Self {
        PlacementProfiles {
            active: "spread".to_string(),
            profiles: default_placement_profiles(),
        }
    }
}

impl PlacementProfiles {
    /// The active profile, or the first one, or `None` if empty.
    pub fn active_profile(&self) -> Option<&PlacementProfile> {
        self.profiles
            .iter()
            .find(|p| p.name == self.active)
            .or_else(|| self.profiles.first())
    }
}

/// Built-in seed profiles. Weights are keyed by scorer name; the
/// baseline (Spread) mirrors the engine's default weights.
fn default_placement_profiles() -> Vec<PlacementProfile> {
    fn w(pairs: &[(&str, f32)]) -> std::collections::BTreeMap<String, f32> {
        pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }
    vec![
        PlacementProfile {
            name: "spread".to_string(),
            description: "Availability-first: distribute across fault domains and hosts."
                .to_string(),
            builtin: true,
            weights: w(&[
                ("score-ram-headroom", 2.0),
                ("score-disk-headroom", 1.0),
                ("score-spread-by-fault-domain", 1.5),
                ("score-pack-by-fault-domain", 0.0),
                ("score-affinity-preferred", 1.0),
                ("score-platform-current", 0.5),
                ("score-fewer-cotenant-zones", 0.5),
                ("score-uniform-random", 0.1),
                ("score-avoid-hot-now", 1.5),
                ("score-prefer-low-baseline", 0.75),
                ("score-diurnal-fit", 0.0),
            ]),
        },
        PlacementProfile {
            name: "consolidate".to_string(),
            description: "Density-first: bin-pack onto the fewest nodes for power/cost."
                .to_string(),
            builtin: true,
            weights: w(&[
                ("score-ram-headroom", 1.0),
                ("score-disk-headroom", 0.5),
                ("score-spread-by-fault-domain", 0.0),
                ("score-pack-by-fault-domain", 1.5),
                ("score-affinity-preferred", 1.0),
                ("score-platform-current", 0.5),
                ("score-fewer-cotenant-zones", 0.0),
                ("score-uniform-random", 0.1),
                ("score-avoid-hot-now", 0.5),
                ("score-prefer-low-baseline", 0.25),
                ("score-diurnal-fit", 0.0),
            ]),
        },
        PlacementProfile {
            name: "balanced".to_string(),
            description: "Middle ground between spread and consolidate.".to_string(),
            builtin: true,
            weights: w(&[
                ("score-ram-headroom", 2.0),
                ("score-disk-headroom", 1.0),
                ("score-spread-by-fault-domain", 0.75),
                ("score-pack-by-fault-domain", 0.75),
                ("score-affinity-preferred", 1.0),
                ("score-platform-current", 0.5),
                ("score-fewer-cotenant-zones", 0.5),
                ("score-uniform-random", 0.1),
                ("score-avoid-hot-now", 1.0),
                ("score-prefer-low-baseline", 0.5),
                ("score-diurnal-fit", 0.0),
            ]),
        },
        PlacementProfile {
            name: "performance".to_string(),
            description: "Latency-first: prefer cool, roomy nodes; avoid hot/peaky.".to_string(),
            builtin: true,
            weights: w(&[
                ("score-ram-headroom", 2.5),
                ("score-disk-headroom", 1.5),
                ("score-spread-by-fault-domain", 1.0),
                ("score-pack-by-fault-domain", 0.0),
                ("score-affinity-preferred", 1.0),
                ("score-platform-current", 0.5),
                ("score-fewer-cotenant-zones", 1.0),
                ("score-uniform-random", 0.0),
                ("score-avoid-hot-now", 2.5),
                ("score-prefer-low-baseline", 1.5),
                ("score-diurnal-fit", 0.5),
            ]),
        },
        PlacementProfile {
            name: "isolation".to_string(),
            description: "Noisy-neighbor averse: minimize co-tenancy and contention.".to_string(),
            builtin: true,
            weights: w(&[
                ("score-ram-headroom", 1.5),
                ("score-disk-headroom", 1.0),
                ("score-spread-by-fault-domain", 2.0),
                ("score-pack-by-fault-domain", 0.0),
                ("score-affinity-preferred", 1.0),
                ("score-platform-current", 0.5),
                ("score-fewer-cotenant-zones", 2.5),
                ("score-uniform-random", 0.1),
                ("score-avoid-hot-now", 1.5),
                ("score-prefer-low-baseline", 0.75),
                ("score-diurnal-fit", 0.0),
            ]),
        },
        PlacementProfile {
            name: "power-save".to_string(),
            description: "Aggressive pack + diurnal fit so idle nodes can power down.".to_string(),
            builtin: true,
            weights: w(&[
                ("score-ram-headroom", 0.75),
                ("score-disk-headroom", 0.5),
                ("score-spread-by-fault-domain", 0.0),
                ("score-pack-by-fault-domain", 2.5),
                ("score-affinity-preferred", 1.0),
                ("score-platform-current", 0.5),
                ("score-fewer-cotenant-zones", 0.0),
                ("score-uniform-random", 0.0),
                ("score-avoid-hot-now", 0.25),
                ("score-prefer-low-baseline", 0.0),
                ("score-diurnal-fit", 1.5),
            ]),
        },
    ]
}

impl Settings {
    /// Current value of one key, as JSON. Always `Some`-equivalent —
    /// every field is serialized, so the returned `Value` is never a
    /// missing entry.
    pub fn get(&self, key: ConfigKey) -> serde_json::Value {
        let obj = serde_json::to_value(self).expect("Settings always serializes to a JSON object");
        obj.get(key.as_str())
            .cloned()
            .expect("every ConfigKey maps to a serialized field")
    }

    /// Overwrite one key from a JSON value, then re-validate the whole
    /// struct. A wrong-typed value (e.g. a string where a `u64` is
    /// expected) is rejected with [`ConfigError::InvalidValue`] and
    /// `self` is left unchanged.
    pub fn set(&mut self, key: ConfigKey, value: serde_json::Value) -> Result<(), ConfigError> {
        let mut obj = match serde_json::to_value(&*self) {
            Ok(serde_json::Value::Object(m)) => m,
            _ => unreachable!("Settings always serializes to a JSON object"),
        };
        obj.insert(key.as_str().to_string(), value);
        let next: Settings =
            serde_json::from_value(serde_json::Value::Object(obj)).map_err(|e| {
                ConfigError::InvalidValue {
                    key: key.as_str().to_string(),
                    message: e.to_string(),
                }
            })?;
        *self = next;
        Ok(())
    }

    /// Reset one key to its built-in default.
    pub fn reset(&mut self, key: ConfigKey) {
        let default_value = Settings::default().get(key);
        self.set(key, default_value)
            .expect("a default value always round-trips");
    }
}

/// Stable identifier for one [`Settings`] field. Used as the path
/// segment in `/v1/config/{key}`, the key argument to `tcadm config`,
/// and the JSON field name in the stored blob. Centralising the
/// string⇆field mapping here keeps the rest of the codebase off
/// hardcoded config-key strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ConfigKey {
    /// [`Settings::provisioner_inprocess_disabled`]
    ProvisionerInprocessDisabled,
    /// [`Settings::sweeper_interval_secs`]
    SweeperIntervalSecs,
    /// [`Settings::stale_claim_threshold_secs`]
    StaleClaimThresholdSecs,
    /// [`Settings::dhcp_reconcile_interval_secs`]
    DhcpReconcileIntervalSecs,
    /// [`Settings::dhcp_lease_gc_threshold_secs`]
    DhcpLeaseGcThresholdSecs,
    /// [`Settings::metrics_backend`]
    MetricsBackend,
    /// [`Settings::metrics_clickhouse_url`]
    MetricsClickhouseUrl,
    /// [`Settings::imds_enabled_default`]
    ImdsEnabledDefault,
    /// [`Settings::imds_hop_limit_default`]
    ImdsHopLimitDefault,
    /// [`Settings::reservoir_enabled_default`]
    ReservoirEnabledDefault,
    /// [`Settings::reservoir_percent_default`]
    ReservoirPercentDefault,
    /// [`Settings::saga_retention_secs`]
    SagaRetentionSecs,
    /// [`Settings::placement_profiles`]
    PlacementProfiles,
    /// [`Settings::placement_load_materializer_interval_secs`]
    PlacementLoadMaterializerIntervalSecs,
    /// [`Settings::placement_load_materializer_staleness_ticks`]
    PlacementLoadMaterializerStalenessTicks,
    /// [`Settings::placement_load_materializer_clickhouse_url`]
    PlacementLoadMaterializerClickhouseUrl,
    /// [`Settings::migration_sync_delta_threshold_bytes`]
    MigrationSyncDeltaThresholdBytes,
    /// [`Settings::migration_max_sync_rounds`]
    MigrationMaxSyncRounds,
}

impl ConfigKey {
    /// Every key, in the order `tcadm config list` displays them.
    pub const ALL: [ConfigKey; 18] = [
        ConfigKey::ProvisionerInprocessDisabled,
        ConfigKey::SweeperIntervalSecs,
        ConfigKey::StaleClaimThresholdSecs,
        ConfigKey::DhcpReconcileIntervalSecs,
        ConfigKey::DhcpLeaseGcThresholdSecs,
        ConfigKey::MetricsBackend,
        ConfigKey::MetricsClickhouseUrl,
        ConfigKey::ImdsEnabledDefault,
        ConfigKey::ImdsHopLimitDefault,
        ConfigKey::ReservoirEnabledDefault,
        ConfigKey::ReservoirPercentDefault,
        ConfigKey::SagaRetentionSecs,
        ConfigKey::PlacementProfiles,
        ConfigKey::PlacementLoadMaterializerIntervalSecs,
        ConfigKey::PlacementLoadMaterializerStalenessTicks,
        ConfigKey::PlacementLoadMaterializerClickhouseUrl,
        ConfigKey::MigrationSyncDeltaThresholdBytes,
        ConfigKey::MigrationMaxSyncRounds,
    ];

    /// Dotted wire name. Must exactly equal the `#[serde(rename = ...)]`
    /// on the matching [`Settings`] field.
    pub fn as_str(self) -> &'static str {
        match self {
            ConfigKey::ProvisionerInprocessDisabled => "provisioner.inprocess_disabled",
            ConfigKey::SweeperIntervalSecs => "sweeper.interval_secs",
            ConfigKey::StaleClaimThresholdSecs => "sweeper.stale_claim_threshold_secs",
            ConfigKey::DhcpReconcileIntervalSecs => "dhcp.reconcile_interval_secs",
            ConfigKey::DhcpLeaseGcThresholdSecs => "dhcp.lease_gc_threshold_secs",
            ConfigKey::MetricsBackend => "metrics.backend",
            ConfigKey::MetricsClickhouseUrl => "metrics.clickhouse_url",
            ConfigKey::ImdsEnabledDefault => "imds.enabled_default",
            ConfigKey::ImdsHopLimitDefault => "imds.hop_limit_default",
            ConfigKey::ReservoirEnabledDefault => "reservoir.enabled_default",
            ConfigKey::ReservoirPercentDefault => "reservoir.percent_default",
            ConfigKey::SagaRetentionSecs => "saga.retention_secs",
            ConfigKey::PlacementProfiles => "placement.profiles",
            ConfigKey::PlacementLoadMaterializerIntervalSecs => {
                "placement.load_materializer.interval_secs"
            }
            ConfigKey::PlacementLoadMaterializerStalenessTicks => {
                "placement.load_materializer.staleness_ticks"
            }
            ConfigKey::PlacementLoadMaterializerClickhouseUrl => {
                "placement.load_materializer.clickhouse_url"
            }
            ConfigKey::MigrationSyncDeltaThresholdBytes => "migration.sync_delta_threshold_bytes",
            ConfigKey::MigrationMaxSyncRounds => "migration.max_sync_rounds",
        }
    }

    /// Parse a dotted wire name back to a key. Returns `None` for an
    /// unrecognised string.
    pub fn from_wire(s: &str) -> Option<Self> {
        ConfigKey::ALL.into_iter().find(|k| k.as_str() == s)
    }

    /// One-line human description, surfaced by `tcadm config list` and
    /// the admin console.
    pub fn description(self) -> &'static str {
        match self {
            ConfigKey::ProvisionerInprocessDisabled => {
                "skip the in-process stub provisioner (an external tritonagent drains the queue)"
            }
            ConfigKey::SweeperIntervalSecs => "stale-claim sweeper cadence, in seconds",
            ConfigKey::StaleClaimThresholdSecs => {
                "age in seconds a job claim must reach before the sweeper reaps it"
            }
            ConfigKey::DhcpReconcileIntervalSecs => "DHCP lease reconciler cadence, in seconds",
            ConfigKey::DhcpLeaseGcThresholdSecs => {
                "idle seconds before a DHCP lease becomes garbage-collection eligible"
            }
            ConfigKey::MetricsBackend => "metrics backend: \"memory\" or \"clickhouse\"",
            ConfigKey::MetricsClickhouseUrl => {
                "ClickHouse HTTP base URL (used only when metrics.backend is clickhouse)"
            }
            ConfigKey::ImdsEnabledDefault => {
                "cluster default for config/imds/enabled when no scope pins a value"
            }
            ConfigKey::ImdsHopLimitDefault => {
                "cluster default for config/imds/hop-limit when no scope pins one (1..64)"
            }
            ConfigKey::ReservoirEnabledDefault => {
                "cluster default for bhyve memory reservoir management when a CN has no override"
            }
            ConfigKey::ReservoirPercentDefault => {
                "cluster default reservoir floor as a fraction of RAM (0.0..1.0) when a CN has no override"
            }
            ConfigKey::SagaRetentionSecs => {
                "how long terminal sagas stay in FDB before the retention pass deletes them (seconds)"
            }
            ConfigKey::PlacementProfiles => {
                "placement strategy profiles + active selection (managed from the Placement console)"
            }
            ConfigKey::PlacementLoadMaterializerIntervalSecs => {
                "placement load-materializer cadence, in seconds (default 60)"
            }
            ConfigKey::PlacementLoadMaterializerStalenessTicks => {
                "materializer ticks a cn-load-summary row may go un-refreshed before placement treats it as stale (default 3; applied live at pick time)"
            }
            ConfigKey::PlacementLoadMaterializerClickhouseUrl => {
                "ClickHouse HTTP base URL for the load materializer (falls back to metrics.clickhouse_url)"
            }
            ConfigKey::MigrationSyncDeltaThresholdBytes => {
                "stop migration sync rounds once a round streams fewer bytes than this (applied live per saga run)"
            }
            ConfigKey::MigrationMaxSyncRounds => {
                "upper bound on incremental sync rounds per migration (applied live per saga run)"
            }
        }
    }

    /// Whether changing this key requires a `tritond` restart. Most
    /// settings are read once at startup; the exceptions are read live
    /// from FDB on every placement pick.
    pub fn restart_required(self) -> bool {
        !matches!(
            self,
            // Resolved per pick (placement::pick re-reads Settings).
            ConfigKey::PlacementProfiles
                | ConfigKey::PlacementLoadMaterializerStalenessTicks
                // Resolved per saga run (the migrate-instance saga
                // re-reads Settings inside sync_convergence).
                | ConfigKey::MigrationSyncDeltaThresholdBytes
                | ConfigKey::MigrationMaxSyncRounds
        )
    }

    /// Name of the legacy environment variable that, when set,
    /// overrides this key at boot (env > FDB > default). `None` when no
    /// env override exists.
    pub fn env_var(self) -> Option<&'static str> {
        Some(match self {
            ConfigKey::ProvisionerInprocessDisabled => "TRITOND_DISABLE_INPROCESS_PROVISIONER",
            ConfigKey::SweeperIntervalSecs => "TRITOND_SWEEPER_INTERVAL_SECS",
            ConfigKey::StaleClaimThresholdSecs => "TRITOND_STALE_CLAIM_THRESHOLD_SECS",
            ConfigKey::DhcpReconcileIntervalSecs => "TRITOND_DHCP_RECONCILE_INTERVAL_SECS",
            ConfigKey::DhcpLeaseGcThresholdSecs => "TRITOND_DHCP_LEASE_GC_THRESHOLD_SECS",
            ConfigKey::MetricsBackend => "TRITOND_METRICS_STORE",
            ConfigKey::MetricsClickhouseUrl => "TRITOND_METRICS_CLICKHOUSE_URL",
            ConfigKey::ImdsEnabledDefault => "TRITOND_IMDS_ENABLED_DEFAULT",
            ConfigKey::ImdsHopLimitDefault => "TRITOND_IMDS_HOP_LIMIT_DEFAULT",
            ConfigKey::ReservoirEnabledDefault => "TRITOND_RESERVOIR_ENABLED_DEFAULT",
            ConfigKey::ReservoirPercentDefault => "TRITOND_RESERVOIR_PERCENT_DEFAULT",
            ConfigKey::SagaRetentionSecs => "TRITOND_SAGA_RETENTION_SECS",
            // No env override; placement profiles are managed via the
            // config API / Placement console only.
            ConfigKey::PlacementProfiles => return None,
            ConfigKey::PlacementLoadMaterializerIntervalSecs => {
                "TRITOND_PLACEMENT_LOAD_MATERIALIZER_INTERVAL_SECS"
            }
            ConfigKey::PlacementLoadMaterializerStalenessTicks => {
                "TRITOND_PLACEMENT_LOAD_MATERIALIZER_STALENESS_TICKS"
            }
            ConfigKey::PlacementLoadMaterializerClickhouseUrl => {
                "TRITOND_PLACEMENT_LOAD_MATERIALIZER_CLICKHOUSE_URL"
            }
            ConfigKey::MigrationSyncDeltaThresholdBytes => {
                "TRITOND_MIGRATION_SYNC_DELTA_THRESHOLD_BYTES"
            }
            ConfigKey::MigrationMaxSyncRounds => "TRITOND_MIGRATION_MAX_SYNC_ROUNDS",
        })
    }
}

/// Errors from validating a config-key update.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The supplied key string does not name a known [`ConfigKey`].
    #[error("unknown config key: {0}")]
    UnknownKey(String),
    /// The supplied value is the wrong shape for the target key.
    #[error("invalid value for {key}: {message}")]
    InvalidValue {
        /// The key that was being set.
        key: String,
        /// Deserialiser error detail.
        message: String,
    },
}

#[cfg(test)]
mod settings_tests {
    use super::*;

    #[test]
    fn default_values_match_constants() {
        let s = Settings::default();
        assert!(!s.provisioner_inprocess_disabled);
        assert_eq!(s.sweeper_interval_secs, DEFAULT_SWEEPER_INTERVAL_SECS);
        assert_eq!(
            s.stale_claim_threshold_secs,
            DEFAULT_STALE_CLAIM_THRESHOLD_SECS
        );
        assert_eq!(
            s.dhcp_reconcile_interval_secs,
            DEFAULT_DHCP_RECONCILE_INTERVAL_SECS
        );
        assert_eq!(
            s.dhcp_lease_gc_threshold_secs,
            DEFAULT_DHCP_LEASE_GC_THRESHOLD_SECS
        );
        assert_eq!(s.metrics_backend, MetricsBackend::Memory);
        assert_eq!(s.metrics_clickhouse_url, None);
    }

    #[test]
    fn serde_round_trip() {
        let mut s = Settings::default();
        s.sweeper_interval_secs = 42;
        s.metrics_backend = MetricsBackend::Clickhouse;
        s.metrics_clickhouse_url = Some("http://ch:8123".to_string());
        let json = serde_json::to_string(&s).unwrap();
        let back: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn forward_compat_missing_and_unknown_fields() {
        // Empty object -> all defaults.
        let s: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(s, Settings::default());
        // Unknown keys are ignored, known ones still apply.
        let s: Settings =
            serde_json::from_str(r#"{"sweeper.interval_secs": 7, "future.knob": true}"#).unwrap();
        assert_eq!(s.sweeper_interval_secs, 7);
    }

    #[test]
    fn config_key_wire_round_trips() {
        for key in ConfigKey::ALL {
            assert_eq!(ConfigKey::from_wire(key.as_str()), Some(key));
        }
        assert_eq!(ConfigKey::from_wire("nope.not.real"), None);
    }

    #[test]
    fn every_key_serializes_a_field() {
        // get() must not panic for any key — guards as_str() drift from
        // the serde rename attributes.
        let s = Settings::default();
        for key in ConfigKey::ALL {
            let _ = s.get(key);
        }
    }

    #[test]
    fn get_set_reset() {
        let mut s = Settings::default();
        s.set(ConfigKey::SweeperIntervalSecs, serde_json::json!(30))
            .unwrap();
        assert_eq!(s.sweeper_interval_secs, 30);
        assert_eq!(s.get(ConfigKey::SweeperIntervalSecs), serde_json::json!(30));
        s.reset(ConfigKey::SweeperIntervalSecs);
        assert_eq!(s.sweeper_interval_secs, DEFAULT_SWEEPER_INTERVAL_SECS);
    }

    #[test]
    fn set_rejects_wrong_type() {
        let mut s = Settings::default();
        let err = s
            .set(ConfigKey::SweeperIntervalSecs, serde_json::json!("lots"))
            .expect_err("string is not a u64");
        assert!(matches!(err, ConfigError::InvalidValue { .. }));
        // Unchanged.
        assert_eq!(s.sweeper_interval_secs, DEFAULT_SWEEPER_INTERVAL_SECS);
    }

    #[test]
    fn set_accepts_enum_and_option() {
        let mut s = Settings::default();
        s.set(ConfigKey::MetricsBackend, serde_json::json!("clickhouse"))
            .unwrap();
        assert_eq!(s.metrics_backend, MetricsBackend::Clickhouse);
        s.set(
            ConfigKey::MetricsClickhouseUrl,
            serde_json::json!("http://ch:8123"),
        )
        .unwrap();
        assert_eq!(s.metrics_clickhouse_url.as_deref(), Some("http://ch:8123"));
        s.set(ConfigKey::MetricsClickhouseUrl, serde_json::Value::Null)
            .unwrap();
        assert_eq!(s.metrics_clickhouse_url, None);
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
    /// rest of the `/v1/agent/*` surface.
    Approved,
    /// Explicitly disabled by an operator. The bound API key is
    /// revoked and the record stays for audit visibility. A fresh
    /// registration from the same `server_uuid` re-arms the record
    /// back to `Pending` (awaiting approval) — i.e. "re-enable with
    /// fresh credentials"; the disable event remains in the audit
    /// chain.
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
    /// `/v1/agent/register/status`. Rotated on every (re-)registration
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
    /// TCP port the agent's on-CN console listener binds (on the
    /// admin IP). Reported by the agent at (re-)registration.
    /// `None` until then — serial / VNC consoles are unavailable
    /// for this CN while it is `None`.
    #[serde(default)]
    pub console_listen_port: Option<u16>,
    /// SHA-256 of the agent console listener's TLS
    /// SubjectPublicKeyInfo, reported at registration. tritond pins
    /// this when it dials the listener so a hijacked admin IP cannot
    /// MITM the console byte stream.
    #[serde(default)]
    pub console_tls_spki_sha256: Option<[u8; 32]>,
    /// Per-CN HS256 key for minting short-lived console tickets
    /// (see `tritond_auth::ConsoleTicketKey`). Generated when the
    /// CN is approved and handed to the agent on its first
    /// long-poll-after-approval (same delivery path as the API key).
    ///
    /// Secret. Stored at rest unencrypted in Phase 0 — same cost as
    /// [`Cn::pending_credential`]; a future slice encrypts it
    /// against the manta-storage secrets engine. Never serialized
    /// into any wire-level view.
    #[serde(default)]
    pub console_ticket_key: Option<[u8; 32]>,
    /// Per-CN HS256 key for minting / verifying IMDSv2 session
    /// tokens (`tritond_auth::ImdsTokenKey`). Generated alongside
    /// `console_ticket_key` when the CN is approved; delivered to
    /// the agent in the same registration-response envelope so a
    /// CN reboot doesn't invalidate live IMDS tokens. See
    /// `IMDS_DESIGN.md` §3 ("Token signing" + the "per-CN keying"
    /// rationale that mirrors the console-ticket key separation).
    ///
    /// Secret. Same at-rest storage caveat as `console_ticket_key`;
    /// never appears in any wire-level view.
    #[serde(default)]
    pub imds_token_key: Option<[u8; 32]>,
    /// Per-CN HS256 key for minting / verifying live-migration
    /// tickets (`tritond_auth::MigrateTicketKey`). Generated
    /// alongside `console_ticket_key` when the CN is approved
    /// and delivered to the agent on its first long-poll-after-
    /// approval. Distinct from the console key so a compromised
    /// console-cred file doesn't grant cross-CN ZFS-receive
    /// dial access.
    ///
    /// Secret. Same at-rest storage caveat as `console_ticket_key`;
    /// never appears in any wire-level view.
    #[serde(default)]
    pub migrate_ticket_key: Option<[u8; 32]>,
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

// ---------------------------------------------------------------------
// Placement keyspaces (RFD 00005 doc 01)
//
// Five independent rows feed the placement engine. Each has a single
// writer (in the operational sense - at most one role authoritatively
// updates the row), a typed shape here, and a small set of Store
// trait methods on the trait. The placement engine in
// `tritond-placement` reads all five into its `CnView` projection
// inside one FDB read snapshot at pick time.
// ---------------------------------------------------------------------

/// Structured hardware capacity reported by `tritonagent`. Written
/// once at startup and on hardware change events.
///
/// The legacy opaque `Cn.sysinfo` blob stays read-only for
/// compatibility with the existing `tcadm cn show` command; placement
/// never reads it (RFD 00005 invariant 7). A CN with no `CnCapacity`
/// row is invisible to placement - every filter rejects it with
/// reason `"cn-capacity row absent"` and the row surfaces on
/// `tcadm cn list` with a `no-capacity-report` badge.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CnCapacity {
    pub server_uuid: Uuid,
    pub cpu_cores_physical: u32,
    pub cpu_threads_logical: u32,
    /// Length == 1 on a UMA box.
    pub numa_nodes: Vec<NumaNode>,
    pub ram_total_mb: u64,
    /// Live instantaneous *available* RAM in MB as the CN itself
    /// reports it (kstat / sysinfo), refreshed every capacity post.
    /// This is the ClickHouse-independent floor the scorers fall back
    /// to during a metrics outage: `ram_total_mb` minus declared
    /// footprints is the planning view, but this is what's actually
    /// free right now. `#[serde(default)]` so reports from agents
    /// predating the live fields still deserialise (0 = "unknown",
    /// scorers treat it as no-signal).
    #[serde(default)]
    pub ram_available_mb: u64,
    /// Live CPU utilisation (0.0 ..= 1.0) over the last capacity
    /// interval, as the CN reports it (load average / kstat). The
    /// ClickHouse-independent "hot right now" signal; the
    /// `score-avoid-hot-now` scorer falls back to it when the
    /// `cn-load-summary` rollup is stale or absent. `0.0` = unknown.
    #[serde(default)]
    pub cpu_utilization_pct: f32,
    pub zpools: Vec<ZpoolCapacity>,
    /// Authoritative for the `cn-nic-tags` filter; tritond does not
    /// re-derive NIC tags from `Cn.sysinfo`.
    pub nic_tags: Vec<String>,
    pub underlay: UnderlayCapability,
    pub devices: Vec<DeviceCapacity>,
    /// SmartOS / illumos platform ID; matches legacy DAPI
    /// `min_platform`.
    pub platform_version: String,
    /// Whether the CN's CPU advertises hardware virtualisation
    /// extensions (VMX / SVM). The `cn-hvm-supported` filter consults
    /// this for bhyve / KVM brands.
    #[serde(default)]
    pub hvm_supported: bool,
    /// Agent-side clock when the row was last published. Staleness
    /// is judged against the agent heartbeat (`Cn.last_seen`), not
    /// against this field - the agent only re-reports on hardware
    /// change, so an old `reported_at` plus a fresh heartbeat means
    /// "hardware is steady", not "row is stale".
    pub reported_at: DateTime<Utc>,

    // ----- LM-0 live-migration compatibility fingerprint -----
    //
    // These fields feed the migration filters in tritond-placement
    // (`cn-bhyve-compatible`, `cn-cpu-feature-superset`,
    // `cn-time-synced`, `cn-zfs-compatible`). All are
    // #[serde(default)] / Option-shaped so older agent reports still
    // deserialise; the placement filters return `Verdict::Skip` when
    // the matching field is absent.
    /// vmm-migrate wire protocol the agent's userspace speaks (e.g.
    /// `"vmm-migrate-ron/0"`). `None` until the agent reports the
    /// capability probe.
    #[serde(default)]
    pub vmm_protocol_version: Option<String>,

    /// CPU feature flags bhyve exposes to guests on this CN (e.g.
    /// `["vmx", "avx2", "sse4_2", "aes"]`). Default empty.
    #[serde(default)]
    pub cpu_features: Vec<String>,

    /// CN's NTP-corrected clock offset relative to UTC, in
    /// nanoseconds. `None` when the agent hasn't reported a probe.
    #[serde(default)]
    pub tsc_offset_ns: Option<i64>,

    /// Per-zpool ZFS on-disk-format property fingerprint, keyed by
    /// pool name. Default empty.
    #[serde(default)]
    pub zpool_props: std::collections::BTreeMap<String, ZpoolPropFingerprint>,
}

/// Per-zpool ZFS properties the live-migration designate filter
/// compares between source and target. Only the values that affect
/// on-disk-format compatibility live here — performance-only knobs
/// (`atime`, `sync`, etc.) are out of scope.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct ZpoolPropFingerprint {
    /// `zfs get encryption`: `"off"`, `"on"`, `"aes-256-gcm"`, etc.
    pub encryption: String,
    /// `zfs get compression`: `"off"`, `"lz4"`, `"zstd"`, etc.
    pub compression: String,
    /// `zfs get recordsize` in bytes (e.g. `131072` for 128K).
    pub recordsize_bytes: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct NumaNode {
    pub node_id: u8,
    pub cores: u32,
    pub ram_mb: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ZpoolCapacity {
    pub name: String,
    pub total_bytes: u64,
    pub free_bytes: u64,
    pub tier: StorageTier,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum StorageTier {
    Ssd,
    Nvme,
    Hdd,
    Mixed,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct UnderlayCapability {
    pub ipv4: bool,
    pub ipv6: bool,
}

impl UnderlayCapability {
    /// Is this CN's underlay sufficient for a request that requires
    /// `req`? Every protocol the request asks for must be advertised
    /// by the CN.
    pub fn satisfies(self, req: UnderlayCapability) -> bool {
        (!req.ipv4 || self.ipv4) && (!req.ipv6 || self.ipv6)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DeviceCapacity {
    pub kind: DeviceKind,
    pub model: String,
    pub free_count: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum DeviceKind {
    Gpu,
    SrIovVf,
}

/// Operator-edited per-CN placement policy. Written through
/// `tcadm cn` / adminui. Defaults are the "fresh CN" shape: not
/// reserved, not cordoned, no pins, no traits, no overprovision
/// overrides, no fault-domain tag.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CnPlacement {
    pub server_uuid: Uuid,
    /// Operator-level "out of service for placement" flag. Force-place
    /// still works (the operator is overriding the chain explicitly).
    #[serde(default)]
    pub reserved: bool,
    #[serde(default)]
    pub reserved_reason: Option<String>,
    /// Operator labels: `gpu=a100`, `pci=zone-a`, `customer=acme`.
    /// Equality-matched per key by the `cn-traits-required` filter.
    #[serde(default)]
    pub traits: BTreeMap<String, String>,
    /// `None` → use the cluster default. The placement engine reads
    /// the cluster setting at chain build time, not at row write
    /// time.
    #[serde(default)]
    pub overprovision_cpu: Option<f32>,
    #[serde(default)]
    pub overprovision_ram: Option<f32>,
    #[serde(default)]
    pub overprovision_disk: Option<f32>,
    /// Free-form operator label: `rack-3`, `pdu-a`, `az-east`. Two
    /// CNs with the same `fault_domain` are co-located from
    /// placement's perspective.
    #[serde(default)]
    pub fault_domain: Option<String>,
    /// Silo pin (D-Pl-5). When set, only requests whose
    /// `silo_uuid` matches may place here.
    #[serde(default)]
    pub pinned_silo_uuid: Option<Uuid>,
    /// Tenant pin (D-Pl-5). When set, only requests whose
    /// `tenant_uuid` matches may place here. Requires
    /// `pinned_silo_uuid` to be `None` or to match the tenant's
    /// silo - enforced inside the FDB transaction by
    /// [`Store::put_cn_placement`] with [`StoreError::PinConflict`].
    #[serde(default)]
    pub pinned_tenant_uuid: Option<Uuid>,
    /// Drain-only: existing instances stay running, restart still
    /// hits the same CN, new placements skip.
    #[serde(default)]
    pub cordoned: bool,
    /// "Why cordoned" - set to `"drain"` by `tcadm cn drain` in a
    /// later slice. Until drain lands, `cn-not-evacuating` is
    /// effectively `cn-not-cordoned`.
    #[serde(default)]
    pub cordoned_reason: Option<String>,
    /// Operator note surfaced in adminui. Free-form, never read
    /// by the engine.
    #[serde(default)]
    pub note: Option<String>,
    /// Per-CN override for whether tritonagent manages the bhyve memory
    /// reservoir. `None` inherits [`Settings::reservoir_enabled_default`].
    /// `#[serde(default)]` so rows written before this field existed
    /// deserialize as `None`.
    #[serde(default)]
    pub reservoir_enabled: Option<bool>,
    /// Per-CN override for the reservoir floor (fraction of physical RAM).
    /// `None` inherits [`Settings::reservoir_percent_default`]. tritonagent
    /// clamps the effective value to `[0.0, 1.0]` and the kernel limit.
    #[serde(default)]
    pub reservoir_percent: Option<f32>,
    /// When the row was last updated. The Store sets this to the
    /// caller-supplied `now` on every write.
    pub updated_at: DateTime<Utc>,
    /// Who applied the edit. The audit row (per
    /// RFD 00005 invariant 4) carries the same value.
    pub updated_by: String,
}

impl CnPlacement {
    /// The "fresh CN" shape, with `updated_at` / `updated_by` left to
    /// the caller. Used by [`Store::get_cn_placement`] to synthesise
    /// a row when no FDB entry exists yet (the engine reads it as
    /// "no operator policy applied").
    pub fn fresh(server_uuid: Uuid, now: DateTime<Utc>) -> Self {
        Self {
            server_uuid,
            reserved: false,
            reserved_reason: None,
            traits: BTreeMap::new(),
            overprovision_cpu: None,
            overprovision_ram: None,
            overprovision_disk: None,
            fault_domain: None,
            pinned_silo_uuid: None,
            pinned_tenant_uuid: None,
            cordoned: false,
            cordoned_reason: None,
            note: None,
            reservoir_enabled: None,
            reservoir_percent: None,
            updated_at: now,
            updated_by: String::new(),
        }
    }

    /// Resolve the effective reservoir policy for this CN: the per-CN
    /// override when set, otherwise the cluster defaults. Mirrors the
    /// IMDS `enabled_default` / `hop_limit_default` precedence so the
    /// agent receives a single flat answer.
    pub fn effective_reservoir(&self, default_enabled: bool, default_percent: f32) -> (bool, f32) {
        (
            self.reservoir_enabled.unwrap_or(default_enabled),
            self.reservoir_percent.unwrap_or(default_percent),
        )
    }
}

#[cfg(test)]
mod reservoir_config_tests {
    use super::*;

    #[test]
    fn effective_reservoir_override_wins_else_default() {
        let mut p = CnPlacement::fresh(Uuid::nil(), Utc::now());
        // No override → inherit both defaults.
        assert_eq!(p.effective_reservoir(true, 0.8), (true, 0.8));
        // Partial override: percent set, enabled inherits.
        p.reservoir_percent = Some(0.5);
        assert_eq!(p.effective_reservoir(true, 0.8), (true, 0.5));
        // Full override.
        p.reservoir_enabled = Some(false);
        assert_eq!(p.effective_reservoir(true, 0.8), (false, 0.5));
    }

    #[test]
    fn settings_default_carries_reservoir_knobs() {
        // The generic `every_key_serializes_a_field` / `config_key_wire_round_trips`
        // tests already cover the ConfigKey plumbing; this pins the default
        // values and that `set` reaches the reservoir fields.
        let mut s = Settings::default();
        assert_eq!(s.reservoir_enabled_default, DEFAULT_RESERVOIR_ENABLED);
        assert_eq!(s.reservoir_percent_default, DEFAULT_RESERVOIR_PERCENT);
        s.set(ConfigKey::ReservoirPercentDefault, serde_json::json!(0.6))
            .expect("set percent");
        assert_eq!(s.reservoir_percent_default, 0.6);
    }
}

/// In-flight provision capacity ticket. Written by the `designate`
/// saga action inside the same FDB transaction that pins
/// `Instance.host_cn_uuid`; deleted by `undesignate` on saga unwind
/// or by the reaper when `expires_at` has passed and the owning
/// saga is in a terminal state (`Done` / `Unwound` / `Stuck`).
///
/// Scorers on the *next* `designate` read this row and subtract its
/// resources from the CN's free capacity. The two-row CAS shape is
/// what closes the race the bin-packer can't survive (D-Pl-2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CnReservation {
    pub server_uuid: Uuid,
    /// `steno::SagaId.0`. Stored raw so this crate doesn't take a
    /// path dep on `tritond-saga` (which itself depends on us).
    pub saga_id: Uuid,
    pub instance_id: Uuid,
    /// 1 vCPU = 100 cpu_units (legacy DAPI `cpu_cap` convention).
    pub cpu_units: u32,
    pub ram_mb: u64,
    /// Per-zpool reservation in bytes. Keys match
    /// `CnCapacity.zpools[].name`.
    #[serde(default)]
    pub disk: BTreeMap<String, u64>,
    #[serde(default)]
    pub devices: Vec<DeviceReservation>,
    pub created_at: DateTime<Utc>,
    /// Saga deadline + slack. The reaper deletes reservations whose
    /// `expires_at` has passed *and* whose owning saga is terminal;
    /// reservations whose saga is still running are left alone (the
    /// SEC reassignment sweep - RFD 00004 D-Sg-4 - is the right
    /// mechanism to advance them).
    pub expires_at: DateTime<Utc>,
    /// `tritond_saga::SecId.0` - the SEC that wrote the row. Recorded
    /// for audit, not for fence validation (the engine layer does
    /// that). The store does not require this to match the *current*
    /// SEC: a reservation written by a now-dead SEC is still the
    /// durable record of consumed capacity.
    pub created_by_sec_id: Uuid,
    /// `tritond_saga::SecEpoch.0`.
    pub created_at_epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DeviceReservation {
    pub kind: DeviceKind,
    pub model: String,
    pub count: u32,
}

/// Materializer-owned per-CN ClickHouse rollup. Read by the
/// load-history scorers on the hot path; refreshed by every tritond's
/// load-materializer tick (default 60s) as an idempotent
/// last-writer-wins overwrite — there is no leader election.
///
/// The materializer writes the row unconditionally on every tick
/// that produces fresh data (capacity-only scorers don't care; load
/// scorers gate on `stale`). It does *not* try to be clever about
/// "only write if changed" - the row is small and FDB MVCC handles
/// the no-op write cheaply.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CnLoadSummary {
    pub server_uuid: Uuid,

    // CPU utilisation (0.0 .. 1.0 - fraction of total physical cores busy).
    pub cpu_p50_5m: f32,
    pub cpu_p95_5m: f32,
    pub cpu_max_5m: f32,
    pub cpu_p50_1d: f32,
    pub cpu_p95_1d: f32,
    pub cpu_max_1d: f32,

    // RAM used (bytes).
    pub ram_used_p95_5m: u64,
    pub ram_used_p95_1d: u64,

    // Disk used per zpool (bytes). Keys match `CnCapacity.zpools[].name`.
    #[serde(default)]
    pub disk_used_bytes_p95_5m: BTreeMap<String, u64>,
    #[serde(default)]
    pub disk_used_bytes_p95_1d: BTreeMap<String, u64>,

    // NIC throughput (bytes/sec).
    pub nic_tx_bps_p95_5m: u64,
    pub nic_tx_bps_p95_1d: u64,
    pub nic_rx_bps_p95_5m: u64,
    pub nic_rx_bps_p95_1d: u64,

    // Sample-thinness gate: if any of these is below a per-window
    // minimum, the row is treated as `stale`.
    pub samples_5m: u32,
    pub samples_1d: u32,

    pub last_refreshed_at: DateTime<Utc>,
    /// Set by the materializer when (a) the per-window CPU/RAM sample
    /// count is below the per-window minimum, or (b) the ClickHouse
    /// query for the pass errored. The row is still written with
    /// `stale = true` so a future surface can distinguish "no data"
    /// from "data says zero" for the CPU/RAM feeds (net/disk carry no
    /// per-feed sample counts, so zeros there are ambiguous).
    ///
    /// Age is enforced on the read side: the placement projection
    /// additionally treats a row older than `staleness_ticks ×
    /// interval_secs` as stale, so a dead materializer cannot leave
    /// frozen rows scoring as fresh.
    pub stale: bool,
}

/// Per-instance affinity / anti-affinity / topology-spread rules.
/// Written at instance create (the rules carried on the request);
/// editable later by the operator via `tcadm instance affinity set`.
/// Future restart / move actions read this row; v1 reads it during
/// the initial `designate`.
///
/// An instance with no rules carries an empty `rules: []` row, not
/// the absence of a row, so the read path is a single get rather
/// than "get-or-default".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct InstanceAffinity {
    pub instance_id: Uuid,
    pub tenant_uuid: Uuid,
    #[serde(default)]
    pub rules: Vec<AffinityRule>,
    #[serde(default)]
    pub spread: Option<TopologySpread>,
    pub updated_at: DateTime<Utc>,
    pub updated_by: String,
}

impl InstanceAffinity {
    /// The "no rules" shape for an instance whose request carried
    /// no affinity asks.
    pub fn empty(instance_id: Uuid, tenant_uuid: Uuid, now: DateTime<Utc>) -> Self {
        Self {
            instance_id,
            tenant_uuid,
            rules: Vec::new(),
            spread: None,
            updated_at: now,
            updated_by: String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AffinityRule {
    pub kind: AffinityKind,
    /// `Required` rules failing satisfaction are a hard reject from
    /// the `cn-affinity-required` filter; `Preferred` rules feed
    /// the `score-affinity-preferred` scorer.
    pub scope: AffinityScope,
    pub op: AffinityOp,
    pub selector: AffinitySelector,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum AffinityKind {
    VmToVm,
    VmToHost,
    Topology,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum AffinityScope {
    Required,
    Preferred,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum AffinityOp {
    In,
    NotIn,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
#[non_exhaustive]
pub enum AffinitySelector {
    /// `vm_to_vm` by explicit instance ids.
    InstanceIds(Vec<Uuid>),
    /// `vm_to_vm` by tag-match (key/value over instance tags).
    InstanceTagMatch { key: String, value: String },
    /// `vm_to_host` by explicit CN ids.
    CnUuids(Vec<Uuid>),
    /// `vm_to_host` by trait-match (key/value over `CnPlacement.traits`).
    CnTraitMatch { key: String, value: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TopologySpread {
    pub key: TopologyKey,
    pub max_skew: u32,
    pub scope: AffinityScope,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
#[non_exhaustive]
pub enum TopologyKey {
    FaultDomain,
    CnUuid,
    Trait(String),
}

/// Joined snapshot of every placement-relevant row for one CN.
/// Returned by `Store::get_cn_pick_snapshot` inside one read
/// transaction so the placement chain sees a consistent view
/// across `Cn` / `CnCapacity` / `CnPlacement` / `CnReservation` /
/// `CnLoadSummary` / assigned `Instance` rows (RFD 00005 PL-5).
///
/// The placement engine (`tritond_placement::CnView`) is a
/// projection over this snapshot; the caller in `tritond` performs
/// the projection so the store and the engine stay independent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CnPickSnapshot {
    pub cn: Cn,
    pub capacity: Option<CnCapacity>,
    /// Defaulted via [`CnPlacement::fresh`] when no operator
    /// edit exists.
    pub placement: CnPlacement,
    /// All `cn-reservation/<server_uuid>/*` rows.
    pub reservations: Vec<CnReservation>,
    pub load_summary: Option<CnLoadSummary>,
    /// Instances currently host-bound to this CN.
    pub assigned_instances: Vec<Instance>,
    /// Read-version timestamp the snapshot was materialised at.
    pub computed_at: DateTime<Utc>,
}

/// Joined tenant-scoped instance projection for the placement
/// engine's `sibling_instances` slice. Bundles the `Instance`
/// row with the host CN's `fault_domain` so the spread / pack
/// scorers don't need a second lookup.
///
/// `host_fault_domain` is `None` when the instance is not yet
/// host-bound (mid-saga) or when the host's `CnPlacement` row
/// has no fault-domain tag.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TenantInstanceProjection {
    pub instance: Instance,
    pub host_fault_domain: Option<String>,
}

// ---------------------------------------------------------------------
// Per-VM status report types
// ---------------------------------------------------------------------

/// Lifecycle states a SmartOS zone may be in, as reported by `vmadm`.
///
/// `Unknown` catches any vmadm state we haven't enumerated (forward
/// compatibility per type-safety rule #5). Phase 0 only acts on
/// `Running`, `Stopped`, and `Destroyed`; other states surface in
/// the operator view but do not feed reconciliation rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum VmState {
    Running,
    Stopped,
    Provisioning,
    Receiving,
    Sending,
    Configured,
    Incomplete,
    Failed,
    /// vmadm uses both `installed` (zone is configured but not booted)
    /// and `installed`-as-init in different paths. We treat it as
    /// equivalent to Stopped for reconciliation.
    Installed,
    /// `vmadm destroy` ran. The zone is gone but vmadm may still report
    /// it briefly during the reap window.
    Destroyed,
    #[serde(other)]
    Unknown,
}

/// One NIC as reported by `vmadm lookup`. Used by the discovery
/// classifier to populate `LegacyVm.nics` and by the (deferred)
/// adoption flow's IP-collision pre-flight.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VmNicReport {
    /// Guest MAC, lowercase colon-separated.
    #[serde(default)]
    pub mac: Option<String>,
    /// Primary IPv4 in the zone. May be DHCP-assigned, set by the
    /// guest, or static via vmadm; we record it as-is.
    #[serde(default)]
    pub ip: Option<IpAddr>,
    /// SmartOS NIC tag the link rides on (e.g. `admin`, `external`,
    /// or a proteus link tag for managed VPCs). Required to
    /// classify legacy fabric vs managed-by-proteus.
    #[serde(default)]
    pub nic_tag: Option<String>,
    /// VLAN id when the NIC is on a tagged tag. Optional.
    #[serde(default)]
    pub vlan_id: Option<u16>,
    /// Gateway address advertised to the guest (DHCP / cloud-init).
    /// Optional.
    #[serde(default)]
    pub gateway: Option<IpAddr>,
    /// `true` if this is the zone's primary NIC.
    #[serde(default)]
    pub primary: bool,
}

/// One VM as reported by tritonagent's status collector.
///
/// Mirrors the per-VM object inside `Cn.last_status["vms"][uuid]`.
/// All fields are optional except `uuid` because vmadm fields can
/// be missing on partially-configured zones, and missing fields
/// must not abort the parse for the rest of the inventory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VmReport {
    /// SmartOS zone uuid (== tritond `Instance::id` for managed zones).
    pub uuid: Uuid,
    /// Operator-assigned zone alias (the human-readable name shown by
    /// `vmadm list`). Surfaced in the admin UI as the row name for
    /// legacy zones; managed zones use `Instance::name` instead.
    #[serde(default)]
    pub alias: Option<String>,
    #[serde(default)]
    pub brand: Option<String>,
    #[serde(default)]
    pub state: Option<VmState>,
    #[serde(default)]
    pub zone_state: Option<String>,
    #[serde(default)]
    pub max_physical_memory: Option<u64>,
    #[serde(default)]
    pub quota: Option<u64>,
    #[serde(default)]
    pub cpu_cap: Option<u32>,
    /// Legacy SmartOS `owner_uuid`. Distinct from tritond
    /// `tenant_id`/`project_id`; preserved on the report so a legacy
    /// VM can be filtered by its operator-assigned owner.
    #[serde(default)]
    pub owner_uuid: Option<Uuid>,
    /// ISO-8601 string from vmadm. Kept as a string because vmadm's
    /// timezone handling has historically varied across SmartOS
    /// platform images and we don't want a parse failure to drop the
    /// whole VM from the report.
    #[serde(default)]
    pub last_modified: Option<String>,
    /// Full `internal_metadata` map. The classifier reads the
    /// `tritond:*` identity keys out of this. Other keys (legacy
    /// `tritond.image_sha256`, `cloudinit_datasource`, etc.) are
    /// preserved opaquely.
    #[serde(default)]
    pub internal_metadata: BTreeMap<String, String>,
    #[serde(default)]
    pub nics: Vec<VmNicReport>,
}

impl VmReport {
    /// Return the four `tritond:*` identity fields if all four are
    /// present in `internal_metadata` and parseable. Returns `None`
    /// if any field is missing, malformed, or the HMAC string is
    /// empty -- the classifier treats that as "no managed identity"
    /// (i.e. legacy / unmanaged), not as a tampered record.
    #[must_use]
    pub fn extract_managed_identity(&self) -> Option<ManagedIdentity> {
        let instance_id = self.internal_metadata.get(TRITOND_METADATA_INSTANCE_ID)?;
        let tenant_id = self.internal_metadata.get(TRITOND_METADATA_TENANT_ID)?;
        let project_id = self.internal_metadata.get(TRITOND_METADATA_PROJECT_ID)?;
        let identity_hmac = self.internal_metadata.get(TRITOND_METADATA_IDENTITY_HMAC)?;
        if identity_hmac.is_empty() {
            return None;
        }
        Some(ManagedIdentity {
            instance_id: Uuid::parse_str(instance_id).ok()?,
            tenant_id: Uuid::parse_str(tenant_id).ok()?,
            project_id: Uuid::parse_str(project_id).ok()?,
            identity_hmac: identity_hmac.clone(),
        })
    }
}

/// Parse the `vms` section of a [`Cn::last_status`] payload into a
/// list of typed [`VmReport`]. The status payload is shaped as
/// `{ "vms": { "<uuid>": { ... } }, "zpools": ..., ... }` per the
/// legacy reporter convention.
///
/// Per-VM parse errors are silently dropped: a single malformed VM
/// must not erase the rest of a CN's inventory from the operator
/// view. Returns an empty Vec when the input has no `vms` key or
/// when the entire blob is malformed.
#[must_use]
pub fn parse_vm_reports(status: &serde_json::Value) -> Vec<VmReport> {
    let Some(vms) = status.get("vms").and_then(|v| v.as_object()) else {
        return Vec::new();
    };
    vms.values()
        .filter_map(|v| serde_json::from_value::<VmReport>(v.clone()).ok())
        .collect()
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

// ---------------------------------------------------------------------
// Legacy VMs (zones discovered on a CN that aren't tritond-managed)
// ---------------------------------------------------------------------

/// Whether a [`LegacyVm`] is eligible for adoption into a tritond
/// tenant/project. Phase B leaves every legacy VM as `Unevaluated`;
/// the (deferred) Phase D adoption flow promotes to `Yes` after
/// brand + NIC compatibility checks, or `No(reason)` when the zone
/// can't be rewritten onto the proteus dataplane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AdoptableState {
    /// Adoption has not yet been evaluated for this zone.
    Unevaluated,
    /// Zone can be adopted: brand + NIC layout are compatible with
    /// a tritond VPC + proteus port.
    Yes,
    /// Zone cannot be adopted; reason is operator-readable.
    No { reason: String },
}

/// Per-NIC layout for a [`LegacyVm`]. Distinct from [`Nic`] (the
/// tritond-managed NIC type) because legacy zones may carry
/// non-tritond NIC tags (`admin`, `external`, customer VLAN tags),
/// and the IP may have come from DHCP rather than a tritond subnet
/// allocation. Mirrors the per-NIC fields the Phase D adoption flow's
/// pre-flight needs (IP-collision check, network-rewrite preview).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LegacyNic {
    #[serde(default)]
    pub mac: Option<String>,
    #[serde(default)]
    pub ip: Option<IpAddr>,
    #[serde(default)]
    pub nic_tag: Option<String>,
    #[serde(default)]
    pub vlan_id: Option<u16>,
    #[serde(default)]
    pub gateway: Option<IpAddr>,
    #[serde(default)]
    pub primary: bool,
}

impl From<VmNicReport> for LegacyNic {
    fn from(r: VmNicReport) -> Self {
        Self {
            mac: r.mac,
            ip: r.ip,
            nic_tag: r.nic_tag,
            vlan_id: r.vlan_id,
            gateway: r.gateway,
            primary: r.primary,
        }
    }
}

/// A SmartOS zone observed on a registered CN that does not carry
/// the tritond `internal_metadata` identity tags -- i.e. it
/// pre-existed before tritonagent was installed on the CN, or was
/// created by an operator running `vmadm create` directly.
///
/// Legacy VMs live in their own FDB keyspace (`legacy_vm/...`) and
/// are visible only to fleet-admin operators via
/// `/v1/admin/legacy/vms`. They are NOT part of any tenant's
/// workload tree until adopted (deferred Phase D).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LegacyVm {
    /// SmartOS zone uuid. Stable across status reports.
    pub smartos_uuid: Uuid,
    /// CN currently hosting the zone. Updated by the classifier when
    /// the same `smartos_uuid` reports from a different CN (caused by
    /// an out-of-band `vmadm send|recv` evacuation).
    pub host_cn_uuid: Uuid,
    /// `vmadm`'s `owner_uuid` field (the legacy SmartOS owner).
    /// Distinct from any tritond identity. May be the zero UUID for
    /// system zones.
    #[serde(default)]
    pub legacy_owner_uuid: Option<Uuid>,
    /// Operator-assigned zone alias (the human-readable name shown by
    /// `vmadm list`). Used as the row name in the admin UI.
    #[serde(default)]
    pub alias: Option<String>,
    #[serde(default)]
    pub brand: Option<String>,
    #[serde(default)]
    pub state: Option<VmState>,
    #[serde(default)]
    pub zone_state: Option<String>,
    #[serde(default)]
    pub memory_bytes: Option<u64>,
    #[serde(default)]
    pub quota_bytes: Option<u64>,
    #[serde(default)]
    pub cpu_cap: Option<u32>,
    #[serde(default)]
    pub last_modified: Option<String>,
    #[serde(default)]
    pub nics: Vec<LegacyNic>,
    #[serde(default = "default_adoptable_state")]
    pub adoptable: AdoptableState,
    pub first_seen_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
}

fn default_adoptable_state() -> AdoptableState {
    AdoptableState::Unevaluated
}

#[cfg(test)]
mod vm_report_tests {
    use super::*;

    fn ids() -> (Uuid, Uuid, Uuid) {
        (
            Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap(),
            Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap(),
            Uuid::parse_str("33333333-3333-3333-3333-333333333333").unwrap(),
        )
    }

    fn report_with_metadata(metadata: BTreeMap<String, String>) -> VmReport {
        VmReport {
            uuid: Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap(),
            alias: None,
            brand: Some("joyent-minimal".to_string()),
            state: Some(VmState::Running),
            zone_state: Some("running".to_string()),
            max_physical_memory: Some(512),
            quota: Some(20),
            cpu_cap: Some(200),
            owner_uuid: Some(Uuid::nil()),
            last_modified: Some("2026-05-08T10:00:00Z".to_string()),
            internal_metadata: metadata,
            nics: Vec::new(),
        }
    }

    #[test]
    fn extract_managed_identity_returns_some_for_complete_metadata() {
        let (i, t, p) = ids();
        let mut md = BTreeMap::new();
        md.insert(TRITOND_METADATA_INSTANCE_ID.to_string(), i.to_string());
        md.insert(TRITOND_METADATA_TENANT_ID.to_string(), t.to_string());
        md.insert(TRITOND_METADATA_PROJECT_ID.to_string(), p.to_string());
        md.insert(
            TRITOND_METADATA_IDENTITY_HMAC.to_string(),
            "deadbeef".to_string(),
        );
        let report = report_with_metadata(md);
        let identity = report.extract_managed_identity().expect("identity present");
        assert_eq!(identity.instance_id, i);
        assert_eq!(identity.tenant_id, t);
        assert_eq!(identity.project_id, p);
        assert_eq!(identity.identity_hmac, "deadbeef");
    }

    #[test]
    fn extract_managed_identity_returns_none_when_any_key_is_missing() {
        let (i, t, p) = ids();
        for omit in [
            TRITOND_METADATA_INSTANCE_ID,
            TRITOND_METADATA_TENANT_ID,
            TRITOND_METADATA_PROJECT_ID,
            TRITOND_METADATA_IDENTITY_HMAC,
        ] {
            let mut md = BTreeMap::new();
            md.insert(TRITOND_METADATA_INSTANCE_ID.to_string(), i.to_string());
            md.insert(TRITOND_METADATA_TENANT_ID.to_string(), t.to_string());
            md.insert(TRITOND_METADATA_PROJECT_ID.to_string(), p.to_string());
            md.insert(
                TRITOND_METADATA_IDENTITY_HMAC.to_string(),
                "abc".to_string(),
            );
            md.remove(omit);
            assert!(
                report_with_metadata(md)
                    .extract_managed_identity()
                    .is_none(),
                "expected None when {omit} is missing",
            );
        }
    }

    #[test]
    fn extract_managed_identity_returns_none_for_unparseable_uuid() {
        let mut md = BTreeMap::new();
        md.insert(
            TRITOND_METADATA_INSTANCE_ID.to_string(),
            "not-a-uuid".to_string(),
        );
        md.insert(
            TRITOND_METADATA_TENANT_ID.to_string(),
            Uuid::nil().to_string(),
        );
        md.insert(
            TRITOND_METADATA_PROJECT_ID.to_string(),
            Uuid::nil().to_string(),
        );
        md.insert(
            TRITOND_METADATA_IDENTITY_HMAC.to_string(),
            "abc".to_string(),
        );
        assert!(
            report_with_metadata(md)
                .extract_managed_identity()
                .is_none()
        );
    }

    #[test]
    fn extract_managed_identity_returns_none_for_empty_hmac() {
        let (i, t, p) = ids();
        let mut md = BTreeMap::new();
        md.insert(TRITOND_METADATA_INSTANCE_ID.to_string(), i.to_string());
        md.insert(TRITOND_METADATA_TENANT_ID.to_string(), t.to_string());
        md.insert(TRITOND_METADATA_PROJECT_ID.to_string(), p.to_string());
        md.insert(TRITOND_METADATA_IDENTITY_HMAC.to_string(), String::new());
        assert!(
            report_with_metadata(md)
                .extract_managed_identity()
                .is_none()
        );
    }

    #[test]
    fn parse_vm_reports_returns_empty_vec_for_blob_without_vms_key() {
        assert!(parse_vm_reports(&serde_json::json!({})).is_empty());
        assert!(parse_vm_reports(&serde_json::json!({"timestamp": "..."})).is_empty());
    }

    #[test]
    fn parse_vm_reports_skips_unparseable_per_vm_entries_but_keeps_rest() {
        let blob = serde_json::json!({
            "vms": {
                "11111111-1111-1111-1111-111111111111": {
                    "uuid": "11111111-1111-1111-1111-111111111111",
                    "brand": "joyent-minimal",
                    "state": "running",
                },
                // Malformed entry: not an object.
                "22222222-2222-2222-2222-222222222222": "garbage",
                "33333333-3333-3333-3333-333333333333": {
                    "uuid": "33333333-3333-3333-3333-333333333333",
                    "brand": "bhyve",
                    "state": "stopped",
                },
            }
        });
        let reports = parse_vm_reports(&blob);
        assert_eq!(reports.len(), 2);
        let states: Vec<_> = reports.iter().filter_map(|r| r.state).collect();
        assert!(states.contains(&VmState::Running));
        assert!(states.contains(&VmState::Stopped));
    }

    #[test]
    fn vm_state_unknown_catches_unrecognized_values() {
        // Forward-compat invariant per type-safety rule #5: an unknown
        // wire value does not abort the parse.
        let blob = serde_json::json!({
            "vms": {
                "11111111-1111-1111-1111-111111111111": {
                    "uuid": "11111111-1111-1111-1111-111111111111",
                    "state": "some-future-state",
                }
            }
        });
        let reports = parse_vm_reports(&blob);
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].state, Some(VmState::Unknown));
    }

    #[test]
    fn parse_vm_reports_populates_internal_metadata_and_nics() {
        let blob = serde_json::json!({
            "vms": {
                "11111111-1111-1111-1111-111111111111": {
                    "uuid": "11111111-1111-1111-1111-111111111111",
                    "internal_metadata": {
                        "tritond:instance_id": "11111111-1111-1111-1111-111111111111",
                        "tritond:tenant_id": "22222222-2222-2222-2222-222222222222",
                    },
                    "nics": [
                        {
                            "mac": "02:00:00:de:ad:01",
                            "ip": "10.199.199.77",
                            "nic_tag": "admin",
                            "primary": true,
                        }
                    ],
                }
            }
        });
        let reports = parse_vm_reports(&blob);
        assert_eq!(reports.len(), 1);
        let r = &reports[0];
        assert_eq!(r.internal_metadata.len(), 2);
        assert_eq!(r.nics.len(), 1);
        assert_eq!(r.nics[0].nic_tag.as_deref(), Some("admin"));
        assert!(r.nics[0].primary);
    }
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
// `POST /v1/agent/network-realization` (Slice H-13). The control
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
/// (`/v1/storage/clusters/{id}/s3/*` etc.); attempting to call a
/// surface's endpoint family on a cluster registered under a
/// different surface returns a `409 Conflict`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
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
/// Populated by the `/v1/storage/clusters/{id}/health` endpoint, which
/// runs a probe against the cluster's `/admin/v1/cluster` summary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, Default)]
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
/// at `/v1/storage/clusters/{id}/...`.
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
    /// HTTP base URL for the cluster's S3 data plane (where presigned
    /// URLs point — typically `https://<host>:7443`). When `None`,
    /// presign endpoints return 409.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub s3_endpoint: Option<String>,
    /// IAM access key tritond signs presigned S3 URLs with. The
    /// operator configures this once per cluster via
    /// `POST /v1/storage/clusters/{id}/presigner`. Both fields are
    /// either `Some(_)` together or both `None`. Same Phase 0
    /// plaintext caveat as `admin_token`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presigner_access_key_id: Option<String>,
    /// Cleartext IAM secret key for the presigner identity. Never
    /// returned by any read endpoint; the wire-side
    /// [`StorageClusterView`] omits it entirely.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presigner_secret_access_key: Option<String>,
}

/// Body of `POST /v1/storage/clusters`.
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

/// Wire-side projection of [`StorageCluster`] that **redacts every
/// secret field** (admin token, presigner secret access key). This
/// is what `GET /v1/storage/clusters` and `GET /v1/storage/clusters/{id}`
/// return.
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
    /// S3 data-plane endpoint, surfaced to the UI so it can render
    /// "presigned uploads target X". `None` when no presigner is
    /// configured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub s3_endpoint: Option<String>,
    /// True when the cluster has a presigner identity configured.
    /// The frontend can hide upload UI when this is false. The
    /// AKID itself is non-sensitive (stable + auditable) and is
    /// surfaced so operators can confirm which identity is signing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presigner_access_key_id: Option<String>,
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
            s3_endpoint: c.s3_endpoint,
            // Surface the AKID but never the secret.
            presigner_access_key_id: c.presigner_access_key_id,
        }
    }
}

/// Body of `POST /v1/storage/clusters/{id}/presigner`. Operator
/// configures the IAM credential tritond signs presigned S3 URLs with.
/// Both fields are required so the call is unambiguous — to clear
/// the presigner, send empty strings (the handler treats that as
/// "unset").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SetPresignerRequest {
    /// Optional. When `None`, the cluster's existing
    /// `s3_endpoint` is left unchanged; when `Some`, it's replaced.
    /// Omit on subsequent rotations of the credential alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub s3_endpoint: Option<String>,
    pub access_key_id: String,
    pub secret_access_key: String,
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

// === Layered instance metadata (IMDS) =====================================
//
// See `IMDS_DESIGN.md` (rev 4). Metadata is stored at four scopes
// (silo / tenant / project / instance); a per-instance "realized"
// view is the precedence merge of all four plus a set of computed
// "system" keys. This section defines the storage-layer types and the
// key/value validation rules; the realized-view builder, the wire
// surface, and the IMDS daemon live in higher layers.

/// One of the four scopes a [`MetaValue`] can be attached to. The
/// realized view for an instance merges all four in
/// `Silo < Tenant < Project < Instance` precedence (most-specific
/// wins). Serialized lowercase for the `/v1/meta/{scope}/...` path
/// parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum MetaScope {
    Silo,
    Tenant,
    Project,
    Instance,
}

impl MetaScope {
    /// Stable lowercase tag (matches the wire form and the FDB
    /// keyspace segment).
    pub fn as_str(self) -> &'static str {
        match self {
            MetaScope::Silo => "silo",
            MetaScope::Tenant => "tenant",
            MetaScope::Project => "project",
            MetaScope::Instance => "instance",
        }
    }

    /// True for the one scope that hosts per-instance namespaces
    /// (`instance/*`, `guest/*`, `user-data`) and the only scope a
    /// guest writeback can touch.
    pub fn is_instance(self) -> bool {
        matches!(self, MetaScope::Instance)
    }

    /// Precedence rank used by the realized-view merge: higher wins.
    pub fn precedence(self) -> u8 {
        match self {
            MetaScope::Silo => 0,
            MetaScope::Tenant => 1,
            MetaScope::Project => 2,
            MetaScope::Instance => 3,
        }
    }
}

/// One metadata entry at one scope. JSON-encoded in FDB under
/// `meta/{silo,tenant,project,instance}/<uuid>/<key>`.
///
/// `value` is an arbitrary JSON value (strings are the common case;
/// IMDS serves a JSON string as `text/plain` and any other JSON as
/// `application/json`). The two boolean flags control the in-VM view:
/// `guest_visible` gates whether IMDS exposes the key at all, and
/// `guest_writable` gates whether the in-VM `PUT` may modify it (only
/// ever true for `guest/*` keys at instance scope on a writeback-
/// enabled instance — enforced by [`validate_meta_entry`] and the
/// store layer).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MetaValue {
    /// The stored value. Capped at [`MAX_META_VALUE_BYTES`] when
    /// serialized.
    pub value: serde_json::Value,
    /// Visible to processes inside the VM via IMDS. See
    /// [`default_guest_visible`] for the per-scope/per-prefix default.
    /// Generalizes the legacy SmartOS `internal_metadata` notion
    /// (`guest_visible == false` at instance scope).
    pub guest_visible: bool,
    /// The in-VM IMDS `PUT` may write this key. Only ever true for
    /// `guest/*` keys at instance scope on a writeback-enabled
    /// instance.
    pub guest_writable: bool,
    /// Who last wrote it: a user UUID, `"guest:<instance-id>"` for an
    /// in-VM `PUT`, or `"system"` for seed values.
    pub updated_by: String,
    pub updated_at: DateTime<Utc>,
}

impl MetaValue {
    /// Build a value with the conventional defaults for `scope`/`key`
    /// (`guest_visible` per [`default_guest_visible`], `guest_writable`
    /// false). `updated_at` is set to now.
    pub fn new(scope: MetaScope, key: &str, value: serde_json::Value, updated_by: String) -> Self {
        MetaValue {
            value,
            guest_visible: default_guest_visible(scope, key),
            guest_writable: false,
            updated_by,
            updated_at: Utc::now(),
        }
    }
}

/// Per-value byte cap (serialized JSON of `MetaValue.value`).
pub const MAX_META_VALUE_BYTES: usize = 64 * 1024;
/// Per-scope key-count cap (one scope's whole `meta/<scope>/<id>/` map).
pub const MAX_META_KEYS_PER_SCOPE: usize = 256;
/// Total realized-view byte cap per instance (sum of all merged
/// values' serialized JSON).
pub const MAX_REALIZED_BYTES_PER_INSTANCE: usize = 256 * 1024;
/// Maximum length of a metadata key in bytes.
pub const MAX_META_KEY_BYTES: usize = 256;
/// Maximum number of `/`-separated segments in a metadata key.
pub const MAX_META_KEY_DEPTH: usize = 8;

/// The distinguished instance-scope key carrying the cloud-init blob,
/// surfaced at the AWS path `/latest/user-data`.
pub const META_KEY_USER_DATA: &str = "user-data";
/// IMDS option: whether IMDS is served to this instance at all.
pub const META_KEY_IMDS_ENABLED: &str = "config/imds/enabled";
/// IMDS option: the response IP TTL / hop-limit.
pub const META_KEY_IMDS_HOP_LIMIT: &str = "config/imds/hop-limit";
/// Minimum legal `config/imds/hop-limit` value.
pub const IMDS_HOP_LIMIT_MIN: u64 = 1;
/// Maximum legal `config/imds/hop-limit` value.
pub const IMDS_HOP_LIMIT_MAX: u64 = 64;
/// Default `config/imds/hop-limit` when unset at every scope: on-box
/// only (the AWS SSRF-relay mitigation default).
pub const IMDS_HOP_LIMIT_DEFAULT: u64 = 1;

/// Top-level key namespace that decides who may write a key and at
/// which scopes it is allowed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MetaNamespace {
    /// `config/*` — layered shared configuration; any scope; settable.
    Config,
    /// `state/*` — layered low-frequency shared runtime state; any
    /// scope; settable.
    State,
    /// `instance/*` — per-instance operator metadata; instance scope
    /// only; settable.
    Instance,
    /// `guest/*` — guest-written per-instance state; instance scope
    /// only; settable (with the writeback gate at a higher layer).
    Guest,
    /// `user-data` — the distinguished cloud-init blob; instance scope
    /// only; settable.
    UserData,
    /// `meta-data/*`, `system/*`, `dynamic/*` — computed, never
    /// stored; `meta set` is rejected.
    Computed,
}

/// Validation failures for metadata keys/values. Higher layers map
/// these to HTTP 400.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MetaError {
    #[error("metadata key must not be empty")]
    EmptyKey,
    #[error("metadata key exceeds {MAX_META_KEY_BYTES} bytes")]
    KeyTooLong,
    #[error("metadata key exceeds {MAX_META_KEY_DEPTH} path segments")]
    KeyTooDeep,
    #[error("metadata key has an empty path segment (leading/trailing/double '/')")]
    EmptySegment,
    #[error("metadata key contains an illegal character: {0:?}")]
    BadChar(char),
    #[error("metadata key must start with a lowercase letter or digit, not {0:?}")]
    BadFirstChar(char),
    #[error(
        "unknown metadata key namespace: {0:?} (expected one of config/, state/, instance/, guest/, user-data)"
    )]
    UnknownNamespace(String),
    #[error("metadata key namespace {0:?} is only valid at instance scope")]
    ScopeNotInstance(&'static str),
    #[error("metadata key {0:?} is computed and cannot be set")]
    ReservedKey(&'static str),
    #[error("metadata key {0:?} requires at least one child segment")]
    MissingChildSegment(&'static str),
    #[error("config/imds/enabled must be a boolean")]
    ImdsEnabledNotBool,
    #[error(
        "config/imds/hop-limit must be an integer in {IMDS_HOP_LIMIT_MIN}..={IMDS_HOP_LIMIT_MAX}"
    )]
    ImdsHopLimitOutOfRange,
    #[error(
        "guest_writable is only allowed for guest/* keys at tenant/project/instance scope (not silo)"
    )]
    GuestWritableNotAllowed,
    #[error("metadata value exceeds {MAX_META_VALUE_BYTES} bytes")]
    ValueTooLarge,
}

/// Classify a key's top-level namespace and validate its syntax
/// (charset, segment structure, depth, length). Does not look at the
/// scope.
fn classify_meta_key(key: &str) -> Result<MetaNamespace, MetaError> {
    if key.is_empty() {
        return Err(MetaError::EmptyKey);
    }
    if key.len() > MAX_META_KEY_BYTES {
        return Err(MetaError::KeyTooLong);
    }
    let first = key.chars().next().unwrap();
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return Err(MetaError::BadFirstChar(first));
    }
    for ch in key.chars() {
        let ok =
            ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '.' | '_' | '/' | '-');
        if !ok {
            return Err(MetaError::BadChar(ch));
        }
    }
    let segments: Vec<&str> = key.split('/').collect();
    if segments.len() > MAX_META_KEY_DEPTH {
        return Err(MetaError::KeyTooDeep);
    }
    if segments.iter().any(|s| s.is_empty()) {
        return Err(MetaError::EmptySegment);
    }
    let ns = match segments[0] {
        "config" => MetaNamespace::Config,
        "state" => MetaNamespace::State,
        "instance" => MetaNamespace::Instance,
        "guest" => MetaNamespace::Guest,
        "user-data" => MetaNamespace::UserData,
        "meta-data" | "system" | "dynamic" => MetaNamespace::Computed,
        other => return Err(MetaError::UnknownNamespace(other.to_string())),
    };
    // The layered/per-instance namespaces are directory-shaped: they
    // need at least one child segment. `user-data` is a leaf.
    match ns {
        MetaNamespace::Config => {
            if segments.len() < 2 {
                return Err(MetaError::MissingChildSegment("config"));
            }
        }
        MetaNamespace::State => {
            if segments.len() < 2 {
                return Err(MetaError::MissingChildSegment("state"));
            }
        }
        MetaNamespace::Instance => {
            if segments.len() < 2 {
                return Err(MetaError::MissingChildSegment("instance"));
            }
        }
        MetaNamespace::Guest => {
            if segments.len() < 2 {
                return Err(MetaError::MissingChildSegment("guest"));
            }
        }
        MetaNamespace::UserData => {
            if segments.len() != 1 {
                return Err(MetaError::MissingChildSegment("user-data"));
            }
        }
        MetaNamespace::Computed => {}
    }
    Ok(ns)
}

/// Validate that `key` may be **set** at `scope` (syntax + namespace +
/// scope rules). Does not validate the value — see
/// [`validate_meta_entry`].
///
/// Scope rules:
///   * `config/`, `state/`, `instance/`, `guest/` — allowed at every
///     scope. The cascade handles inheritance: anything authored at a
///     wider scope flows downstream until a narrower scope overrides.
///   * `user-data` — instance scope only (cloud-init payload is by
///     nature per-instance; authoring it at silo / tenant / project
///     would conflate operator config with per-VM bootstrap data).
///   * `meta-data/*` / `system/*` / `dynamic/*` — reserved (computed
///     by tritond at request time, not operator-set).
pub fn validate_meta_key(scope: MetaScope, key: &str) -> Result<(), MetaError> {
    let ns = classify_meta_key(key)?;
    match ns {
        // The four operator namespaces all travel the cascade.
        MetaNamespace::Config
        | MetaNamespace::State
        | MetaNamespace::Instance
        | MetaNamespace::Guest => Ok(()),
        MetaNamespace::UserData => {
            if scope.is_instance() {
                Ok(())
            } else {
                Err(MetaError::ScopeNotInstance("user-data"))
            }
        }
        MetaNamespace::Computed => {
            // Map back to a stable label for the error.
            let label = match key.split('/').next().unwrap() {
                "meta-data" => "meta-data/",
                "system" => "system/",
                "dynamic" => "dynamic/",
                _ => "meta-data/",
            };
            Err(MetaError::ReservedKey(label))
        }
    }
}

/// True if `key` may carry `guest_writable == true` at `scope`.
///
/// Two conditions, both required:
///   * The key must be under the `guest/` namespace. `config/`,
///     `state/`, `instance/` are operator-author-only as far as the
///     guest is concerned — the guest may read them when they're
///     `guest_visible`, but the agent will never write them back.
///   * The scope must be tenant, project, or instance. Silo-scope
///     keys are operator-policy keys (DNS, NTP, cluster defaults);
///     allowing guests to mutate them through the writeback path
///     would let one tenant's instance break the silo for every
///     other tenant in the silo.
///
/// (Whether writeback is *enabled* on the instance is a separate,
/// higher-layer check — see `agent_set_instance_guest_meta`.)
pub fn meta_key_guest_writable_allowed(scope: MetaScope, key: &str) -> bool {
    !matches!(scope, MetaScope::Silo) && key.starts_with("guest/")
}

/// The conventional default for `MetaValue.guest_visible` given the
/// scope and key: `config/*` and `state/*` are guest-facing by design
/// (true at every scope); everything else defaults visible only at
/// project/instance scope (false at silo/tenant — the legacy
/// `internal_metadata` shape).
pub fn default_guest_visible(scope: MetaScope, key: &str) -> bool {
    if key.starts_with("config/") || key.starts_with("state/") {
        true
    } else {
        matches!(scope, MetaScope::Project | MetaScope::Instance)
    }
}

/// Full validation of a `(scope, key, value)` triple plus the
/// `guest_writable` flag the caller wants to set: syntax + namespace +
/// scope rules + value-type rules for the reserved `config/imds/*`
/// option keys + the byte cap + the `guest_writable` placement rule.
pub fn validate_meta_entry(
    scope: MetaScope,
    key: &str,
    value: &serde_json::Value,
    guest_writable: bool,
) -> Result<(), MetaError> {
    validate_meta_key(scope, key)?;

    // Reserved option keys are type-constrained.
    if key == META_KEY_IMDS_ENABLED {
        if !value.is_boolean() {
            return Err(MetaError::ImdsEnabledNotBool);
        }
    } else if key == META_KEY_IMDS_HOP_LIMIT {
        match value.as_u64() {
            Some(n) if (IMDS_HOP_LIMIT_MIN..=IMDS_HOP_LIMIT_MAX).contains(&n) => {}
            _ => return Err(MetaError::ImdsHopLimitOutOfRange),
        }
    }

    // Byte cap on the serialized value.
    let encoded = serde_json::to_vec(value).map_err(|_| MetaError::ValueTooLarge)?;
    if encoded.len() > MAX_META_VALUE_BYTES {
        return Err(MetaError::ValueTooLarge);
    }

    if guest_writable && !meta_key_guest_writable_allowed(scope, key) {
        return Err(MetaError::GuestWritableNotAllowed);
    }

    Ok(())
}

#[cfg(test)]
mod meta_tests {
    use super::*;

    #[test]
    fn meta_scope_wire_tags_and_precedence_are_stable() {
        for (s, tag, p) in [
            (MetaScope::Silo, "silo", 0u8),
            (MetaScope::Tenant, "tenant", 1),
            (MetaScope::Project, "project", 2),
            (MetaScope::Instance, "instance", 3),
        ] {
            assert_eq!(s.as_str(), tag);
            assert_eq!(s.precedence(), p);
            assert_eq!(serde_json::to_value(s).unwrap(), serde_json::json!(tag));
            assert_eq!(
                serde_json::from_value::<MetaScope>(serde_json::json!(tag)).unwrap(),
                s
            );
        }
        assert!(MetaScope::Instance.is_instance());
        assert!(!MetaScope::Project.is_instance());
        assert!(MetaScope::Instance.precedence() > MetaScope::Silo.precedence());
    }

    #[test]
    fn config_and_state_keys_are_valid_at_every_scope() {
        for scope in [
            MetaScope::Silo,
            MetaScope::Tenant,
            MetaScope::Project,
            MetaScope::Instance,
        ] {
            validate_meta_key(scope, "config/ntp-servers").unwrap();
            validate_meta_key(scope, "state/active-color").unwrap();
            validate_meta_key(scope, "config/imds/enabled").unwrap();
            validate_meta_key(scope, "config/imds/hop-limit").unwrap();
            // Defaults: config/* and state/* are guest-visible everywhere.
            assert!(default_guest_visible(scope, "config/ntp-servers"));
            assert!(default_guest_visible(scope, "state/leader"));
        }
    }

    #[test]
    fn user_data_is_rejected_above_instance() {
        // user-data is per-VM cloud-init payload — authoring it at a
        // wider scope would conflate operator config with bootstrap
        // data. The other operator namespaces (config/, state/,
        // instance/, guest/) all travel the cascade and are tested in
        // `config_state_instance_guest_allowed_at_every_scope`.
        for scope in [MetaScope::Silo, MetaScope::Tenant, MetaScope::Project] {
            assert!(matches!(
                validate_meta_key(scope, "user-data"),
                Err(MetaError::ScopeNotInstance("user-data"))
            ));
        }
        validate_meta_key(MetaScope::Instance, "user-data").unwrap();
        // instance/* and guest/* are guest-visible-by-default at
        // instance scope; not at silo scope.
        assert!(default_guest_visible(MetaScope::Instance, "instance/role"));
        assert!(default_guest_visible(MetaScope::Instance, "user-data"));
        assert!(!default_guest_visible(MetaScope::Silo, "instance/role"));
    }

    #[test]
    fn computed_namespaces_cannot_be_set() {
        for key in [
            "meta-data/instance-id",
            "system/package",
            "dynamic/realized",
            "system/cn-uuid",
        ] {
            assert!(matches!(
                validate_meta_key(MetaScope::Instance, key),
                Err(MetaError::ReservedKey(_))
            ));
        }
    }

    #[test]
    fn unknown_namespace_and_syntax_errors() {
        assert!(matches!(
            validate_meta_key(MetaScope::Instance, "tritond.instance_id"),
            Err(MetaError::UnknownNamespace(_))
        ));
        assert!(matches!(
            validate_meta_key(MetaScope::Instance, ""),
            Err(MetaError::EmptyKey)
        ));
        assert!(matches!(
            validate_meta_key(MetaScope::Instance, "config//x"),
            Err(MetaError::EmptySegment)
        ));
        assert!(matches!(
            validate_meta_key(MetaScope::Instance, "config/x/"),
            Err(MetaError::EmptySegment)
        ));
        assert!(matches!(
            validate_meta_key(MetaScope::Instance, "/config/x"),
            // leading '/' -> first char is '/', which fails BadFirstChar
            Err(MetaError::BadFirstChar('/'))
        ));
        assert!(matches!(
            validate_meta_key(MetaScope::Instance, "Config/x"),
            Err(MetaError::BadFirstChar('C'))
        ));
        assert!(matches!(
            validate_meta_key(MetaScope::Instance, "config/x y"),
            Err(MetaError::BadChar(' '))
        ));
        assert!(matches!(
            validate_meta_key(MetaScope::Instance, "config"),
            Err(MetaError::MissingChildSegment("config"))
        ));
        assert!(matches!(
            validate_meta_key(MetaScope::Instance, "user-data/foo"),
            Err(MetaError::MissingChildSegment("user-data"))
        ));
        let deep = format!("config/{}", "a/".repeat(MAX_META_KEY_DEPTH));
        assert!(matches!(
            validate_meta_key(MetaScope::Instance, &deep),
            Err(MetaError::KeyTooDeep)
        ));
        let long = format!("config/{}", "a".repeat(MAX_META_KEY_BYTES));
        assert!(matches!(
            validate_meta_key(MetaScope::Instance, &long),
            Err(MetaError::KeyTooLong)
        ));
    }

    #[test]
    fn imds_option_value_types_are_enforced() {
        validate_meta_entry(
            MetaScope::Tenant,
            META_KEY_IMDS_ENABLED,
            &serde_json::json!(true),
            false,
        )
        .unwrap();
        assert!(matches!(
            validate_meta_entry(
                MetaScope::Tenant,
                META_KEY_IMDS_ENABLED,
                &serde_json::json!("yes"),
                false
            ),
            Err(MetaError::ImdsEnabledNotBool)
        ));
        validate_meta_entry(
            MetaScope::Project,
            META_KEY_IMDS_HOP_LIMIT,
            &serde_json::json!(2),
            false,
        )
        .unwrap();
        for bad in [
            serde_json::json!(0),
            serde_json::json!(65),
            serde_json::json!("2"),
        ] {
            assert!(matches!(
                validate_meta_entry(MetaScope::Project, META_KEY_IMDS_HOP_LIMIT, &bad, false),
                Err(MetaError::ImdsHopLimitOutOfRange)
            ));
        }
    }

    #[test]
    fn guest_writable_for_guest_keys_at_tenant_project_instance() {
        // Allowed at the three narrower scopes when the key is under
        // `guest/`.
        for scope in [MetaScope::Tenant, MetaScope::Project, MetaScope::Instance] {
            assert!(
                meta_key_guest_writable_allowed(scope, "guest/role"),
                "guest/role should be writable at {scope:?}"
            );
        }
        // Rejected at silo even for guest/ keys — silo-scope keys are
        // operator-policy and guests never mutate them.
        assert!(!meta_key_guest_writable_allowed(
            MetaScope::Silo,
            "guest/role"
        ));
        // Wrong namespace at the right scope is still rejected — the
        // guest writeback path only ever touches `guest/*`.
        assert!(!meta_key_guest_writable_allowed(
            MetaScope::Instance,
            "config/x"
        ));
        assert!(!meta_key_guest_writable_allowed(
            MetaScope::Tenant,
            "config/x"
        ));

        // validate_meta_entry mirrors the same rule end-to-end.
        for scope in [MetaScope::Tenant, MetaScope::Project, MetaScope::Instance] {
            validate_meta_entry(scope, "guest/role", &serde_json::json!("replica"), true)
                .unwrap_or_else(|e| panic!("expected guest/role to validate at {scope:?}: {e}"));
        }
        assert!(matches!(
            validate_meta_entry(
                MetaScope::Silo,
                "guest/role",
                &serde_json::json!("replica"),
                true,
            ),
            Err(MetaError::GuestWritableNotAllowed)
        ));
        assert!(matches!(
            validate_meta_entry(
                MetaScope::Instance,
                "instance/role",
                &serde_json::json!("web"),
                true
            ),
            Err(MetaError::GuestWritableNotAllowed)
        ));
    }

    #[test]
    fn config_state_instance_guest_allowed_at_every_scope() {
        // The four operator namespaces travel the full cascade — set
        // any of them at any scope and validation accepts the syntax.
        for scope in [
            MetaScope::Silo,
            MetaScope::Tenant,
            MetaScope::Project,
            MetaScope::Instance,
        ] {
            for key in [
                "config/dns-resolvers",
                "state/marker",
                "instance/role",
                "guest/role",
            ] {
                validate_meta_key(scope, key)
                    .unwrap_or_else(|e| panic!("expected {key} to validate at {scope:?}: {e}"));
            }
        }
    }

    #[test]
    fn user_data_is_instance_only() {
        // user-data is per-VM cloud-init; authoring it at silo /
        // tenant / project would conflate operator config with
        // bootstrap payload.
        validate_meta_key(MetaScope::Instance, "user-data").unwrap();
        for scope in [MetaScope::Silo, MetaScope::Tenant, MetaScope::Project] {
            assert!(matches!(
                validate_meta_key(scope, "user-data"),
                Err(MetaError::ScopeNotInstance("user-data"))
            ));
        }
    }

    #[test]
    fn value_byte_cap_is_enforced() {
        let big = serde_json::Value::String("x".repeat(MAX_META_VALUE_BYTES + 1));
        assert!(matches!(
            validate_meta_entry(MetaScope::Instance, "user-data", &big, false),
            Err(MetaError::ValueTooLarge)
        ));
        let ok = serde_json::Value::String("x".repeat(1024));
        validate_meta_entry(MetaScope::Instance, "user-data", &ok, false).unwrap();
    }

    #[test]
    fn meta_value_new_sets_conventional_defaults() {
        let v = MetaValue::new(
            MetaScope::Tenant,
            "config/ntp-servers",
            serde_json::json!("10.0.0.2"),
            "user:abc".to_string(),
        );
        assert!(v.guest_visible);
        assert!(!v.guest_writable);
        assert_eq!(v.updated_by, "user:abc");

        let v2 = MetaValue::new(
            MetaScope::Silo,
            "instance/x",
            serde_json::json!("y"),
            "system".to_string(),
        );
        // non-config/state at silo scope -> not guest-visible by default
        assert!(!v2.guest_visible);
    }
}

/// The precedence merge of one instance's four metadata scopes — the
/// "stored" half of the realized view (`IMDS_DESIGN.md` §1.5). The
/// computed "system" keys (`meta-data/*`, `triton/system/*`, …) are
/// layered on top of this by `tritond` from the Instance/NIC/Subnet/…
/// records; this struct is just the part the storage layer can produce
/// on its own.
///
/// Merge rule: for any key present at more than one scope the
/// highest-precedence scope wins (`Silo < Tenant < Project <
/// Instance`). In practice only `config/*` and `state/*` are
/// cross-scope (validation pins `instance/*`, `guest/*`, `user-data`
/// to instance scope), so for those the answer is just "the instance's
/// copy"; the merge handles all of it uniformly.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RealizedMeta {
    /// `key` → (effective value, the scope it came from). `BTreeMap`
    /// so iteration is key-sorted.
    pub entries: BTreeMap<String, (MetaValue, MetaScope)>,
}

impl RealizedMeta {
    /// Merge the four scopes' stored metadata (each a key→value list,
    /// e.g. as returned by [`crate::Store::list_meta`]).
    pub fn merge(
        silo: &[(String, MetaValue)],
        tenant: &[(String, MetaValue)],
        project: &[(String, MetaValue)],
        instance: &[(String, MetaValue)],
    ) -> Self {
        let mut entries: BTreeMap<String, (MetaValue, MetaScope)> = BTreeMap::new();
        for (scope, list) in [
            (MetaScope::Silo, silo),
            (MetaScope::Tenant, tenant),
            (MetaScope::Project, project),
            (MetaScope::Instance, instance),
        ] {
            for (k, v) in list {
                // Iterating in ascending precedence order means a later
                // (higher-precedence) scope overwrites an earlier one.
                entries.insert(k.clone(), (v.clone(), scope));
            }
        }
        RealizedMeta { entries }
    }

    /// The subset a process inside the VM may see: entries with
    /// `guest_visible == true`.
    pub fn guest_visible(&self) -> RealizedMeta {
        RealizedMeta {
            entries: self
                .entries
                .iter()
                .filter(|(_, (v, _))| v.guest_visible)
                .map(|(k, vs)| (k.clone(), vs.clone()))
                .collect(),
        }
    }

    /// Look up an effective value (and the scope it came from) by key.
    pub fn get(&self, key: &str) -> Option<&(MetaValue, MetaScope)> {
        self.entries.get(key)
    }

    /// Whether IMDS is served to this instance: the realized
    /// `config/imds/enabled` if any scope pins one, otherwise the
    /// cluster default `default_enabled` (sourced from
    /// `Settings::imds_enabled_default`; the compiled-in fallback is
    /// [`DEFAULT_IMDS_ENABLED`]).
    pub fn imds_enabled(&self, default_enabled: bool) -> bool {
        self.entries
            .get(META_KEY_IMDS_ENABLED)
            .and_then(|(v, _)| v.value.as_bool())
            .unwrap_or(default_enabled)
    }

    /// The IMDS response hop-limit: the realized `config/imds/hop-limit`
    /// clamped to `[IMDS_HOP_LIMIT_MIN, IMDS_HOP_LIMIT_MAX]`, falling
    /// back to the cluster default `default_hop_limit` (sourced from
    /// `Settings::imds_hop_limit_default`; the compiled-in fallback is
    /// [`DEFAULT_IMDS_HOP_LIMIT`]).
    pub fn imds_hop_limit(&self, default_hop_limit: u64) -> u64 {
        self.entries
            .get(META_KEY_IMDS_HOP_LIMIT)
            .and_then(|(v, _)| v.value.as_u64())
            .map(|n| n.clamp(IMDS_HOP_LIMIT_MIN, IMDS_HOP_LIMIT_MAX))
            .unwrap_or_else(|| default_hop_limit.clamp(IMDS_HOP_LIMIT_MIN, IMDS_HOP_LIMIT_MAX))
    }
}

#[cfg(test)]
mod realized_meta_tests {
    use super::*;

    fn mv(scope: MetaScope, key: &str, val: serde_json::Value) -> (String, MetaValue) {
        (
            key.to_string(),
            MetaValue::new(scope, key, val, "system".to_string()),
        )
    }

    #[test]
    fn instance_wins_over_project_wins_over_tenant_wins_over_silo() {
        let silo = vec![
            mv(
                MetaScope::Silo,
                "config/ntp-servers",
                serde_json::json!("silo"),
            ),
            mv(
                MetaScope::Silo,
                "config/ca-bundle",
                serde_json::json!("silo-ca"),
            ),
        ];
        let tenant = vec![mv(
            MetaScope::Tenant,
            "config/ntp-servers",
            serde_json::json!("tenant"),
        )];
        let project = vec![
            mv(
                MetaScope::Project,
                "config/ntp-servers",
                serde_json::json!("project"),
            ),
            mv(
                MetaScope::Project,
                "state/active-color",
                serde_json::json!("blue"),
            ),
        ];
        let instance = vec![
            mv(
                MetaScope::Instance,
                "config/ntp-servers",
                serde_json::json!("instance"),
            ),
            mv(
                MetaScope::Instance,
                "instance/role",
                serde_json::json!("web"),
            ),
        ];
        let r = RealizedMeta::merge(&silo, &tenant, &project, &instance);
        let (ntp, ntp_from) = r.get("config/ntp-servers").unwrap();
        assert_eq!(ntp.value, serde_json::json!("instance"));
        assert_eq!(*ntp_from, MetaScope::Instance);
        assert_eq!(r.get("config/ca-bundle").unwrap().1, MetaScope::Silo);
        assert_eq!(r.get("state/active-color").unwrap().1, MetaScope::Project);
        assert_eq!(r.get("instance/role").unwrap().1, MetaScope::Instance);
        // Iteration is key-sorted.
        assert_eq!(
            r.entries.keys().cloned().collect::<Vec<_>>(),
            [
                "config/ca-bundle",
                "config/ntp-servers",
                "instance/role",
                "state/active-color"
            ]
        );
    }

    #[test]
    fn guest_visible_filters_invisible_entries() {
        let mut hidden = MetaValue::new(
            MetaScope::Tenant,
            "config/secret",
            serde_json::json!("x"),
            "u".to_string(),
        );
        hidden.guest_visible = false;
        let tenant = vec![
            mv(
                MetaScope::Tenant,
                "config/ntp-servers",
                serde_json::json!("a"),
            ),
            ("config/secret".to_string(), hidden),
        ];
        let r = RealizedMeta::merge(&[], &tenant, &[], &[]);
        assert!(r.get("config/secret").is_some());
        let g = r.guest_visible();
        assert!(g.get("config/secret").is_none());
        assert!(g.get("config/ntp-servers").is_some());
    }

    #[test]
    fn imds_options_resolve_with_defaults_and_clamp() {
        // Unset -> cluster defaults passed in by the caller. The
        // builtin fallback constants are the value tritond loads when
        // Settings hasn't been written yet.
        let empty = RealizedMeta::default();
        assert!(empty.imds_enabled(DEFAULT_IMDS_ENABLED));
        assert!(!empty.imds_enabled(false), "default flips with caller");
        assert_eq!(
            empty.imds_hop_limit(IMDS_HOP_LIMIT_DEFAULT),
            IMDS_HOP_LIMIT_DEFAULT
        );
        assert_eq!(empty.imds_hop_limit(3), 3, "default flips with caller");

        // Set at tenant, overridden at project.
        let tenant = vec![
            mv(
                MetaScope::Tenant,
                META_KEY_IMDS_ENABLED,
                serde_json::json!(false),
            ),
            mv(
                MetaScope::Tenant,
                META_KEY_IMDS_HOP_LIMIT,
                serde_json::json!(8),
            ),
        ];
        let project = vec![mv(
            MetaScope::Project,
            META_KEY_IMDS_HOP_LIMIT,
            serde_json::json!(2),
        )];
        let r = RealizedMeta::merge(&[], &tenant, &project, &[]);
        // Pinned value wins; the caller-supplied default is ignored.
        assert!(!r.imds_enabled(true));
        assert_eq!(r.imds_hop_limit(IMDS_HOP_LIMIT_DEFAULT), 2);
        assert_eq!(
            r.get(META_KEY_IMDS_HOP_LIMIT).unwrap().1,
            MetaScope::Project
        );

        // Out-of-range value gets clamped (defensive).
        let bad = vec![mv(
            MetaScope::Silo,
            META_KEY_IMDS_HOP_LIMIT,
            serde_json::json!(9999),
        )];
        assert_eq!(
            RealizedMeta::merge(&bad, &[], &[], &[]).imds_hop_limit(IMDS_HOP_LIMIT_DEFAULT),
            IMDS_HOP_LIMIT_MAX
        );
    }
}

/// Build the *computed* ("system") metadata keys for one instance —
/// the AWS-compatible `meta-data/*` facts plus the Triton-native
/// `triton/system/*` facts (`IMDS_DESIGN.md` §1.2 / §1.5). These are
/// never stored: `tritond` derives them from the Instance / NIC / VPC /
/// Image records (and the resolved SSH public keys) on every realized-
/// view build, and they layer on top of the stored [`RealizedMeta`]
/// merge. Returned entries all have provenance "system" (the caller
/// knows this) and `guest_writable == false`; `guest_visible` is
/// `false` for `triton/system/cn-uuid` (operator-only) and `true`
/// otherwise.
///
/// Records that aren't available yet (a half-provisioned instance with
/// no NIC, say) are simply skipped — the function degrades to whatever
/// it can compute.
pub fn computed_metadata(
    instance: &Instance,
    primary_nic: Option<&Nic>,
    vpc: Option<&Vpc>,
    image: Option<&Image>,
    ssh_public_keys: &[String],
) -> Vec<(String, MetaValue)> {
    fn entry(
        key: &str,
        value: serde_json::Value,
        guest_visible: bool,
        at: DateTime<Utc>,
    ) -> (String, MetaValue) {
        (
            key.to_string(),
            MetaValue {
                value,
                guest_visible,
                guest_writable: false,
                updated_by: "system".to_string(),
                updated_at: at,
            },
        )
    }
    let at = instance.updated_at;
    let mut out: Vec<(String, MetaValue)> = Vec::new();

    let hostname = if instance.name.is_empty() {
        format!("tritond-{}", instance.id)
    } else {
        instance.name.clone()
    };
    let memory_mib = instance.memory_bytes / (1024 * 1024);
    let brand = serde_json::to_value(instance.brand)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| "unknown".to_string());

    // ---- AWS-compatible `meta-data/*` ----
    out.push(entry(
        "meta-data/instance-id",
        serde_json::json!(instance.id.to_string()),
        true,
        at,
    ));
    out.push(entry(
        "meta-data/ami-id",
        serde_json::json!(instance.image_id.to_string()),
        true,
        at,
    ));
    out.push(entry(
        "meta-data/instance-type",
        serde_json::json!(format!("{}vcpu-{}m", instance.cpu, memory_mib)),
        true,
        at,
    ));
    out.push(entry(
        "meta-data/local-hostname",
        serde_json::json!(hostname.clone()),
        true,
        at,
    ));
    out.push(entry(
        "meta-data/hostname",
        serde_json::json!(hostname.clone()),
        true,
        at,
    ));
    if let Some(nic) = primary_nic {
        out.push(entry(
            "meta-data/mac",
            serde_json::json!(nic.mac.clone()),
            true,
            at,
        ));
        if let Some(ip) = nic.primary_ipv4 {
            out.push(entry(
                "meta-data/local-ipv4",
                serde_json::json!(ip.to_string()),
                true,
                at,
            ));
        }
        if let Some(ip) = nic.primary_ipv6 {
            out.push(entry(
                "meta-data/local-ipv6",
                serde_json::json!(ip.to_string()),
                true,
                at,
            ));
        }
    }
    for (i, key) in ssh_public_keys.iter().enumerate() {
        out.push(entry(
            &format!("meta-data/public-keys/{i}/openssh-key"),
            serde_json::json!(key),
            true,
            at,
        ));
    }

    // ---- Triton-native `triton/system/*` ----
    if let Some(nic) = primary_nic {
        out.push(entry(
            "triton/system/vpc-id",
            serde_json::json!(nic.vpc_id.to_string()),
            true,
            at,
        ));
        out.push(entry(
            "triton/system/subnet-id",
            serde_json::json!(nic.subnet_id.to_string()),
            true,
            at,
        ));
    }
    if let Some(v) = vpc {
        out.push(entry(
            "triton/system/vni",
            serde_json::json!(v.vni),
            true,
            at,
        ));
    }
    out.push(entry(
        "triton/system/image-id",
        serde_json::json!(instance.image_id.to_string()),
        true,
        at,
    ));
    if let Some(img) = image {
        out.push(entry(
            "triton/system/image-name",
            serde_json::json!(img.name.clone()),
            true,
            at,
        ));
        out.push(entry(
            "triton/system/image-os",
            serde_json::json!(img.os.clone()),
            true,
            at,
        ));
        out.push(entry(
            "triton/system/image-version",
            serde_json::json!(img.version.clone()),
            true,
            at,
        ));
    }
    out.push(entry(
        "triton/system/brand",
        serde_json::json!(brand),
        true,
        at,
    ));
    out.push(entry(
        "triton/system/cpu",
        serde_json::json!(instance.cpu),
        true,
        at,
    ));
    out.push(entry(
        "triton/system/memory-mib",
        serde_json::json!(memory_mib),
        true,
        at,
    ));
    // Owner facts as two flat leaves rather than one nested object.
    // Matches the rest of `triton/system/*` (every key is a single
    // scalar value) and AWS-style key-path convention; renders
    // cleanly in any UI without special-casing structured values.
    out.push(entry(
        "triton/system/owner/tenant",
        serde_json::json!(instance.tenant_id.to_string()),
        true,
        at,
    ));
    out.push(entry(
        "triton/system/owner/project",
        serde_json::json!(instance.project_id.to_string()),
        true,
        at,
    ));
    out.push(entry(
        "triton/system/created-at",
        serde_json::json!(instance.created_at.to_rfc3339()),
        true,
        at,
    ));
    if let Some(cn) = instance.host_cn_uuid {
        // Operator-only: a guest should not learn which physical host
        // it runs on.
        out.push(entry(
            "triton/system/cn-uuid",
            serde_json::json!(cn.to_string()),
            false,
            at,
        ));
    }

    out
}

#[cfg(test)]
mod computed_meta_tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn sample_instance() -> Instance {
        let now = Utc::now();
        Instance {
            id: Uuid::parse_str("0a1b2c3d-4e5f-6071-8293-a4b5c6d7e8f9").unwrap(),
            tenant_id: Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap(),
            project_id: Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap(),
            name: "web0".to_string(),
            description: String::new(),
            image_id: Uuid::parse_str("33333333-3333-3333-3333-333333333333").unwrap(),
            brand: InstanceBrand::Bhyve,
            primary_subnet_id: Uuid::parse_str("44444444-4444-4444-4444-444444444444").unwrap(),
            ssh_key_ids: vec![],
            cpu: 2,
            memory_bytes: 2 * 1024 * 1024 * 1024,
            host_cn_uuid: Some(Uuid::parse_str("55555555-5555-5555-5555-555555555555").unwrap()),
            lifecycle: LifecycleState::Running,
            created_at: now,
            updated_at: now,
        }
    }

    fn sample_nic(instance_id: Uuid) -> Nic {
        Nic {
            id: Uuid::parse_str("66666666-6666-6666-6666-666666666666").unwrap(),
            tenant_id: Uuid::nil(),
            project_id: Uuid::nil(),
            instance_id,
            vpc_id: Uuid::parse_str("77777777-7777-7777-7777-777777777777").unwrap(),
            subnet_id: Uuid::parse_str("44444444-4444-4444-4444-444444444444").unwrap(),
            name: "net0".to_string(),
            mac: "02:1a:b3:cd:ef:42".to_string(),
            primary_ipv4: Some(Ipv4Addr::new(10, 0, 0, 42)),
            primary_ipv6: None,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn computed_keys_cover_aws_and_triton_facts() {
        let inst = sample_instance();
        let nic = sample_nic(inst.id);
        let keys = computed_metadata(
            &inst,
            Some(&nic),
            None,
            None,
            &["ssh-ed25519 AAAA".to_string()],
        );
        let map: std::collections::BTreeMap<_, _> =
            keys.iter().map(|(k, v)| (k.clone(), v.clone())).collect();

        assert_eq!(
            map["meta-data/instance-id"].value,
            serde_json::json!(inst.id.to_string())
        );
        assert_eq!(
            map["meta-data/local-hostname"].value,
            serde_json::json!("web0")
        );
        assert_eq!(map["meta-data/hostname"].value, serde_json::json!("web0"));
        assert_eq!(
            map["meta-data/instance-type"].value,
            serde_json::json!("2vcpu-2048m")
        );
        assert_eq!(
            map["meta-data/mac"].value,
            serde_json::json!("02:1a:b3:cd:ef:42")
        );
        assert_eq!(
            map["meta-data/local-ipv4"].value,
            serde_json::json!("10.0.0.42")
        );
        assert!(!map.contains_key("meta-data/local-ipv6"));
        assert_eq!(
            map["meta-data/public-keys/0/openssh-key"].value,
            serde_json::json!("ssh-ed25519 AAAA")
        );
        assert_eq!(
            map["triton/system/vpc-id"].value,
            serde_json::json!(nic.vpc_id.to_string())
        );
        assert_eq!(
            map["triton/system/subnet-id"].value,
            serde_json::json!(nic.subnet_id.to_string())
        );
        assert_eq!(map["triton/system/brand"].value, serde_json::json!("bhyve"));
        assert_eq!(map["triton/system/cpu"].value, serde_json::json!(2));
        assert_eq!(
            map["triton/system/memory-mib"].value,
            serde_json::json!(2048)
        );
        // Owner is two flat leaves, matching the rest of triton/system/*.
        assert_eq!(
            map["triton/system/owner/tenant"].value,
            serde_json::json!(inst.tenant_id.to_string())
        );
        assert_eq!(
            map["triton/system/owner/project"].value,
            serde_json::json!(inst.project_id.to_string())
        );
        // cn-uuid present but guest-invisible.
        assert!(map.contains_key("triton/system/cn-uuid"));
        assert!(!map["triton/system/cn-uuid"].guest_visible);
        // Everything else is guest-visible and never guest-writable.
        for (k, v) in &keys {
            assert!(!v.guest_writable, "{k} should not be guest_writable");
            if k != "triton/system/cn-uuid" {
                assert!(v.guest_visible, "{k} should be guest_visible");
            }
        }
    }

    #[test]
    fn computed_keys_degrade_without_nic_or_name() {
        let mut inst = sample_instance();
        inst.name = String::new();
        inst.host_cn_uuid = None;
        let keys = computed_metadata(&inst, None, None, None, &[]);
        let map: std::collections::BTreeMap<_, _> =
            keys.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        assert_eq!(
            map["meta-data/local-hostname"].value,
            serde_json::json!(format!("tritond-{}", inst.id))
        );
        assert!(!map.contains_key("meta-data/mac"));
        assert!(!map.contains_key("meta-data/local-ipv4"));
        assert!(!map.contains_key("triton/system/vpc-id"));
        assert!(!map.contains_key("triton/system/cn-uuid"));
        // Image-id is always available (it's a field on Instance).
        assert_eq!(
            map["triton/system/image-id"].value,
            serde_json::json!(inst.image_id.to_string())
        );
    }
}

/// Where a leaf in the realized view came from. The four storage
/// scopes plus `System` for the computed keys ([`computed_metadata`]).
/// Serialized lowercase for the `triton/dynamic/realized` payload and
/// the `/v1/meta/instance/{id}/realized` response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum MetaProvenance {
    Silo,
    Tenant,
    Project,
    Instance,
    System,
}

impl MetaProvenance {
    /// Stable lowercase tag.
    pub fn as_str(self) -> &'static str {
        match self {
            MetaProvenance::Silo => "silo",
            MetaProvenance::Tenant => "tenant",
            MetaProvenance::Project => "project",
            MetaProvenance::Instance => "instance",
            MetaProvenance::System => "system",
        }
    }
}

impl From<MetaScope> for MetaProvenance {
    fn from(s: MetaScope) -> Self {
        match s {
            MetaScope::Silo => MetaProvenance::Silo,
            MetaScope::Tenant => MetaProvenance::Tenant,
            MetaScope::Project => MetaProvenance::Project,
            MetaScope::Instance => MetaProvenance::Instance,
        }
    }
}

/// One instance's *full* realized view: the stored-scope precedence
/// merge ([`RealizedMeta`]) with the computed "system" keys
/// ([`computed_metadata`]) layered on, each leaf tagged with its
/// [`MetaProvenance`]. This is what the `triton/dynamic/realized` IMDS
/// endpoint and the `/v1/meta/instance/{id}/realized` API serve
/// (the former filtered to `guest_visible`).
///
/// Computed keys are layered first, then the stored merge overrides;
/// in practice their namespaces are disjoint (`meta-data/*`,
/// `triton/system/*` are computed-only; `config/*`, `state/*`,
/// `instance/*`, `guest/*`, `user-data` are stored) so no real
/// collision occurs — the ordering just makes the rule unambiguous.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RealizedView {
    /// `key` → (effective value, where it came from). `BTreeMap` so
    /// iteration is key-sorted.
    pub entries: BTreeMap<String, (MetaValue, MetaProvenance)>,
}

impl RealizedView {
    /// Build the full realized view from the four storage scopes'
    /// metadata maps (as returned by [`crate::Store::list_meta`]) and
    /// the computed system keys (as returned by [`computed_metadata`]).
    pub fn build(
        silo: &[(String, MetaValue)],
        tenant: &[(String, MetaValue)],
        project: &[(String, MetaValue)],
        instance: &[(String, MetaValue)],
        computed: &[(String, MetaValue)],
    ) -> Self {
        let mut entries: BTreeMap<String, (MetaValue, MetaProvenance)> = BTreeMap::new();
        for (k, v) in computed {
            entries.insert(k.clone(), (v.clone(), MetaProvenance::System));
        }
        let merged = RealizedMeta::merge(silo, tenant, project, instance);
        for (k, (v, scope)) in merged.entries {
            entries.insert(k, (v, MetaProvenance::from(scope)));
        }
        RealizedView { entries }
    }

    /// The subset a process inside the VM may see: `guest_visible`
    /// entries only.
    pub fn guest_visible(&self) -> RealizedView {
        RealizedView {
            entries: self
                .entries
                .iter()
                .filter(|(_, (v, _))| v.guest_visible)
                .map(|(k, vp)| (k.clone(), vp.clone()))
                .collect(),
        }
    }

    /// Effective value + provenance for a key.
    pub fn get(&self, key: &str) -> Option<&(MetaValue, MetaProvenance)> {
        self.entries.get(key)
    }

    /// Whether IMDS is served to this instance: realized
    /// `config/imds/enabled` when any scope pins one, otherwise the
    /// cluster-default `default_enabled` (sourced from
    /// `Settings::imds_enabled_default`; the compiled-in fallback is
    /// [`DEFAULT_IMDS_ENABLED`]).
    pub fn imds_enabled(&self, default_enabled: bool) -> bool {
        self.entries
            .get(META_KEY_IMDS_ENABLED)
            .and_then(|(v, _)| v.value.as_bool())
            .unwrap_or(default_enabled)
    }

    /// The IMDS response hop-limit: realized `config/imds/hop-limit`
    /// when any scope pins one, otherwise the cluster-default
    /// `default_hop_limit` (sourced from
    /// `Settings::imds_hop_limit_default`; the compiled-in fallback
    /// is [`DEFAULT_IMDS_HOP_LIMIT`]). Clamped in either case to
    /// `[IMDS_HOP_LIMIT_MIN, IMDS_HOP_LIMIT_MAX]`.
    pub fn imds_hop_limit(&self, default_hop_limit: u64) -> u64 {
        self.entries
            .get(META_KEY_IMDS_HOP_LIMIT)
            .and_then(|(v, _)| v.value.as_u64())
            .map(|n| n.clamp(IMDS_HOP_LIMIT_MIN, IMDS_HOP_LIMIT_MAX))
            .unwrap_or_else(|| default_hop_limit.clamp(IMDS_HOP_LIMIT_MIN, IMDS_HOP_LIMIT_MAX))
    }
}

#[cfg(test)]
mod realized_view_tests {
    use super::*;

    fn mv(scope: MetaScope, key: &str, val: serde_json::Value) -> (String, MetaValue) {
        (
            key.to_string(),
            MetaValue::new(scope, key, val, "system".to_string()),
        )
    }

    #[test]
    fn provenance_wire_tags_and_from_scope() {
        for (p, tag) in [
            (MetaProvenance::Silo, "silo"),
            (MetaProvenance::Tenant, "tenant"),
            (MetaProvenance::Project, "project"),
            (MetaProvenance::Instance, "instance"),
            (MetaProvenance::System, "system"),
        ] {
            assert_eq!(p.as_str(), tag);
            assert_eq!(serde_json::to_value(p).unwrap(), serde_json::json!(tag));
        }
        assert_eq!(
            MetaProvenance::from(MetaScope::Tenant),
            MetaProvenance::Tenant
        );
        assert_eq!(
            MetaProvenance::from(MetaScope::Instance),
            MetaProvenance::Instance
        );
    }

    #[test]
    fn build_layers_computed_then_stored_with_provenance() {
        let computed = vec![
            mv(
                MetaScope::Instance,
                "meta-data/instance-id",
                serde_json::json!("i-1"),
            ),
            mv(
                MetaScope::Instance,
                "triton/system/brand",
                serde_json::json!("bhyve"),
            ),
        ];
        let silo = vec![mv(
            MetaScope::Silo,
            "config/ntp-servers",
            serde_json::json!("silo"),
        )];
        let project = vec![
            mv(
                MetaScope::Project,
                "config/ntp-servers",
                serde_json::json!("project"),
            ),
            mv(
                MetaScope::Project,
                "state/active-color",
                serde_json::json!("blue"),
            ),
        ];
        let instance = vec![mv(
            MetaScope::Instance,
            "instance/role",
            serde_json::json!("web"),
        )];
        let v = RealizedView::build(&silo, &[], &project, &instance, &computed);

        assert_eq!(
            v.get("meta-data/instance-id").unwrap().1,
            MetaProvenance::System
        );
        assert_eq!(
            v.get("triton/system/brand").unwrap().1,
            MetaProvenance::System
        );
        // config/ntp-servers set at silo and project -> project wins.
        let (ntp, ntp_from) = v.get("config/ntp-servers").unwrap();
        assert_eq!(ntp.value, serde_json::json!("project"));
        assert_eq!(*ntp_from, MetaProvenance::Project);
        assert_eq!(
            v.get("state/active-color").unwrap().1,
            MetaProvenance::Project
        );
        assert_eq!(v.get("instance/role").unwrap().1, MetaProvenance::Instance);
        // Key-sorted iteration.
        assert_eq!(
            v.entries.keys().cloned().collect::<Vec<_>>(),
            [
                "config/ntp-servers",
                "instance/role",
                "meta-data/instance-id",
                "state/active-color",
                "triton/system/brand",
            ]
        );
    }

    #[test]
    fn guest_visible_filter_and_imds_options_delegate() {
        let mut hidden = MetaValue::new(
            MetaScope::Silo,
            "config/cn-secret",
            serde_json::json!("x"),
            "u".to_string(),
        );
        hidden.guest_visible = false;
        let silo = vec![
            mv(
                MetaScope::Silo,
                "config/ntp-servers",
                serde_json::json!("a"),
            ),
            ("config/cn-secret".to_string(), hidden),
            mv(
                MetaScope::Silo,
                META_KEY_IMDS_ENABLED,
                serde_json::json!(false),
            ),
            mv(
                MetaScope::Silo,
                META_KEY_IMDS_HOP_LIMIT,
                serde_json::json!(2),
            ),
        ];
        let v = RealizedView::build(&silo, &[], &[], &[], &[]);
        assert!(v.get("config/cn-secret").is_some());
        assert!(!v.imds_enabled(DEFAULT_IMDS_ENABLED));
        assert_eq!(v.imds_hop_limit(DEFAULT_IMDS_HOP_LIMIT), 2);
        let g = v.guest_visible();
        assert!(g.get("config/cn-secret").is_none());
        assert!(g.get("config/ntp-servers").is_some());
        // imds-option keys are guest_visible by default, so still present.
        assert!(g.get(META_KEY_IMDS_ENABLED).is_some());
    }
}

/// Per-port IMDS binding shipped from tritond to tritonagent in the
/// provisioning blueprint -- the data the agent's `imds_bindings`
/// reverse-lookup table needs so the IMDS HTTP listener can resolve
/// caller identity from a connection's peer address (the design's
/// "Nitro card" caller-ID rule). See `IMDS_DESIGN.md` §2.1.
///
/// `pseudo_src` is the CN-unique address the proteus kmod SNATs this
/// port's IMDS-bound traffic to; `port_id` is the kmod-side port
/// identifier; `instance_id` is the VM this port belongs to.
///
/// On the wire this is one entry per port that has IMDS wired; a
/// blueprint may carry zero, one, or several (one per VPC the
/// instance has a NIC on, when multi-VPC IMDS lands; today: 0 or 1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ImdsBindingWire {
    #[schemars(with = "String")]
    pub pseudo_src: IpAddr,
    pub port_id: Uuid,
    pub instance_id: Uuid,
}

// ===========================================================================
// LM-1: Live-migration MigrationRecord + supporting types.
//
// This is the FDB row that anchors the live-migration saga
// (see /Users/nwilkens/.claude/plans/we-need-to-build-ancient-scone.md).
// The wire enum names match `vmapi-api::types::migrations` so the
// existing legacy-shape JSON is unchanged when we surface a record
// to the operator UI; the saga-internal phase machine uses these
// values directly.
// ===========================================================================

/// Lifecycle state of a migration row.
///
/// The state machine is:
///
/// ```text
///   begin → sync ⇄ paused
///     │       │
///     │       └──→ switch → successful | failed
///     │
///     └──→ aborted | failed | rolled_back
/// ```
///
/// `running` is a holding state used in legacy JSON for in-flight
/// records that don't fit the strict phase enum; vnext writes the
/// concrete phase but accepts `running` on read for forward compat.
/// `unknown` catches values we don't recognise yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum MigrationState {
    Begin,
    Estimate,
    Sync,
    Paused,
    Switch,
    Aborted,
    #[serde(rename = "rollback")]
    RolledBack,
    Successful,
    Failed,
    Running,
    #[serde(other)]
    Unknown,
}

impl MigrationState {
    /// Whether this state ends the migration. Terminal-state writes
    /// release the `migration/active/<instance>` guard (see
    /// `Store::put_migration`).
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            MigrationState::Aborted
                | MigrationState::RolledBack
                | MigrationState::Successful
                | MigrationState::Failed
        )
    }
}

/// Coarse-grained phase the migration is in. The saga catalog at
/// `services/tritond/src/sagas/migration.rs` (LM-5) drives the
/// transitions; `Begin` covers designate / pre-flight / target zone
/// create / initial ZFS, `Sync` covers iterative ZFS + memory
/// transfer, `Switch` covers pause / handoff / resume / cleanup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum MigrationPhase {
    Begin,
    Sync,
    Switch,
    #[serde(other)]
    Unknown,
}

/// Operator-initiated action against a migration record. The HTTP
/// handler at `POST /v1/instances/{id}/actions/migrate` (LM-1 task
/// #16 / LM-5) dispatches on this enum; `Begin` starts a saga, the
/// others address an existing migration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum MigrationAction {
    Begin,
    Estimate,
    Sync,
    Pause,
    Switch,
    Abort,
    Rollback,
    Finalize,
    #[serde(other)]
    Unknown,
}

/// Source-dataset details captured at migration start so the saga
/// can restore them on abort or rollback. The quota dance for bhyve
/// (legacy compatibility — see plan §5) requires temporarily zeroing
/// the source's `quota` and `refreservation` so snapshot send works;
/// the original values live here and the terminal action either
/// restores them on the source (abort) or applies them to the
/// target (successful migration).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SourceFilesystemDetails {
    /// `zones/<instance>` or similar — the dataset that hosts the
    /// instance's root.
    pub dataset: String,
    /// Original `quota` in bytes, if any.
    #[serde(default)]
    pub original_quota_bytes: Option<u64>,
    /// Original `refreservation` in bytes, if any.
    #[serde(default)]
    pub original_refreservation_bytes: Option<u64>,
    /// Snapshots taken during the migration (in send/receive order).
    /// On terminal cleanup these are destroyed on whichever side
    /// loses authority over the dataset.
    #[serde(default)]
    pub snapshots: Vec<String>,
    /// Whether the source dataset had `encryption=on`. v1 rejects
    /// encrypted-source migrations at the API edge; the flag is
    /// recorded so audits can show why.
    #[serde(default)]
    pub encrypted: bool,
}

/// First-class FDB row tracking one live migration. Created by the
/// `migrate-instance` saga's first action (LM-5) and mutated by
/// subsequent actions; on terminal-state transitions the saga
/// clears the `migration/active/<instance>` guard so a fresh
/// migration can start.
///
/// Persisted via the keys documented in `libs/tritond-store/src/fdb.rs`:
///
/// ```text
/// migration/by_id/<uuid>                          JSON-encoded MigrationRecord
/// migration/by_instance/<inst>/<inv_ts>/<uuid>    uuid bytes (history, newest-first)
/// migration/by_source_cn/<cn>/<inv_ts>/<uuid>     uuid bytes
/// migration/by_target_cn/<cn>/<inv_ts>/<uuid>     uuid bytes
/// migration/active/<instance>                     uuid bytes (presence == active)
/// migration/progress/<migration>/<seq>            JSON-encoded MigrationProgressEvent
/// ```
///
/// The active guard plus `Instance.migration_in_progress`
/// (added in LM-5) is the cross-handler advisory lock that keeps
/// start/stop/restart/delete from racing a migration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MigrationRecord {
    pub id: Uuid,
    pub instance_id: Uuid,
    pub tenant_id: Uuid,
    pub project_id: Uuid,
    /// CN the instance currently lives on. Pinned at saga create.
    pub source_cn: Uuid,
    /// CN the saga designates as the target. `None` until the
    /// designate action commits (LM-5).
    #[serde(default)]
    pub target_cn: Option<Uuid>,
    /// Steno saga id, bound after `saga_execute` returns.
    #[serde(default)]
    pub saga_id: Option<Uuid>,
    pub phase: MigrationPhase,
    pub state: MigrationState,
    /// Last action the operator asked for. The list of valid
    /// transitions per state is enforced by the dispatch handler;
    /// invalid combinations return 409.
    pub action_requested: MigrationAction,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub finished_at: Option<DateTime<Utc>>,
    /// Populated on `Failed` / `Aborted` / `RolledBack` terminal
    /// states with the kind+message string that surfaced from the
    /// failing saga node.
    #[serde(default)]
    pub error: Option<String>,
    /// NIC uuids whose `host_cn_uuid` is being moved by this
    /// migration. Tracked here for audit visibility and abort
    /// undo; the source-of-truth for current NIC location remains
    /// the `Nic` row.
    #[serde(default)]
    pub reserved_nics: Vec<Uuid>,
    /// Captured source dataset state. `None` for migrations that
    /// haven't completed the snapshot_source_quota saga node yet.
    #[serde(default)]
    pub source_filesystem_details: Option<SourceFilesystemDetails>,
    /// Cursor into the `migration/progress/<id>/<seq>` event log.
    /// Each agent progress POST CAS-increments this; the watch
    /// endpoint pages from `since=<seq>`.
    #[serde(default)]
    pub last_progress_seq: u64,
    /// Once set (typically after the switch action commits), the
    /// migration cannot be retried in either direction. Prevents
    /// accidental re-migration after a successful cutover.
    #[serde(default)]
    pub disallow_retry: bool,
    /// True if the migration was triggered automatically (eg. by
    /// the rebalance / evacuate driver) rather than by an
    /// operator. Used for audit log labels and metrics.
    #[serde(default)]
    pub automatic: bool,
}

/// One progress entry appended to the per-migration event log. The
/// agent's progress POSTs (LM-3) write one of these per phase
/// transition + per N-byte threshold during streaming; the admin UI
/// + `tcadm migrations get` pages through them via the `?since=<seq>`
/// query parameter.
///
/// The shape mirrors the legacy `MigrationProgress` JSON in
/// `vmapi-api::types::migrations` so existing operators see the
/// fields they expect.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MigrationProgressEvent {
    /// Monotonic per-migration sequence number. CAS-increments on
    /// each append; the FDB key trailer matches.
    pub seq: u64,
    /// Free-form type label (e.g. `"progress"`, `"phase_transition"`,
    /// `"end"`). Legacy VMAPI wrote arbitrary strings here; we keep
    /// it as `String` so we don't have to bump the wire on every
    /// new event kind.
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<MigrationPhase>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<MigrationState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub percentage: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transferred_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eta_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub timestamp: DateTime<Utc>,
}

/// Inputs to `Store::create_migration`. The handler validates the
/// caller is acting on a real instance + the source CN; the store
/// fills in `id`, `created_at`, `phase = Begin`, `state = Begin`,
/// `last_progress_seq = 0`, and atomically takes the
/// `migration/active/<instance>` guard.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NewMigration {
    pub instance_id: Uuid,
    pub tenant_id: Uuid,
    pub project_id: Uuid,
    pub source_cn: Uuid,
    pub action_requested: MigrationAction,
    #[serde(default)]
    pub automatic: bool,
}
