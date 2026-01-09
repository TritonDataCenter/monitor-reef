// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! SSH key CLI tests
//!
//! Tests for `triton key` commands.
//!
//! Tests are split into:
//! - Offline tests (help, usage) - run without API access
//! - API tests (list, get, add, delete) - marked with #[ignore], require config.json

// Allow deprecated - cargo_bin is standard for CLI testing
#![allow(deprecated, clippy::expect_used)]

mod common;

use assert_cmd::Command;
use predicates::prelude::*;

fn triton_cmd() -> Command {
    Command::cargo_bin("triton").expect("Failed to find triton binary")
}

// =============================================================================
// Offline tests - no API access required
// =============================================================================

/// Test `triton key -h` shows help
#[test]
fn test_key_help_short() {
    triton_cmd()
        .args(["key", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Manage SSH keys"));
}

/// Test `triton key --help` shows help
#[test]
fn test_key_help_long() {
    triton_cmd()
        .args(["key", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Manage SSH keys"));
}

/// Test `triton help key` shows help
#[test]
fn test_help_key() {
    triton_cmd()
        .args(["help", "key"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Manage SSH keys"));
}

/// Test `triton key list -h` shows help
#[test]
fn test_key_list_help() {
    triton_cmd()
        .args(["key", "list", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("List SSH keys"));
}

/// Test `triton key ls` alias works
#[test]
fn test_key_ls_alias() {
    triton_cmd()
        .args(["key", "ls", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("List SSH keys"));
}

/// Test `triton keys` shortcut works
#[test]
fn test_keys_shortcut() {
    triton_cmd()
        .args(["keys", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("List SSH keys"));
}

/// Test `triton key get -h` shows help
#[test]
fn test_key_get_help() {
    triton_cmd()
        .args(["key", "get", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Get SSH key details"));
}

/// Test `triton key help get` shows help
#[test]
fn test_key_help_get() {
    triton_cmd()
        .args(["key", "help", "get"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Get SSH key details"));
}

/// Test `triton key get` without args shows error
#[test]
fn test_key_get_no_args() {
    triton_cmd()
        .args(["key", "get"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("required"));
}

/// Test `triton key add -h` shows help
#[test]
fn test_key_add_help() {
    triton_cmd()
        .args(["key", "add", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Add SSH key"));
}

/// Test `triton key delete -h` shows help
#[test]
fn test_key_delete_help() {
    triton_cmd()
        .args(["key", "delete", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Delete SSH key"));
}

/// Test `triton key rm` alias works
#[test]
fn test_key_rm_alias() {
    triton_cmd()
        .args(["key", "rm", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Delete SSH key"));
}

// =============================================================================
// API tests - require config.json with valid profile
// These tests are ignored by default and run with `make triton-test-api`
// =============================================================================

/// Get a triton command with profile environment configured
fn triton_with_profile() -> Command {
    let mut cmd = triton_cmd();

    // Load profile environment from config
    let env_vars = common::config::get_profile_env();
    for (key, value) in env_vars {
        cmd.env(key, value);
    }

    cmd
}

// Note: Full key API tests that add/delete keys require allowWriteActions: true.
// Read-only tests (list, get) can run without it.

#[test]
#[ignore = "requires API access - run with make triton-test-api"]
fn test_key_list() {
    if !common::config::has_integration_config() {
        eprintln!("Skipping: no test config found");
        return;
    }

    let output = triton_with_profile()
        .args(["key", "list"])
        .output()
        .expect("Failed to execute command");

    assert!(
        output.status.success(),
        "key list should succeed.\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    // Table output should have FINGERPRINT header
    assert!(
        stdout.contains("FINGERPRINT"),
        "key list should show FINGERPRINT column"
    );
}

#[test]
#[ignore = "requires API access - run with make triton-test-api"]
fn test_key_list_json() {
    if !common::config::has_integration_config() {
        eprintln!("Skipping: no test config found");
        return;
    }

    let output = triton_with_profile()
        .args(["key", "list", "-j"])
        .output()
        .expect("Failed to execute command");

    assert!(
        output.status.success(),
        "key list -j should succeed.\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    // JSON output should parse as array of keys
    let keys: Vec<serde_json::Value> = common::json_stream_parse(&stdout);
    assert!(!keys.is_empty(), "Should have at least one key");
    assert!(
        keys[0].get("fingerprint").is_some(),
        "Keys should have fingerprint field"
    );
}
