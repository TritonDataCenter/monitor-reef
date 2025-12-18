// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! SSH agent key tests for triton-auth
//!
//! Mirrors: target/node-smartdc-auth/test/agent-keys.test.js
//!
//! Tests SSH agent integration for signing operations.
//!
//! Note: These tests require a running SSH agent with keys loaded.
//! Tests are designed to pass gracefully when no agent is available.

use std::path::PathBuf;
use triton_auth::{
    fingerprint::md5_fingerprint_bytes, key_loader::KeyLoader, signature::encode_signature,
};

/// Test key fingerprints from node-smartdc-auth test suite
const ID_RSA_MD5: &str = "fa:56:a1:6b:cc:04:97:fe:e2:98:54:c4:2e:0d:26:c6";
const ID_DSA_MD5: &str = "a6:e6:68:d3:28:2b:0a:a0:12:54:da:c4:c0:22:8d:ba";
const ID_ECDSA_MD5: &str = "00:74:32:ae:0a:24:3c:7a:e7:07:b8:ee:91:c4:c7:27";

/// Known signature for "foobar" with id_rsa using RSA-SHA256
const SIG_RSA_SHA256: &str = "KX1okEE5wWjgrDYM35z9sO49WRk/DeZy7QeSNCFdOsn45BO6rVOIH5v\
V7WD25/VWyGCiN86Pml/Eulhx3Xx4ZUEHHc18K0BAKU5CSu/jCRI0dEFt4q1bXCyM7aK\
FlAXpk7CJIM0Gx91CJEXcZFuUddngoqljyt9hu4dpMhrjVFA=";

fn test_keys_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/keys")
}

// ============================================================================
// Agent Connection Tests (agent-keys.test.js lines 40-48)
// ============================================================================

/// Mirrors: 'agentsigner throws with no agent' test
/// When no SSH agent is available, operations should fail gracefully
#[tokio::test]
async fn test_no_agent_returns_error() {
    // Clear agent environment to simulate no agent
    let old_sock = std::env::var("SSH_AUTH_SOCK").ok();

    // SAFETY: We're in a test environment and immediately restore the value
    unsafe {
        std::env::remove_var("SSH_AUTH_SOCK");
    }

    let result = triton_auth::agent::find_key_in_agent(ID_RSA_MD5).await;

    // Restore environment
    // SAFETY: We're in a test environment restoring the original value
    if let Some(sock) = old_sock {
        unsafe {
            std::env::set_var("SSH_AUTH_SOCK", sock);
        }
    }

    // Should fail when no agent socket is set
    assert!(
        result.is_err(),
        "Should fail when SSH agent is not available"
    );
}

// ============================================================================
// Agent Key Tests (agent-keys.test.js lines 77-148)
// These tests require an SSH agent with keys loaded - they skip gracefully
// ============================================================================

/// Helper to check if agent is available and has the test key
async fn agent_has_key(fingerprint: &str) -> bool {
    triton_auth::agent::find_key_in_agent(fingerprint)
        .await
        .is_ok()
}

/// Mirrors: 'agentsigner rsa' test
/// Signs "foobar" using RSA key from agent
#[tokio::test]
async fn test_agent_signer_rsa() {
    if !agent_has_key(ID_RSA_MD5).await {
        eprintln!(
            "Skipping test_agent_signer_rsa: RSA key not in agent. \
            Add key with: ssh-add {}",
            test_keys_dir().join("id_rsa").display()
        );
        return;
    }

    // Find key in agent
    let pub_key = triton_auth::agent::find_key_in_agent(ID_RSA_MD5)
        .await
        .expect("Key should be in agent");

    // Verify algorithm
    let key_type = triton_auth::signature::KeyType::from_public_key(&pub_key);
    assert_eq!(key_type.algorithm_string(), "rsa-sha256");

    // Sign "foobar"
    let data = b"foobar";
    let sig_bytes = triton_auth::agent::sign_with_agent(ID_RSA_MD5, data)
        .await
        .expect("Agent signing should succeed");

    let signature = encode_signature(&sig_bytes);

    // RSA signatures are deterministic, so should match test vector
    assert_eq!(
        signature, SIG_RSA_SHA256,
        "RSA-SHA256 signature from agent should match test vector"
    );
}

