// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Abstract storage trait plus backend implementations.
//!
//! `MorayStore` is the one seam between the wire/rpc layer and the
//! persistence layer. The FDB backend (`fdb.rs`) is the production path;
//! the memory backend (`mem.rs`) is for tests and for building on a
//! developer laptop without libfdb_c.

pub mod mem;

#[cfg(feature = "fdb")]
pub mod fdb;

use crate::error::Result;
use crate::types::{Bucket, BucketConfig, ObjectMeta};
use serde_json::Value;
use std::future::Future;

/// Put-object options from the wire. `etag` enables optimistic concurrency:
/// if set to `Some(s)`, the put commits only if the current object has
/// that etag (or is absent and s is empty); otherwise returns
/// `EtagConflict`.
#[derive(Debug, Default, Clone)]
pub struct PutOpts {
    /// `Some("")` means "must not exist"; `Some(etag)` means "must match".
    /// `None` means unconditional upsert.
    pub etag: Option<String>,
    /// Headers forwarded from the wire options — `x-ufds-operation`
    /// etc. Drives the `fixTypes` built-in trigger.
    pub headers: serde_json::Map<String, Value>,
}

/// One operation inside a `batch` RPC. The store applies a slice of these
/// **atomically**: either every op commits or none do. This is the
/// invariant Moray callers depend on (sdc-napi's `commitBatch` issues
/// `[put, update, put]` and would loop forever on stale etags if a
/// partial commit landed). Construct via the builder-style `From` impls
/// or manually; the order of variants in the slice is the order the
/// store applies them.
#[derive(Debug, Clone)]
pub enum BatchOp {
    Put {
        bucket: String,
        key: String,
        value: Value,
        opts: PutOpts,
        unique_cols: Vec<(String, Vec<String>)>,
    },
    Delete {
        bucket: String,
        key: String,
        expected_etag: Option<String>,
    },
    /// Read every row in `bucket` matching `filter_str`, merge `fields`
    /// over the value, then put the rewritten value under the same key.
    /// `prepared_rows` carries the already-fetched + already-trigger-
    /// processed rows so the store can apply them inside the same FDB
    /// transaction as the surrounding `put`/`delete` ops.
    UpdateRows {
        bucket: String,
        rows: Vec<(String, Value, Vec<(String, Vec<String>)>)>,
    },
    /// Delete the keys listed (already resolved by the rpc layer from a
    /// filter scan).
    DeleteKeys {
        bucket: String,
        keys: Vec<String>,
    },
}

/// Per-op result echoed back to the wire. `etag`/`_id` only set for puts.
#[derive(Debug, Clone)]
pub struct BatchOpResult {
    pub bucket: String,
    pub key: Option<String>,
    pub etag: Option<String>,
    pub id: Option<u64>,
    pub count: Option<usize>,
}

#[allow(async_fn_in_trait)]
pub trait MorayStore: Send + Sync + 'static {
    /// Create a bucket. Fails with `BucketAlreadyExists` if one already
    /// exists; use updateBucket to mutate.
    fn create_bucket(
        &self,
        name: &str,
        config: BucketConfig,
    ) -> impl Future<Output = Result<Bucket>> + Send;

    fn get_bucket(&self, name: &str) -> impl Future<Output = Result<Bucket>> + Send;

    /// Overwrite a bucket's schema. Moray exposes this as `updateBucket`.
    /// The object data stays put; clients run `reindexObjects` afterwards
    /// if they need the index rebuilt.
    fn update_bucket(
        &self,
        name: &str,
        config: BucketConfig,
    ) -> impl Future<Output = Result<Bucket>> + Send;

    /// Delete a bucket and every object in it. Returns Ok(()) even if the
    /// bucket did not exist — Moray tolerates delete-of-missing.
    fn delete_bucket(&self, name: &str) -> impl Future<Output = Result<()>> + Send;

    fn list_buckets(&self) -> impl Future<Output = Result<Vec<Bucket>>> + Send;

    /// Upsert an object. Returns the new object metadata (id, etag, mtime).
    ///
    /// `unique_cols` is the list of `(column, scalar_value)` pairs that must
    /// be unique across the bucket. The store writes the matching entries
    /// in a secondary index subspace and aborts with
    /// `MorayError::UniqueConstraint` if any are already claimed by a
    /// different key.
    fn put_object(
        &self,
        bucket: &str,
        key: &str,
        value: Value,
        opts: PutOpts,
        unique_cols: Vec<(String, Vec<String>)>,
    ) -> impl Future<Output = Result<ObjectMeta>> + Send;

    fn get_object(
        &self,
        bucket: &str,
        key: &str,
    ) -> impl Future<Output = Result<ObjectMeta>> + Send;

    /// If `expected_etag` is `Some`, the delete must match the current
    /// object's etag (or be `Some("")` to require absence → which then
    /// just no-ops). Otherwise returns `EtagConflict`.
    fn delete_object(
        &self,
        bucket: &str,
        key: &str,
        expected_etag: Option<String>,
    ) -> impl Future<Output = Result<()>> + Send;

    /// Walk every object in a bucket (bounded by `limit`, skipping `offset`)
    /// and return the raw list. The caller applies the LDAP filter — the
    /// store just streams the bodies. Sort and filter happen in the RPC
    /// layer so all backends share the behaviour.
    fn scan_objects(
        &self,
        bucket: &str,
        limit: usize,
    ) -> impl Future<Output = Result<Vec<ObjectMeta>>> + Send;

    /// Process up to `count` pending reindex rows. Returns
    /// `(processed, remaining)`. Called by the `reindexObjects` RPC.
    fn reindex_step(
        &self,
        bucket: &str,
        count: u64,
    ) -> impl Future<Output = Result<(u64, u64)>> + Send;

    /// Clear the bucket's one-shot `reindex_just_finished` sentinel.
    fn clear_reindex_just_finished(
        &self,
        bucket: &str,
    ) -> impl Future<Output = Result<()>> + Send;

    /// Apply a slice of `BatchOp`s atomically. Either every op commits or
    /// none do. This is the invariant `commitBatch` callers depend on —
    /// see the comment on [`BatchOp`].
    fn apply_batch(
        &self,
        ops: Vec<BatchOp>,
    ) -> impl Future<Output = Result<Vec<BatchOpResult>>> + Send;
}
