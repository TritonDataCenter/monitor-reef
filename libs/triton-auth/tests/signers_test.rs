// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Signature tests for triton-auth
//!
//! Mirrors: target/node-smartdc-auth/test/signers.test.js
//!
//! Tests signature generation, request signing, and known test vectors.

use std::path::PathBuf;
use triton_auth::{
    fingerprint::md5_fingerprint_bytes,
    key_loader::KeyLoader,
    signature::{encode_signature, KeyType, RequestSigner},
};

/// Test key fingerprints from node-smartdc-auth test suite
const ID_RSA_MD5: &str = "fa:56:a1:6b:cc:04:97:fe:e2:98:54:c4:2e:0d:26:c6";
const ID_DSA_MD5: &str = "a6:e6:68:d3:28:2b:0a:a0:12:54:da:c4:c0:22:8d:ba";
const ID_ECDSA_MD5: &str = "00:74:32:ae:0a:24:3c:7a:e7:07:b8:ee:91:c4:c7:27";

/// Known signature for "foobar" with id_rsa using RSA-SHA256
/// From signers.test.js line 25-27
const SIG_RSA_SHA256: &str = "KX1okEE5wWjgrDYM35z9sO49WRk/DeZy7QeSNCFdOsn45BO6rVOIH5v\
V7WD25/VWyGCiN86Pml/Eulhx3Xx4ZUEHHc18K0BAKU5CSu/jCRI0dEFt4q1bXCyM7aK\
FlAXpk7CJIM0Gx91CJEXcZFuUddngoqljyt9hu4dpMhrjVFA=";

/// Known signature for "foobar" with id_rsa using RSA-SHA1
/// From signers.test.js line 29-31
/// Note: RSA-SHA1 is not currently implemented; we use RSA-SHA256 by default
#[allow(dead_code)]
const SIG_RSA_SHA1: &str = "parChQDdkj8wFY75IUW/W7KN9q5FFTPYfcAf+W7PmN8yxnRJB884NHYNT\
hl/TjZB2s0vt+kkfX3nldi54heTKbDKFwCOoDmVWQ2oE2ZrJPPFiUHReUAIRvwD0V/q7\
4c/DiRR6My7FEa8Szce27DBrjBmrMvMcmd7/jDbhaGusy4=";

fn test_keys_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/keys")
}

// ============================================================================
// Basic cliSigner Tests (signers.test.js lines 68-81)
// ============================================================================

/// Mirrors: 'basic cliSigner rsa' test
/// Signs "foobar" with id_rsa and verifies:
/// - keyId matches MD5 fingerprint
/// - algorithm is "rsa-sha256"
/// - signature matches known test vector
#[test]
fn test_basic_signer_rsa() {
    let key_path = test_keys_dir().join("id_rsa");
    let key = KeyLoader::load_legacy_from_file(&key_path, None).expect("Failed to load RSA key");

    // Get fingerprint
    let pub_blob = key.public_key_blob().expect("Failed to get public key blob");
    let key_id = md5_fingerprint_bytes(&pub_blob);

    // Sign "foobar" - the same test data used in node-smartdc-auth
    let data = b"foobar";
    let sig_bytes = key.sign(data).expect("Failed to sign");
    let signature = encode_signature(&sig_bytes);

    // Verify keyId
    assert_eq!(key_id, ID_RSA_MD5);

    // Verify algorithm
    let key_type = key.key_type();
    assert_eq!(key_type.algorithm_string(), "rsa-sha256");

    // Verify signature matches known test vector
    assert_eq!(
        signature, SIG_RSA_SHA256,
        "RSA-SHA256 signature does not match node-smartdc-auth test vector"
    );
}

