// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Domain types for the identity service store.
//!
//! See `rfd/00003/01-data-model-and-store.md` for the design rationale.
//! These types deal only in plain Rust; the OIDC wire surface lives in
//! `identityd-api` and re-uses them so there is no API↔storage
//! conversion layer to keep in sync.

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use zeroize::Zeroize;

// ---------------------------------------------------------------------------
// RedactedString — wire-transparent newtype that hides its plaintext from
// `Debug` and zeroes it on drop. Local copy (a leaf store crate should not
// pull in `tritond-auth`'s JWT/bcrypt/OIDC dependency tree just for this).
// ---------------------------------------------------------------------------

/// String that redacts itself from `Debug` and zeroes its memory on drop.
/// Wire-transparent for serde and JsonSchema.
#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct RedactedString(String);

impl RedactedString {
    /// Wrap an existing `String` (moved in, nothing copied).
    #[must_use]
    pub fn new(value: String) -> Self {
        Self(value)
    }

    /// Borrow the underlying plaintext. Pass the `&str` straight into the
    /// consumer rather than copying it into another `String`.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl From<String> for RedactedString {
    fn from(value: String) -> Self {
        Self(value)
    }
}
impl From<&str> for RedactedString {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}
impl PartialEq for RedactedString {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}
impl Eq for RedactedString {}
impl std::fmt::Debug for RedactedString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RedactedString(***)")
    }
}
impl Drop for RedactedString {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

// ---------------------------------------------------------------------------
// Realms
// ---------------------------------------------------------------------------

/// What a [`Realm`] is attached to. Exactly one `System` realm exists
/// fleet-wide; tenant realms and silo realms are many. A tenant "uses its
/// parent silo's realm" simply by not having a `Tenant` realm of its own —
/// `iss → realm` then resolves to the `Silo` realm.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RealmScope {
    /// One tenant's directory.
    Tenant { tenant_id: Uuid },
    /// A silo-wide directory shared by the silo's tenants (MSP / reseller).
    Silo { silo_id: Uuid },
    /// The fleet-wide operator directory. Singleton.
    System,
}

impl RealmScope {
    /// Stable lowercase tag used as an FDB key segment and audit field.
    #[must_use]
    pub fn tag(&self) -> &'static str {
        match self {
            RealmScope::Tenant { .. } => "tenant",
            RealmScope::Silo { .. } => "silo",
            RealmScope::System => "system",
        }
    }
}

/// Signature algorithm a realm's tokens are signed with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "UPPERCASE")]
pub enum SigningAlg {
    /// RSASSA-PKCS1-v1_5 with SHA-256. The interoperable default.
    #[default]
    Rs256,
    /// ECDSA with P-256 and SHA-256. Smaller keys/signatures.
    Es256,
}

/// Per-realm token-lifetime and login knobs. v1 wires the TTLs; the login
/// policy fields are placeholders for when the hosted login UI lands.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LoginPolicy {
    /// Require a second factor at login. Not yet enforced (v1 stub).
    pub mfa_required: bool,
    /// Allow username/password login (vs broker-only).
    pub password_login_allowed: bool,
}

impl Default for LoginPolicy {
    fn default() -> Self {
        Self {
            mfa_required: false,
            password_login_allowed: true,
        }
    }
}

/// Default token TTLs (seconds).
pub const DEFAULT_ACCESS_TOKEN_TTL_SECS: u32 = 900;
pub const DEFAULT_ID_TOKEN_TTL_SECS: u32 = 900;
pub const DEFAULT_REFRESH_TOKEN_TTL_SECS: u32 = 86_400;
pub const DEFAULT_AUTH_CODE_TTL_SECS: u32 = 60;
pub const DEFAULT_DEVICE_CODE_TTL_SECS: u32 = 600;

/// An isolated identity directory.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Realm {
    pub id: Uuid,
    pub scope: RealmScope,
    pub name: String,
    pub description: String,
    /// OIDC issuer URL this realm serves under. Unique across realms;
    /// immutable after create (changing it would orphan every token and
    /// every cached JWKS entry).
    pub issuer_url: String,
    pub signing_alg: SigningAlg,
    pub access_token_ttl_secs: u32,
    pub id_token_ttl_secs: u32,
    pub refresh_token_ttl_secs: u32,
    pub auth_code_ttl_secs: u32,
    pub device_code_ttl_secs: u32,
    pub login_policy: LoginPolicy,
    pub created_at: DateTime<Utc>,
}

