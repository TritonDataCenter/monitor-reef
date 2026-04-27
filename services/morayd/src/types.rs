// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Shared types for morayd's storage layer and wire interface.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// A Moray bucket schema. node-moray sends this as the second argument to
/// `createBucket`; we persist it verbatim so that getBucket returns the same
/// shape the client originally supplied.
///
/// See <https://github.com/TritonDataCenter/node-moray/blob/master/docs/man/moray-client.md#morayclientcreatebucket>.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BucketConfig {
    /// Indexed columns. Each entry is `{ "type": "string"|"number"|..., "unique": bool? }`.
    #[serde(default)]
    pub index: serde_json::Map<String, Value>,
    /// Pre- and post-triggers — node-moray uses these for server-side hooks.
    /// We store them but do not execute them; any caller that depends on
    /// server-side triggers gets a NotImplementedError when they fire.
    #[serde(default)]
    pub pre: Vec<Value>,
    #[serde(default)]
    pub post: Vec<Value>,
    /// Top-level options bag (version, trackModification, guaranteeOrder, etc).
    /// node-moray sends this as `options.options.<key>` on the wire — a
    /// discrete sub-object, not flattened.
    #[serde(default)]
    pub options: serde_json::Map<String, Value>,
}

impl Default for BucketConfig {
    fn default() -> Self {
        Self {
            index: serde_json::Map::new(),
            pre: Vec::new(),
            post: Vec::new(),
            options: serde_json::Map::new(),
        }
    }
}

/// Persistent record for a bucket. Held under ("m","b",<name>) in FDB.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Bucket {
    pub name: String,
    pub id: Uuid,
    pub options: BucketConfig,
    pub mtime: DateTime<Utc>,
    /// Columns that need to be walked over by `reindexObjects`. Set when
    /// `updateBucket` adds a new index to a non-empty bucket, cleared
    /// once `reindexObjects` has processed every object. When `None`,
    /// the bucket has no reindex work pending.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reindex_active: Option<ReindexState>,
    /// One-shot flag for the `reindexObjects` response. Upstream Moray
    /// emits `{processed: 0, remaining: 0}` for one call after a reindex
    /// completes, then flips back to the no-remaining shape. We track it
    /// explicitly so we can reproduce that sequence.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub reindex_just_finished: bool,
}

/// Tracks which columns are still being indexed and how many rows remain.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReindexState {
    /// Columns whose values still need to be realized into the secondary
    /// index. Keyed by the bucket version that introduced them — matches
    /// what upstream Moray exposes.
    pub columns: Vec<String>,
    /// Number of objects still to process. Decremented by
    /// `reindexObjects`.
    pub remaining: u64,
}

/// Per-object metadata that node-moray exposes as the object envelope on
/// findObjects / getObject: `{ bucket, key, value, _txn_snap, _count, _etag,
/// _mtime, _id }`. We populate the fields nobody can live without — the
/// rest fall through as `null` when absent.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ObjectMeta {
    pub key: String,
    /// Monotonic per-object id. node-moray uses `_id` as a cursor for
    /// streaming findObjects. We mint a u64 from a per-bucket counter.
    pub id: u64,
    /// ETag is the MD5 of the value's JSON encoding. node-moray compares
    /// etags for conditional put (`etag` in putObject options).
    pub etag: String,
    pub mtime: DateTime<Utc>,
    pub value: Value,
}
