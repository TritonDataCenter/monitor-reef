// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Persistence layer for `identityd`, the Triton Cloud identity service.
//!
//! This crate defines the domain types (see [`types`]) and the
//! [`IdentityStore`] trait. Two backends ship here:
//!
//! * [`MemStore`] — an in-process `Mutex<…HashMap…>` implementation, used
//!   for tests and for `identityd` runs that don't need durable state.
//!   Always available.
//! * `FdbStore` (behind a `foundationdb` cargo feature) — the production
//!   FoundationDB-backed implementation, laying keys under the `identity/…`
//!   prefix on the region's shared FDB cluster. **Not yet implemented**;
//!   the keyspace is fully specified in `rfd/00003/01-data-model-and-store.md`
//!   and it lands in a follow-up.
//!
//! The trait deals only in plain Rust types — no HTTP, JSON, or Dropshot.
//! The OIDC wire surface lives in `identityd-api` and re-uses these types
//! so there is no API↔storage conversion layer to keep in sync.
//!
//! Design: `rfd/00003/01-data-model-and-store.md`.

pub mod mem;
pub mod types;

pub use mem::MemStore;
pub use types::{
    AssignmentSubject, AssignmentTarget, AuthCode, BrokerState, BrokeredLink, ClaimMapping,
    ConnectionKind, DEFAULT_ACCESS_TOKEN_TTL_SECS, DEFAULT_AUTH_CODE_TTL_SECS,
    DEFAULT_DEVICE_CODE_TTL_SECS, DEFAULT_ID_TOKEN_TTL_SECS, DEFAULT_REFRESH_TOKEN_TTL_SECS,
    DeviceCode, DeviceCodeStatus, GrantType, Group, KeyStatus, LoginPolicy, MappedField, MfaConfig,
    NewGroup, NewOAuthClient, NewRealm, NewRoleAssignment, NewSigningKey, NewUpstreamConnection,
    NewUser, OAuthClient, OAuthClientUpdate, Realm, RealmScope, RealmSettings, RedactedString,
    RefreshToken, Role, RoleAssignment, RotationLock, Session, SigningAlg, SigningKey,
    UpstreamConnection, User, UserStatus,
};

use async_trait::async_trait;
use uuid::Uuid;

/// Errors an [`IdentityStore`] implementation may produce.
///
/// Mirrors `tritond-store::StoreError`: `identityd` maps `NotFound` →
/// 404, `Conflict` → 409, and `Backend` → 500, and (for cross-tenancy
/// surfaces) deliberately collapses `NotFound`/`Conflict` to 404 so a
/// caller can't enumerate.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// The requested entity does not exist.
    #[error("not found")]
    NotFound,

    /// A uniqueness or structural invariant was violated (e.g. realm
    /// issuer already claimed, second `System` realm, cross-silo grant).
    #[error("conflict: {0}")]
    Conflict(String),

    /// The backing store reported an error the caller cannot meaningfully
    /// react to beyond surfacing it.
    #[error("store backend error: {0}")]
    Backend(String),
}