/// Mirrors: 'agentsigner dsa' test
/// Signs "foobar" using DSA key from agent
#[tokio::test]
async fn test_agent_signer_dsa() {
    if !agent_has_key(ID_DSA_MD5).await {
        eprintln!(
            "Skipping test_agent_signer_dsa: DSA key not in agent. \
            Add key with: ssh-add {}",
            test_keys_dir().join("id_dsa").display()
        );
        return;
    }

    // Sign "foobar"
    let data = b"foobar";
    let sig_bytes = triton_auth::agent::sign_with_agent(ID_DSA_MD5, data)
        .await
        .expect("Agent signing should succeed");

    let signature = encode_signature(&sig_bytes);

    // DSA signatures are not deterministic, just verify it's valid base64
    assert!(!signature.is_empty());
    assert!(base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &signature).is_ok());
}

/// Mirrors: 'agentsigner ecdsa + buffer' test
/// Signs random data using ECDSA key from agent
#[tokio::test]
async fn test_agent_signer_ecdsa() {
    if !agent_has_key(ID_ECDSA_MD5).await {
        eprintln!(
            "Skipping test_agent_signer_ecdsa: ECDSA key not in agent. \
            Add key with: ssh-add {}",
            test_keys_dir().join("id_ecdsa").display()
        );
        return;
    }

    // Sign some data
    let data = b"test data for ECDSA agent signing";
    let sig_bytes = triton_auth::agent::sign_with_agent(ID_ECDSA_MD5, data)
        .await
        .expect("Agent signing should succeed");

    let signature = encode_signature(&sig_bytes);

    // ECDSA signatures are not deterministic, just verify it's valid base64
    assert!(!signature.is_empty());
    assert!(base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &signature).is_ok());
}

/// Mirrors: 'agentsigner with empty agent' test
/// Verifies error when key is not in agent
#[tokio::test]
async fn test_agent_key_not_found() {
    // Use a fingerprint that definitely won't be in the agent
    let fake_fp = "00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00";

    let result = triton_auth::agent::find_key_in_agent(fake_fp).await;

    // Should return KeyNotFound error
    assert!(result.is_err(), "Should fail when key is not in agent");
}

// ============================================================================
// Fallback Tests (agent-keys.test.js lines 151-168)
// ============================================================================

/// Mirrors: 'clisigner with only agent' test
/// When agent has key, file fallback is not needed
#[tokio::test]
async fn test_agent_preferred_over_file() {
    if !agent_has_key(ID_RSA_MD5).await {
        eprintln!("Skipping test_agent_preferred_over_file: RSA key not in agent");
        return;
    }

    // When key is in agent, we should be able to sign without loading from file
    let data = b"test data";
    let sig_result = triton_auth::agent::sign_with_agent(ID_RSA_MD5, data).await;

    assert!(
        sig_result.is_ok(),
        "Should sign with agent when key is available"
    );
}

// ============================================================================
// Comparison Tests: Agent vs File Signing
// ============================================================================

/// Verifies that signing with agent produces same result as file-based signing (for RSA)
#[tokio::test]
async fn test_agent_matches_file_signing_rsa() {
    if !agent_has_key(ID_RSA_MD5).await {
        eprintln!("Skipping test_agent_matches_file_signing_rsa: RSA key not in agent");
        return;
    }

    let data = b"foobar";

    // Sign with agent
    let agent_sig = triton_auth::agent::sign_with_agent(ID_RSA_MD5, data)
        .await
        .expect("Agent signing should succeed");

    // Sign with file
    let key_path = test_keys_dir().join("id_rsa");
    let key = KeyLoader::load_legacy_from_file(&key_path, None).expect("Failed to load RSA key");
    let file_sig = key.sign(data).expect("File signing should succeed");

    // RSA signatures are deterministic, so they should match
    assert_eq!(
        agent_sig, file_sig,
        "Agent and file-based RSA signatures should match"
    );
}

/// Verifies fingerprints match between agent and file-loaded keys
#[tokio::test]
async fn test_agent_fingerprint_matches_file() {
    if !agent_has_key(ID_RSA_MD5).await {
        eprintln!("Skipping test_agent_fingerprint_matches_file: RSA key not in agent");
        return;
    }

    // Get fingerprint from agent key
    let pub_key = triton_auth::agent::find_key_in_agent(ID_RSA_MD5)
        .await
        .expect("Key should be in agent");
    let agent_fp = triton_auth::fingerprint::md5_fingerprint(&pub_key);

    // Get fingerprint from file-loaded key
    let key_path = test_keys_dir().join("id_rsa");
    let key = KeyLoader::load_legacy_from_file(&key_path, None).expect("Failed to load RSA key");
    let pub_blob = key
        .public_key_blob()
        .expect("Failed to get public key blob");
    let file_fp = md5_fingerprint_bytes(&pub_blob);

    assert_eq!(
        agent_fp, file_fp,
        "Agent and file fingerprints should match"
    );
}
