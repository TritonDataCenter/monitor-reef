// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Persistence layer for the Triton Cloud control plane.
//!
//! This crate defines the domain types and the [`Store`] trait that
//! `tritond` uses to read and write its state. Two backends ship in
//! this crate:
//!
//! * [`MemStore`] — an in-process `RwLock<HashMap>` implementation,
//!   used for tests and for `tritond` runs that don't need durable
//!   state. Always available.
//! * `FdbStore` (behind the `foundationdb` cargo feature, added in a
//!   subsequent commit) — the production FoundationDB-backed
//!   implementation.
//!
//! The trait deliberately deals only in plain Rust types. It does not
//! know about HTTP, JSON, or Dropshot; the wire surface lives in
//! `tritond-api` and re-uses the types defined here so there is no
//! API↔storage conversion layer to keep in sync.

#[cfg(feature = "foundationdb")]
pub mod fdb;
pub mod mem;
mod types;

#[cfg(feature = "foundationdb")]
pub use fdb::FdbStore;
pub use mem::MemStore;
pub use types::{
    ApiKey, ApiKeyView, Federation, IdpConfig, IdpConfigView, NewProject, NewSilo, NewSubnet,
    NewVpc, Project, Silo, Subnet, SystemKey, User, UserView, VPC_VNI_MAX,
    VPC_VNI_RESERVED_CEILING, Vpc,
};

use async_trait::async_trait;
use uuid::Uuid;

/// Errors that a [`Store`] implementation may produce.
///
/// Phase 0 surfaces only the conditions that are meaningful to API
/// callers (not-found, name conflicts) plus a catch-all `Backend`
/// variant for transport / driver failures. Once `FdbStore` lands the
/// catch-all gains FoundationDB-specific context.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// The requested entity does not exist.
    #[error("not found")]
    NotFound,

    /// A unique constraint was violated (e.g. silo name already taken).
    #[error("conflict: {0}")]
    Conflict(String),

    /// The backing store reported an error that the caller cannot meaningfully
    /// react to beyond surfacing it.
    #[error("store backend error: {0}")]
    Backend(String),
}

