// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

use serde_json::Value;
use thiserror::Error;

pub type Result<T> = std::result::Result<T, MorayError>;

/// Format a list of attribute names the way upstream Moray does:
/// `[ 'foo', 'bar' ]` for non-empty lists, `[]` for empty.
fn fmt_attrs(v: &[String]) -> String {
    if v.is_empty() {
        return "[]".into();
    }
    let inner = v
        .iter()
        .map(|s| format!("'{s}'"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("[ {inner} ]")
}

/// Errors morayd returns. The `name` field is what we put on the wire for a
/// node-moray client — node-moray's error decoder uses it to pick a JS class
/// (BucketNotFoundError, ObjectNotFoundError, etc.). Keep the names
/// byte-identical to what node-moray expects.
#[derive(Debug, Error)]
pub enum MorayError {
    #[error("bucket not found: {0}")]
    BucketNotFound(String),

    #[error("bucket already exists: {0}")]
    BucketAlreadyExists(String),

    #[error("object not found: bucket={bucket} key={key}")]
    ObjectNotFound { bucket: String, key: String },

    #[error("invariant violation: {0}")]
    Invariant(String),

    #[error("invalid argument: {0}")]
    InvalidArg(String),

    /// Moray uses `InvocationError` for RPC-level input errors (missing
    /// required args, wrong types at the message boundary) — distinct from
    /// application-level validation failures. The message is emitted
    /// verbatim; the error name classifies it.
    #[error("{0}")]
    Invocation(String),

    /// Raised when a bucket name violates Moray's naming rules.
    #[error("{0} is not a valid bucket name")]
    InvalidBucketName(String),

    /// Raised when a bucket config is structurally wrong (bad index, bad
    /// pre/post shape, unknown index type, …).
    #[error("{0}")]
    InvalidBucketConfig(String),

    /// Raised when a `pre`/`post` trigger entry is not parseable as a
    /// function-string. Moray uses `NotFunctionError` for this.
    #[error("{0}")]
    NotFunction(String),

    /// A findObjects/updateObjects/deleteMany filter failed to parse. Moray
    /// reports this as `InvalidQueryError` (distinct from the more generic
    /// `InvalidArgumentError`) so callers can tell the two apart.
    #[error("invalid query: {0}")]
    InvalidQuery(String),

    #[error("etag conflict: bucket={bucket} key={key}")]
    EtagConflict {
        bucket: String,
        key: String,
        /// What the caller asserted (`"null"` for "must not exist" or
        /// the etag string they passed). Ships on the wire as
        /// `context.expected` so node-moray's test suite can inspect.
        expected: String,
        /// The current etag on the server — `"null"` when the key is
        /// absent.
        actual: String,
    },

    #[error("unique constraint violation on bucket={bucket} column={column}")]
    UniqueConstraint {
        bucket: String,
        column: String,
        value: String,
    },

    #[error("{} does not have indexes that support {}. Reindexing fields: {}. Unindexed fields: {}",
        bucket, filter, fmt_attrs(reindexing), fmt_attrs(unindexed))]
    NotIndexed {
        bucket: String,
        filter: String,
        /// Attributes referenced in the filter that exist in the schema
        /// but are still being reindexed.
        reindexing: Vec<String>,
        /// Attributes referenced in the filter that aren't in the
        /// schema at all.
        unindexed: Vec<String>,
    },

    /// Raised when `updateObjects` is called with a `null` value for any
    /// field. Moray currently does not support nullable updates.
    #[error("null values are not currently supported for updateObjects")]
    NotNullable { field: String },

    /// `updateObjects` called with an empty `fields` object — upstream
    /// classes this as `FieldUpdateError` (nothing to update).
    #[error("updateObjects must specify at least one field")]
    EmptyFieldUpdate,

    #[error("not implemented: {0}")]
    NotImplemented(String),

    /// RPC name the server doesn't know — matches upstream's
    /// `FastError: unsupported RPC method: "<name>"` shape.
    #[error("unsupported RPC method: \"{0}\"")]
    UnsupportedRpc(String),

    #[error("storage: {0}")]
    Storage(#[source] anyhow::Error),

    #[error(transparent)]
    Serde(#[from] serde_json::Error),

    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl MorayError {
    /// The node-moray error class name for this error.
    pub fn wire_name(&self) -> &'static str {
        match self {
            MorayError::BucketNotFound(_) => "BucketNotFoundError",
            MorayError::BucketAlreadyExists(_) => "BucketConflictError",
            MorayError::ObjectNotFound { .. } => "ObjectNotFoundError",
            MorayError::EtagConflict { .. } => "EtagConflictError",
            MorayError::UniqueConstraint { .. } => "UniqueAttributeError",
            MorayError::NotIndexed { .. } => "NotIndexedError",
            MorayError::NotNullable { .. } => "NotNullableError",
            MorayError::EmptyFieldUpdate => "FieldUpdateError",
            MorayError::InvalidArg(_) => "InvalidArgumentError",
            MorayError::InvalidQuery(_) => "InvalidQueryError",
            MorayError::Invocation(_) => "InvocationError",
            MorayError::InvalidBucketName(_) => "InvalidBucketNameError",
            MorayError::InvalidBucketConfig(_) => "InvalidBucketConfigError",
            MorayError::NotFunction(_) => "NotFunctionError",
            MorayError::NotImplemented(_) => "NotImplementedError",
            MorayError::UnsupportedRpc(_) => "FastError",
            MorayError::Invariant(_) => "InternalError",
            MorayError::Storage(_) => "InternalError",
            MorayError::Serde(_) => "InvalidArgumentError",
            MorayError::Io(_) => "InternalError",
        }
    }

    /// JSON payload placed into a fast-protocol error response. Shape matches
    /// what node-moray's client parses: `{ name, message, context? }`.
    pub fn to_wire(&self) -> Value {
        let mut v = serde_json::json!({
            "name": self.wire_name(),
            "message": self.to_string(),
        });
        if let Some(ctx) = self.context() {
            v["context"] = ctx;
        }
        v
    }

    /// Error-specific "context" object. Only emitted for errors that
    /// carry structured side-data that node-moray's tests inspect —
    /// everything else returns None.
    fn context(&self) -> Option<Value> {
        match self {
            MorayError::EtagConflict { bucket, key, expected, actual } => {
                Some(serde_json::json!({
                    "bucket": bucket,
                    "key": key,
                    "expected": expected,
                    "actual": actual,
                }))
            }
            _ => None,
        }
    }
}
