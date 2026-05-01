// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! FDB-backed `MorayStore`.
//!
//! Keyspace (all under the `"m"` top-level prefix — reserved for morayd in
//! the shared cluster):
//!
//! ```text
//! ("m","b",<name>)                    -> Bucket (JSON)
//! ("m","k",<bucket>,<key>)            -> ObjectMeta (JSON)
//! ("m","c",<bucket>,"id")             -> u64 counter (little-endian)
//! ```
//!
//! The JSON encoding for values is serde_json — the space cost over postcard
//! is real but we intentionally accept it so that future debugging (and
//! cross-language clients that want to peek at values) can round-trip
//! values without a schema. Mantad paid its way with postcard; morayd's
//! workload is dominated by schemas the client supplies, so JSON wins.

use foundationdb::options::StreamingMode;
use foundationdb::tuple::pack;
use foundationdb::{FdbBindingError, RangeOption};
use serde_json::Value;
use triton_fdb::FdbClient;

use crate::error::{MorayError, Result};
use crate::store::{BatchOp, BatchOpResult, MorayStore, PutOpts};
use crate::types::{Bucket, BucketConfig, ObjectMeta, ReindexState};

pub struct FdbStore {
    client: FdbClient,
}

impl FdbStore {
    pub fn open(cluster_file: &str) -> Result<Self> {
        let client = FdbClient::open(cluster_file)
            .map_err(|e| MorayError::Storage(anyhow::anyhow!("open: {e}")))?;
        Ok(Self { client })
    }
}

fn to_store_err(e: triton_fdb::Error) -> MorayError {
    MorayError::Storage(anyhow::anyhow!("fdb: {e}"))
}

fn bucket_key(name: &str) -> Vec<u8> {
    pack(&("m", "b", name))
}

fn object_key(bucket: &str, key: &str) -> Vec<u8> {
    pack(&("m", "k", bucket, key))
}

fn counter_key(bucket: &str) -> Vec<u8> {
    pack(&("m", "c", bucket, "id"))
}

/// Secondary index subspace for unique columns:
/// `("m","u",<bucket>,<column>,<value>) -> <owner_key>`.
fn unique_index_key(bucket: &str, column: &str, value: &str) -> Vec<u8> {
    pack(&("m", "u", bucket, column, value))
}

/// Per-object reverse pointer so we can clear a key's unique claims on
/// delete / overwrite: `("m","U",<bucket>,<key>) -> JSON [[col,val],…]`.
fn unique_back_key(bucket: &str, key: &str) -> Vec<u8> {
    pack(&("m", "U", bucket, key))
}

fn range_end(prefix: &[u8]) -> Vec<u8> {
    let mut end = prefix.to_vec();
    end.push(0xff);
    end
}

// Route every value read/write through the framed wire codec, which
// zstd-compresses above a small threshold. That keeps large workflow-
// job bodies (100 KB+ raw) under FDB's 100 KB per-value cap while
// preserving readable JSON for small rows and legacy uncompressed
// data on disk.
fn encode_json<T: serde::Serialize>(v: &T) -> Result<Vec<u8>> {
    crate::wire::encode(v)
}

fn decode_json<T: serde::de::DeserializeOwned>(b: &[u8]) -> Result<T> {
    crate::wire::decode(b)
}

// FDB-closure-safe wrappers for use inside `db.run(...)` — the closure's
// Result type is `FdbBindingError`, not the crate-wide `MorayError`.
fn wenc<T: serde::Serialize>(v: &T) -> std::result::Result<Vec<u8>, FdbBindingError> {
    crate::wire::encode(v)
        .map_err(|e| FdbBindingError::new_custom_error(format!("encode: {e}").into()))
}

fn wdec<T: serde::de::DeserializeOwned>(b: &[u8]) -> std::result::Result<T, FdbBindingError> {
    crate::wire::decode(b)
        .map_err(|e| FdbBindingError::new_custom_error(format!("decode: {e}").into()))
}

