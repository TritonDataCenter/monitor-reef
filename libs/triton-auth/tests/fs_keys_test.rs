// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Filesystem key loading tests for triton-auth
//!
//! Mirrors: target/node-smartdc-auth/test/fs-keys.test.js
//!
//! Tests loading SSH keys from files in various formats.

use std::path::PathBuf;
use triton_auth::{
    error::AuthError,
    fingerprint::{md5_fingerprint_bytes, parse_fingerprint},
    key_loader::KeyLoader,
    legacy_pem::PemKeyFormat,
    signature::KeyType,
};

/// Test key fingerprints for OpenSSH-format ed25519 key
const ID_ED25519_MD5: &str = "4c:2d:7d:ef:1a:f7:37:1a:9e:d8:e8:27:5d:c0:3a:40";

/// Test key fingerprints from node-smartdc-auth test suite
const ID_RSA_MD5: &str = "fa:56:a1:6b:cc:04:97:fe:e2:98:54:c4:2e:0d:26:c6";
const ID_DSA_MD5: &str = "a6:e6:68:d3:28:2b:0a:a0:12:54:da:c4:c0:22:8d:ba";
const ID_ECDSA_MD5: &str = "00:74:32:ae:0a:24:3c:7a:e7:07:b8:ee:91:c4:c7:27";
const ID_RSA2_MD5: &str = "9f:cf:50:5b:df:c2:c5:2a:ad:ad:96:38:31:a5:0d:9e";

fn test_keys_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/keys")
}

// ============================================================================
// Key Loading Tests (fs-keys.test.js lines 53-103)
// ============================================================================

/// Mirrors: 'loadSSHKey full pair' test
/// Verifies RSA key loading and properties
#[tokio::test]
async fn test_load_rsa_key() {
    let key_path = test_keys_dir().join("id_rsa");
    let key = KeyLoader::load_legacy_from_file(&key_path, None)
        .await
        .expect("Failed to load RSA key");

    // Verify key type
    let key_type = key.key_type().unwrap();
    assert!(matches!(key_type, KeyType::Rsa));
}

/// Mirrors: 'loadSSHKey private only dsa' test
/// Verifies DSA key loading
#[tokio::test]
async fn test_load_dsa_key() {
    let key_path = test_keys_dir().join("id_dsa");
    let key = KeyLoader::load_legacy_from_file(&key_path, None)
        .await
        .expect("Failed to load DSA key");

    // Verify key type
    let key_type = key.key_type().unwrap();
    assert!(matches!(key_type, KeyType::Dsa));
}

/// Verifies ECDSA key loading (P-256)
#[tokio::test]
async fn test_load_ecdsa_key() {
    let key_path = test_keys_dir().join("id_ecdsa");
    let key = KeyLoader::load_legacy_from_file(&key_path, None)
        .await
        .expect("Failed to load ECDSA key");

    // Verify key type - the test key is P-256
    let key_type = key.key_type().unwrap();
    assert!(matches!(key_type, KeyType::Ecdsa256));
}

// ============================================================================
// Fingerprint Tests
// ============================================================================

/// Verifies RSA key MD5 fingerprint matches expected value
#[tokio::test]
async fn test_rsa_key_fingerprint() {
    let key_path = test_keys_dir().join("id_rsa");
    let key = KeyLoader::load_legacy_from_file(&key_path, None)
        .await
        .expect("Failed to load RSA key");
    let pub_blob = key
        .public_key_blob()
        .expect("Failed to get public key blob");
    let fp = md5_fingerprint_bytes(&pub_blob);
    assert_eq!(fp, ID_RSA_MD5, "RSA key MD5 fingerprint mismatch");
}

/// Verifies DSA key MD5 fingerprint matches expected value
#[tokio::test]
async fn test_dsa_key_fingerprint() {
    let key_path = test_keys_dir().join("id_dsa");
    let key = KeyLoader::load_legacy_from_file(&key_path, None)
        .await
        .expect("Failed to load DSA key");
    let pub_blob = key
        .public_key_blob()
        .expect("Failed to get public key blob");
    let fp = md5_fingerprint_bytes(&pub_blob);
    assert_eq!(fp, ID_DSA_MD5, "DSA key MD5 fingerprint mismatch");
}

