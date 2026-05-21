// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Verification tests for triton-channel.
//!
//! `verify_minisign` is exercised end-to-end by generating an
//! ephemeral keypair, signing the example fixture with the (full)
//! `minisign` dev-dependency, and then verifying with our production
//! `minisign-verify`-backed code path. This keeps the test hermetic
//! and decoupled from the operator's real publisher key.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use minisign::{KeyPair, sign};
use sha2::{Digest, Sha256};

use triton_channel::{IntegrityError, VerifyError, verify_minisign, verify_sha256};

const EXAMPLE: &[u8] = include_bytes!("fixtures/example_channel.json");

/// Generate an ephemeral minisign keypair and produce both the public
/// key string (as the `.pub` file would hold it) and a detached
/// signature over `manifest_bytes` (as the `.minisig` file would hold
/// it).
fn sign_with_ephemeral_keypair(manifest_bytes: &[u8]) -> (String, Vec<u8>) {
    let kp = KeyPair::generate_unencrypted_keypair().expect("keypair generation");

    let pubkey_str = kp.pk.to_box().expect("encode pubkey").into_string();

    let cursor = std::io::Cursor::new(manifest_bytes);
    let sig_box = sign(None, &kp.sk, cursor, None, None).expect("sign manifest");

    (pubkey_str, sig_box.into_string().into_bytes())
}

#[test]
fn verify_accepts_a_good_signature() {
    let (pubkey, sig) = sign_with_ephemeral_keypair(EXAMPLE);
    verify_minisign(EXAMPLE, &sig, &pubkey).expect("verify succeeds on valid signature");
}

#[test]
fn verify_rejects_tampered_manifest() {
    let (pubkey, sig) = sign_with_ephemeral_keypair(EXAMPLE);

    // Flip a single byte. The signature should no longer validate.
    let mut tampered = EXAMPLE.to_vec();
    tampered[10] ^= 0x01;

    match verify_minisign(&tampered, &sig, &pubkey) {
        Err(VerifyError::BadSignature(_)) => {}
        other => panic!("expected BadSignature, got {other:?}"),
    }
}

#[test]
fn verify_rejects_wrong_pubkey() {
    let (_correct_pubkey, sig) = sign_with_ephemeral_keypair(EXAMPLE);
    let (other_pubkey, _other_sig) = sign_with_ephemeral_keypair(EXAMPLE);

    // Same message, valid signature, but the public key belongs to a
    // different keypair. Must be rejected.
    match verify_minisign(EXAMPLE, &sig, &other_pubkey) {
        Err(VerifyError::BadSignature(_)) => {}
        other => panic!("expected BadSignature, got {other:?}"),
    }
}

#[test]
fn verify_rejects_malformed_pubkey() {
    let (_pubkey, sig) = sign_with_ephemeral_keypair(EXAMPLE);
    let bogus_pubkey = "not a valid pubkey";

    match verify_minisign(EXAMPLE, &sig, bogus_pubkey) {
        Err(VerifyError::InvalidPublicKey(_)) => {}
        other => panic!("expected InvalidPublicKey, got {other:?}"),
    }
}

#[test]
fn verify_rejects_malformed_signature() {
    let (pubkey, _sig) = sign_with_ephemeral_keypair(EXAMPLE);
    let bogus_sig = b"not a valid signature";

    match verify_minisign(EXAMPLE, bogus_sig, &pubkey) {
        Err(VerifyError::InvalidSignature(_)) => {}
        other => panic!("expected InvalidSignature, got {other:?}"),
    }
}

#[test]
fn sha256_accepts_matching_content() {
    let content = b"the triton cloud is rising";
    let mut hasher = Sha256::new();
    hasher.update(content);
    let expected = hex::encode(hasher.finalize());

    verify_sha256(content, &expected).expect("matching sha256 accepts");
}

#[test]
fn sha256_rejects_mismatch() {
    let content = b"the triton cloud is rising";
    // Plausible-looking but wrong hash.
    let wrong = "0000000000000000000000000000000000000000000000000000000000000000";

    match verify_sha256(content, wrong) {
        Err(IntegrityError::Mismatch { expected, actual }) => {
            assert_eq!(expected, wrong);
            assert_eq!(actual.len(), 64);
            assert_ne!(actual, wrong);
        }
        other => panic!("expected Mismatch, got {other:?}"),
    }
}

#[test]
fn sha256_rejects_uppercase_hash() {
    // We require lowercase hex so that comparisons cannot accidentally
    // pass due to case-folding the strings before content matching.
    let content = b"anything";
    let uppercase = "AB12CD34EF56789012345678901234567890ABCDEF0123456789ABCDEF012345";
    match verify_sha256(content, uppercase) {
        Err(IntegrityError::MalformedExpected(_)) => {}
        other => panic!("expected MalformedExpected, got {other:?}"),
    }
}

#[test]
fn sha256_rejects_short_hash() {
    let content = b"anything";
    let short = "abcd";
    match verify_sha256(content, short) {
        Err(IntegrityError::MalformedExpected(_)) => {}
        other => panic!("expected MalformedExpected, got {other:?}"),
    }
}

#[test]
fn sha256_rejects_non_hex_characters() {
    let content = b"anything";
    let not_hex = "z1234567890abcdef0000000000000000000000000000000000000000000000a";
    match verify_sha256(content, not_hex) {
        Err(IntegrityError::MalformedExpected(_)) => {}
        other => panic!("expected MalformedExpected, got {other:?}"),
    }
}
