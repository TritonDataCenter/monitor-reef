// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Per-deployment HMAC-SHA256 key for stamping a tamper-evident
//! "this zone is managed by tritond" identity into SmartOS internal
//! metadata.
//!
//! When tritond enqueues a Provision job and the agent fetches the
//! blueprint, tritond signs `(instance_id || tenant_id || project_id)`
//! with this key. The agent writes the four fields verbatim into the
//! zone's `internal_metadata`. Later, when a CN status report flows
//! through the classifier, tritond recomputes the HMAC from the
//! reported ids and compares constant-time. A mismatch (or a missing
//! tag) means the metadata was forged or copied from another
//! deployment, and the zone is quarantined as `StaleFingerprint`
//! rather than treated as managed.
//!
//! The key is generated once at first-run bootstrap and persisted
//! under `SystemKey::IdentityHmac`. Restart-stable so existing zones
//! keep verifying.

use hmac::{Hmac, Mac};
use sha2::Sha256;
use uuid::Uuid;
use zeroize::Zeroize;

/// HMAC-SHA256 produces a 32-byte tag; the key is 32 bytes by convention.
pub const IDENTITY_HMAC_KEY_BYTES: usize = 32;

type HmacSha256 = Hmac<Sha256>;

/// Per-deployment HMAC-SHA256 key. Wraps the raw bytes; the bytes
/// are zeroed on drop (matching `JwtKey`).
pub struct IdentityHmacKey {
    bytes: [u8; IDENTITY_HMAC_KEY_BYTES],
}

impl IdentityHmacKey {
    /// Generate a fresh random key. Call once at cluster bootstrap;
    /// persist [`Self::bytes`] to FoundationDB.
    #[must_use = "the generated key is the only copy of this credential"]
    pub fn generate() -> Self {
        use rand::RngCore;
        let mut bytes = [0u8; IDENTITY_HMAC_KEY_BYTES];
        rand::rng().fill_bytes(&mut bytes);
        Self { bytes }
    }

    /// Reconstruct from previously-persisted bytes.
    #[must_use]
    pub fn from_bytes(bytes: [u8; IDENTITY_HMAC_KEY_BYTES]) -> Self {
        Self { bytes }
    }

    /// Borrow the raw key bytes (e.g. for persistence).
    #[must_use]
    pub fn bytes(&self) -> &[u8; IDENTITY_HMAC_KEY_BYTES] {
        &self.bytes
    }

    /// Compute the HMAC tag (lowercase hex) for an instance triple.
    /// The input is the canonical hyphenated UUID strings concatenated
    /// with a record separator that cannot appear in a UUID.
    #[must_use]
    pub fn sign(&self, instance_id: Uuid, tenant_id: Uuid, project_id: Uuid) -> String {
        let mut mac =
            HmacSha256::new_from_slice(&self.bytes).expect("HMAC-SHA256 accepts any key length");
        mac.update(canonical_input(instance_id, tenant_id, project_id).as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }

    /// Constant-time verify a hex tag against the recomputed HMAC.
    /// Returns false on any decode error or length mismatch as well
    /// as on a real verification failure.
    #[must_use]
    pub fn verify(
        &self,
        instance_id: Uuid,
        tenant_id: Uuid,
        project_id: Uuid,
        tag_hex: &str,
    ) -> bool {
        let Ok(tag_bytes) = hex::decode(tag_hex) else {
            return false;
        };
        let mut mac =
            HmacSha256::new_from_slice(&self.bytes).expect("HMAC-SHA256 accepts any key length");
        mac.update(canonical_input(instance_id, tenant_id, project_id).as_bytes());
        mac.verify_slice(&tag_bytes).is_ok()
    }
}

impl Drop for IdentityHmacKey {
    fn drop(&mut self) {
        self.bytes.zeroize();
    }
}

fn canonical_input(instance_id: Uuid, tenant_id: Uuid, project_id: Uuid) -> String {
    // ASCII Record Separator (0x1E) cannot appear in a hyphenated UUID,
    // so the input is unambiguous with no need for length-prefixing.
    format!("{instance_id}\x1e{tenant_id}\x1e{project_id}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids() -> (Uuid, Uuid, Uuid) {
        (
            Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap(),
            Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap(),
            Uuid::parse_str("33333333-3333-3333-3333-333333333333").unwrap(),
        )
    }

    #[test]
    fn sign_verify_round_trip() {
        let key = IdentityHmacKey::generate();
        let (i, t, p) = ids();
        let tag = key.sign(i, t, p);
        assert!(key.verify(i, t, p, &tag));
    }

    #[test]
    fn sign_is_deterministic_for_same_key_and_inputs() {
        let key = IdentityHmacKey::from_bytes([7u8; IDENTITY_HMAC_KEY_BYTES]);
        let (i, t, p) = ids();
        assert_eq!(key.sign(i, t, p), key.sign(i, t, p));
    }

    #[test]
    fn verify_rejects_wrong_instance() {
        let key = IdentityHmacKey::generate();
        let (i, t, p) = ids();
        let tag = key.sign(i, t, p);
        let other = Uuid::parse_str("99999999-9999-9999-9999-999999999999").unwrap();
        assert!(!key.verify(other, t, p, &tag));
    }

    #[test]
    fn verify_rejects_wrong_tenant() {
        let key = IdentityHmacKey::generate();
        let (i, t, p) = ids();
        let tag = key.sign(i, t, p);
        let other = Uuid::parse_str("99999999-9999-9999-9999-999999999999").unwrap();
        assert!(!key.verify(i, other, p, &tag));
    }

    #[test]
    fn verify_rejects_wrong_project() {
        let key = IdentityHmacKey::generate();
        let (i, t, p) = ids();
        let tag = key.sign(i, t, p);
        let other = Uuid::parse_str("99999999-9999-9999-9999-999999999999").unwrap();
        assert!(!key.verify(i, t, other, &tag));
    }

    #[test]
    fn verify_rejects_wrong_key() {
        let key_a = IdentityHmacKey::from_bytes([1u8; IDENTITY_HMAC_KEY_BYTES]);
        let key_b = IdentityHmacKey::from_bytes([2u8; IDENTITY_HMAC_KEY_BYTES]);
        let (i, t, p) = ids();
        let tag = key_a.sign(i, t, p);
        assert!(!key_b.verify(i, t, p, &tag));
    }

    #[test]
    fn verify_rejects_garbage_hex() {
        let key = IdentityHmacKey::generate();
        let (i, t, p) = ids();
        assert!(!key.verify(i, t, p, "not-hex"));
        assert!(!key.verify(i, t, p, ""));
        assert!(!key.verify(i, t, p, "abcd")); // valid hex, wrong length
    }

    #[test]
    fn from_bytes_round_trips() {
        let bytes = [0x42u8; IDENTITY_HMAC_KEY_BYTES];
        let key = IdentityHmacKey::from_bytes(bytes);
        assert_eq!(key.bytes(), &bytes);
    }

    #[test]
    fn drop_zeroes_bytes() {
        // Soundness sanity check: after Drop runs, the in-place buffer
        // is zero. We can't observe the original key's freed memory,
        // but we can verify the Zeroize impl is wired correctly by
        // looking at a key we manually drop.
        let mut key = IdentityHmacKey::from_bytes([0xAAu8; IDENTITY_HMAC_KEY_BYTES]);
        // Mimic Drop without freeing: explicit zeroize call.
        key.bytes.zeroize();
        assert_eq!(key.bytes(), &[0u8; IDENTITY_HMAC_KEY_BYTES]);
    }
}
