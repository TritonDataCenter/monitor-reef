// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! FoundationDB-backed [`Store`] implementation.
//!
//! Compiled in only when the `foundationdb` cargo feature is enabled,
//! because linking pulls in `libfdb_c.so` (FoundationDB client
//! library, currently 7.3.x). Default builds don't need FDB installed
//! and use [`crate::MemStore`] instead.
//!
//! # Boot semantics
//!
//! The FDB Rust binding requires exactly one `boot()` call per
//! process; the returned `NetworkAutoStop` guard must outlive every
//! `Database` handle. We satisfy this with a `OnceLock` plus a
//! `mem::forget` so the network thread runs until the process exits,
//! which is the right shape for a long-running daemon.
//!
//! # Schema
//!
//! Phase 0 lays down two index keys per silo:
//!
//! ```text
//! silo/by_id/<uuid>      -> JSON-encoded Silo
//! silo/by_name/<name>    -> uuid hyphenated bytes
//! ```
//!
//! Both writes happen in a single transaction so name uniqueness is
//! enforced atomically.

use std::sync::OnceLock;

use async_trait::async_trait;
use chrono::Utc;
use foundationdb::{Database, FdbBindingError};
use uuid::Uuid;

use crate::{NewSilo, Silo, Store, StoreError};

static FDB_NETWORK: OnceLock<()> = OnceLock::new();

/// Boot the FDB network thread (idempotent). The returned guard is
/// intentionally leaked so FDB stays alive for the rest of the
/// process.
fn ensure_fdb_booted() {
    FDB_NETWORK.get_or_init(|| {
        // SAFETY: boot() must be called at most once per process. The
        // OnceLock guarantees that. The returned guard is leaked so it
        // outlives all Database instances, which is the requirement.
        let guard = unsafe { foundationdb::boot() };
        std::mem::forget(guard);
    });
}

/// FoundationDB-backed [`Store`].
pub struct FdbStore {
    db: Database,
}

impl FdbStore {
    /// Open the database described by `cluster_file_path`. Pass `None`
    /// to use FoundationDB's default cluster file resolution
    /// (`FDB_CLUSTER_FILE` env, `/etc/foundationdb/fdb.cluster`).
    pub fn open(cluster_file_path: Option<&str>) -> Result<Self, StoreError> {
        ensure_fdb_booted();
        let db = Database::new(cluster_file_path)
            .map_err(|e| StoreError::Backend(format!("open FDB cluster: {e}")))?;
        Ok(Self { db })
    }

    fn by_id_key(id: Uuid) -> Vec<u8> {
        format!("silo/by_id/{id}").into_bytes()
    }

    fn by_name_key(name: &str) -> Vec<u8> {
        format!("silo/by_name/{name}").into_bytes()
    }
}

/// Outcome carried out of a transaction closure when the conflict
/// reason is one of *our* invariants (e.g. duplicate name) rather
/// than an FDB-level retryable error.
enum CreateOutcome {
    Created,
    NameTaken,
}

#[async_trait]
impl Store for FdbStore {
    async fn create_silo(&self, req: NewSilo) -> Result<Silo, StoreError> {
        let silo = Silo {
            id: Uuid::new_v4(),
            name: req.name,
            description: req.description.unwrap_or_default(),
            created_at: Utc::now(),
        };
        let value = serde_json::to_vec(&silo)
            .map_err(|e| StoreError::Backend(format!("serialize silo: {e}")))?;
        let by_id_key = Self::by_id_key(silo.id);
        let by_name_key = Self::by_name_key(&silo.name);
        let id_str = silo.id.to_string();

        let outcome: Result<CreateOutcome, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let by_id_key = by_id_key.clone();
                let by_name_key = by_name_key.clone();
                let value = value.clone();
                let id_bytes = id_str.as_bytes().to_vec();
                async move {
                    if tr.get(&by_name_key, false).await?.is_some() {
                        return Ok(CreateOutcome::NameTaken);
                    }
                    tr.set(&by_id_key, &value);
                    tr.set(&by_name_key, &id_bytes);
                    Ok(CreateOutcome::Created)
                }
            })
            .await;

        match outcome {
            Ok(CreateOutcome::Created) => Ok(silo),
            Ok(CreateOutcome::NameTaken) => Err(StoreError::Conflict(format!(
                "silo with name {:?} already exists",
                silo.name
            ))),
            Err(e) => Err(StoreError::Backend(format!("FDB transaction: {e}"))),
        }
    }

    async fn get_silo(&self, id: Uuid) -> Result<Silo, StoreError> {
        let key = Self::by_id_key(id);
        let bytes: Result<Option<Vec<u8>>, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let key = key.clone();
                async move { Ok(tr.get(&key, false).await?.map(|s| s.to_vec())) }
            })
            .await;

        match bytes {
            Ok(Some(bytes)) => serde_json::from_slice(&bytes)
                .map_err(|e| StoreError::Backend(format!("deserialize silo: {e}"))),
            Ok(None) => Err(StoreError::NotFound),
            Err(e) => Err(StoreError::Backend(format!("FDB transaction: {e}"))),
        }
    }
}
