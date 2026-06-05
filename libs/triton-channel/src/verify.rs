// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Signature and content-integrity checks.
//!
//! Two independent checks compose to give us end-to-end integrity:
//!
//! 1. [`verify_minisign`] proves the channel-manifest JSON was signed
//!    by the holder of the publisher private key. The `sha256` fields
//!    inside the JSON are thus also trusted.
//! 2. [`verify_sha256`] proves a downloaded artifact (image content,
//!    agent tarball, tritonadm tarball) matches the `sha256` claimed in
//!    the (already-trusted) manifest.

use minisign_verify::{PublicKey, Signature};
use sha2::{Digest, Sha256};

use crate::errors::{IntegrityError, VerifyError};

/// Verify that `signature_bytes` is a valid minisign signature of
/// `manifest_bytes` made by the holder of `publisher_pubkey`.
///
/// `publisher_pubkey` is the contents of a `.pub` file produced by
/// `minisign -G` (a two-line ASCII document: `untrusted comment: ...`
/// then the base64 key). The committed key lives at
/// `monitor-reef/cli/tritonadm/publisher.pub` and is embedded at compile
/// time via `include_str!` in `tritonadm` (and via heredoc in
/// `install.sh`).
///
/// Returns `Ok(())` on success; on failure, the error variant
/// distinguishes a bad pubkey, a bad signature blob, and a signature
/// that did not validate.
pub fn verify_minisign(
    manifest_bytes: &[u8],
    signature_bytes: &[u8],
    publisher_pubkey: &str,
) -> Result<(), VerifyError> {
    let pk = PublicKey::decode(publisher_pubkey)
        .map_err(|e| VerifyError::InvalidPublicKey(e.to_string()))?;

    let sig_str = std::str::from_utf8(signature_bytes)
        .map_err(|e| VerifyError::InvalidSignature(format!("not utf-8: {e}")))?;

    let sig =
        Signature::decode(sig_str).map_err(|e| VerifyError::InvalidSignature(e.to_string()))?;

    pk.verify(manifest_bytes, &sig, false)
        .map_err(|e| VerifyError::BadSignature(e.to_string()))
}

/// Verify that the SHA-256 of `content` matches `expected_hex`.
///
/// `expected_hex` must be exactly 64 lowercase hex characters; this is
/// the form written into channel-manifest `sha256` fields by the
/// publisher tool. Uppercase or mixed-case input is treated as
/// malformed (we do not normalize) so that a comparison failure here
/// always means "the bytes do not match," never "the strings happened
/// to differ by case."
pub fn verify_sha256(content: &[u8], expected_hex: &str) -> Result<(), IntegrityError> {
    if expected_hex.len() != 64 || !expected_hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(IntegrityError::MalformedExpected(expected_hex.to_string()));
    }
    if expected_hex.bytes().any(|b| b.is_ascii_uppercase()) {
        return Err(IntegrityError::MalformedExpected(expected_hex.to_string()));
    }

    let mut hasher = Sha256::new();
    hasher.update(content);
    let actual = hex::encode(hasher.finalize());

    if actual == expected_hex {
        Ok(())
    } else {
        Err(IntegrityError::Mismatch {
            expected: expected_hex.to_string(),
            actual,
        })
    }
}
