// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Domain types shared between the storage layer and the wire surface.

use chrono::{DateTime, Utc};
use ipnetwork::{Ipv4Network, Ipv6Network};
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
/// * **Password-auth operators** — `silo_id = None`, `federation =
///   None`, `password_hash` is the bcrypt hash of the operator's
///   password. The bootstrap root user is the canonical example.
/// * **Federated users** — `silo_id = Some(...)`, `federation =
///   Some(...)` carries the OIDC `(issuer, subject)` pair, and
///   `password_hash` is empty. Created just-in-time on the first
///   successful OIDC login per `(silo_id, issuer, subject)`.
///
/// `is_root` is mutually exclusive with `silo_id`: the root
/// operator is cluster-wide; federated users are silo-scoped.
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
    /// Silo this user belongs to. `None` for the bootstrap root
    /// operator and other cluster-wide accounts. Federated users
    /// are always silo-scoped.
    #[serde(default)]
    pub silo_id: Option<Uuid>,
    /// External-IdP linkage when this user authenticates via OIDC.
    /// `None` for password-auth users.
    #[serde(default)]
    pub federation: Option<Federation>,
}

/// External-IdP linkage for a federated [`User`]. Combined with
/// [`User::silo_id`] this is the unique key the auth middleware
/// resolves on each OIDC login.
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

/// Sub-tenancy boundary inside a silo. Workload resources
/// (instances, volumes, networks) eventually nest under projects.
/// Phase 0e-c carries only the bare-minimum identifying fields;
/// quota envelopes and per-project Cedar bindings come in later
/// slices.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Project {
    pub id: Uuid,
    pub silo_id: Uuid,
    pub name: String,
    pub description: String,
    pub created_at: DateTime<Utc>,
}

/// Request body for creating a project. The owning silo comes from
/// the URL path, not the body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NewProject {
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

/// Tenant VPC. Project-scoped (Phase 1 URL shape). Mirrors the per-VPC
/// fields OPTE consumes in `oxide_vpc::api::VpcCfg`: a 24-bit Geneve
/// VNI plus an optional primary IPv4 CIDR and optional primary IPv6
/// CIDR (matching OPTE's `IpCfg` enum: Ipv4-only, Ipv6-only, or
/// dual-stack). Per-NIC concerns (guest MAC, private IPs, external
/// IPs, attached_subnets, DHCP) are *not* on the VPC record — those
/// land on subnet/instance/NIC resources in later slices.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Vpc {
    pub id: Uuid,
    pub silo_id: Uuid,
    pub project_id: Uuid,
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

/// Request body for creating a VPC. The owning silo + project come
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
    pub silo_id: Uuid,
    pub project_id: Uuid,
    pub vpc_id: Uuid,
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

/// Request body for creating a subnet. The owning silo, project, and
/// VPC come from the URL path. The server assigns `id` and
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
    pub created_at: DateTime<Utc>,
}

/// Wire-safe view of an [`ApiKey`]: identifying metadata, no hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ApiKeyView {
    pub id: Uuid,
    pub user_id: Uuid,
    pub description: String,
    pub created_at: DateTime<Utc>,
}

impl From<ApiKey> for ApiKeyView {
    fn from(key: ApiKey) -> Self {
        ApiKeyView {
            id: key.id,
            user_id: key.user_id,
            description: key.description,
            created_at: key.created_at,
        }
    }
}

/// User-supplied SSH public key, registered in a silo's catalog so
/// instance launches can pick keys to inject into authorized_keys.
/// Phase 0 is silo-scoped (any user in the silo can pick from the
/// pool). A future slice may add per-user ownership; the silo_id
/// field is forward-compatible with that.
///
/// The server validates the openssh wire format at create time and
/// computes the SHA-256 fingerprint. The raw `public_key` string is
/// stored verbatim so it can be handed to cloud-init without
/// reformatting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SshKey {
    pub id: Uuid,
    pub silo_id: Uuid,
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

/// Request body for registering a new SSH key in a silo's catalog.
/// The server assigns `id`, `fingerprint`, and `created_at`; the
/// owning silo comes from the URL path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NewSshKey {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub public_key: String,
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
