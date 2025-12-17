// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Volume CLI tests
//!
//! Tests for `triton volume` commands.
//!
//! Tests are split into:
//! - Offline tests (help, usage) - run without API access
//! - API tests (list, get, create, delete) - marked with #[ignore]

// Allow deprecated - cargo_bin is standard for CLI testing
#![allow(deprecated)]

mod common;

use assert_cmd::Command;
use predicates::prelude::*;

fn triton_cmd() -> Command {
    Command::cargo_bin("triton").expect("Failed to find triton binary")
}

// =============================================================================
// Offline tests - no API access required
// =============================================================================

/// Test `triton volume -h` shows help
#[test]
fn test_volume_help_short() {
    triton_cmd()
        .args(["volume", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Manage volumes"));
}

/// Test `triton volume --help` shows help
#[test]
fn test_volume_help_long() {
    triton_cmd()
        .args(["volume", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Manage volumes"));
}

/// Test `triton help volume` shows help
#[test]
fn test_help_volume() {
    triton_cmd()
        .args(["help", "volume"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Manage volumes"));
}

/// Test `triton vol` alias works
#[test]
fn test_vol_alias() {
    triton_cmd()
        .args(["vol", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Manage volumes"));
}

/// Test `triton volume list -h` shows help
#[test]
fn test_volume_list_help() {
    triton_cmd()
        .args(["volume", "list", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("List volumes"));
}

/// Test `triton volume ls` alias works
#[test]
fn test_volume_ls_alias() {
    triton_cmd()
        .args(["volume", "ls", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("List volumes"));
}

/// Test `triton volumes` shortcut works
#[test]
fn test_volumes_shortcut() {
    triton_cmd()
        .args(["volumes", "-h"])
        .assert()
        .success()
        // The shortcut goes through 'vols' which shows List volumes
        .stdout(predicate::str::contains("List volumes"));
}

/// Test `triton vols` shortcut works
#[test]
fn test_vols_shortcut() {
    triton_cmd()
        .args(["vols", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("List volumes"));
}

/// Test `triton volume get -h` shows help
#[test]
fn test_volume_get_help() {
    triton_cmd()
        .args(["volume", "get", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Get volume details"));
}

/// Test `triton volume help get` shows help
#[test]
fn test_volume_help_get() {
    triton_cmd()
        .args(["volume", "help", "get"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Get volume details"));
}

/// Test `triton volume get` without args shows error
#[test]
fn test_volume_get_no_args() {
    triton_cmd()
        .args(["volume", "get"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("required"));
}

/// Test `triton volume create -h` shows help
#[test]
fn test_volume_create_help() {
    triton_cmd()
        .args(["volume", "create", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Create volume"));
}

/// Test `triton volume delete -h` shows help
#[test]
fn test_volume_delete_help() {
    triton_cmd()
        .args(["volume", "delete", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Delete volume"));
}

/// Test `triton volume rm` alias works
#[test]
fn test_volume_rm_alias() {
    triton_cmd()
        .args(["volume", "rm", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Delete volume"));
}

/// Test `triton volume sizes -h` shows help
#[test]
fn test_volume_sizes_help() {
    triton_cmd()
        .args(["volume", "sizes", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("List available volume sizes"));
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

#[test]
#[ignore = "requires API access - run with make triton-test-api"]
fn test_volume_list() {
    if !common::config::has_integration_config() {
        eprintln!("Skipping: no test config found");
        return;
    }

    let output = triton_with_profile()
        .args(["volume", "list"])
        .output()
        .expect("Failed to execute command");

    assert!(
        output.status.success(),
        "volume list should succeed.\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    // Output can be empty if no volumes exist, or show a table
    let stdout = String::from_utf8_lossy(&output.stdout);
    // If there are volumes, should have NAME header
    if !stdout.trim().is_empty() && !stdout.contains("No volumes") {
        assert!(
            stdout.contains("NAME") || stdout.contains("SHORTID"),
            "volume list should show NAME or SHORTID column"
        );
    }
}

#[test]
#[ignore = "requires API access - run with make triton-test-api"]
fn test_volume_list_json() {
    if !common::config::has_integration_config() {
        eprintln!("Skipping: no test config found");
        return;
    }

    let output = triton_with_profile()
        .args(["volume", "list", "-j"])
        .output()
        .expect("Failed to execute command");

    assert!(
        output.status.success(),
        "volume list -j should succeed.\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    // JSON output should parse as array (may be empty)
    let volumes: Vec<serde_json::Value> = common::json_stream_parse(&stdout);
    // Volumes array may be empty, but if not empty should have id field
    if !volumes.is_empty() {
        assert!(
            volumes[0].get("id").is_some(),
            "Volumes should have id field"
        );
    }
}

#[test]
#[ignore = "requires API access - run with make triton-test-api"]
fn test_volume_sizes() {
    if !common::config::has_integration_config() {
        eprintln!("Skipping: no test config found");
        return;
    }

    let output = triton_with_profile()
        .args(["volume", "sizes"])
        .output()
        .expect("Failed to execute command");

    assert!(
        output.status.success(),
        "volume sizes should succeed.\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    // Should list available sizes
    assert!(
        stdout.contains("SIZE") || stdout.contains("G"),
        "volume sizes should show SIZE column or G suffix"
    );
}
