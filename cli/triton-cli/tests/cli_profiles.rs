// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Profile CLI tests - read-only operations
//!
//! Ported from node-triton test/integration/cli-profiles.test.js
//!
//! Note: Write operations (profile create/delete) require `allow_write_actions`
//! and are implemented in a separate test file.

// Allow deprecated - cargo_bin is standard for CLI testing
#![allow(deprecated)]

mod common;

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::Value;

fn triton_cmd() -> Command {
    Command::cargo_bin("triton").expect("Failed to find triton binary")
}

/// Test profile get with "env" - should read from environment variables
///
/// Equivalent to Node.js test:
/// ```js
/// h.safeTriton(t, {json: true, args: ['profile', 'get', '-j', 'env']}, function(err, p) {
///     t.equal(p.account, h.CONFIG.profile.account, 'env account correct');
///     t.equal(p.keyId, h.CONFIG.profile.keyId, 'env keyId correct');
///     t.equal(p.url, h.CONFIG.profile.url, 'env url correct');
/// });
/// ```
#[test]
fn test_profile_get_env() {
    let test_url = "https://cloudapi.test.example.com";
    let test_account = "test-account";
    let test_key_id = "00:11:22:33:44:55:66:77:88:99:aa:bb:cc:dd:ee:ff";

    let output = triton_cmd()
        .args(["profile", "get", "-j", "env"])
        .env("TRITON_URL", test_url)
        .env("TRITON_ACCOUNT", test_account)
        .env("TRITON_KEY_ID", test_key_id)
        // Clear any saved profile that might interfere
        .env("HOME", "/nonexistent")
        .output()
        .expect("Failed to run command");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "Command should succeed.\nstdout: {}\nstderr: {}",
        stdout,
        stderr
    );

    let profile: Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|_| panic!("Should parse JSON output: {}", stdout));

    assert_eq!(profile["name"], "env", "Profile name should be 'env'");
    assert_eq!(profile["url"], test_url, "URL should match TRITON_URL");
    assert_eq!(
        profile["account"], test_account,
        "Account should match TRITON_ACCOUNT"
    );
    assert_eq!(
        profile["keyId"], test_key_id,
        "Key ID should match TRITON_KEY_ID"
    );
}

/// Test profile get env with SDC_* environment variables (legacy)
#[test]
fn test_profile_get_env_sdc_vars() {
    let test_url = "https://cloudapi.sdc.example.com";
    let test_account = "sdc-account";
    let test_key_id = "aa:bb:cc:dd:ee:ff:00:11:22:33:44:55:66:77:88:99";

    let output = triton_cmd()
        .args(["profile", "get", "-j", "env"])
        // Clear TRITON_* vars first (they take precedence over SDC_*)
        .env_remove("TRITON_URL")
        .env_remove("TRITON_ACCOUNT")
        .env_remove("TRITON_KEY_ID")
        .env_remove("TRITON_USER")
        .env_remove("TRITON_TLS_INSECURE")
        // Set SDC_* vars
        .env("SDC_URL", test_url)
        .env("SDC_ACCOUNT", test_account)
        .env("SDC_KEY_ID", test_key_id)
        // Clear any saved profile that might interfere
        .env("HOME", "/nonexistent")
        .output()
        .expect("Failed to run command");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "Command should succeed with SDC_* vars.\nstdout: {}\nstderr: {}",
        stdout,
        stderr
    );

    let profile: Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|_| panic!("Should parse JSON output: {}", stdout));

    assert_eq!(profile["name"], "env");
    assert_eq!(profile["url"], test_url);
    assert_eq!(profile["account"], test_account);
    assert_eq!(profile["keyId"], test_key_id);
}

/// Test profile get env with optional user field
#[test]
fn test_profile_get_env_with_user() {
    let output = triton_cmd()
        .args(["profile", "get", "-j", "env"])
        .env("TRITON_URL", "https://cloudapi.test.example.com")
        .env("TRITON_ACCOUNT", "test-account")
        .env(
            "TRITON_KEY_ID",
            "00:11:22:33:44:55:66:77:88:99:aa:bb:cc:dd:ee:ff",
        )
        .env("TRITON_USER", "subuser")
        .env("HOME", "/nonexistent")
        .output()
        .expect("Failed to run command");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success());

    let profile: Value = serde_json::from_str(&stdout).expect("Should parse JSON");
    assert_eq!(profile["user"], "subuser");
}

