// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Profile CLI tests - read-only operations
//!
//! Ported from node-triton test/integration/cli-profiles.test.js
//!
//! Note: Write operations (profile create/delete) require `allow_write_actions`
//! and are implemented in a separate test file.

// Allow deprecated - cargo_bin is standard for CLI testing
#![allow(deprecated, clippy::expect_used)]

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
///
/// Uses TRITON_CONFIG_DIR to ensure no profiles are found, since
/// dirs::home_dir() may resolve the real home via the password
/// database even when HOME is overridden.
#[test]
fn test_profile_list_empty() {
    let output = triton_cmd()
        .args(["profile", "list", "-j"])
        .env("HOME", "/nonexistent")
        .env("TRITON_CONFIG_DIR", "/nonexistent/.triton")
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

/// Test profile get (no name) falls back to env vars
///
/// Regression: `triton profile get` with no name used to fail when no
/// saved profile existed, even if TRITON_* env vars were set. It now
/// uses `resolve_profile()` which checks env vars at step 4.
#[test]
fn test_profile_get_default_uses_env_vars() {
    let test_url = "https://cloudapi.test.example.com";
    let test_account = "test-account";
    let test_key_id = "00:11:22:33:44:55:66:77:88:99:aa:bb:cc:dd:ee:ff";

    let output = triton_cmd()
        .args(["profile", "get"])
        .env("TRITON_URL", test_url)
        .env("TRITON_ACCOUNT", test_account)
        .env("TRITON_KEY_ID", test_key_id)
        .env_remove("TRITON_PROFILE")
        .env("HOME", "/nonexistent")
        .env("TRITON_CONFIG_DIR", "/nonexistent/.triton")
        .output()
        .expect("Failed to run command");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "profile get (no name) should succeed with env vars.\nstdout: {}\nstderr: {}",
        stdout,
        stderr
    );

    // Text output should match node-triton format
    assert!(
        stdout.contains("name: env"),
        "Should show 'name: env' in text output. Got: {}",
        stdout
    );
    assert!(
        stdout.contains("curr: true"),
        "Should show 'curr: true' in text output. Got: {}",
        stdout
    );
}

/// Test profile get -j includes `curr` field
#[test]
fn test_profile_get_json_includes_curr() {
    let output = triton_cmd()
        .args(["profile", "get", "-j", "env"])
        .env("TRITON_URL", "https://cloudapi.test.example.com")
        .env("TRITON_ACCOUNT", "test-account")
        .env(
            "TRITON_KEY_ID",
            "00:11:22:33:44:55:66:77:88:99:aa:bb:cc:dd:ee:ff",
        )
        .env_remove("TRITON_PROFILE")
        .env("HOME", "/nonexistent")
        .env("TRITON_CONFIG_DIR", "/nonexistent/.triton")
        .output()
        .expect("Failed to run command");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success());

    let profile: Value = serde_json::from_str(&stdout).expect("Should parse JSON");
    assert!(
        profile.get("curr").is_some(),
        "JSON output should include 'curr' field. Got: {}",
        stdout
    );
}

/// Test profile get -j omits insecure when false
#[test]
fn test_profile_get_json_omits_insecure_false() {
    let output = triton_cmd()
        .args(["profile", "get", "-j", "env"])
        .env("TRITON_URL", "https://cloudapi.test.example.com")
        .env("TRITON_ACCOUNT", "test-account")
        .env(
            "TRITON_KEY_ID",
            "00:11:22:33:44:55:66:77:88:99:aa:bb:cc:dd:ee:ff",
        )
        .env_remove("TRITON_TLS_INSECURE")
        .env_remove("SDC_TLS_INSECURE")
        .env("HOME", "/nonexistent")
        .env("TRITON_CONFIG_DIR", "/nonexistent/.triton")
        .output()
        .expect("Failed to run command");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success());

    let profile: Value = serde_json::from_str(&stdout).expect("Should parse JSON");
    assert!(
        profile.get("insecure").is_none(),
        "JSON should omit 'insecure' when false. Got: {}",
        stdout
    );
}

/// Test profile list -j includes `curr` field on each profile
#[test]
fn test_profile_list_json_includes_curr() {
    let output = triton_cmd()
        .args(["profile", "list", "-j"])
        .env("TRITON_URL", "https://cloudapi.test.example.com")
        .env("TRITON_ACCOUNT", "test-account")
        .env(
            "TRITON_KEY_ID",
            "00:11:22:33:44:55:66:77:88:99:aa:bb:cc:dd:ee:ff",
        )
        .env("HOME", "/nonexistent")
        .env("TRITON_CONFIG_DIR", "/nonexistent/.triton")
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

    let profiles: Vec<Value> = stdout
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_str(line).expect("Should parse JSON line"))
        .collect();

    assert!(!profiles.is_empty(), "Should have at least one profile");

    for profile in &profiles {
        assert!(
            profile.get("curr").is_some(),
            "Each profile in JSON list should include 'curr' field. Got: {}",
            profile
        );
    }

    // The env profile should be current
    let env_profile = profiles
        .iter()
        .find(|p| p["name"] == "env")
        .expect("Should include env profile");
    assert_eq!(env_profile["curr"], true, "env profile should be current");
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