/// Request body for `create_realm`. The store assigns `id`/`created_at`,
/// applies TTL defaults for any field left `None`, and atomically writes
/// the realm's first signing-key ring (one `Active` + one `Next`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NewRealm {
    pub scope: RealmScope,
    pub name: String,
    pub description: Option<String>,
    pub issuer_url: String,
    pub signing_alg: Option<SigningAlg>,
    pub access_token_ttl_secs: Option<u32>,
    pub id_token_ttl_secs: Option<u32>,
    pub refresh_token_ttl_secs: Option<u32>,
    pub auth_code_ttl_secs: Option<u32>,
    pub device_code_ttl_secs: Option<u32>,
    pub login_policy: Option<LoginPolicy>,
}

/// Mutable per-realm settings (everything on a [`Realm`] except `id`,
/// `scope`, `issuer_url`, `signing_alg`, and `created_at`, which are fixed
/// at create time). Used by `update_realm_settings`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RealmSettings {
    pub access_token_ttl_secs: u32,
    pub id_token_ttl_secs: u32,
    pub refresh_token_ttl_secs: u32,
    pub auth_code_ttl_secs: u32,
    pub device_code_ttl_secs: u32,
    pub login_policy: LoginPolicy,
}

impl Realm {
    /// Snapshot of this realm's mutable settings.
    #[must_use]
    pub fn settings(&self) -> RealmSettings {
        RealmSettings {
            access_token_ttl_secs: self.access_token_ttl_secs,
            id_token_ttl_secs: self.id_token_ttl_secs,
            refresh_token_ttl_secs: self.refresh_token_ttl_secs,
            auth_code_ttl_secs: self.auth_code_ttl_secs,
            device_code_ttl_secs: self.device_code_ttl_secs,
            login_policy: self.login_policy.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// Users
// ---------------------------------------------------------------------------

/// Account state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum UserStatus {
    /// Normal, can authenticate.
    Active,
    /// Login disabled; all refresh-token families revoked.
    Disabled,
}

/// Link from a native [`User`] record to the upstream IdP that JIT-created
/// it. `None` for users created directly in this realm.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BrokeredLink {
    pub connection_id: Uuid,
    pub upstream_issuer: String,
    pub upstream_subject: String,
}

/// Second-factor configuration. v1 stub — defined so the data model is
/// stable; not yet enforced at login.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MfaConfig {
    /// Base32 TOTP secret.
    pub totp_secret: RedactedString,
    pub enrolled_at: DateTime<Utc>,
}

/// A principal inside a realm — native or brokered.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct User {
    pub id: Uuid,
    pub realm_id: Uuid,
    /// Unique within the realm.
    pub username: String,
    /// Unique within the realm when present.
    pub email: Option<String>,
    pub display_name: String,
    /// bcrypt hash; empty string for brokered-only users.
    pub password_hash: String,
    /// Meaningful only in the `System` realm.
    pub is_root: bool,
    /// Meaningful only in the `System` realm.
    pub fleet_admin: bool,
    pub status: UserStatus,
    pub mfa: Option<MfaConfig>,
    pub brokered: Option<BrokeredLink>,
    pub created_at: DateTime<Utc>,
}

/// Request body for `create_user`. Password hashing is the caller's job
/// (same stance as `tritond-store`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NewUser {
    pub realm_id: Uuid,
    pub username: String,
    pub email: Option<String>,
    pub display_name: Option<String>,
    /// bcrypt hash; pass `""` for a brokered-only user.
    pub password_hash: String,
    pub is_root: bool,
    pub fleet_admin: bool,
    pub brokered: Option<BrokeredLink>,
}

// ---------------------------------------------------------------------------
// Groups
// ---------------------------------------------------------------------------

/// A named set of users within a realm. Membership is an index, not an
/// embedded list (see the FDB keyspace doc-comment in `fdb.rs`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Group {
    pub id: Uuid,
    pub realm_id: Uuid,
    /// Unique within the realm.
    pub name: String,
    pub description: String,
    pub created_at: DateTime<Utc>,
}

/// Request body for `create_group`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NewGroup {
    pub realm_id: Uuid,
    pub name: String,
    pub description: Option<String>,
}

// ---------------------------------------------------------------------------
// Role assignments — the cross-tenant grant
// ---------------------------------------------------------------------------