/// Verifies ECDSA key MD5 fingerprint matches expected value
#[tokio::test]
async fn test_ecdsa_key_fingerprint() {
    let key_path = test_keys_dir().join("id_ecdsa");
    let key = KeyLoader::load_legacy_from_file(&key_path, None)
        .await
        .expect("Failed to load ECDSA key");
    let pub_blob = key
        .public_key_blob()
        .expect("Failed to get public key blob");
    let fp = md5_fingerprint_bytes(&pub_blob);
    assert_eq!(fp, ID_ECDSA_MD5, "ECDSA key MD5 fingerprint mismatch");
}

/// Verifies fingerprint parsing
#[test]
fn test_parse_md5_fingerprint() {
    let bytes = parse_fingerprint(ID_RSA_MD5).expect("Failed to parse fingerprint");
    assert_eq!(bytes[0], 0xfa);
    assert_eq!(bytes[1], 0x56);
    assert_eq!(bytes[15], 0xc6);
}

// ============================================================================
// Encrypted Key Tests (fs-keys.test.js lines 116-167)
// ============================================================================

/// Verifies that encrypted keys return KeyEncrypted without passphrase
#[tokio::test]
async fn test_encrypted_key_without_passphrase_fails() {
    let key_path = test_keys_dir().join("id_rsa2");
    let result = KeyLoader::load_legacy_from_file(&key_path, None).await;
    match &result {
        Err(AuthError::KeyEncrypted(_)) => {} // expected
        Err(e) => panic!("Expected KeyEncrypted, got error: {}", e),
        Ok(_) => panic!("Expected KeyEncrypted, but key loaded successfully"),
    }
}

/// Mirrors: 'keyring unlock' test
/// Encrypted PKCS#1 key decrypts with correct passphrase
#[tokio::test]
async fn test_encrypted_key_with_passphrase() {
    // id_rsa2 is encrypted with passphrase "asdfasdf"
    let key_path = test_keys_dir().join("id_rsa2");
    let key = KeyLoader::load_legacy_from_file(&key_path, Some("asdfasdf"))
        .await
        .expect("Should decrypt encrypted key with correct passphrase");

    // Verify key type is RSA
    assert!(matches!(key.key_type().unwrap(), KeyType::Rsa));

    // Verify fingerprint matches
    let pub_blob = key.public_key_blob().expect("Should get public key blob");
    let fp = md5_fingerprint_bytes(&pub_blob);
    assert_eq!(fp, ID_RSA2_MD5, "Decrypted key fingerprint should match");
}

/// Encrypted key with wrong passphrase fails
#[tokio::test]
async fn test_encrypted_key_wrong_passphrase() {
    let key_path = test_keys_dir().join("id_rsa2");
    let result = KeyLoader::load_legacy_from_file(&key_path, Some("wrongpassword")).await;
    assert!(result.is_err(), "Wrong passphrase should fail");
}

/// Tests encrypted key generated from id_rsa (passphrase "testpass123")
#[tokio::test]
async fn test_encrypted_key_decrypts_to_same_fingerprint() {
    let key_path = test_keys_dir().join("id_rsa_encrypted_test.pem");
    let key = KeyLoader::load_legacy_from_file(&key_path, Some("testpass123"))
        .await
        .expect("Should decrypt encrypted test key");

    // Should produce the same fingerprint as the unencrypted id_rsa
    let pub_blob = key.public_key_blob().expect("Should get public key blob");
    let fp = md5_fingerprint_bytes(&pub_blob);
    assert_eq!(
        fp, ID_RSA_MD5,
        "Decrypted key should have same fingerprint as original"
    );
}

// ============================================================================
// Key Format Detection Tests
// ============================================================================

/// Verifies PEM format detection
#[test]
fn test_pem_format_detection() {
    assert_eq!(
        PemKeyFormat::detect("-----BEGIN OPENSSH PRIVATE KEY-----"),
        PemKeyFormat::OpenSsh
    );
    assert_eq!(
        PemKeyFormat::detect("-----BEGIN RSA PRIVATE KEY-----"),
        PemKeyFormat::Pkcs1Rsa
    );
    assert_eq!(
        PemKeyFormat::detect("-----BEGIN EC PRIVATE KEY-----"),
        PemKeyFormat::Sec1Ecdsa
    );
    assert_eq!(
        PemKeyFormat::detect("-----BEGIN DSA PRIVATE KEY-----"),
        PemKeyFormat::Dsa
    );
    assert_eq!(
        PemKeyFormat::detect("-----BEGIN PRIVATE KEY-----"),
        PemKeyFormat::Pkcs8
    );
}

