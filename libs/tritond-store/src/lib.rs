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
pub use types::{ApiKey, ApiKeyView, NewSilo, Silo, SystemKey, User, UserView};

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
}