/// Who a [`RoleAssignment`] grants to.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AssignmentSubject {
    User { user_id: Uuid },
    Group { group_id: Uuid },
}

impl AssignmentSubject {
    /// Stable tag used as an FDB key segment.
    #[must_use]
    pub fn tag(&self) -> &'static str {
        match self {
            AssignmentSubject::User { .. } => "user",
            AssignmentSubject::Group { .. } => "group",
        }
    }
}

/// What tenancy a [`RoleAssignment`] grants the subject into.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AssignmentTarget {
    Tenant {
        tenant_id: Uuid,
    },
    Silo {
        silo_id: Uuid,
    },
    /// Fleet-wide. Only a `System`-scoped realm may emit this.
    Fleet,
}

impl AssignmentTarget {
    /// Stable tag used as an FDB key segment.
    #[must_use]
    pub fn tag(&self) -> &'static str {
        match self {
            AssignmentTarget::Tenant { .. } => "tenant",
            AssignmentTarget::Silo { .. } => "silo",
            AssignmentTarget::Fleet => "fleet",
        }
    }
}

/// Coarse role granted by a [`RoleAssignment`]. v1 is a small fixed set;
/// fine-grained custom roles are future work.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    TenantAdmin,
    TenantMember,
    SiloAdmin,
    FleetAdmin,
    Operator,
    ReadOnly,
}

/// Grants a [`AssignmentSubject`] a [`Role`] over a [`AssignmentTarget`].
/// Multiple assignments with different `Tenant` targets on a `Silo`-scoped
/// realm are how "one human, many tenants" works.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RoleAssignment {
    pub id: Uuid,
    pub realm_id: Uuid,
    pub subject: AssignmentSubject,
    pub target: AssignmentTarget,
    pub role: Role,
    pub created_at: DateTime<Utc>,
    pub created_by: Uuid,
}

/// Request body for `create_role_assignment`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NewRoleAssignment {
    pub realm_id: Uuid,
    pub subject: AssignmentSubject,
    pub target: AssignmentTarget,
    pub role: Role,
    pub created_by: Uuid,
}

// ---------------------------------------------------------------------------
// OAuth clients
// ---------------------------------------------------------------------------

/// OAuth 2.0 grant types a client is permitted to use.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GrantType {
    AuthorizationCode,
    RefreshToken,
    DeviceCode,
    ClientCredentials,
    /// Resource-owner password credentials. Dev/demo convenience for the
    /// Workbench client; not recommended for production clients.
    Password,
}

/// A registered OAuth 2.0 / OIDC client.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OAuthClient {
    /// Also the `client_id` on the wire.
    pub id: Uuid,
    pub realm_id: Uuid,
    pub name: String,
    /// bcrypt hash of the client secret; `None` for a public (PKCE-only)
    /// client such as the `tcadm`/`triton` CLI.
    pub client_secret_hash: Option<String>,
    pub redirect_uris: Vec<String>,
    pub grant_types: Vec<GrantType>,
    /// Forced `true` for public clients.
    pub pkce_required: bool,
    pub scopes_allowed: Vec<String>,
    /// `client_credentials` clients that mint workload identities.
    pub is_workload: bool,
    /// Per-CN binding; `Some` for the agent client of a single compute
    /// node (preserves the per-CN binding security property).
    pub bound_to_cn: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

/// Mutable fields of an [`OAuthClient`] (everything except `id`,
/// `realm_id`, `is_workload`, `bound_to_cn`, `created_at`, which are fixed
/// at create time). Used by `update_oauth_client`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OAuthClientUpdate {
    pub name: String,
    pub client_secret_hash: Option<String>,
    pub redirect_uris: Vec<String>,
    pub grant_types: Vec<GrantType>,
    pub pkce_required: bool,
    pub scopes_allowed: Vec<String>,
}

/// Request body for `create_oauth_client`. Client-secret hashing is the
/// caller's job.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NewOAuthClient {
    pub realm_id: Uuid,
    pub name: String,
    pub client_secret_hash: Option<String>,
    pub redirect_uris: Vec<String>,
    pub grant_types: Vec<GrantType>,
    pub pkce_required: bool,
    pub scopes_allowed: Vec<String>,
    pub is_workload: bool,
    pub bound_to_cn: Option<Uuid>,
}

