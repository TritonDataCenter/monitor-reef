// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! In-process `MorayStore` for tests and laptop dev. Not persistent.

use std::collections::HashMap;

use chrono::Utc;
// Non-poisoning lock: the workspace lints deny `Result::unwrap` / `expect`,
// and a regular std Mutex would force one of those on every `.lock()`.
use parking_lot::Mutex;
use serde_json::Value;
use uuid::Uuid;

use crate::error::{MorayError, Result};
use crate::store::{MorayStore, PutOpts};
use crate::types::{Bucket, BucketConfig, ObjectMeta, ReindexState};

#[derive(Default)]
struct Inner {
    buckets: HashMap<String, Bucket>,
    /// (bucket, key) -> (value, id, etag, mtime)
    objects: HashMap<(String, String), ObjectMeta>,
    /// per-bucket monotonic id counter (matches node-moray's _id semantics)
    next_id: HashMap<String, u64>,
    /// (bucket, column, value) -> key — enforces `unique: true` on an
    /// indexed column. Populated on put, cleared on delete.
    unique_index: HashMap<(String, String, String), String>,
}

pub struct MemStore {
    inner: Mutex<Inner>,
}

impl MemStore {
    pub fn new() -> Self {
        Self { inner: Mutex::new(Inner::default()) }
    }
}

impl Default for MemStore {
    fn default() -> Self {
        Self::new()
    }
}

fn etag_of(v: &Value) -> String {
    // Same algorithm Moray uses: md5 of the canonical JSON encoding.
    // We accept a non-canonical encoding here (serde_json's default
    // ordering is insertion-order for Maps) because the Mem backend is
    // only used for tests — FDB backend computes the real etag.
    let bytes = serde_json::to_vec(v).unwrap_or_default();
    format!("{:032x}", md5_like(&bytes))
}

/// Hash function stand-in — the Mem backend uses a cheap FNV so we don't
/// drag in the `md-5` crate for a dev-only path. FDB backend uses real MD5.
fn md5_like(bytes: &[u8]) -> u128 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    (h as u128) | ((h.rotate_left(17) as u128) << 64)
}

impl MorayStore for MemStore {
    async fn create_bucket(&self, name: &str, config: BucketConfig) -> Result<Bucket> {
        let mut inner = self.inner.lock();
        if inner.buckets.contains_key(name) {
            return Err(MorayError::BucketAlreadyExists(name.into()));
        }
        let b = Bucket {
            name: name.into(),
            id: Uuid::new_v4(),
            options: config,
            mtime: Utc::now(),
            reindex_active: None,
            reindex_just_finished: false,
        };
        inner.buckets.insert(name.into(), b.clone());
        inner.next_id.insert(name.into(), 0);
        Ok(b)
    }

    async fn get_bucket(&self, name: &str) -> Result<Bucket> {
        let inner = self.inner.lock();
        inner
            .buckets
            .get(name)
            .cloned()
            .ok_or_else(|| MorayError::BucketNotFound(name.into()))
    }

    async fn update_bucket(&self, name: &str, config: BucketConfig) -> Result<Bucket> {
        let mut inner = self.inner.lock();
        let existing = inner
            .buckets
            .get(name)
            .cloned()
            .ok_or_else(|| MorayError::BucketNotFound(name.into()))?;

        // Compute which index columns are new vs existing. New ones
        // (that Moray would have to back-index for old rows) go into
        // reindex_active. Nothing to reindex when the bucket is empty.
        let new_columns: Vec<String> = config
            .index
            .keys()
            .filter(|k| !existing.options.index.contains_key(*k))
            .cloned()
            .collect();
        let object_count: u64 = inner
            .objects
            .iter()
            .filter(|((b, _), _)| b == name)
            .count() as u64;
        let reindex_active = if new_columns.is_empty() || object_count == 0 {
            None
        } else {
            Some(ReindexState {
                columns: new_columns,
                remaining: object_count,
            })
        };

        let updated = Bucket {
            options: config,
            mtime: Utc::now(),
            reindex_active,
            reindex_just_finished: false,
            ..existing
        };
        inner.buckets.insert(name.into(), updated.clone());
        Ok(updated)
    }

    async fn clear_reindex_just_finished(&self, bucket: &str) -> Result<()> {
        let mut inner = self.inner.lock();
        if let Some(b) = inner.buckets.get_mut(bucket) {
            b.reindex_just_finished = false;
        }
        Ok(())
    }

