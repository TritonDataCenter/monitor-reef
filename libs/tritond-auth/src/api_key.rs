// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! API-key issuance and verification.
//!
//! API keys are long-lived opaque bearer credentials for automation
//! callers (CI pipelines, Terraform, scripts). Wire format is split
//! into a non-secret lookup id and a secret payload:
//!
//! ```text
//! tcadm_<12-char lookup_id><32-char secret>
//! ```
//!
//! * `lookup_id` is 9 random bytes encoded url-safe-no-pad. It is
//!   stored in plaintext server-side and indexed for O(1) lookup.
//! * `secret` is 24 random bytes (192 bits) encoded url-safe-no-pad.
//!   Server keeps only its bcrypt hash.
//!
//! On every authenticated request the auth middleware parses the
//! lookup id, resolves the matching record, and bcrypt-verifies the
//! secret against that one record — turning the previous O(N)
//! bcrypt scan into O(1) bcrypt verify.

use rand::RngCore;

/// Recognisable prefix for tritond API keys. Matches `tcadm_…` so a
/// leaked key in a log is greppable and so secret-scanning heuristics
/// (gitleaks, trufflehog) have a hook.
pub const API_KEY_PREFIX: &str = "tcadm_";

/// Length in bytes of the lookup-id random material. 9 bytes encodes
/// to exactly 12 url-safe-no-pad base64 characters.
const LOOKUP_ID_BYTES: usize = 9;

/// Length in characters of the lookup-id segment in a wire-form key.
pub const LOOKUP_ID_CHARS: usize = 12;

/// Length in bytes of the secret random material. 24 bytes (192 bits
/// of entropy) encodes to exactly 32 url-safe-no-pad base64 characters.
const SECRET_BYTES: usize = 24;

/// Length in characters of the secret segment in a wire-form key.
pub const SECRET_CHARS: usize = 32;

/// Bcrypt cost for API keys. Lower than passwords because the
/// underlying material is high-entropy random and keys are checked
/// on every authenticated request — the cost is paid per call.
const API_KEY_BCRYPT_COST: u32 = 8;

/// Errors returned by the API-key helpers.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ApiKeyError {
    /// Wire format does not match `tcadm_<12-char id><32-char secret>`.
    #[error("api key is malformed")]
    Malformed,

    /// Bcrypt rejected the input or stored hash.
    #[error("bcrypt error: {0}")]
    Bcrypt(String),

    /// The blocking task panicked or was cancelled.
    #[error("api key worker thread joined with error: {0}")]
    Join(String),
}

impl From<bcrypt::BcryptError> for ApiKeyError {
    fn from(err: bcrypt::BcryptError) -> Self {
        ApiKeyError::Bcrypt(err.to_string())
    }
}

/// A freshly minted API key, exposing the wire-form plaintext
/// (returned to the operator once), the lookup id (stored in
/// plaintext, indexed), and the bcrypt hash of the secret payload.
#[derive(Debug)]
pub struct ApiKeyMaterial {
    /// Wire form, e.g. `tcadm_AAAAAAAAAAAA<32-char secret>`. Show to
    /// the user once; never store this.
    pub plaintext: String,
    /// Non-secret identifier — exactly [`LOOKUP_ID_CHARS`] base64
    /// characters. Stored server-side and used for O(1) lookup.
    pub lookup_id: String,
    /// Bcrypt hash of the secret payload (the suffix after the
    /// lookup id).
    pub hash: String,
}

/// Parse a wire-form api key into its `(lookup_id, secret)` pair.
/// Returns `None` if the format does not match exactly.
#[must_use]
pub fn parse_api_key(plaintext: &str) -> Option<(&str, &str)> {
    let body = plaintext.strip_prefix(API_KEY_PREFIX)?;
    if body.len() != LOOKUP_ID_CHARS + SECRET_CHARS {
        return None;
    }
    let (lookup_id, secret) = body.split_at(LOOKUP_ID_CHARS);
    Some((lookup_id, secret))
}