// ---------------------------------------------------------------------------
// Upstream connections (broker) + claim mappings
// ---------------------------------------------------------------------------

/// Protocol-specific configuration for an upstream IdP a realm federates.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "protocol", rename_all = "snake_case")]
pub enum ConnectionKind {
    /// OpenID Connect upstream (Okta / Entra / Google / Keycloak …).
    Oidc {
        issuer_url: String,
        client_id: String,
        client_secret: RedactedString,
        scopes: Vec<String>,
        audience: Option<String>,
    },
    /// SAML 2.0 upstream. `identityd` acts only as an SP, never an IdP.
    Saml {
        /// Metadata URL, or inline metadata XML.
        idp_metadata: String,
        sp_entity_id: String,
        sp_acs_url: String,
        want_signed_assertions: bool,
    },
}

impl ConnectionKind {
    /// Stable tag used in audit fields.
    #[must_use]
    pub fn tag(&self) -> &'static str {
        match self {
            ConnectionKind::Oidc { .. } => "oidc",
            ConnectionKind::Saml { .. } => "saml",
        }
    }
}

/// One upstream IdP federated into a realm. Subsumes today's per-tenant
/// `tritond_store::IdpConfig`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct UpstreamConnection {
    pub id: Uuid,
    pub realm_id: Uuid,
    pub name: String,
    pub kind: ConnectionKind,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
}

/// Request body for `create_upstream_connection`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NewUpstreamConnection {
    pub realm_id: Uuid,
    pub name: String,
    pub kind: ConnectionKind,
    pub enabled: bool,
}

/// Which realm-user field an upstream claim/attribute maps to.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MappedField {
    Username,
    Email,
    DisplayName,
    Group,
}

/// One rule mapping an upstream claim/attribute onto a realm-user field.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ClaimMapping {
    pub connection_id: Uuid,
    /// Ordering within a connection's mapping list.
    pub seq: u32,
    /// Upstream claim name (OIDC) or attribute name (SAML).
    pub source: String,
    pub target: MappedField,
    /// When `target == Group`, the group name this rule grants membership
    /// of; ignored otherwise.
    pub group_value: Option<String>,
}

// ---------------------------------------------------------------------------
// Signing keys — the per-realm key ring
// ---------------------------------------------------------------------------

/// Position of a [`SigningKey`] in its realm's rotation ring.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum KeyStatus {
    /// Signs new tokens. Exactly one per realm.
    Active,
    /// Pre-published in JWKS so RPs cache it before it goes active.
    Next,
    /// No longer signs, still published in JWKS until `not_after` so
    /// in-flight tokens validate.
    Retiring,
    /// Compromised — removed from JWKS immediately.
    Revoked,
}

/// One signing key in a realm's ring.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SigningKey {
    /// JWK `kid`. Unique within the realm.
    pub kid: String,
    pub realm_id: Uuid,
    pub alg: SigningAlg,
    /// PKCS#8 private key, PEM-encoded. Encrypted-at-rest later (same
    /// deferral as `IdpConfig.client_secret`).
    pub private_pem: RedactedString,
    /// The public JWK published in `…/jwks`.
    pub public_jwk: serde_json::Value,
    pub status: KeyStatus,
    pub not_before: DateTime<Utc>,
    pub not_after: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

/// Request body for adding a signing key (and for seeding a realm's
/// initial ring at create time). The store stamps `realm_id` (with the
/// owning realm) and `created_at`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NewSigningKey {
    pub kid: String,
    pub alg: SigningAlg,
    pub private_pem: RedactedString,
    pub public_jwk: serde_json::Value,
    pub status: KeyStatus,
    pub not_before: DateTime<Utc>,
    pub not_after: DateTime<Utc>,
}

/// Advisory leader-election lock for the key-rotation loop.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RotationLock {
    pub holder: String,
    pub expires_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Short-lived flow records
// ---------------------------------------------------------------------------

/// An OAuth 2.0 authorization code (auth-code grant). Delete-on-read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AuthCode {
    pub code: String,
    pub realm_id: Uuid,
    pub client_id: Uuid,
    pub user_id: Uuid,
    pub redirect_uri: String,
    pub pkce_challenge: Option<String>,
    pub scope: String,
    pub granted_tenant: Option<Uuid>,
    pub nonce: Option<String>,
    pub expires_at: DateTime<Utc>,
}