/// Verifies encrypted key format detection
#[test]
fn test_encrypted_pem_format_detection() {
    let encrypted_pem = "-----BEGIN RSA PRIVATE KEY-----\nProc-Type: 4,ENCRYPTED\nDEK-Info: AES";
    assert_eq!(
        PemKeyFormat::detect(encrypted_pem),
        PemKeyFormat::EncryptedPkcs1
    );
}

// ============================================================================
// Error Cases (fs-keys.test.js lines 63-69, signers.test.js lines 249-276)
// ============================================================================

/// Verifies loading a non-existent key file fails appropriately
#[tokio::test]
async fn test_load_nonexistent_key_fails() {
    let key_path = test_keys_dir().join("nonexistent_key");
    let result = KeyLoader::load_legacy_from_file(&key_path, None).await;
    assert!(result.is_err(), "Should fail to load non-existent key");
}

/// Verifies invalid fingerprint format is detected
#[test]
fn test_parse_invalid_fingerprint_fails() {
    // Too short
    let result = parse_fingerprint("aa:bb:cc");
    assert!(result.is_err(), "Should fail on short fingerprint");

    // Too long
    let result = parse_fingerprint(&format!("{}:aa", ID_RSA_MD5));
    assert!(result.is_err(), "Should fail on long fingerprint");

    // Invalid hex
    let result = parse_fingerprint("gg:hh:ii:jj:kk:ll:mm:nn:oo:pp:qq:rr:ss:tt:uu:vv");
    assert!(result.is_err(), "Should fail on invalid hex");
}

// ============================================================================
// scan_ssh_dir_for_key tests (auto-discovery of keys by fingerprint)
// ============================================================================

/// RSA key found via .pub file scan
#[tokio::test]
async fn test_scan_finds_rsa_via_pub() {
    let tmp_dir = tempfile::tempdir().unwrap();
    let ssh_dir = tmp_dir.path().join(".ssh");
    tokio::fs::create_dir_all(&ssh_dir).await.unwrap();

    let src = test_keys_dir();
    tokio::fs::copy(src.join("id_rsa"), ssh_dir.join("id_rsa"))
        .await
        .unwrap();
    tokio::fs::copy(src.join("id_rsa.pub"), ssh_dir.join("id_rsa.pub"))
        .await
        .unwrap();

    let key = KeyLoader::scan_ssh_dir_for_key(&ssh_dir, ID_RSA_MD5)
        .await
        .expect("Should find RSA key via .pub scan");

    assert!(matches!(key.key_type().unwrap(), KeyType::Rsa));
}

/// PKCS#1 RSA key discovered via fallback (no .pub file)
///
/// This is the core bug from monitor-reef-e56: load_from_common_paths
/// previously only tried OpenSSH-format loading, which skips PKCS#1 keys.
#[tokio::test]
async fn test_scan_finds_pkcs1_rsa_key_fallback() {
    let tmp_dir = tempfile::tempdir().unwrap();
    let ssh_dir = tmp_dir.path().join(".ssh");
    tokio::fs::create_dir_all(&ssh_dir).await.unwrap();

    // No .pub file — fallback to loading private key directly
    let src = test_keys_dir().join("id_rsa");
    tokio::fs::copy(&src, ssh_dir.join("id_rsa")).await.unwrap();

    let key = KeyLoader::scan_ssh_dir_for_key(&ssh_dir, ID_RSA_MD5)
        .await
        .expect("Should find PKCS#1 RSA key via fallback scan");

    assert!(matches!(key.key_type().unwrap(), KeyType::Rsa));
}