/// Mirrors: 'basic cliSigner dsa' test
/// Signs "foobar" with id_dsa and verifies:
/// - keyId matches MD5 fingerprint
/// - algorithm is "dsa-sha1"
/// - signature is valid (not deterministic, so can't compare exact value)
#[test]
fn test_basic_signer_dsa() {
    let key_path = test_keys_dir().join("id_dsa");
    let key = KeyLoader::load_legacy_from_file(&key_path, None).expect("Failed to load DSA key");

    // Get fingerprint
    let pub_blob = key.public_key_blob().expect("Failed to get public key blob");
    let key_id = md5_fingerprint_bytes(&pub_blob);

    // Sign "foobar"
    let data = b"foobar";
    let sig_bytes = key.sign(data).expect("Failed to sign");
    let signature = encode_signature(&sig_bytes);

    // Verify keyId
    assert_eq!(key_id, ID_DSA_MD5);

    // Verify algorithm
    let key_type = key.key_type();
    assert_eq!(key_type.algorithm_string(), "dsa-sha1");

    // DSA signatures are not deterministic, but should be valid base64
    assert!(!signature.is_empty());
    assert!(
        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &signature).is_ok()
    );

    // DSA-SHA1 signatures should be 40 bytes (two 20-byte integers)
    assert_eq!(sig_bytes.len(), 40, "DSA signature should be 40 bytes");
}

/// Mirrors: 'basic cliSigner ecdsa' from agent-keys.test.js (also applicable here)
/// Signs data with id_ecdsa and verifies:
/// - keyId matches MD5 fingerprint
/// - algorithm is "ecdsa-sha256"
/// - signature is valid format
#[test]
fn test_basic_signer_ecdsa() {
    let key_path = test_keys_dir().join("id_ecdsa");
    let key = KeyLoader::load_legacy_from_file(&key_path, None).expect("Failed to load ECDSA key");

    // Get fingerprint
    let pub_blob = key.public_key_blob().expect("Failed to get public key blob");
    let key_id = md5_fingerprint_bytes(&pub_blob);

    // Sign some data
    let data = b"test data for ECDSA";
    let sig_bytes = key.sign(data).expect("Failed to sign");
    let signature = encode_signature(&sig_bytes);

    // Verify keyId
    assert_eq!(key_id, ID_ECDSA_MD5);

    // Verify algorithm
    let key_type = key.key_type();
    assert_eq!(key_type.algorithm_string(), "ecdsa-sha256");

    // ECDSA signatures are not deterministic, but should be valid base64
    assert!(!signature.is_empty());
    assert!(
        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &signature).is_ok()
    );

    // P-256 ECDSA signatures should be 64 bytes (two 32-byte integers)
    assert_eq!(
        sig_bytes.len(),
        64,
        "P-256 ECDSA signature should be 64 bytes"
    );
}

// ============================================================================
// RequestSigner Tests (signers.test.js lines 124-174)
// ============================================================================

/// Mirrors: 'requestSigner rsa' test
/// Verifies the signing string and authorization header format
#[test]
fn test_request_signer_rsa() {
    let key_path = test_keys_dir().join("id_rsa");
    let key = KeyLoader::load_legacy_from_file(&key_path, None).expect("Failed to load RSA key");
    let pub_blob = key.public_key_blob().expect("Failed to get public key blob");
    let fp = md5_fingerprint_bytes(&pub_blob);

    // Create signer
    let signer = RequestSigner::new("foo", &fp, KeyType::Rsa);

    // Generate date and signing string
    let date = "foo"; // node-smartdc-auth uses "foo" as test date
    let signing_string = signer.signing_string("GET", "/foo/machines", date);

    // Sign
    let sig_bytes = key.sign(signing_string.as_bytes()).expect("Failed to sign");
    let signature = encode_signature(&sig_bytes);

    // Generate authorization header
    let auth_header = signer.authorization_header(&signature);

    // Verify authorization header format matches node-smartdc-auth
    assert!(auth_header.starts_with("Signature keyId=\""));
    assert!(auth_header.contains(&format!("keyId=\"/foo/keys/{}", ID_RSA_MD5)));
    assert!(auth_header.contains("algorithm=\"rsa-sha256\""));
    assert!(auth_header.contains("signature=\""));
}