/// A refresh token. `family_id` ties together a rotation chain; presenting
/// a non-latest `jti` in a family revokes the whole family (theft
/// detection).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RefreshToken {
    pub jti: Uuid,
    pub realm_id: Uuid,
    pub client_id: Uuid,
    pub user_id: Uuid,
    pub scope: String,
    pub granted_tenant: Option<Uuid>,
    pub family_id: Uuid,
    pub revoked: bool,
    pub expires_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

/// Device-authorization grant state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeviceCodeStatus {
    /// Awaiting the user's approval at the verification page.
    Pending,
    /// Approved; the polling client may exchange it for tokens.
    Approved,
    /// Explicitly denied at the verification page.
    Denied,
}

/// A device-authorization grant in flight.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DeviceCode {
    pub device_code: String,
    /// Human-typable code shown at the verification page (`XXXX-XXXX`).
    pub user_code: String,
    pub realm_id: Uuid,
    pub client_id: Uuid,
    pub scope: String,
    pub status: DeviceCodeStatus,
    /// Set once a user approves it.
    pub user_id: Option<Uuid>,
    pub granted_tenant: Option<Uuid>,
    pub interval_secs: u32,
    pub expires_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

/// A login session — minimal v1; used for RP-initiated logout / SSO.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Session {
    pub id: Uuid,
    pub realm_id: Uuid,
    pub user_id: Uuid,
    /// Upstream IdP's session index (SAML) or `sid` (OIDC), if brokered.
    pub idp_session_index: Option<String>,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