/// Test profile get env with insecure flag
#[test]
fn test_profile_get_env_insecure() {
    let output = triton_cmd()
        .args(["profile", "get", "-j", "env"])
        // Clear any existing SDC_* vars
        .env_remove("SDC_URL")
        .env_remove("SDC_ACCOUNT")
        .env_remove("SDC_KEY_ID")
        .env_remove("SDC_TLS_INSECURE")
        // Set TRITON_* vars
        .env("TRITON_URL", "https://cloudapi.test.example.com")
        .env("TRITON_ACCOUNT", "test-account")
        .env(
            "TRITON_KEY_ID",
            "00:11:22:33:44:55:66:77:88:99:aa:bb:cc:dd:ee:ff",
        )
        .env("TRITON_TLS_INSECURE", "true")
        .env("HOME", "/nonexistent")
        .output()
        .expect("Failed to run command");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "Command should succeed.\nstdout: {}\nstderr: {}",
        stdout,
        stderr
    );

    let profile: Value = serde_json::from_str(&stdout).expect("Should parse JSON");
    assert_eq!(profile["insecure"], true);
}

/// Test profile get env fails when required vars are missing
#[test]
fn test_profile_get_env_missing_vars() {
    // Missing all required vars
    triton_cmd()
        .args(["profile", "get", "env"])
        .env("HOME", "/nonexistent")
        .env_remove("TRITON_URL")
        .env_remove("SDC_URL")
        .env_remove("TRITON_ACCOUNT")
        .env_remove("SDC_ACCOUNT")
        .env_remove("TRITON_KEY_ID")
        .env_remove("SDC_KEY_ID")
        .assert()
        .failure()
        .stderr(predicate::str::contains("TRITON_URL").or(predicate::str::contains("SDC_URL")));
}

/// Test profile list includes env profile when env vars are set
#[test]
fn test_profile_list_shows_env() {
    let output = triton_cmd()
        .args(["profile", "list", "-j"])
        .env("TRITON_URL", "https://cloudapi.test.example.com")
        .env("TRITON_ACCOUNT", "test-account")
        .env(
            "TRITON_KEY_ID",
            "00:11:22:33:44:55:66:77:88:99:aa:bb:cc:dd:ee:ff",
        )
        .env("HOME", "/nonexistent")
        .output()
        .expect("Failed to run command");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success());

    // node-triton outputs NDJSON (one JSON object per line), not a JSON array
    let profiles: Vec<Value> = stdout
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_str(line).expect("Should parse JSON line"))
        .collect();

    // Should have at least the env profile
    assert!(!profiles.is_empty(), "Should have at least one profile");

    // Find the env profile
    let env_profile = profiles.iter().find(|p| p["name"] == "env");
    assert!(
        env_profile.is_some(),
        "Should include 'env' profile when env vars are set"
    );
}

/// Test profile list works with empty HOME (no saved profiles)
#[test]
fn test_profile_list_empty() {
    let output = triton_cmd()
        .args(["profile", "list", "-j"])
        .env("HOME", "/nonexistent")
        .env_remove("TRITON_URL")
        .env_remove("SDC_URL")
        .output()
        .expect("Failed to run command");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success());

    // node-triton outputs NDJSON (one JSON object per line), not a JSON array
    // With no profiles, output should be empty
    let profiles: Vec<Value> = stdout
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_str(line).expect("Should parse JSON line"))
        .collect();
    assert!(profiles.is_empty(), "Should be empty with no profiles");
}

/// Test profile list help
#[test]
fn test_profile_list_help() {
    triton_cmd()
        .args(["profile", "list", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test profile get help
#[test]
fn test_profile_get_help() {
    triton_cmd()
        .args(["profile", "get", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test profile ls alias (alias for list)
#[test]
fn test_profile_ls_alias() {
    triton_cmd()
        .args(["profile", "ls", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}
