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
pub use types::{NewSilo, Silo};

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
    /// Create a new silo.
    ///
    /// Returns [`StoreError::Conflict`] if `req.name` is already in use.
    async fn create_silo(&self, req: NewSilo) -> Result<Silo, StoreError>;

    /// Look up a silo by id.
    ///
    /// Returns [`StoreError::NotFound`] if no silo with that id exists.
    async fn get_silo(&self, id: Uuid) -> Result<Silo, StoreError>;
}