/// Bridges a downstream authorization-code request through an upstream
/// (broker) round-trip. Delete-on-read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BrokerState {
    /// The `state` value handed to the upstream IdP.
    pub state: String,
    pub realm_id: Uuid,
    pub connection_id: Uuid,
    pub downstream_client_id: Uuid,
    pub downstream_redirect_uri: String,
    pub downstream_pkce_challenge: Option<String>,
    pub downstream_nonce: Option<String>,
    /// The `state` the downstream client sent us, to echo back.
    pub downstream_state: Option<String>,
    pub expires_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use serde::de::DeserializeOwned;

    // ---------- RedactedString — the security-relevant newtype ----------

    #[test]
    fn redacted_debug_does_not_leak_plaintext() {
        let secret = RedactedString::from("p@ssw0rd-very-distinctive");
        let dbg = format!("{secret:?}");
        assert!(!dbg.contains("p@ssw0rd-very-distinctive"));
        assert!(dbg.contains("***"));

        // Embedded in a struct: Debug derives must inherit the redaction.
        let key = NewSigningKey {
            kid: "k".into(),
            alg: SigningAlg::Rs256,
            private_pem: RedactedString::from("PRIVATE-KEY-MATERIAL-DO-NOT-LEAK"),
            public_jwk: serde_json::json!({}),
            status: KeyStatus::Active,
            not_before: Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
            not_after: Utc.with_ymd_and_hms(2027, 1, 1, 0, 0, 0).unwrap(),
        };
        let dbg = format!("{key:?}");
        assert!(
            !dbg.contains("PRIVATE-KEY-MATERIAL"),
            "RedactedString leaked through a derived Debug: {dbg}"
        );
    }

    #[test]
    fn redacted_serde_is_transparent() {
        let secret = RedactedString::from("hunter2");
        // Serializes as a bare string (no wrapper object), same as the field
        // would if it were a plain `String`.
        let json = serde_json::to_string(&secret).unwrap();
        assert_eq!(json, "\"hunter2\"");
        let back: RedactedString = serde_json::from_str(&json).unwrap();
        assert_eq!(back.expose(), "hunter2");
        assert_eq!(back, secret);
    }

    #[test]
    fn redacted_equality_compares_plaintext() {
        assert_eq!(RedactedString::from("same"), RedactedString::from("same"),);
        assert_ne!(RedactedString::from("a"), RedactedString::from("b"),);
    }

    // ---------- Tagged-enum serde round-trips ----------
    //
    // FdbStore JSON-encodes these types. A `#[serde(tag = ...)]` mistake or a
    // renamed variant must fail a test, not a production deserialize.

    fn roundtrip<T: serde::Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(v: T) {
        let json = serde_json::to_string(&v).unwrap();
        let back: T = serde_json::from_str(&json)
            .unwrap_or_else(|e| panic!("round-trip failed for {json}: {e}"));
        assert_eq!(v, back);
    }

    #[test]
    fn realm_scope_round_trips_every_variant() {
        roundtrip(RealmScope::System);
        roundtrip(RealmScope::Tenant {
            tenant_id: Uuid::nil(),
        });
        roundtrip(RealmScope::Silo {
            silo_id: Uuid::nil(),
        });
    }

    #[test]
    fn realm_scope_wire_shape_is_kind_tagged() {
        // Pin the wire shape so a refactor that flips `#[serde(tag = "kind")]`
        // off (or renames variants) fails here, not at the OpenAPI boundary.
        assert_eq!(
            serde_json::to_value(RealmScope::System).unwrap(),
            serde_json::json!({"kind": "system"}),
        );
        assert_eq!(
            serde_json::to_value(RealmScope::Tenant {
                tenant_id: Uuid::nil()
            })
            .unwrap(),
            serde_json::json!({"kind": "tenant", "tenant_id": "00000000-0000-0000-0000-000000000000"}),
        );
    }

    #[test]
    fn assignment_subject_and_target_round_trip() {
        roundtrip(AssignmentSubject::User {
            user_id: Uuid::nil(),
        });
        roundtrip(AssignmentSubject::Group {
            group_id: Uuid::nil(),
        });
        roundtrip(AssignmentTarget::Fleet);
        roundtrip(AssignmentTarget::Tenant {
            tenant_id: Uuid::nil(),
        });
        roundtrip(AssignmentTarget::Silo {
            silo_id: Uuid::nil(),
        });
    }

    #[test]
    fn connection_kind_round_trips() {
        roundtrip(ConnectionKind::Oidc {
            issuer_url: "https://example".into(),
            client_id: "c".into(),
            client_secret: RedactedString::from("s"),
            scopes: vec!["openid".into()],
            audience: None,
        });
        roundtrip(ConnectionKind::Saml {
            idp_metadata: "https://example/meta".into(),
            sp_entity_id: "sp".into(),
            sp_acs_url: "https://sp/acs".into(),
            want_signed_assertions: true,
        });
    }

    #[test]
    fn small_enums_round_trip() {
        for s in [UserStatus::Active, UserStatus::Disabled] {
            roundtrip(s);
        }
        for s in [
            KeyStatus::Active,
            KeyStatus::Next,
            KeyStatus::Retiring,
            KeyStatus::Revoked,
        ] {
            roundtrip(s);
        }
        for g in [
            GrantType::AuthorizationCode,
            GrantType::RefreshToken,
            GrantType::DeviceCode,
            GrantType::ClientCredentials,
        ] {
            roundtrip(g);
        }
        for f in [
            MappedField::Username,
            MappedField::Email,
            MappedField::DisplayName,
            MappedField::Group,
        ] {
            roundtrip(f);
        }
        for r in [
            Role::TenantAdmin,
            Role::TenantMember,
            Role::SiloAdmin,
            Role::FleetAdmin,
            Role::Operator,
            Role::ReadOnly,
        ] {
            roundtrip(r);
        }
        roundtrip(SigningAlg::Rs256);
        roundtrip(SigningAlg::Es256);
    }

    #[test]
    fn realm_settings_round_trip_is_identity() {
        // Realm::settings() should be a faithful snapshot of the mutable
        // fields. Any field added to RealmSettings but missed in `settings()`
        // surfaces here.
        let realm = Realm {
            id: Uuid::nil(),
            scope: RealmScope::System,
            name: "r".into(),
            description: "d".into(),
            issuer_url: "https://example".into(),
            signing_alg: SigningAlg::Rs256,
            access_token_ttl_secs: 100,
            id_token_ttl_secs: 200,
            refresh_token_ttl_secs: 300,
            auth_code_ttl_secs: 30,
            device_code_ttl_secs: 600,
            login_policy: LoginPolicy {
                mfa_required: true,
                password_login_allowed: false,
            },
            created_at: Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
        };
        let s = realm.settings();
        assert_eq!(s.access_token_ttl_secs, 100);
        assert_eq!(s.id_token_ttl_secs, 200);
        assert_eq!(s.refresh_token_ttl_secs, 300);
        assert_eq!(s.auth_code_ttl_secs, 30);
        assert_eq!(s.device_code_ttl_secs, 600);
        assert!(s.login_policy.mfa_required);
        assert!(!s.login_policy.password_login_allowed);
    }
}