/// Generate a new API key. Bcrypt-hashing the secret runs on the
/// blocking pool so this stays away from the async runtime.
#[must_use = "ignoring the returned material discards the credential"]
pub async fn generate_api_key() -> Result<ApiKeyMaterial, ApiKeyError> {
    use base64::Engine;
    let (lookup_id, secret, plaintext) = tokio::task::spawn_blocking(|| {
        let mut id_bytes = [0u8; LOOKUP_ID_BYTES];
        let mut secret_bytes = [0u8; SECRET_BYTES];
        let mut rng = rand::rng();
        rng.fill_bytes(&mut id_bytes);
        rng.fill_bytes(&mut secret_bytes);
        let lookup_id = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(id_bytes);
        let secret = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(secret_bytes);
        let plaintext = format!("{API_KEY_PREFIX}{lookup_id}{secret}");
        (lookup_id, secret, plaintext)
    })
    .await
    .map_err(|e| ApiKeyError::Join(e.to_string()))?;

    let hash = tokio::task::spawn_blocking({
        let secret = secret.clone();
        move || bcrypt::hash(&secret, API_KEY_BCRYPT_COST)
    })
    .await
    .map_err(|e| ApiKeyError::Join(e.to_string()))?
    .map_err(ApiKeyError::from)?;

    Ok(ApiKeyMaterial {
        plaintext,
        lookup_id,
        hash,
    })
}

/// Verify an externally-supplied secret segment against a stored
/// bcrypt hash. Used by the auth middleware after it has resolved a
/// candidate record by lookup id.
#[must_use = "the verification result must be checked"]
pub async fn verify_api_key_secret(secret: &str, hash: &str) -> Result<bool, ApiKeyError> {
    let secret = secret.to_string();
    let hash = hash.to_string();
    tokio::task::spawn_blocking(move || bcrypt::verify(&secret, &hash))
        .await
        .map_err(|e| ApiKeyError::Join(e.to_string()))?
        .map_err(ApiKeyError::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn generate_round_trips_through_parse_and_verify() {
        let material = generate_api_key().await.unwrap();
        assert!(material.plaintext.starts_with(API_KEY_PREFIX));

        let (lookup_id, secret) = parse_api_key(&material.plaintext).expect("parse should succeed");
        assert_eq!(lookup_id, material.lookup_id);
        assert!(verify_api_key_secret(secret, &material.hash).await.unwrap());
    }

    #[tokio::test]
    async fn distinct_keys_do_not_collide() {
        let a = generate_api_key().await.unwrap();
        let b = generate_api_key().await.unwrap();
        assert_ne!(a.plaintext, b.plaintext);
        assert_ne!(a.lookup_id, b.lookup_id);

        let (_, a_secret) = parse_api_key(&a.plaintext).unwrap();
        assert!(!verify_api_key_secret(a_secret, &b.hash).await.unwrap());
    }

    #[test]
    fn parse_rejects_short_input() {
        assert!(parse_api_key("tcadm_short").is_none());
    }

    #[test]
    fn parse_rejects_missing_prefix() {
        let no_prefix = format!(
            "{}{}",
            "x".repeat(LOOKUP_ID_CHARS),
            "y".repeat(SECRET_CHARS)
        );
        assert!(parse_api_key(&no_prefix).is_none());
    }

    #[test]
    fn parse_splits_on_lookup_id_boundary() {
        let plaintext = format!(
            "{}{}{}",
            API_KEY_PREFIX,
            "L".repeat(LOOKUP_ID_CHARS),
            "S".repeat(SECRET_CHARS)
        );
        let (lookup_id, secret) = parse_api_key(&plaintext).unwrap();
        assert_eq!(lookup_id.len(), LOOKUP_ID_CHARS);
        assert_eq!(secret.len(), SECRET_CHARS);
    }
}