/// A handle to the identity-service state store.
///
/// All methods are async because the production backend (FoundationDB)
/// is async; `MemStore` is trivially `await`-able.
#[async_trait]
pub trait IdentityStore: Send + Sync + 'static {
    // ------------------------------------------------------------------
    // Realms
    // ------------------------------------------------------------------

    /// Create a realm and atomically seed its initial signing-key ring.
    ///
    /// `initial_keys` is what the caller (`identityd`) generated — it owns
    /// the crypto; the store stamps each key's `realm_id` and `created_at`
    /// and writes them in the same transaction as the realm record and its
    /// `by_issuer` / `by_scope` / `all` indices.
    ///
    /// Returns [`StoreError::Conflict`] if `req.issuer_url` is already
    /// claimed by another realm, if a realm with the same scope already
    /// exists (including a second `System` realm), or if `initial_keys` is
    /// empty.
    async fn create_realm(
        &self,
        req: types::NewRealm,
        initial_keys: Vec<types::NewSigningKey>,
    ) -> Result<Realm, StoreError>;

    /// Look up a realm by id.
    async fn get_realm(&self, id: Uuid) -> Result<Realm, StoreError>;

    /// Resolve a realm by its OIDC issuer URL (the `iss` claim). Used by
    /// the token-verify path to map `iss → realm`.
    async fn get_realm_by_issuer(&self, issuer: &str) -> Result<Realm, StoreError>;

    /// Resolve a realm by its scope (tenant id, silo id, or `System`).
    async fn get_realm_by_scope(&self, scope: &types::RealmScope) -> Result<Realm, StoreError>;

    /// List every realm. Order unspecified.
    async fn list_realms(&self) -> Result<Vec<Realm>, StoreError>;

    /// Replace a realm's token-TTL knobs and login policy. Issuer, scope,
    /// and signing algorithm are immutable; the latter is set only via
    /// [`Self::create_realm`].
    async fn update_realm_settings(
        &self,
        id: Uuid,
        settings: types::RealmSettings,
    ) -> Result<Realm, StoreError>;

    /// Delete a realm. Returns [`StoreError::Conflict`] if it still has
    /// users, clients, or upstream connections (no cascade — mirrors
    /// `tritond-store`'s `delete_tenant`).
    async fn delete_realm(&self, id: Uuid) -> Result<(), StoreError>;

    // ------------------------------------------------------------------
    // Users
    // ------------------------------------------------------------------

    /// Create a user inside a realm. Returns [`StoreError::NotFound`] if
    /// the realm doesn't exist; [`StoreError::Conflict`] if `username`
    /// (or `email`, when set) is already taken in that realm.
    async fn create_user(&self, req: types::NewUser) -> Result<User, StoreError>;

    /// Look up a user by id.
    async fn get_user(&self, id: Uuid) -> Result<User, StoreError>;

    /// Look up a user by `(realm, username)`.
    async fn get_user_by_username(
        &self,
        realm_id: Uuid,
        username: &str,
    ) -> Result<User, StoreError>;

    /// Look up the user JIT-created from `(realm, connection, upstream
    /// subject)`. Returns [`StoreError::NotFound`] on first login (the
    /// caller then creates the row).
    async fn get_user_by_brokered(
        &self,
        realm_id: Uuid,
        connection_id: Uuid,
        upstream_subject: &str,
    ) -> Result<User, StoreError>;

    /// List every user in a realm. Order unspecified.
    async fn list_users_in_realm(&self, realm_id: Uuid) -> Result<Vec<User>, StoreError>;

    /// Replace a user's bcrypt password hash.
    async fn update_user_password_hash(
        &self,
        id: Uuid,
        password_hash: String,
    ) -> Result<User, StoreError>;

    /// Set a user's status. Setting `Disabled` also revokes every
    /// refresh-token family belonging to the user.
    async fn set_user_status(
        &self,
        id: Uuid,
        status: types::UserStatus,
    ) -> Result<User, StoreError>;

    /// Delete a user (and its group memberships and refresh tokens).
    async fn delete_user(&self, id: Uuid) -> Result<(), StoreError>;

    /// True if any user exists in the realm. Bootstrap helper.
    async fn has_any_user_in_realm(&self, realm_id: Uuid) -> Result<bool, StoreError>;

    // ------------------------------------------------------------------
    // Groups
    // ------------------------------------------------------------------

    async fn create_group(&self, req: types::NewGroup) -> Result<Group, StoreError>;
    async fn get_group(&self, id: Uuid) -> Result<Group, StoreError>;
    async fn list_groups_in_realm(&self, realm_id: Uuid) -> Result<Vec<Group>, StoreError>;
    async fn delete_group(&self, id: Uuid) -> Result<(), StoreError>;

    /// Add a user to a group. Both must exist; [`StoreError::NotFound`]
    /// otherwise. Idempotent.
    async fn add_group_member(&self, group_id: Uuid, user_id: Uuid) -> Result<(), StoreError>;
    /// Remove a user from a group. [`StoreError::NotFound`] if either
    /// doesn't exist; a no-op if the user wasn't a member.
    async fn remove_group_member(&self, group_id: Uuid, user_id: Uuid) -> Result<(), StoreError>;
    async fn list_group_members(&self, group_id: Uuid) -> Result<Vec<Uuid>, StoreError>;
    async fn list_groups_of_user(&self, user_id: Uuid) -> Result<Vec<Uuid>, StoreError>;

    // ------------------------------------------------------------------
    // Role assignments
    // ------------------------------------------------------------------

    /// Create a role assignment. Structural-scope rules (a `Tenant{t}`
    /// realm may only target `Tenant{t}`; a `Silo{s}` realm may target
    /// `Silo{s}` or any `Tenant`; only a `System` realm may target
    /// `Fleet`) are enforced here — violations return
    /// [`StoreError::Conflict`]. (The *cross-silo* check that needs the
    /// tenant→silo mapping is `identityd`'s job, not the store's.) An
    /// exact duplicate `(subject, target, role)` also returns
    /// [`StoreError::Conflict`].
    async fn create_role_assignment(
        &self,
        req: types::NewRoleAssignment,
    ) -> Result<RoleAssignment, StoreError>;
    async fn get_role_assignment(&self, id: Uuid) -> Result<RoleAssignment, StoreError>;
    async fn list_assignments_of_subject(
        &self,
        subject: &types::AssignmentSubject,
    ) -> Result<Vec<RoleAssignment>, StoreError>;
    async fn list_assignments_for_target(
        &self,
        target: &types::AssignmentTarget,
    ) -> Result<Vec<RoleAssignment>, StoreError>;
    async fn delete_role_assignment(&self, id: Uuid) -> Result<(), StoreError>;

    // ------------------------------------------------------------------
    // OAuth clients
    // ------------------------------------------------------------------

    /// Create a client. [`StoreError::NotFound`] if the realm is missing;
    /// [`StoreError::Conflict`] if `bound_to_cn` is already claimed by
    /// another client.
    async fn create_oauth_client(
        &self,
        req: types::NewOAuthClient,
    ) -> Result<OAuthClient, StoreError>;
    async fn get_oauth_client(&self, id: Uuid) -> Result<OAuthClient, StoreError>;
    /// Resolve the workload client bound to a compute node.
    async fn get_oauth_client_by_cn(&self, server_uuid: Uuid) -> Result<OAuthClient, StoreError>;
    async fn list_oauth_clients_in_realm(
        &self,
        realm_id: Uuid,
    ) -> Result<Vec<OAuthClient>, StoreError>;
    /// Replace a client's mutable fields. The id, realm, `is_workload`,
    /// and `bound_to_cn` are fixed at create time.
    async fn update_oauth_client(
        &self,
        id: Uuid,
        update: types::OAuthClientUpdate,
    ) -> Result<OAuthClient, StoreError>;
    async fn delete_oauth_client(&self, id: Uuid) -> Result<(), StoreError>;

    // ------------------------------------------------------------------
    // Upstream connections (broker)
    // ------------------------------------------------------------------

    async fn create_upstream_connection(
        &self,
        req: types::NewUpstreamConnection,
    ) -> Result<UpstreamConnection, StoreError>;
    async fn get_upstream_connection(&self, id: Uuid) -> Result<UpstreamConnection, StoreError>;
    async fn list_connections_in_realm(
        &self,
        realm_id: Uuid,
    ) -> Result<Vec<UpstreamConnection>, StoreError>;
    async fn set_connection_enabled(
        &self,
        id: Uuid,
        enabled: bool,
    ) -> Result<UpstreamConnection, StoreError>;
    async fn delete_upstream_connection(&self, id: Uuid) -> Result<(), StoreError>;

    /// Replace a connection's full claim-mapping list.
    async fn put_claim_mappings(
        &self,
        connection_id: Uuid,
        mappings: Vec<types::ClaimMapping>,
    ) -> Result<(), StoreError>;
    async fn list_claim_mappings(
        &self,
        connection_id: Uuid,
    ) -> Result<Vec<types::ClaimMapping>, StoreError>;

    // ------------------------------------------------------------------
    // Signing keys
    // ------------------------------------------------------------------

    /// Add a key to a realm's ring (rotation `Next`-mint, etc.).
    /// [`StoreError::NotFound`] if the realm is missing;
    /// [`StoreError::Conflict`] if `kid` is already used in the realm.
    async fn add_signing_key(
        &self,
        realm_id: Uuid,
        key: types::NewSigningKey,
    ) -> Result<SigningKey, StoreError>;
    async fn get_signing_key(&self, realm_id: Uuid, kid: &str) -> Result<SigningKey, StoreError>;
    /// List a realm's keys, oldest-first (the order the JWKS endpoint
    /// publishes them in).
    async fn list_signing_keys(&self, realm_id: Uuid) -> Result<Vec<SigningKey>, StoreError>;
    async fn set_signing_key_status(
        &self,
        realm_id: Uuid,
        kid: &str,
        status: types::KeyStatus,
    ) -> Result<SigningKey, StoreError>;
    async fn delete_signing_key(&self, realm_id: Uuid, kid: &str) -> Result<(), StoreError>;

    /// Try to acquire the rotation lock for `holder` for `ttl_secs`.
    /// Returns `true` on success (lock free, expired, or already held by
    /// `holder`); `false` if another holder's lock is still valid.
    async fn try_acquire_rotation_lock(
        &self,
        holder: &str,
        ttl_secs: u32,
    ) -> Result<bool, StoreError>;
    /// Release the rotation lock if `holder` holds it (no-op otherwise).
    async fn release_rotation_lock(&self, holder: &str) -> Result<(), StoreError>;

    // ------------------------------------------------------------------
    // Short-lived flow records
    // ------------------------------------------------------------------

    async fn put_auth_code(&self, code: types::AuthCode) -> Result<(), StoreError>;
    /// Atomically read-and-delete an authorization code (single use).
    async fn take_auth_code(&self, code: &str) -> Result<types::AuthCode, StoreError>;

    async fn put_refresh_token(&self, token: types::RefreshToken) -> Result<(), StoreError>;
    async fn get_refresh_token(&self, jti: Uuid) -> Result<types::RefreshToken, StoreError>;
    async fn revoke_refresh_token(&self, jti: Uuid) -> Result<(), StoreError>;
    /// Revoke every refresh token in a family (theft-detection trigger,
    /// and the mechanism behind `set_user_status(_, Disabled)`).
    async fn revoke_refresh_family(&self, family_id: Uuid) -> Result<(), StoreError>;

    async fn put_device_code(&self, dc: types::DeviceCode) -> Result<(), StoreError>;
    async fn get_device_code_by_dc(
        &self,
        device_code: &str,
    ) -> Result<types::DeviceCode, StoreError>;
    async fn get_device_code_by_uc(&self, user_code: &str)
    -> Result<types::DeviceCode, StoreError>;
    async fn update_device_code_status(
        &self,
        device_code: &str,
        status: types::DeviceCodeStatus,
        user_id: Option<Uuid>,
        granted_tenant: Option<Uuid>,
    ) -> Result<types::DeviceCode, StoreError>;

    async fn put_session(&self, session: types::Session) -> Result<(), StoreError>;
    async fn get_session(&self, id: Uuid) -> Result<types::Session, StoreError>;
    async fn delete_session(&self, id: Uuid) -> Result<(), StoreError>;

    async fn put_broker_state(&self, st: types::BrokerState) -> Result<(), StoreError>;
    /// Atomically read-and-delete a broker-bridge state (single use).
    async fn take_broker_state(&self, state: &str) -> Result<types::BrokerState, StoreError>;

    /// Drop every short-lived record whose `expires_at` is `< now`.
    /// Returns how many rows were removed. The `identityd` sweeper calls
    /// this on a timer.
    async fn sweep_expired(&self, now: chrono::DateTime<chrono::Utc>) -> Result<usize, StoreError>;
}
