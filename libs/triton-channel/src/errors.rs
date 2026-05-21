// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Error types for parsing, verification, and integrity checks.

use thiserror::Error;

/// Errors surfaced when parsing a channel manifest.
#[derive(Debug, Error)]
pub enum ParseError {
    /// The bytes are not valid JSON, or do not match the schema.
    #[error("malformed channel manifest: {0}")]
    Json(#[from] serde_json::Error),

    /// The manifest's `schema` field is newer than the consumer
    /// understands. Consumer should `tcadm self-update` and retry.
    #[error(
        "channel manifest schema is {found}, this client only understands up to {supported}; \
         update your client and retry"
    )]
    UnsupportedSchema { found: u32, supported: u32 },
}

/// Errors surfaced when verifying a minisign signature.
#[derive(Debug, Error)]
pub enum VerifyError {
    /// The publisher public key string is malformed.
    #[error("invalid publisher public key: {0}")]
    InvalidPublicKey(String),

    /// The detached signature bytes do not parse as a minisign signature.
    #[error("invalid minisign signature: {0}")]
    InvalidSignature(String),

    /// The signature did not validate against the manifest + pubkey.
    /// This indicates tampering, key mismatch, or use of a non-prehashed
    /// signature when we require prehashed (or vice versa).
    #[error("signature verification failed: {0}")]
    BadSignature(String),
}

/// Errors surfaced when verifying an artifact's SHA-256 against its
/// channel-manifest entry.
#[derive(Debug, Error)]
pub enum IntegrityError {
    /// The expected SHA-256 string in the channel manifest is not 64
    /// lowercase hex characters.
    #[error("malformed sha256 in channel manifest: {0}")]
    MalformedExpected(String),

    /// The computed SHA-256 does not match the expected value.
    /// `expected` and `actual` are both lowercase hex.
    #[error("sha256 mismatch: expected {expected}, got {actual}")]
    Mismatch { expected: String, actual: String },
}