/// Mirrors: 'requestSigner with custom signer' test
/// Verifies subuser keyId format
#[test]
fn test_request_signer_with_subuser() {
    let signer =
        RequestSigner::new("foo", "12:34:56:78:90:ab:cd:ef:12:34:56:78:90:ab:cd:ef", KeyType::Rsa)
            .with_subuser("test");

    let key_id = signer.key_id_string();

    // Verify subuser format: /user/users/subuser/keys/fingerprint
    assert_eq!(
        key_id,
        "/foo/users/test/keys/12:34:56:78:90:ab:cd:ef:12:34:56:78:90:ab:cd:ef"
    );
}

// ============================================================================
// Algorithm String Tests (signers.test.js implicitly tests these)
// ============================================================================

/// Verifies algorithm strings match what node-smartdc-auth expects
#[test]
fn test_algorithm_strings_match_node_smartdc_auth() {
    assert_eq!(KeyType::Rsa.algorithm_string(), "rsa-sha256");
    assert_eq!(KeyType::Dsa.algorithm_string(), "dsa-sha1");
    assert_eq!(KeyType::Ecdsa256.algorithm_string(), "ecdsa-sha256");
    assert_eq!(KeyType::Ecdsa384.algorithm_string(), "ecdsa-sha384");
    assert_eq!(KeyType::Ecdsa521.algorithm_string(), "ecdsa-sha512");
    assert_eq!(KeyType::Ed25519.algorithm_string(), "ed25519-sha512");
}

// ============================================================================
// End-to-End Signing Flow Tests
// ============================================================================

/// Full request signing flow test
#[test]
fn test_full_request_signing_flow() {
    let key_path = test_keys_dir().join("id_rsa");
    let key = KeyLoader::load_legacy_from_file(&key_path, None).expect("Failed to load RSA key");
    let pub_blob = key.public_key_blob().expect("Failed to get public key blob");
    let fp = md5_fingerprint_bytes(&pub_blob);

    // Create signer
    let signer = RequestSigner::new("testuser", &fp, KeyType::Rsa);

    // Generate date and signing string
    let date = RequestSigner::date_header();
    let signing_string = signer.signing_string("GET", "/testuser/machines", &date);

    // Sign
    let sig_bytes = key.sign(signing_string.as_bytes()).expect("Failed to sign");
    let signature = encode_signature(&sig_bytes);

    // Generate authorization header
    let auth_header = signer.authorization_header(&signature);

    // Verify the header format
    assert!(auth_header.starts_with("Signature keyId=\"/testuser/keys/"));
    assert!(auth_header.contains("algorithm=\"rsa-sha256\""));
    assert!(auth_header.contains("signature=\""));
}

/// Signing string format test
#[test]
fn test_signing_string_format() {
    let signer = RequestSigner::new("foo", ID_RSA_MD5, KeyType::Rsa);

    let date = "Thu, 05 Jan 2024 00:00:00 GMT";
    let signing_string = signer.signing_string("GET", "/foo/machines", date);

    // Verify the format matches what node-smartdc-auth expects:
    // date: <date>\n(request-target): <method> <path>
    assert!(signing_string.starts_with("date: "));
    assert!(signing_string.contains("\n(request-target): get /foo/machines"));
}

/// KeyId format test (without subuser)
#[test]
fn test_key_id_format() {
    let signer = RequestSigner::new("foo", ID_RSA_MD5, KeyType::Rsa);
    assert_eq!(signer.key_id_string(), format!("/foo/keys/{}", ID_RSA_MD5));
}

/// Authorization header format test
#[test]
fn test_authorization_header_format() {
    let signer = RequestSigner::new("foo", ID_RSA_MD5, KeyType::Rsa);
    let auth_header = signer.authorization_header("dGVzdHNpZw==");

    // Verify format: Signature keyId="...",algorithm="...",signature="..."
    assert!(auth_header.starts_with("Signature keyId=\""));
    assert!(auth_header.contains(&format!("keyId=\"/foo/keys/{}", ID_RSA_MD5)));
    assert!(auth_header.contains("algorithm=\"rsa-sha256\""));
    assert!(auth_header.contains("signature=\"dGVzdHNpZw==\""));
}