    async fn reindex_step(&self, bucket: &str, count: u64) -> Result<(u64, u64)> {
        let mut inner = self.inner.lock();
        let Some(b) = inner.buckets.get_mut(bucket) else {
            return Err(MorayError::BucketNotFound(bucket.into()));
        };
        let Some(state) = b.reindex_active.as_mut() else {
            return Ok((0, 0));
        };
        let processed = count.min(state.remaining);
        state.remaining -= processed;
        if state.remaining == 0 {
            b.reindex_active = None;
            b.reindex_just_finished = true;
        }
        Ok((processed, b.reindex_active.as_ref().map_or(0, |s| s.remaining)))
    }

    async fn delete_bucket(&self, name: &str) -> Result<()> {
        let mut inner = self.inner.lock();
        inner.buckets.remove(name);
        inner.objects.retain(|(b, _), _| b != name);
        inner.next_id.remove(name);
        Ok(())
    }

    async fn list_buckets(&self) -> Result<Vec<Bucket>> {
        let inner = self.inner.lock();
        let mut v: Vec<Bucket> = inner.buckets.values().cloned().collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(v)
    }

    async fn put_object(
        &self,
        bucket: &str,
        key: &str,
        value: Value,
        opts: PutOpts,
        unique_cols: Vec<(String, Vec<String>)>,
    ) -> Result<ObjectMeta> {
        let mut inner = self.inner.lock();
        if !inner.buckets.contains_key(bucket) {
            return Err(MorayError::BucketNotFound(bucket.into()));
        }
        // Conditional put: etag==Some("") → require absence; etag==Some(t)
        // → require current etag == t.
        if let Some(want_etag) = opts.etag.as_deref() {
            let current = inner.objects.get(&(bucket.to_string(), key.to_string()));
            let actual = current.map_or_else(|| "null".to_string(), |c| c.etag.clone());
            match (want_etag, current) {
                ("" | "null", None) => {} // absent as required
                ("" | "null", Some(_)) => {
                    return Err(MorayError::EtagConflict {
                        bucket: bucket.into(),
                        key: key.into(),
                        expected: want_etag.to_string(),
                        actual,
                    })
                }
                (want, Some(c)) if c.etag == want => {}
                _ => {
                    return Err(MorayError::EtagConflict {
                        bucket: bucket.into(),
                        key: key.into(),
                        expected: want_etag.to_string(),
                        actual,
                    })
                }
            }
        }
        // Check each uniqueness claim. A claim from the same key is OK —
        // an upsert carrying the same value for a unique column must not
        // collide with itself.
        for (col, values) in &unique_cols {
            for v in values {
                let idx_key = (bucket.to_string(), col.clone(), v.clone());
                if let Some(owner) = inner.unique_index.get(&idx_key)
                    && owner != key
                {
                    return Err(MorayError::UniqueConstraint {
                        bucket: bucket.into(),
                        column: col.clone(),
                        value: v.clone(),
                    });
                }
            }
        }
        // Clear the prior unique-index entries for this key (prior put's
        // values may have differed from the current ones).
        let prior_values: Vec<(String, String)> = inner
            .unique_index
            .iter()
            .filter(|((b, _, _), owner)| b == bucket && owner.as_str() == key)
            .map(|((_, c, v), _)| (c.clone(), v.clone()))
            .collect();
        for (c, v) in prior_values {
            inner.unique_index.remove(&(bucket.to_string(), c, v));
        }
        for (col, values) in &unique_cols {
            for v in values {
                inner.unique_index.insert(
                    (bucket.to_string(), col.clone(), v.clone()),
                    key.to_string(),
                );
            }
        }

        let id = {
            let counter = inner.next_id.entry(bucket.into()).or_insert(0);
            *counter += 1;
            *counter
        };
        let meta = ObjectMeta {
            key: key.into(),
            id,
            etag: etag_of(&value),
            mtime: Utc::now(),
            value,
        };
        inner.objects.insert((bucket.into(), key.into()), meta.clone());
        Ok(meta)
    }