/// Test profile set alias (alias for set-current)
///
/// Node.js `triton profile set NAME` works as shorthand for `set-current`.
/// Verify the alias is recognized and routes to the same subcommand.
#[test]
fn test_profile_set_alias() {
    triton_cmd()
        .args(["profile", "set", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Set the current profile"));
}

/// Test `triton profile set-current env` succeeds when env vars are set
///
/// Rust previously failed with: `Error: Failed to read profile 'env': No such file or directory`
/// because the "env" profile is virtual (constructed from environment variables, no file on disk).
#[test]
fn test_profile_set_current_env() {
    let tmp_dir = tempfile::tempdir().expect("Failed to create temp dir");

    let config_dir = tmp_dir.path().join(".triton");

    let output = triton_cmd()
        .args(["profile", "set-current", "env"])
        .env("TRITON_URL", "https://cloudapi.test.example.com")
        .env("TRITON_ACCOUNT", "test-account")
        .env(
            "TRITON_KEY_ID",
            "00:11:22:33:44:55:66:77:88:99:aa:bb:cc:dd:ee:ff",
        )
        .env("HOME", tmp_dir.path())
        .env("TRITON_CONFIG_DIR", &config_dir)
        .env_remove("SDC_URL")
        .env_remove("SDC_ACCOUNT")
        .env_remove("SDC_KEY_ID")
        .env_remove("TRITON_PROFILE")
        .output()
        .expect("Failed to run command");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "set-current env should succeed.\nstdout: {}\nstderr: {}",
        stdout,
        stderr
    );
    assert!(
        stdout.contains("Set \"env\" as current profile"),
        "Should print 'Set' message when changing profile.\nstdout: {}",
        stdout
    );
}

/// Test `triton profile set-current env` prints "already current" when env is already set
///
/// Node.js: `triton profile set-current env` → `"env" is already the current profile`
#[test]
fn test_profile_set_current_already_current() {
    let tmp_dir = tempfile::tempdir().expect("Failed to create temp dir");
    let config_dir = tmp_dir.path().join(".triton");

    // First, set env as current profile
    triton_cmd()
        .args(["profile", "set-current", "env"])
        .env("TRITON_URL", "https://cloudapi.test.example.com")
        .env("TRITON_ACCOUNT", "test-account")
        .env(
            "TRITON_KEY_ID",
            "00:11:22:33:44:55:66:77:88:99:aa:bb:cc:dd:ee:ff",
        )
        .env("HOME", tmp_dir.path())
        .env("TRITON_CONFIG_DIR", &config_dir)
        .env_remove("SDC_URL")
        .env_remove("SDC_ACCOUNT")
        .env_remove("SDC_KEY_ID")
        .env_remove("TRITON_PROFILE")
        .assert()
        .success();

    // Second call should say "already the current profile"
    let output = triton_cmd()
        .args(["profile", "set-current", "env"])
        .env("TRITON_URL", "https://cloudapi.test.example.com")
        .env("TRITON_ACCOUNT", "test-account")
        .env(
            "TRITON_KEY_ID",
            "00:11:22:33:44:55:66:77:88:99:aa:bb:cc:dd:ee:ff",
        )
        .env("HOME", tmp_dir.path())
        .env("TRITON_CONFIG_DIR", &config_dir)
        .env_remove("SDC_URL")
        .env_remove("SDC_ACCOUNT")
        .env_remove("SDC_KEY_ID")
        .env_remove("TRITON_PROFILE")
        .output()
        .expect("Failed to run command");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "set-current env (already current) should succeed.\nstdout: {}\nstderr: {}",
        stdout,
        stderr
    );
    assert!(
        stdout.contains("\"env\" is already the current profile"),
        "Should print 'already current' message.\nstdout: {}",
        stdout
    );
}

