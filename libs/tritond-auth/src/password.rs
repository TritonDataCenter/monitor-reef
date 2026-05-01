// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Password hashing, verification, and bootstrap-password generation.
//!
//! `bcrypt` is CPU-bound (~250 ms at cost 12), so [`hash_password`]
//! and [`verify_password`] return futures that internally
//! `spawn_blocking` and never stall a Tokio worker.

use rand::RngCore;

use crate::RedactedString;

/// Bcrypt cost factor. 12 takes ~250 ms on contemporary hardware,
/// which is the right side of the OWASP guidance for interactive
/// login while staying responsive for the bootstrap path.
const BCRYPT_COST: u32 = 12;

/// Length in bytes of the random material backing a generated
/// password. 18 random bytes encode to 24 url-safe base64 characters,
/// which is comfortably above the 128-bit security floor.
const PASSWORD_RANDOM_BYTES: usize = 18;

/// Errors returned by the password helpers.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PasswordError {
    /// Bcrypt rejected the input or hash. The most common cause is a
    /// password longer than bcrypt's 72-byte ceiling.
    #[error("bcrypt error: {0}")]
    Bcrypt(String),

    /// The blocking task panicked or was cancelled.
    #[error("password worker thread joined with error: {0}")]
    Join(String),
}

impl From<bcrypt::BcryptError> for PasswordError {
    fn from(err: bcrypt::BcryptError) -> Self {
        PasswordError::Bcrypt(err.to_string())
    }
}

/// Hash a plaintext password with bcrypt at the default cost.
///
/// Runs the bcrypt computation on Tokio's blocking pool so the
/// caller's executor isn't held up for ~250 ms.
#[must_use = "ignoring the returned hash discards the credential"]
pub async fn hash_password(plaintext: &RedactedString) -> Result<String, PasswordError> {
    let owned = plaintext.expose().to_string();
    tokio::task::spawn_blocking(move || bcrypt::hash(&owned, BCRYPT_COST))
        .await
        .map_err(|e| PasswordError::Join(e.to_string()))?
        .map_err(PasswordError::from)
}

/// Constant-time verify a plaintext password against a stored hash.
///
/// Returns `Ok(true)` on a match, `Ok(false)` on a non-match, and an
/// error only if the stored hash itself is malformed. Callers should
/// treat malformed-hash errors as "auth fails" rather than surfacing
/// the underlying bcrypt error to the user.
#[must_use = "the verification result must be checked"]
pub async fn verify_password(
    plaintext: &RedactedString,
    hash: &str,
) -> Result<bool, PasswordError> {
    let owned = plaintext.expose().to_string();
    let hash = hash.to_string();
    tokio::task::spawn_blocking(move || bcrypt::verify(&owned, &hash))
        .await
        .map_err(|e| PasswordError::Join(e.to_string()))?
        .map_err(PasswordError::from)
}

/// Generate a strong random password suitable for the bootstrap
/// operator. Returned as a [`RedactedString`] so the caller can
/// hand it to [`hash_password`] and the bootstrap banner without
/// it leaking via `Debug`.
#[must_use = "the generated password is the only copy of this credential"]
pub fn generate_random_password() -> RedactedString {
    use base64::Engine;
    let mut buf = [0u8; PASSWORD_RANDOM_BYTES];
    rand::rng().fill_bytes(&mut buf);
    let body = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf);
    RedactedString::new(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn hash_then_verify_round_trip() {
        let secret = RedactedString::new("correct horse battery staple".to_string());
        let hash = hash_password(&secret).await.unwrap();
        assert!(verify_password(&secret, &hash).await.unwrap());

        let wrong = RedactedString::new("wrong password".to_string());
        assert!(!verify_password(&wrong, &hash).await.unwrap());
    }

    #[test]
    fn generated_passwords_are_distinct_and_sized() {
        let a = generate_random_password();
        let b = generate_random_password();
        assert_ne!(a.expose(), b.expose());
        // 18 random bytes -> 24 url-safe-no-pad base64 chars.
        assert_eq!(a.expose().len(), 24);
        assert_eq!(b.expose().len(), 24);
    }

    #[tokio::test]
    async fn malformed_hash_surfaces_error() {
        let secret = RedactedString::new("anything".to_string());
        let err = verify_password(&secret, "not-a-bcrypt-hash").await;
        assert!(err.is_err(), "expected error, got {err:?}");
    }
}
