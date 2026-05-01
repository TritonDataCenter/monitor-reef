// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Password hashing, verification, and bootstrap-password generation.

use rand::RngCore;

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
pub enum PasswordError {
    /// Bcrypt rejected the input or hash. The most common cause is a
    /// password longer than bcrypt's 72-byte ceiling.
    #[error("bcrypt error: {0}")]
    Bcrypt(#[from] bcrypt::BcryptError),
}

/// Hash a plaintext password with bcrypt at the default cost.
pub fn hash_password(plaintext: &str) -> Result<String, PasswordError> {
    Ok(bcrypt::hash(plaintext, BCRYPT_COST)?)
}

/// Constant-time verify a plaintext password against a stored hash.
///
/// Returns `Ok(true)` on a match, `Ok(false)` on a non-match, and an
/// error only if the stored hash itself is malformed. Callers should
/// treat malformed-hash errors as "auth fails" rather than surfacing
/// the underlying bcrypt error to the user.
pub fn verify_password(plaintext: &str, hash: &str) -> Result<bool, PasswordError> {
    Ok(bcrypt::verify(plaintext, hash)?)
}

/// Generate a strong random password suitable for the bootstrap
/// operator. The returned string is url-safe base64 of
/// [`PASSWORD_RANDOM_BYTES`] bytes from the OS RNG.
pub fn generate_random_password() -> String {
    use base64::Engine;
    let mut buf = [0u8; PASSWORD_RANDOM_BYTES];
    rand::rng().fill_bytes(&mut buf);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_then_verify_round_trip() {
        let hash = hash_password("correct horse battery staple").unwrap();
        assert!(verify_password("correct horse battery staple", &hash).unwrap());
        assert!(!verify_password("wrong password", &hash).unwrap());
    }

    #[test]
    fn generated_passwords_are_distinct_and_sized() {
        let a = generate_random_password();
        let b = generate_random_password();
        assert_ne!(a, b);
        // 18 random bytes -> 24 url-safe-no-pad base64 chars.
        assert_eq!(a.len(), 24);
        assert_eq!(b.len(), 24);
    }

    #[test]
    fn malformed_hash_surfaces_error() {
        let err = verify_password("anything", "not-a-bcrypt-hash");
        assert!(err.is_err(), "expected error, got {err:?}");
    }
}