/// DSA key discovered via .pub scan
#[tokio::test]
async fn test_scan_finds_dsa_key() {
    let tmp_dir = tempfile::tempdir().unwrap();
    let ssh_dir = tmp_dir.path().join(".ssh");
    tokio::fs::create_dir_all(&ssh_dir).await.unwrap();

    let src = test_keys_dir();
    tokio::fs::copy(src.join("id_dsa"), ssh_dir.join("id_dsa"))
        .await
        .unwrap();
    tokio::fs::copy(src.join("id_dsa.pub"), ssh_dir.join("id_dsa.pub"))
        .await
        .unwrap();

    let key = KeyLoader::scan_ssh_dir_for_key(&ssh_dir, ID_DSA_MD5)
        .await
        .expect("Should find DSA key via scan");

    assert!(matches!(key.key_type().unwrap(), KeyType::Dsa));
}

/// OpenSSH ed25519 key still works (regression guard) — no .pub, fallback path
#[tokio::test]
async fn test_scan_finds_openssh_ed25519_key() {
    let tmp_dir = tempfile::tempdir().unwrap();
    let ssh_dir = tmp_dir.path().join(".ssh");
    tokio::fs::create_dir_all(&ssh_dir).await.unwrap();

    let src = test_keys_dir().join("id_ed25519");
    tokio::fs::copy(&src, ssh_dir.join("id_ed25519"))
        .await
        .unwrap();

    let key = KeyLoader::scan_ssh_dir_for_key(&ssh_dir, ID_ED25519_MD5)
        .await
        .expect("Should find OpenSSH ed25519 key via scan");

    assert!(matches!(key.key_type().unwrap(), KeyType::Ed25519));
}

/// Encrypted key with .pub companion returns KeyEncrypted (not KeyNotFound)
#[tokio::test]
async fn test_scan_encrypted_key_with_pub_returns_key_encrypted() {
    let tmp_dir = tempfile::tempdir().unwrap();
    let ssh_dir = tmp_dir.path().join(".ssh");
    tokio::fs::create_dir_all(&ssh_dir).await.unwrap();

    let src = test_keys_dir();
    // Copy encrypted id_rsa2 and its .pub file
    tokio::fs::copy(src.join("id_rsa2"), ssh_dir.join("id_rsa2"))
        .await
        .unwrap();
    tokio::fs::copy(src.join("id_rsa2.pub"), ssh_dir.join("id_rsa2.pub"))
        .await
        .unwrap();

    let result = KeyLoader::scan_ssh_dir_for_key(&ssh_dir, ID_RSA2_MD5).await;
    match &result {
        Err(AuthError::KeyEncrypted(_)) => {} // expected
        Err(e) => panic!("Expected KeyEncrypted, got error: {}", e),
        Ok(_) => panic!("Expected KeyEncrypted, but key loaded successfully"),
    }
}

/// Non-standard filename: key found via .pub file with custom name
#[tokio::test]
async fn test_scan_finds_non_standard_filename() {
    let tmp_dir = tempfile::tempdir().unwrap();
    let ssh_dir = tmp_dir.path().join(".ssh");
    tokio::fs::create_dir_all(&ssh_dir).await.unwrap();

    let src = test_keys_dir();
    // Copy RSA key with non-standard names
    tokio::fs::copy(src.join("id_rsa"), ssh_dir.join("my_custom_key"))
        .await
        .unwrap();
    tokio::fs::copy(src.join("id_rsa.pub"), ssh_dir.join("my_custom_key.pub"))
        .await
        .unwrap();

    let key = KeyLoader::scan_ssh_dir_for_key(&ssh_dir, ID_RSA_MD5)
        .await
        .expect("Should find key with non-standard filename via .pub scan");

    assert!(matches!(key.key_type().unwrap(), KeyType::Rsa));
}

/// Non-matching fingerprint returns error
#[tokio::test]
async fn test_scan_no_match_returns_error() {
    let tmp_dir = tempfile::tempdir().unwrap();
    let ssh_dir = tmp_dir.path().join(".ssh");
    tokio::fs::create_dir_all(&ssh_dir).await.unwrap();

    let src = test_keys_dir();
    tokio::fs::copy(src.join("id_rsa"), ssh_dir.join("id_rsa"))
        .await
        .unwrap();
    tokio::fs::copy(src.join("id_rsa.pub"), ssh_dir.join("id_rsa.pub"))
        .await
        .unwrap();

    // Use a fingerprint that doesn't match any key in the dir
    let result = KeyLoader::scan_ssh_dir_for_key(
        &ssh_dir,
        "00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00",
    )
    .await;
    assert!(
        result.is_err(),
        "Should fail when no key matches fingerprint"
    );
}