fn etag_of(v: &Value) -> String {
    // Cheap, stable, good enough for optimistic-concurrency compare: xxhash64
    // would be nicer, but avoiding another dep for v1. Hash the JSON bytes.
    let bytes = serde_json::to_vec(v).unwrap_or_default();
    let mut h: u64 = 0xcbf29ce484222325;
    for b in &bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

impl MorayStore for FdbStore {
    async fn create_bucket(&self, name: &str, config: BucketConfig) -> Result<Bucket> {
        let client = self.client.clone();
        let name = name.to_string();
        let bk = bucket_key(&name);

        let now = chrono::Utc::now();
        let bucket = Bucket {
            name: name.clone(),
            id: uuid::Uuid::new_v4(),
            options: config,
            mtime: now,
            reindex_active: None,
            reindex_just_finished: false,
        };
        let value = encode_json(&bucket)?;

        client.transact("create_bucket", |trx, _| {
            let bk = bk.clone();
            let value = value.clone();
            let name = name.clone();
            async move {
                if trx.get(&bk, false).await?.is_some() {
                    // Signal existing-bucket with a distinct inner error;
                    // FdbBindingError can carry a payload via custom display.
                    return Err(FdbBindingError::new_custom_error(
                        format!("bucket_exists:{name}").into(),
                    ));
                }
                trx.set(&bk, &value);
                Ok::<_, FdbBindingError>(())
            }
        })
        .await
        .map_err(|e| match e.to_string().as_str() {
            s if s.contains("bucket_exists:") => {
                MorayError::BucketAlreadyExists(name.clone())
            }
            _ => to_store_err(e),
        })?;

        Ok(bucket)
    }

    async fn update_bucket(&self, name: &str, config: BucketConfig) -> Result<Bucket> {
        let client = self.client.clone();
        let bk = bucket_key(name);
        let obj_prefix = pack(&("m", "k", name));
        let obj_end = range_end(&obj_prefix);
        let name_s = name.to_string();
        let now = chrono::Utc::now();

        let bucket = client
            .transact("update_bucket", |trx, _| {
                let bk = bk.clone();
                let obj_prefix = obj_prefix.clone();
                let obj_end = obj_end.clone();
                let config = config.clone();
                let name_s = name_s.clone();
                async move {
                    let existing = trx.get(&bk, false).await?;
                    let Some(existing) = existing else {
                        return Err(FdbBindingError::new_custom_error(
                            format!("bucket_missing:{name_s}").into(),
                        ));
                    };
                    let mut current: Bucket = wdec(&existing)?;

                    let new_columns: Vec<String> = config
                        .index
                        .keys()
                        .filter(|k| !current.options.index.contains_key(*k))
                        .cloned()
                        .collect();

                    // Count existing objects so reindexObjects has an
                    // accurate "remaining" to count down from.
                    let mut range = foundationdb::RangeOption::from((
                        obj_prefix.clone(),
                        obj_end.clone(),
                    ));
                    range.mode = StreamingMode::WantAll;
                    range.limit = Some(100_000);
                    let kvs = trx.get_range(&range, 1, false).await?;
                    let object_count = kvs.iter().count() as u64;

                    current.reindex_active = if new_columns.is_empty()
                        || object_count == 0
                    {
                        None
                    } else {
                        Some(ReindexState {
                            columns: new_columns,
                            remaining: object_count,
                        })
                    };
                    current.reindex_just_finished = false;

                    current.options = config;
                    current.mtime = now;
                    let enc = wenc(&current)?;
                    trx.set(&bk, &enc);
                    Ok::<_, FdbBindingError>(current)
                }
            })
            .await
            .map_err(|e| match e.to_string().as_str() {
                s if s.contains("bucket_missing:") => {
                    MorayError::BucketNotFound(name.to_string())
                }
                _ => to_store_err(e),
            })?;
        Ok(bucket)
    }

    async fn clear_reindex_just_finished(&self, bucket: &str) -> Result<()> {
        let client = self.client.clone();
        let bk = bucket_key(bucket);
        client.transact("clear_reindex_just_finished", |trx, _| {
            let bk = bk.clone();
            async move {
                let existing = trx.get(&bk, false).await?;
                let Some(existing) = existing else {
                    return Ok::<_, FdbBindingError>(());
                };
                let mut b: Bucket = wdec(&existing)?;
                if b.reindex_just_finished {
                    b.reindex_just_finished = false;
                    let enc = wenc(&b)?;
                    trx.set(&bk, &enc);
                }
                Ok(())
            }
        })
        .await
        .map_err(to_store_err)
    }

    async fn reindex_step(&self, bucket: &str, count: u64) -> Result<(u64, u64)> {
        let client = self.client.clone();
        let bk = bucket_key(bucket);
        let name_s = bucket.to_string();
        client.transact("reindex_step", |trx, _| {
            let bk = bk.clone();
            let name_s = name_s.clone();
            async move {
                let existing = trx.get(&bk, false).await?;
                let Some(existing) = existing else {
                    return Err(FdbBindingError::new_custom_error(
                        format!("bucket_missing:{name_s}").into(),
                    ));
                };
                let mut b: Bucket = wdec(&existing)?;
                let Some(state) = b.reindex_active.as_mut() else {
                    return Ok::<_, FdbBindingError>((0u64, 0u64));
                };
                let processed = count.min(state.remaining);
                state.remaining -= processed;
                if state.remaining == 0 {
                    b.reindex_active = None;
                    b.reindex_just_finished = true;
                }
                let remaining = b.reindex_active.as_ref().map_or(0, |s| s.remaining);
                let enc = wenc(&b)?;
                trx.set(&bk, &enc);
                Ok::<_, FdbBindingError>((processed, remaining))
            }
        })
        .await
        .map_err(|e| match e.to_string().as_str() {
            s if s.contains("bucket_missing:") => {
                MorayError::BucketNotFound(bucket.to_string())
            }
            _ => to_store_err(e),
        })
    }

    async fn get_bucket(&self, name: &str) -> Result<Bucket> {
        let client = self.client.clone();
        let bk = bucket_key(name);
        let raw = client
            .transact("get_bucket", |trx, _| {
                let bk = bk.clone();
                async move {
                    Ok::<_, FdbBindingError>(trx.get(&bk, false).await?.map(|s| s.to_vec()))
                }
            })
            .await
            .map_err(to_store_err)?
            .ok_or_else(|| MorayError::BucketNotFound(name.into()))?;
        decode_json(&raw)
    }

    async fn delete_bucket(&self, name: &str) -> Result<()> {
        let client = self.client.clone();
        let bk = bucket_key(name);
        let obj_prefix = pack(&("m", "k", name));
        let cnt = counter_key(name);
        client.transact("delete_bucket", |trx, _| {
            let bk = bk.clone();
            let obj_prefix = obj_prefix.clone();
            let cnt = cnt.clone();
            async move {
                trx.clear(&bk);
                trx.clear(&cnt);
                trx.clear_range(&obj_prefix, &range_end(&obj_prefix));
                Ok::<_, FdbBindingError>(())
            }
        })
        .await
        .map_err(to_store_err)
    }

    async fn list_buckets(&self) -> Result<Vec<Bucket>> {
        let client = self.client.clone();
        let prefix = pack(&("m", "b"));
        let mut range_opt = RangeOption::from((prefix.clone(), range_end(&prefix)));
        range_opt.mode = StreamingMode::WantAll;
        range_opt.limit = Some(10_000);

        let kvs: Vec<Vec<u8>> = client
            .transact("list_buckets", |trx, _| {
                let r = range_opt.clone();
                async move {
                    let kvs = trx.get_range(&r, 1, false).await?;
                    Ok::<_, FdbBindingError>(
                        kvs.iter().map(|kv| kv.value().to_vec()).collect::<Vec<_>>(),
                    )
                }
            })
            .await
            .map_err(to_store_err)?;

        let mut out = Vec::with_capacity(kvs.len());
        for raw in kvs {
            out.push(decode_json::<Bucket>(&raw)?);
        }
        Ok(out)
    }

    async fn put_object(
        &self,
        bucket: &str,
        key: &str,
        value: Value,
        opts: PutOpts,
        unique_cols: Vec<(String, Vec<String>)>,
    ) -> Result<ObjectMeta> {
        let client = self.client.clone();
        let bk = bucket_key(bucket);
        let ok = object_key(bucket, key);
        let ck = counter_key(bucket);

        let etag = etag_of(&value);
        let mtime = chrono::Utc::now();

        let back_key = unique_back_key(bucket, key);
        let expected_etag_for_err = opts.etag.clone().unwrap_or_default();

        let meta = client
            .transact("put_object", |trx, _| {
                let bk = bk.clone();
                let ok = ok.clone();
                let ck = ck.clone();
                let back_key = back_key.clone();
                let value = value.clone();
                let etag = etag.clone();
                let key = key.to_string();
                let bucket_name = bucket.to_string();
                let expected_etag = opts.etag.clone();
                let unique_cols = unique_cols.clone();
                async move {
                    if trx.get(&bk, false).await?.is_none() {
                        return Err(FdbBindingError::new_custom_error(
                            format!("bucket_missing:{bucket_name}").into(),
                        ));
                    }

                    // Unique-index enforcement:
                    //  1. Clear any prior claims owned by this key.
                    //  2. For every requested (col,value), check that
                    //     no OTHER key owns that slot.
                    //  3. Write the new claims + reverse pointer.
                    if let Some(prior) = trx.get(&back_key, false).await? {
                        let prior: Vec<(String, String)> =
                            serde_json::from_slice(&prior).unwrap_or_default();
                        for (c, v) in prior {
                            trx.clear(&unique_index_key(&bucket_name, &c, &v));
                        }
                    }
                    for (col, values) in &unique_cols {
                        for v in values {
                            let idx_key = unique_index_key(&bucket_name, col, v);
                            if let Some(owner) = trx.get(&idx_key, false).await?
                                && owner.as_ref() != key.as_bytes()
                            {
                                return Err(FdbBindingError::new_custom_error(
                                    format!(
                                        "unique_violation:{bucket_name}:{col}:{v}"
                                    )
                                    .into(),
                                ));
                            }
                            trx.set(&idx_key, key.as_bytes());
                        }
                    }
                    let pairs: Vec<(String, String)> = unique_cols
                        .iter()
                        .flat_map(|(c, vs)| {
                            vs.iter().map(move |v| (c.clone(), v.clone()))
                        })
                        .collect();
                    if pairs.is_empty() {
                        trx.clear(&back_key);
                    } else {
                        let back_val =
                            serde_json::to_vec(&pairs).map_err(|e| {
                                FdbBindingError::new_custom_error(
                                    format!("encode: {e}").into(),
                                )
                            })?;
                        trx.set(&back_key, &back_val);
                    }

                    // Conditional put: read current object first if the
                    // caller specified an etag guard. Both `""` and
                    // `"null"` mean "must not exist"; other strings
                    // match the current etag.
                    if let Some(want) = expected_etag.as_deref() {
                        let current_raw = trx.get(&ok, false).await?;
                        let must_be_absent = want.is_empty() || want == "null";
                        match (must_be_absent, current_raw) {
                            (true, None) => {}
                            (true, Some(_)) => {
                                return Err(FdbBindingError::new_custom_error(
                                    format!("etag_conflict:{bucket_name}:{key}").into(),
                                ));
                            }
                            (false, Some(raw)) => {
                                let cur: ObjectMeta = wdec(&raw)?;
                                if cur.etag != want {
                                    return Err(FdbBindingError::new_custom_error(
                                        format!(
                                            "etag_conflict:{bucket_name}:{key}"
                                        )
                                        .into(),
                                    ));
                                }
                            }
                            (false, None) => {
                                return Err(FdbBindingError::new_custom_error(
                                    format!("etag_conflict:{bucket_name}:{key}")
                                        .into(),
                                ));
                            }
                        }
                    }

                    // Bump per-bucket counter. Stored little-endian u64 to
                    // match FDB's atomic-add contract (we use plain rmw
                    // here — the closure retries on conflict, which is
                    // what we want for monotonicity).
                    let current = trx
                        .get(&ck, false)
                        .await?
                        .map(|s| {
                            let mut a = [0u8; 8];
                            let src = s.as_ref();
                            let n = src.len().min(8);
                            a[..n].copy_from_slice(&src[..n]);
                            u64::from_le_bytes(a)
                        })
                        .unwrap_or(0);
                    let next = current + 1;
                    trx.set(&ck, &next.to_le_bytes());

                    let meta = ObjectMeta {
                        key: key.clone(),
                        id: next,
                        etag,
                        mtime,
                        value,
                    };
                    let mv = wenc(&meta)?;
                    trx.set(&ok, &mv);
                    Ok::<_, FdbBindingError>(meta)
                }
            })
            .await
            .map_err(|e| match e.to_string().as_str() {
                s if s.contains("bucket_missing:") => {
                    MorayError::BucketNotFound(bucket.to_string())
                }
                s if s.contains("etag_conflict:") => MorayError::EtagConflict {
                    bucket: bucket.to_string(),
                    key: key.to_string(),
                    expected: expected_etag_for_err.clone(),
                    actual: String::new(),
                },
                s if s.starts_with("unique_violation:") => {
                    // "unique_violation:<bucket>:<col>:<val>"
                    let parts: Vec<&str> =
                        s.splitn(4, ':').collect::<Vec<_>>();
                    let (col, val) = match parts.as_slice() {
                        [_, _, c, v] => ((*c).to_string(), (*v).to_string()),
                        _ => (String::new(), String::new()),
                    };
                    MorayError::UniqueConstraint {
                        bucket: bucket.to_string(),
                        column: col,
                        value: val,
                    }
                }
                _ => to_store_err(e),
            })?;

        Ok(meta)
    }

    async fn get_object(&self, bucket: &str, key: &str) -> Result<ObjectMeta> {
        let client = self.client.clone();
        let bk = bucket_key(bucket);
        let ok = object_key(bucket, key);
        let (bucket_exists, raw) = client
            .transact("get_object", |trx, _| {
                let bk = bk.clone();
                let ok = ok.clone();
                async move {
                    let b = trx.get(&bk, false).await?.is_some();
                    let o = trx.get(&ok, false).await?.map(|s| s.to_vec());
                    Ok::<_, FdbBindingError>((b, o))
                }
            })
            .await
            .map_err(to_store_err)?;
        if !bucket_exists {
            return Err(MorayError::BucketNotFound(bucket.into()));
        }
        let raw = raw.ok_or_else(|| MorayError::ObjectNotFound {
            bucket: bucket.into(),
            key: key.into(),
        })?;
        decode_json(&raw)
    }

    async fn scan_objects(
        &self,
        bucket: &str,
        limit: usize,
    ) -> Result<Vec<ObjectMeta>> {
        let client = self.client.clone();
        let bk = bucket_key(bucket);
        let prefix = pack(&("m", "k", bucket));
        let end = range_end(&prefix);

        // Bucket existence check is a cheap point read; do it up front so
        // we fail early without consuming a scan transaction.
        let exists = client
            .transact("scan_objects", |trx, _| {
                let bk = bk.clone();
                async move {
                    Ok::<_, FdbBindingError>(trx.get(&bk, false).await?.is_some())
                }
            })
            .await
            .map_err(to_store_err)?;
        if !exists {
            return Err(MorayError::BucketNotFound(bucket.into()));
        }

        // Page the scan across multiple transactions. A single FDB
        // transaction caps at 10 MB read and 5 s duration, so a
        // whole-bucket WantAll can silently truncate (the binding
        // returns only one chunk and sets `more=true`). We advance a
        // begin cursor past the last key of each chunk until the range
        // is drained.
        const PAGE_SIZE: usize = 500;
        let mut begin = prefix.clone();
        let mut raws: Vec<Vec<u8>> = Vec::new();

        while raws.len() < limit {
            let page_limit = (limit - raws.len()).min(PAGE_SIZE);
            let range_opt = {
                let mut r = RangeOption::from((begin.clone(), end.clone()));
                r.mode = StreamingMode::WantAll;
                r.limit = Some(page_limit);
                r
            };

            let chunk: Vec<(Vec<u8>, Vec<u8>)> = client
                .transact("scan_objects.2", |trx, _| {
                    let r = range_opt.clone();
                    async move {
                        let kvs = trx.get_range(&r, 1, false).await?;
                        let out: Vec<(Vec<u8>, Vec<u8>)> = kvs
                            .iter()
                            .map(|kv| (kv.key().to_vec(), kv.value().to_vec()))
                            .collect();
                        Ok::<_, FdbBindingError>(out)
                    }
                })
                .await
                .map_err(to_store_err)?;

            if chunk.is_empty() {
                break;
            }

            // Next page starts at <last_key>\x00 — the smallest key
            // strictly greater than last_key, so we don't re-read it.
            // We cannot use chunk.len() < page_limit as an "end of
            // range" signal: FDB's WantAll mode truncates each RPC at
            // ~2.5 MB of data regardless of the row limit, so a short
            // chunk just means "byte-budget hit", not "no more rows".
            let mut next_begin = chunk.last().unwrap().0.clone();
            next_begin.push(0x00);

            for (_, v) in chunk {
                raws.push(v);
                if raws.len() >= limit {
                    break;
                }
            }
            begin = next_begin;
        }

        let mut out: Vec<ObjectMeta> = Vec::with_capacity(raws.len());
        for raw in raws {
            out.push(decode_json(&raw)?);
        }
        out.sort_by_key(|o| o.id);
        Ok(out)
    }

    async fn delete_object(
        &self,
        bucket: &str,
        key: &str,
        expected_etag: Option<String>,
    ) -> Result<()> {
        let client = self.client.clone();
        let bk = bucket_key(bucket);
        let ok = object_key(bucket, key);
        let back_key = unique_back_key(bucket, key);
        let bucket_name = bucket.to_string();
        let expected_etag_for_err = expected_etag.clone().unwrap_or_default();
        let (bucket_exists, object_exists) = client
            .transact("delete_object", |trx, _| {
                let bk = bk.clone();
                let ok = ok.clone();
                let back_key = back_key.clone();
                let bucket_name = bucket_name.clone();
                let want_etag = expected_etag.clone();
                let key_s = key.to_string();
                async move {
                    let b = trx.get(&bk, false).await?.is_some();
                    let o_raw = trx.get(&ok, false).await?;
                    let o = o_raw.is_some();
                    if b && o {
                        if let Some(want) = want_etag.as_deref() {
                            let bytes = match o_raw.as_ref() {
                                Some(r) => r,
                                None => {
                                    return Err(FdbBindingError::new_custom_error(
                                        format!(
                                            "object_missing:{bucket_name}:{key_s}"
                                        )
                                        .into(),
                                    ))
                                }
                            };
                            let meta: ObjectMeta = wdec(bytes)?;
                            if meta.etag != want {
                                return Err(FdbBindingError::new_custom_error(
                                    format!(
                                        "etag_conflict:{bucket_name}:{key_s}"
                                    )
                                    .into(),
                                ));
                            }
                        }
                        trx.clear(&ok);
                        if let Some(prior) = trx.get(&back_key, false).await? {
                            let prior: Vec<(String, String)> =
                                serde_json::from_slice(&prior).unwrap_or_default();
                            for (c, v) in prior {
                                trx.clear(&unique_index_key(&bucket_name, &c, &v));
                            }
                            trx.clear(&back_key);
                        }
                    }
                    Ok::<_, FdbBindingError>((b, o))
                }
            })
            .await
            .map_err(|e| match e.to_string().as_str() {
                s if s.contains("etag_conflict:") => MorayError::EtagConflict {
                    bucket: bucket.to_string(),
                    key: key.to_string(),
                    expected: expected_etag_for_err.clone(),
                    actual: String::new(),
                },
                s if s.contains("object_missing:") => MorayError::ObjectNotFound {
                    bucket: bucket.to_string(),
                    key: key.to_string(),
                },
                _ => to_store_err(e),
            })?;
        if !bucket_exists {
            return Err(MorayError::BucketNotFound(bucket.into()));
        }
        if !object_exists {
            return Err(MorayError::ObjectNotFound {
                bucket: bucket.into(),
                key: key.into(),
            });
        }
        Ok(())
    }

    async fn apply_batch(&self, ops: Vec<BatchOp>) -> Result<Vec<BatchOpResult>> {
        let client = self.client.clone();
        // Pre-compute the etag/mtime per Put so the closure (which may
        // be retried by FDB on conflict) sees the same values across
        // attempts. We deliberately do NOT bump mtime per retry — the
        // caller's intended write should be deterministic.
        let now = chrono::Utc::now();
        // Snapshot the input so the per-retry closure can clone it.
        let ops_snapshot = ops.clone();

        let outcomes = client
            .transact("apply_batch", move |trx, _| {
                let ops = ops_snapshot.clone();
                async move {
                    let mut results: Vec<BatchOpResult> = Vec::with_capacity(ops.len());
                    for op in ops {
                        match op {
                            BatchOp::Put {
                                bucket,
                                key,
                                value,
                                opts,
                                unique_cols,
                            } => {
                                let bk = bucket_key(&bucket);
                                let ok = object_key(&bucket, &key);
                                let ck = counter_key(&bucket);
                                let back = unique_back_key(&bucket, &key);

                                if trx.get(&bk, false).await?.is_none() {
                                    return Err(FdbBindingError::new_custom_error(
                                        format!("bucket_missing:{bucket}").into(),
                                    ));
                                }

                                // Clear prior unique-index claims for
                                // this key, then set the new ones,
                                // refusing to overwrite a slot owned by
                                // a different key.
                                if let Some(prior) = trx.get(&back, false).await? {
                                    let prior: Vec<(String, String)> =
                                        serde_json::from_slice(&prior).unwrap_or_default();
                                    for (c, v) in prior {
                                        trx.clear(&unique_index_key(&bucket, &c, &v));
                                    }
                                }
                                for (col, values) in &unique_cols {
                                    for v in values {
                                        let idx_key =
                                            unique_index_key(&bucket, col, v);
                                        if let Some(owner) =
                                            trx.get(&idx_key, false).await?
                                            && owner.as_ref() != key.as_bytes()
                                        {
                                            return Err(
                                                FdbBindingError::new_custom_error(
                                                    format!(
                                                        "unique_violation:{bucket}:{col}:{v}"
                                                    )
                                                    .into(),
                                                ),
                                            );
                                        }
                                        trx.set(&idx_key, key.as_bytes());
                                    }
                                }
                                let pairs: Vec<(String, String)> = unique_cols
                                    .iter()
                                    .flat_map(|(c, vs)| {
                                        vs.iter().map(move |v| (c.clone(), v.clone()))
                                    })
                                    .collect();
                                if pairs.is_empty() {
                                    trx.clear(&back);
                                } else {
                                    let bv = serde_json::to_vec(&pairs).map_err(|e| {
                                        FdbBindingError::new_custom_error(
                                            format!("encode: {e}").into(),
                                        )
                                    })?;
                                    trx.set(&back, &bv);
                                }

                                // Etag guard.
                                if let Some(want) = opts.etag.as_deref() {
                                    let cur_raw = trx.get(&ok, false).await?;
                                    let must_be_absent = want.is_empty() || want == "null";
                                    match (must_be_absent, cur_raw) {
                                        (true, None) => {}
                                        (true, Some(_)) => {
                                            return Err(FdbBindingError::new_custom_error(
                                                format!(
                                                    "etag_conflict:{bucket}:{key}"
                                                )
                                                .into(),
                                            ));
                                        }
                                        (false, Some(raw)) => {
                                            let cur: ObjectMeta = wdec(&raw)?;
                                            if cur.etag != want {
                                                return Err(
                                                    FdbBindingError::new_custom_error(
                                                        format!(
                                                            "etag_conflict:{bucket}:{key}"
                                                        )
                                                        .into(),
                                                    ),
                                                );
                                            }
                                        }
                                        (false, None) => {
                                            return Err(FdbBindingError::new_custom_error(
                                                format!(
                                                    "etag_conflict:{bucket}:{key}"
                                                )
                                                .into(),
                                            ));
                                        }
                                    }
                                }

                                // Bump per-bucket counter and write.
                                let cur_id = trx
                                    .get(&ck, false)
                                    .await?
                                    .map(|s| {
                                        let mut a = [0u8; 8];
                                        let src = s.as_ref();
                                        let n = src.len().min(8);
                                        a[..n].copy_from_slice(&src[..n]);
                                        u64::from_le_bytes(a)
                                    })
                                    .unwrap_or(0);
                                let next = cur_id + 1;
                                trx.set(&ck, &next.to_le_bytes());

                                let etag = etag_of(&value);
                                let meta = ObjectMeta {
                                    key: key.clone(),
                                    id: next,
                                    etag: etag.clone(),
                                    mtime: now,
                                    value,
                                };
                                let mv = wenc(&meta)?;
                                trx.set(&ok, &mv);
                                results.push(BatchOpResult {
                                    bucket: bucket.clone(),
                                    key: Some(key),
                                    etag: Some(etag),
                                    id: Some(next),
                                    count: None,
                                });
                            }

                            BatchOp::Delete {
                                bucket,
                                key,
                                expected_etag,
                            } => {
                                let bk = bucket_key(&bucket);
                                let ok = object_key(&bucket, &key);
                                let back = unique_back_key(&bucket, &key);

                                if trx.get(&bk, false).await?.is_none() {
                                    return Err(FdbBindingError::new_custom_error(
                                        format!("bucket_missing:{bucket}").into(),
                                    ));
                                }
                                let raw = trx.get(&ok, false).await?;
                                if let Some(ref r) = raw
                                    && let Some(want) = expected_etag.as_deref()
                                {
                                    let cur: ObjectMeta = wdec(r)?;
                                    if cur.etag != want {
                                        return Err(FdbBindingError::new_custom_error(
                                            format!("etag_conflict:{bucket}:{key}")
                                                .into(),
                                        ));
                                    }
                                }
                                if raw.is_some() {
                                    trx.clear(&ok);
                                    if let Some(prior) = trx.get(&back, false).await? {
                                        let prior: Vec<(String, String)> =
                                            serde_json::from_slice(&prior)
                                                .unwrap_or_default();
                                        for (c, v) in prior {
                                            trx.clear(&unique_index_key(&bucket, &c, &v));
                                        }
                                        trx.clear(&back);
                                    }
                                }
                                results.push(BatchOpResult {
                                    bucket: bucket.clone(),
                                    key: Some(key),
                                    etag: None,
                                    id: None,
                                    count: None,
                                });
                            }

                            BatchOp::UpdateRows { bucket, rows } => {
                                let count = rows.len();
                                for (key, value, unique_cols) in rows {
                                    // Reuse the put logic — slightly
                                    // duplicated with the Put arm above
                                    // but inlined to stay inside this
                                    // single closure.
                                    let bk = bucket_key(&bucket);
                                    let ok = object_key(&bucket, &key);
                                    let ck = counter_key(&bucket);
                                    let back = unique_back_key(&bucket, &key);

                                    if trx.get(&bk, false).await?.is_none() {
                                        return Err(FdbBindingError::new_custom_error(
                                            format!("bucket_missing:{bucket}").into(),
                                        ));
                                    }
                                    if let Some(prior) = trx.get(&back, false).await? {
                                        let prior: Vec<(String, String)> =
                                            serde_json::from_slice(&prior)
                                                .unwrap_or_default();
                                        for (c, v) in prior {
                                            trx.clear(&unique_index_key(&bucket, &c, &v));
                                        }
                                    }
                                    for (col, values) in &unique_cols {
                                        for v in values {
                                            let idx_key =
                                                unique_index_key(&bucket, col, v);
                                            if let Some(owner) =
                                                trx.get(&idx_key, false).await?
                                                && owner.as_ref() != key.as_bytes()
                                            {
                                                return Err(
                                                    FdbBindingError::new_custom_error(
                                                        format!(
                                                            "unique_violation:{bucket}:{col}:{v}"
                                                        )
                                                        .into(),
                                                    ),
                                                );
                                            }
                                            trx.set(&idx_key, key.as_bytes());
                                        }
                                    }
                                    let pairs: Vec<(String, String)> = unique_cols
                                        .iter()
                                        .flat_map(|(c, vs)| {
                                            vs.iter()
                                                .map(move |v| (c.clone(), v.clone()))
                                        })
                                        .collect();
                                    if pairs.is_empty() {
                                        trx.clear(&back);
                                    } else {
                                        let bv =
                                            serde_json::to_vec(&pairs).map_err(|e| {
                                                FdbBindingError::new_custom_error(
                                                    format!("encode: {e}").into(),
                                                )
                                            })?;
                                        trx.set(&back, &bv);
                                    }
                                    let cur_id = trx
                                        .get(&ck, false)
                                        .await?
                                        .map(|s| {
                                            let mut a = [0u8; 8];
                                            let src = s.as_ref();
                                            let n = src.len().min(8);
                                            a[..n].copy_from_slice(&src[..n]);
                                            u64::from_le_bytes(a)
                                        })
                                        .unwrap_or(0);
                                    let next = cur_id + 1;
                                    trx.set(&ck, &next.to_le_bytes());
                                    let etag = etag_of(&value);
                                    let meta = ObjectMeta {
                                        key: key.clone(),
                                        id: next,
                                        etag,
                                        mtime: now,
                                        value,
                                    };
                                    let mv = wenc(&meta)?;
                                    trx.set(&ok, &mv);
                                }
                                results.push(BatchOpResult {
                                    bucket,
                                    key: None,
                                    etag: None,
                                    id: None,
                                    count: Some(count),
                                });
                            }

                            BatchOp::DeleteKeys { bucket, keys } => {
                                let count = keys.len();
                                for key in keys {
                                    let ok = object_key(&bucket, &key);
                                    let back = unique_back_key(&bucket, &key);
                                    if trx.get(&ok, false).await?.is_some() {
                                        trx.clear(&ok);
                                        if let Some(prior) =
                                            trx.get(&back, false).await?
                                        {
                                            let prior: Vec<(String, String)> =
                                                serde_json::from_slice(&prior)
                                                    .unwrap_or_default();
                                            for (c, v) in prior {
                                                trx.clear(&unique_index_key(&bucket, &c, &v));
                                            }
                                            trx.clear(&back);
                                        }
                                    }
                                }
                                results.push(BatchOpResult {
                                    bucket,
                                    key: None,
                                    etag: None,
                                    id: None,
                                    count: Some(count),
                                });
                            }
                        }
                    }
                    Ok::<_, FdbBindingError>(results)
                }
            })
            .await
            .map_err(|e| match e.to_string().as_str() {
                s if s.contains("bucket_missing:") => {
                    let bk = s.split(':').nth(1).unwrap_or("").to_string();
                    MorayError::BucketNotFound(bk)
                }
                s if s.contains("etag_conflict:") => {
                    let parts: Vec<&str> = s.splitn(3, ':').collect();
                    let (bk, k) = match parts.as_slice() {
                        [_, b, k] => ((*b).to_string(), (*k).to_string()),
                        _ => (String::new(), String::new()),
                    };
                    MorayError::EtagConflict {
                        bucket: bk,
                        key: k,
                        expected: String::new(),
                        actual: String::new(),
                    }
                }
                s if s.starts_with("unique_violation:") => {
                    let parts: Vec<&str> = s.splitn(4, ':').collect();
                    let (bk, col, val) = match parts.as_slice() {
                        [_, b, c, v] => (
                            (*b).to_string(),
                            (*c).to_string(),
                            (*v).to_string(),
                        ),
                        _ => (String::new(), String::new(), String::new()),
                    };
                    MorayError::UniqueConstraint {
                        bucket: bk,
                        column: col,
                        value: val,
                    }
                }
                _ => to_store_err(e),
            })?;

        // Force `ops` to be moved into the closure-prep snapshot above.
        let _ = ops;
        Ok(outcomes)
    }
}