/// A handle to the control-plane state store.
///
/// All methods are async because the production backend
/// (FoundationDB) is async; the in-memory implementation simulates
/// async by being trivially `await`-able.
#[async_trait]
pub trait Store: Send + Sync + 'static {
    // ------------------------------------------------------------------
    // Silos
    // ------------------------------------------------------------------

    /// Create a new silo.
    ///
    /// Returns [`StoreError::Conflict`] if `req.name` is already in use.
    async fn create_silo(&self, req: NewSilo) -> Result<Silo, StoreError>;

    /// Look up a silo by id.
    ///
    /// Returns [`StoreError::NotFound`] if no silo with that id exists.
    async fn get_silo(&self, id: Uuid) -> Result<Silo, StoreError>;

    // ------------------------------------------------------------------
    // Users
    // ------------------------------------------------------------------

    /// Create a new operator account.
    ///
    /// Returns [`StoreError::Conflict`] if `user.username` is already
    /// in use.
    async fn create_user(&self, user: User) -> Result<User, StoreError>;

    /// Look up an operator by username.
    async fn get_user_by_username(&self, username: &str) -> Result<User, StoreError>;

    /// Look up an operator by id.
    async fn get_user_by_id(&self, id: Uuid) -> Result<User, StoreError>;

    /// True if any user record exists. Used by the bootstrap path to
    /// decide whether to mint the root operator at first run.
    async fn has_any_user(&self) -> Result<bool, StoreError>;

    // ------------------------------------------------------------------
    // API keys
    // ------------------------------------------------------------------

    /// Persist a freshly issued API key (storage form: bcrypt hash).
    async fn create_api_key(&self, key: ApiKey) -> Result<ApiKey, StoreError>;

    /// List the API keys belonging to a single user. Used by `tcadm
    /// api-key list`.
    async fn list_api_keys(&self, user_id: Uuid) -> Result<Vec<ApiKey>, StoreError>;

    /// Look up an API key by its non-secret `lookup_id` segment.
    /// Returns [`StoreError::NotFound`] if no such key exists. The
    /// auth middleware uses this for O(1) credential resolution.
    async fn get_api_key_by_lookup_id(&self, lookup_id: &str) -> Result<ApiKey, StoreError>;

    /// Delete an API key by id. Returns [`StoreError::NotFound`] if
    /// the id does not belong to `user_id`'s set.
    async fn delete_api_key(&self, user_id: Uuid, key_id: Uuid) -> Result<(), StoreError>;

    // ------------------------------------------------------------------
    // System keys
    // ------------------------------------------------------------------

    /// Read a cluster-level system key, e.g. the JWT signing secret.
    async fn get_system_key(&self, key: SystemKey) -> Result<Vec<u8>, StoreError>;

    /// Persist a cluster-level system key. Overwrites any existing
    /// value; rotation policy lives in the caller.
    async fn put_system_key(&self, key: SystemKey, value: Vec<u8>) -> Result<(), StoreError>;

    // ------------------------------------------------------------------
    // Federated users (OIDC)
    // ------------------------------------------------------------------

    /// Look up a user by their `(silo_id, issuer, subject)` triple.
    /// Returns [`StoreError::NotFound`] if no user matches; the auth
    /// middleware uses that to JIT-create the row on first OIDC
    /// login.
    async fn get_user_by_federation(
        &self,
        silo_id: Uuid,
        issuer: &str,
        subject: &str,
    ) -> Result<User, StoreError>;

    // ------------------------------------------------------------------
    // Per-silo IdP configuration
    // ------------------------------------------------------------------

    /// Persist (or replace) the OIDC IdP config for a silo. Eager
    /// discovery happens in the caller before this is invoked, so
    /// failure here is purely storage-side.
    async fn put_idp_config(
        &self,
        silo_id: Uuid,
        config: IdpConfig,
    ) -> Result<IdpConfig, StoreError>;

    /// Read the IdP config for a silo. Returns [`StoreError::NotFound`]
    /// when the silo has no IdP attached.
    async fn get_idp_config(&self, silo_id: Uuid) -> Result<IdpConfig, StoreError>;

    /// Remove the IdP config for a silo.
    async fn delete_idp_config(&self, silo_id: Uuid) -> Result<(), StoreError>;

    /// Iterate every (silo_id, IdpConfig) pair. Used by the auth
    /// middleware to find the IdP whose `issuer` matches an inbound
    /// token's `iss` claim.
    async fn list_idp_configs(&self) -> Result<Vec<(Uuid, IdpConfig)>, StoreError>;

    // ------------------------------------------------------------------
    // Projects (silo-scoped)
    // ------------------------------------------------------------------

    /// Create a project inside a silo. Returns
    /// [`StoreError::Conflict`] if `name` is already in use within
    /// the same silo. Returns [`StoreError::NotFound`] if the silo
    /// itself doesn't exist (the caller is expected to have already
    /// resolved silo existence via Cedar; the check here is a
    /// defence-in-depth race guard).
    async fn create_project(&self, silo_id: Uuid, req: NewProject) -> Result<Project, StoreError>;

    /// Look up a project by id. Returns [`StoreError::NotFound`] when
    /// no such project exists, regardless of silo.
    async fn get_project(&self, project_id: Uuid) -> Result<Project, StoreError>;

    /// List every project belonging to `silo_id`. Order is unspecified
    /// for Phase 0e-c; pagination lands when the list grows beyond a
    /// single response.
    async fn list_projects_in_silo(&self, silo_id: Uuid) -> Result<Vec<Project>, StoreError>;

    /// Delete a project by id. Returns [`StoreError::NotFound`] if the
    /// id does not exist.
    async fn delete_project(&self, project_id: Uuid) -> Result<(), StoreError>;

    // ------------------------------------------------------------------
    // VPCs (project-scoped)
    // ------------------------------------------------------------------

    /// Create a VPC inside a project.
    ///
    /// Invariants enforced by the implementation:
    ///
    /// * The project must exist and `project.silo_id == silo_id`. A
    ///   `silo_id` mismatch returns [`StoreError::NotFound`] (treating
    ///   the project as invisible to the wrong silo).
    /// * `name` must not collide with an existing VPC in the same
    ///   project — collision returns [`StoreError::Conflict`].
    /// * `vni` is server-assigned, drawn uniformly at random from
    ///   `[VPC_VNI_RESERVED_CEILING, VPC_VNI_MAX)`, with
    ///   collision-retry against the rack-wide VNI index. If the
    ///   retry loop is exhausted (operationally unreachable),
    ///   returns [`StoreError::Backend`].
    /// * The caller is expected to have validated `req.ipv4_block.is_some()
    ///   || req.ipv6_block.is_some()` at the API layer; the store
    ///   does not re-validate.
    async fn create_vpc(
        &self,
        silo_id: Uuid,
        project_id: Uuid,
        req: NewVpc,
    ) -> Result<Vpc, StoreError>;

    /// Look up a VPC by id. Returns [`StoreError::NotFound`] when no
    /// such VPC exists, regardless of silo or project. Handlers add
    /// silo_id + project_id rechecks on top.
    async fn get_vpc(&self, vpc_id: Uuid) -> Result<Vpc, StoreError>;

    /// List every VPC belonging to `project_id`. Order is unspecified.
    /// Returns an empty vec if the project has no VPCs *or* if the
    /// project does not exist — distinguishing the two would require
    /// a project-existence read the caller has already done.
    async fn list_vpcs_in_project(&self, project_id: Uuid) -> Result<Vec<Vpc>, StoreError>;

    /// Delete a VPC by id. Returns [`StoreError::NotFound`] if the
    /// id does not exist. Returns [`StoreError::Conflict`] if the
    /// VPC still has subnets attached — the operator must clear
    /// subnets before deleting the VPC. (No cascade in Phase 0;
    /// preserves the "don't accidentally lose tenant data" stance.)
    async fn delete_vpc(&self, vpc_id: Uuid) -> Result<(), StoreError>;

    // ------------------------------------------------------------------
    // Subnets (vpc-scoped)
    // ------------------------------------------------------------------

    /// Create a subnet inside a VPC.
    ///
    /// Invariants enforced by the implementation:
    ///
    /// * The VPC must exist *and* `vpc.silo_id == silo_id` *and*
    ///   `vpc.project_id == project_id`. Any mismatch returns
    ///   [`StoreError::NotFound`] — the caller cannot tell whether
    ///   the VPC is in a different parent or doesn't exist at all,
    ///   which is the cross-tenant probe story we want.
    /// * At least one of `req.ipv4_block` / `req.ipv6_block` must
    ///   be `Some`. The API layer enforces this before calling the
    ///   store; the store does not re-validate.
    /// * Each present family CIDR must be a subnet of the parent
    ///   VPC's same-family CIDR. Each present family must also be
    ///   present on the VPC. Violations return
    ///   [`StoreError::Conflict`] (with a message naming the
    ///   broken invariant).
    /// * No present family CIDR may overlap an existing subnet's
    ///   CIDR in the same VPC. Overlap → [`StoreError::Conflict`].
    /// * `name` must not collide with an existing subnet in the
    ///   same VPC.
    async fn create_subnet(
        &self,
        silo_id: Uuid,
        project_id: Uuid,
        vpc_id: Uuid,
        req: NewSubnet,
    ) -> Result<Subnet, StoreError>;

    /// Look up a subnet by id. Returns [`StoreError::NotFound`] when
    /// no such subnet exists. Handlers add silo_id + project_id +
    /// vpc_id rechecks on top.
    async fn get_subnet(&self, subnet_id: Uuid) -> Result<Subnet, StoreError>;

    /// List every subnet in a VPC. Order is unspecified.
    async fn list_subnets_in_vpc(&self, vpc_id: Uuid) -> Result<Vec<Subnet>, StoreError>;

    /// Delete a subnet by id. Returns [`StoreError::NotFound`] if the
    /// id does not exist.
    async fn delete_subnet(&self, subnet_id: Uuid) -> Result<(), StoreError>;
}