    async fn scan_objects(
        &self,
        bucket: &str,
        limit: usize,
    ) -> Result<Vec<ObjectMeta>> {
        let inner = self.inner.lock();
        if !inner.buckets.contains_key(bucket) {
            return Err(MorayError::BucketNotFound(bucket.into()));
        }
        let mut out: Vec<ObjectMeta> = inner
            .objects
            .iter()
            .filter(|((b, _), _)| b == bucket)
            .map(|(_, v)| v.clone())
            .collect();
        out.sort_by_key(|o| o.id);
        out.truncate(limit);
        Ok(out)
    }

    async fn get_object(&self, bucket: &str, key: &str) -> Result<ObjectMeta> {
        let inner = self.inner.lock();
        if !inner.buckets.contains_key(bucket) {
            return Err(MorayError::BucketNotFound(bucket.into()));
        }
        inner
            .objects
            .get(&(bucket.to_string(), key.to_string()))
            .cloned()
            .ok_or_else(|| MorayError::ObjectNotFound {
                bucket: bucket.into(),
                key: key.into(),
            })
    }

    async fn delete_object(
        &self,
        bucket: &str,
        key: &str,
        expected_etag: Option<String>,
    ) -> Result<()> {
        let mut inner = self.inner.lock();
        if !inner.buckets.contains_key(bucket) {
            return Err(MorayError::BucketNotFound(bucket.into()));
        }
        if let Some(want) = expected_etag.as_deref() {
            let current = inner.objects.get(&(bucket.to_string(), key.to_string()));
            let actual = current.map_or_else(|| "null".to_string(), |c| c.etag.clone());
            match (want, current) {
                (_, None) => {
                    return Err(MorayError::ObjectNotFound {
                        bucket: bucket.into(),
                        key: key.into(),
                    })
                }
                (w, Some(o)) if o.etag == w => {}
                _ => {
                    return Err(MorayError::EtagConflict {
                        bucket: bucket.into(),
                        key: key.into(),
                        expected: want.to_string(),
                        actual,
                    })
                }
            }
        }
        let removed = inner
            .objects
            .remove(&(bucket.to_string(), key.to_string()));
        if removed.is_none() {
            return Err(MorayError::ObjectNotFound {
                bucket: bucket.into(),
                key: key.into(),
            });
        }
        let prior_values: Vec<(String, String)> = inner
            .unique_index
            .iter()
            .filter(|((b, _, _), owner)| b == bucket && owner.as_str() == key)
            .map(|((_, c, v), _)| (c.clone(), v.clone()))
            .collect();
        for (c, v) in prior_values {
            inner.unique_index.remove(&(bucket.to_string(), c, v));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bucket_lifecycle() {
        let s = MemStore::new();
        s.create_bucket("users", BucketConfig::default()).await.unwrap();
        assert!(s.create_bucket("users", BucketConfig::default()).await.is_err());
        assert_eq!(s.get_bucket("users").await.unwrap().name, "users");
        s.delete_bucket("users").await.unwrap();
        assert!(s.get_bucket("users").await.is_err());
    }

    #[tokio::test]
    async fn unique_constraint() {
        let s = MemStore::new();
        s.create_bucket("u", BucketConfig::default()).await.unwrap();
        s.put_object(
            "u",
            "alice",
            serde_json::json!({"email": "a@x"}),
            PutOpts::default(),
            vec![("email".to_string(), vec!["a@x".to_string()])],
        )
        .await
        .unwrap();
        let err = s
            .put_object(
                "u",
                "bob",
                serde_json::json!({"email": "a@x"}),
                PutOpts::default(),
                vec![("email".to_string(), vec!["a@x".to_string()])],
            )
            .await
            .unwrap_err();
        assert!(matches!(err, MorayError::UniqueConstraint { .. }));

        // Same-key re-put with the same unique value is fine.
        s.put_object(
            "u",
            "alice",
            serde_json::json!({"email": "a@x"}),
            PutOpts::default(),
            vec![("email".to_string(), vec!["a@x".to_string()])],
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn object_lifecycle() {
        let s = MemStore::new();
        s.create_bucket("users", BucketConfig::default()).await.unwrap();
        let put = s
            .put_object(
                "users",
                "alice",
                serde_json::json!({"email": "a@x"}),
                PutOpts::default(),
                Vec::new(),
            )
            .await
            .unwrap();
        let got = s.get_object("users", "alice").await.unwrap();
        assert_eq!(put.etag, got.etag);
        assert_eq!(got.value["email"], "a@x");
        s.delete_object("users", "alice", None).await.unwrap();
        assert!(s.get_object("users", "alice").await.is_err());
    }
}
