// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Error surface for `tritond-saga`.
//!
//! Mirrors `tritond_store::StoreError` so callers can map either
//! family of errors through the same handler-error pipeline.

/// Result alias used throughout the crate.
pub type SagaResult<T> = std::result::Result<T, SagaError>;

#[derive(Debug, thiserror::Error)]
pub enum SagaError {
    /// The requested saga / SEC / record does not exist.
    #[error("not found")]
    NotFound,

    /// A CAS-style precondition failed. Surfaces fence violations
    /// (a stale `(sec_id, epoch)` writing to a saga that has been
    /// adopted) and double-adopt races.
    #[error("conflict: {0}")]
    Conflict(String),

    /// The action's fence is stale: another SEC has adopted the
    /// saga (RFD 00004 D-Sg-8 / Invariant 8). Action bodies that
    /// hit this should return immediately so the unwind tail can
    /// run; the adopting SEC is the one driving the saga now.
    #[error(
        "fenced out: saga {saga_id}, expected (sec={expected_sec}, epoch={expected_epoch}), actual (sec={actual_sec}, epoch={actual_epoch})"
    )]
    FencedOut {
        saga_id: String,
        expected_sec: String,
        expected_epoch: u64,
        actual_sec: String,
        actual_epoch: u64,
    },

    /// The persisted saga's `(NAME, VERSION)` is not currently
    /// registered. The saga is left in its prior state; operators
    /// surface it on `tcadm sagas` and can drive it through unwind
    /// via `operations:abandon` once the missing version is
    /// redeployed. Invariant 10 / D-Sg-10.
    #[error("unknown saga version: {name}@{version}")]
    UnknownVersion { name: String, version: u32 },

    /// Steno itself returned an error.
    #[error("steno: {0}")]
    Steno(String),

    /// Backend (FDB / serialisation / IO) error.
    #[error("backend: {0}")]
    Backend(String),
}

impl From<anyhow::Error> for SagaError {
    fn from(value: anyhow::Error) -> Self {
        Self::Steno(value.to_string())
    }
}

impl From<serde_json::Error> for SagaError {
    fn from(value: serde_json::Error) -> Self {
        Self::Backend(format!("serde_json: {value}"))
    }
}