/// Test that a saved profile set as current in config.json takes precedence
/// over the implicit "env" profile from environment variables.
///
/// Regression: `resolve_profile()` previously checked env vars (step 3)
/// before config.json (step 4), so `trs profile set-current mycloud`
/// followed by `trs profile get` would still resolve to "env".
#[test]
fn test_saved_profile_takes_precedence_over_env() {
    let tmp_dir = tempfile::tempdir().expect("Failed to create temp dir");
    let config_dir = tmp_dir.path().join(".triton");
    let profiles_dir = config_dir.join("profiles.d");
    std::fs::create_dir_all(&profiles_dir).expect("create profiles.d");

    // Write a saved profile
    std::fs::write(
        profiles_dir.join("mycloud.json"),
        r#"{
            "url": "https://saved.example.com",
            "account": "saved-account",
            "keyId": "aa:bb:cc:dd:ee:ff:00:11:22:33:44:55:66:77:88:99"
        }"#,
    )
    .expect("write profile");

    // Set mycloud as the current profile in config.json
    std::fs::write(config_dir.join("config.json"), r#"{"profile": "mycloud"}"#)
        .expect("write config.json");

    // Run with env vars also set (different URL/account)
    let output = triton_cmd()
        .args(["profile", "get", "-j"])
        .env("HOME", tmp_dir.path())
        .env("TRITON_CONFIG_DIR", &config_dir)
        .env("TRITON_URL", "https://env.example.com")
        .env("TRITON_ACCOUNT", "env-account")
        .env(
            "TRITON_KEY_ID",
            "00:11:22:33:44:55:66:77:88:99:aa:bb:cc:dd:ee:ff",
        )
        .env_remove("TRITON_PROFILE")
        .output()
        .expect("Failed to run command");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "profile get should succeed.\nstdout: {}\nstderr: {}",
        stdout,
        stderr
    );

    let profile: Value =
        serde_json::from_str(&stdout).unwrap_or_else(|_| panic!("Should parse JSON: {}", stdout));

    assert_eq!(
        profile["name"], "mycloud",
        "Saved profile should take precedence over env. Got: {}",
        stdout
    );
    assert_eq!(profile["url"], "https://saved.example.com");
    assert_eq!(profile["account"], "saved-account");
}

/// Test that `profile list` marks the saved current profile (not env) as current.
///
/// Regression: same root cause as test_saved_profile_takes_precedence_over_env.
#[test]
fn test_profile_list_saved_current_over_env() {
    let tmp_dir = tempfile::tempdir().expect("Failed to create temp dir");
    let config_dir = tmp_dir.path().join(".triton");
    let profiles_dir = config_dir.join("profiles.d");
    std::fs::create_dir_all(&profiles_dir).expect("create profiles.d");

    // Write a saved profile
    std::fs::write(
        profiles_dir.join("mycloud.json"),
        r#"{
            "url": "https://saved.example.com",
            "account": "saved-account",
            "keyId": "aa:bb:cc:dd:ee:ff:00:11:22:33:44:55:66:77:88:99"
        }"#,
    )
    .expect("write profile");

    // Set mycloud as the current profile in config.json
    std::fs::write(config_dir.join("config.json"), r#"{"profile": "mycloud"}"#)
        .expect("write config.json");

    // Run with env vars also set
    let output = triton_cmd()
        .args(["profile", "list", "-j"])
        .env("HOME", tmp_dir.path())
        .env("TRITON_CONFIG_DIR", &config_dir)
        .env("TRITON_URL", "https://env.example.com")
        .env("TRITON_ACCOUNT", "env-account")
        .env(
            "TRITON_KEY_ID",
            "00:11:22:33:44:55:66:77:88:99:aa:bb:cc:dd:ee:ff",
        )
        .env_remove("TRITON_PROFILE")
        .output()
        .expect("Failed to run command");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "profile list should succeed.\nstdout: {}\nstderr: {}",
        stdout,
        stderr
    );

    let profiles: Vec<Value> = stdout
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_str(line).expect("Should parse JSON line"))
        .collect();

    let mycloud = profiles
        .iter()
        .find(|p| p["name"] == "mycloud")
        .expect("Should include mycloud profile");
    assert_eq!(
        mycloud["curr"], true,
        "Saved current profile should have curr: true"
    );

    let env_profile = profiles
        .iter()
        .find(|p| p["name"] == "env")
        .expect("Should include env profile");
    assert_eq!(
        env_profile["curr"], false,
        "env profile should have curr: false when a saved profile is current"
    );
}

/// Test `triton profile set-current env` fails without env vars
#[test]
fn test_profile_set_current_env_missing_vars() {
    let tmp_dir = tempfile::tempdir().expect("Failed to create temp dir");

    triton_cmd()
        .args(["profile", "set-current", "env"])
        .env("HOME", tmp_dir.path())
        .env("TRITON_CONFIG_DIR", tmp_dir.path().join(".triton"))
        .env_remove("TRITON_URL")
        .env_remove("SDC_URL")
        .env_remove("TRITON_ACCOUNT")
        .env_remove("SDC_ACCOUNT")
        .env_remove("TRITON_KEY_ID")
        .env_remove("SDC_KEY_ID")
        .env_remove("TRITON_PROFILE")
        .assert()
        .failure();
}
