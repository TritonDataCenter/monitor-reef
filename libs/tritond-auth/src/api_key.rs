// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! API-key issuance and verification.
//!
//! API keys are long-lived opaque bearer credentials for automation
//! callers (CI pipelines, Terraform, scripts). Wire format is
//! `tcadm_<base64-url-no-pad>` where the random material is 32
//! bytes (256 bits) from the OS RNG. The plaintext is shown to the
//! operator exactly once at creation; the server stores only a
//! bcrypt hash.

use rand::RngCore;

/// Recognisable prefix for tritond API keys. Matches `tcadm_…` so
/// that a leaked key in a log is greppable, and so that
/// presence-based heuristics (gitleaks, trufflehog) have a hook.
pub const API_KEY_PREFIX: &str = "tcadm_";

/// Length in bytes of the random material in an API key. 32 bytes is
/// 256 bits of entropy, which is more than enough.
const API_KEY_RANDOM_BYTES: usize = 32;

/// Bcrypt cost for API keys. Lower than passwords because the
/// underlying material is high-entropy random and keys are checked
/// on every authenticated request — the cost is paid per call.
const API_KEY_BCRYPT_COST: u32 = 8;

/// Errors returned by the API-key helpers.
#[derive(Debug, thiserror::Error)]
pub enum ApiKeyError {
    /// Wire format did not start with [`API_KEY_PREFIX`].
    #[error("api key is missing the {API_KEY_PREFIX} prefix")]
    Malformed,

    /// Bcrypt rejected the input or stored hash.
    #[error("bcrypt error: {0}")]
    Bcrypt(#[from] bcrypt::BcryptError),
}

/// A freshly minted API key, exposing both the plaintext (returned
/// to the operator once) and the bcrypt hash (persisted server-side).
#[derive(Debug)]
pub struct ApiKeyMaterial {
    /// Wire form, e.g. `tcadm_xn3F…`. Show to the user once; never
    /// store this.
    pub plaintext: String,
    /// Bcrypt hash of `plaintext`. Persist this against the
    /// owning user record.
    pub hash: String,
}

/// Generate a new API key. The plaintext is suitable for surfacing to
/// the operator; the hash is what gets stored.
pub fn generate_api_key() -> Result<ApiKeyMaterial, ApiKeyError> {
    use base64::Engine;
    let mut buf = [0u8; API_KEY_RANDOM_BYTES];
    rand::rng().fill_bytes(&mut buf);
    let body = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf);
    let plaintext = format!("{API_KEY_PREFIX}{body}");
    let hash = bcrypt::hash(&plaintext, API_KEY_BCRYPT_COST)?;
    Ok(ApiKeyMaterial { plaintext, hash })
}

/// Hash an externally-supplied API key (used by tests and migrations).
pub fn hash_api_key(plaintext: &str) -> Result<String, ApiKeyError> {
    if !plaintext.starts_with(API_KEY_PREFIX) {
        return Err(ApiKeyError::Malformed);
    }
    Ok(bcrypt::hash(plaintext, API_KEY_BCRYPT_COST)?)
}

/// Constant-time verify a plaintext API key against a stored hash.
///
/// Returns `Ok(false)` if the prefix is wrong (treating malformed
/// input as "doesn't match" rather than a caller-visible error,
/// since the verify call is on every request).
pub fn verify_api_key(plaintext: &str, hash: &str) -> Result<bool, ApiKeyError> {
    if !plaintext.starts_with(API_KEY_PREFIX) {
        return Ok(false);
    }
    Ok(bcrypt::verify(plaintext, hash)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_round_trips_through_verify() {
        let material = generate_api_key().unwrap();
        assert!(material.plaintext.starts_with(API_KEY_PREFIX));
        assert!(verify_api_key(&material.plaintext, &material.hash).unwrap());
    }

    #[test]
    fn distinct_keys_do_not_collide() {
        let a = generate_api_key().unwrap();
        let b = generate_api_key().unwrap();
        assert_ne!(a.plaintext, b.plaintext);
        assert!(!verify_api_key(&a.plaintext, &b.hash).unwrap());
    }

    #[test]
    fn missing_prefix_fails_verification() {
        let material = generate_api_key().unwrap();
        let stripped = material.plaintext.trim_start_matches(API_KEY_PREFIX);
        assert!(!verify_api_key(stripped, &material.hash).unwrap());
    }

    #[test]
    fn hash_api_key_rejects_malformed_input() {
        assert!(matches!(
            hash_api_key("bare-token"),
            Err(ApiKeyError::Malformed)
        ));
    }
}
