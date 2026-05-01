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
