// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Domain types shared between the storage layer and the wire surface.

use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

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
    pub created_at: DateTime<Utc>,
}

/// Wire-safe view of an [`ApiKey`]: identifying metadata, no hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ApiKeyView {
    pub id: Uuid,
    pub user_id: Uuid,
    pub description: String,
    pub scope: ApiKeyScope,
    pub created_at: DateTime<Utc>,
}

impl From<ApiKey> for ApiKeyView {
    fn from(key: ApiKey) -> Self {
        ApiKeyView {
            id: key.id,
            user_id: key.user_id,
            description: key.description,
            scope: key.scope,
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

/// Tenant image catalog entry. Phase 0 ships only the metadata
/// record — image content lives in mantafs / object storage and is
/// not modelled here. Operators register images by URL + sha256 and
/// trust the caller for the content match; an eventual import
/// pipeline will pre-stage content in storage and verify the digest
/// before the record is persisted.
///
/// Silo-scoped: each silo has its own catalog. A future slice may
/// add a fleet-shared catalog (operator-owned) that silos can
/// reference; for now images are tenant-private.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Image {
    pub id: Uuid,
    pub silo_id: Uuid,
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
    pub created_at: DateTime<Utc>,
}

/// Request body for registering an image in a silo's catalog. The
/// owning silo comes from the URL path. The server assigns `id`
/// and `created_at`.
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
    /// from `sha256` via [`derive_image_id`] — same content
    /// always yields the same id across hosts and replays, so
    /// the per-CN agent's content-addressed ZFS dataset
    /// (`zones/<image_id>`) collapses identical bytes into one
    /// import. `Some(...)` is only useful for cross-cluster
    /// mirroring scenarios where the operator wants tritond's id
    /// to match a UUID minted elsewhere; the store rejects the
    /// create with [`StoreError::Conflict`] if the id is already
    /// in use.
    #[serde(default)]
    pub id: Option<Uuid>,
}

/// Stable namespace for [`derive_image_id`]. Picked once on
/// 2026-05-02; **never change this value** — it would
/// retroactively re-key every persisted image. Generated via
/// `python3 -c 'import uuid; print(uuid.uuid4())'`.
pub const TRITOND_IMAGE_NAMESPACE: Uuid = Uuid::from_bytes([
    0xb5, 0xb5, 0x0a, 0x2c, 0xf0, 0x6c, 0x49, 0x09, 0x94, 0x52, 0x11, 0xe2, 0xef, 0xd7, 0xcd, 0x67,
]);

/// Derive an image UUID from its owning silo + content sha256.
///
/// Uses UUID v5 (SHA-1-based) over a fixed tritond namespace
/// so the mapping `(silo_id, sha256) → uuid` is stable across
/// hosts and cluster replays. The same image content
/// registered in the same silo under two different names
/// yields the same id, which makes per-CN `zones/<image_id>`
/// content-addressed storage work without a separate lookup
/// table.
///
/// **Why silo-keyed and not content-only.** Phase 0 image
/// records carry `silo_id` as part of their identity (same
/// name in different silos must coexist as separate records).
/// A purely content-keyed id would force `(silo_id, image_id)`
/// to become a composite primary key — a substantial schema
/// change for a small future-proofing win. The Slice B catalog
/// redesign tackles cross-silo dedup; for now we accept that
/// two silos registering identical bytes on the same CN end
/// up with two ZFS datasets.
///
/// `sha256` is expected to be lowercase hex; case is normalised
/// here so trivial input differences don't desync the mapping.
#[must_use]
pub fn derive_image_id(silo_id: Uuid, sha256: &str) -> Uuid {
    let normalised = sha256.to_ascii_lowercase();
    let mut input = Vec::with_capacity(16 + normalised.len() + 1);
    input.extend_from_slice(silo_id.as_bytes());
    input.push(b':');
    input.extend_from_slice(normalised.as_bytes());
    Uuid::new_v5(&TRITOND_IMAGE_NAMESPACE, &input)
}

#[cfg(test)]
mod derive_image_id_tests {
    use super::*;

    fn fixture_silo() -> Uuid {
        Uuid::from_bytes([0xab; 16])
    }

    #[test]
    fn deterministic_for_same_inputs() {
        let s = fixture_silo();
        assert_eq!(
            derive_image_id(s, "abc123"),
            derive_image_id(s, "abc123"),
            "same (silo, sha256) must yield same UUID",
        );
    }

    #[test]
    fn case_insensitive_on_sha256() {
        let s = fixture_silo();
        assert_eq!(
            derive_image_id(s, "ABCDEF1234"),
            derive_image_id(s, "abcdef1234"),
            "case differences must not desync the mapping",
        );
    }

    #[test]
    fn different_content_yields_different_id() {
        let s = fixture_silo();
        assert_ne!(
            derive_image_id(s, &"a".repeat(64)),
            derive_image_id(s, &"b".repeat(64)),
            "distinct sha256 inputs must not collide",
        );
    }

    #[test]
    fn same_content_in_different_silos_does_not_collide() {
        let a = Uuid::from_bytes([0x01; 16]);
        let b = Uuid::from_bytes([0x02; 16]);
        assert_ne!(
            derive_image_id(a, "abc123"),
            derive_image_id(b, "abc123"),
            "same content in different silos must yield distinct ids",
        );
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
    pub silo_id: Uuid,
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

/// Request body for setting a project's quota. The owning silo and
/// project come from the URL path. The server assigns `updated_at`.
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
/// history. Each will land as the consuming use case ships.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Instance {
    pub id: Uuid,
    pub silo_id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    pub description: String,
    /// Boot image; must be in the same silo as the instance.
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
    pub lifecycle: LifecycleState,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Request body for creating an instance. The owning silo +
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
}

/// What a provisioning job asks an agent to do.
///
/// Each variant carries the target `instance_id` so an agent can
/// look up the current state, do the work, and drive the lifecycle
/// forward without needing the issuer to embed extra context.
///
/// Phase 0 has exactly three kinds. A future slice may add others
/// (Migrate, Resize, etc.) — this enum is `#[non_exhaustive]` so
/// adding a variant is not a breaking change for matchers.
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
}

impl JobKind {
    /// Convenience: extract the target instance id without a
    /// full match.
    #[must_use]
    pub fn instance_id(&self) -> Uuid {
        match self {
            JobKind::Provision { instance_id }
            | JobKind::Stop { instance_id }
            | JobKind::Restart { instance_id }
            | JobKind::Delete { instance_id } => *instance_id,
        }
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
}

/// Request body for enqueuing a new job. Server assigns `id`,
/// `seq`, `created_at`, and starts in `JobStatus::Pending`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NewJob {
    pub kind: JobKind,
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
    pub silo_id: Uuid,
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
/// boot disk per instance, sized to the source image and tagged
/// with that image's id. The disk record is the metadata view; the
/// actual content (zfs dataset, mantafs object, etc.) is materialized
/// by the agent at provisioning time.
///
/// Multi-disk attach (data disks beyond boot) lands as a follow-on
/// slice. The wire shape here is stable across that change — a
/// `kind: Data` disk is a strict superset of what `Boot` carries
/// once `source_image_id` is allowed to be `None`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Disk {
    pub id: Uuid,
    pub silo_id: Uuid,
    pub project_id: Uuid,
    pub instance_id: Uuid,
    pub name: String,
    pub description: String,
    pub kind: DiskKind,
    /// Total size in bytes. Boot disks default to `image.size_bytes`;
    /// future data disks accept an explicit operator-supplied size.
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
    pub silo_id: Uuid,
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
