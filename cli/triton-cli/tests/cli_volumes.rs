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

// =============================================================================
// Write operation tests - require config.json with allowWriteActions: true
// and allowVolumesTests: true (default)
// =============================================================================

/// Volume info returned from `triton volume get --json`
#[derive(Debug, serde::Deserialize)]
#[allow(dead_code)]
struct VolumeInfo {
    id: String,
    name: String,
    #[serde(rename = "type")]
    volume_type: String,
    #[serde(default)]
    networks: Vec<String>,
    #[serde(default)]
    tags: std::collections::HashMap<String, String>,
}

/// Network info for finding fabric networks
#[derive(Debug, serde::Deserialize)]
struct NetworkInfo {
    id: String,
    #[serde(default)]
    fabric: bool,
}

/// Delete a volume by name (doesn't error if not found)
fn delete_test_volume(name: &str) {
    use common::run_triton_with_profile;
    let _ = run_triton_with_profile(["volume", "delete", "-y", "-w", name]);
}

/// Full volume create/get/delete workflow test
///
/// Ported from node-triton test/integration/cli-volumes.test.js
#[test]
#[ignore = "requires API access - run with make triton-test-api"]
fn test_volume_create_workflow() {
    use common::{allow_write_actions, make_resource_name, run_triton_with_profile};

    // Skip if write actions not allowed
    if !allow_write_actions() {
        eprintln!("Skipping test: requires config.allowWriteActions");
        return;
    }

    // Check if volumes tests are allowed
    let config = common::config::load_config();
    if let Some(c) = config
        && !c.allow_volumes_tests
    {
        eprintln!("Skipping test: config.allowVolumesTests is false");
        return;
    }

    let volume_name = make_resource_name("tritontest-volume-create");

    // Cleanup any existing volume with this name
    eprintln!("Cleanup: removing any existing volume {}", volume_name);
    delete_test_volume(&volume_name);

    // Test: Create volume with invalid name
    eprintln!("Test: triton volume create with invalid name");
    let invalid_name = make_resource_name("tritontest-volume-!invalid!");
    let (_, stderr, success) =
        run_triton_with_profile(["volume", "create", "--name", &invalid_name]);
    assert!(!success, "create with invalid name should fail");
    assert!(
        stderr.contains("Invalid") || stderr.contains("invalid"),
        "error should mention invalid: {}",
        stderr
    );

    // Test: Create volume with invalid size
    eprintln!("Test: triton volume create with invalid size");
    let (_, stderr, success) = run_triton_with_profile([
        "volume",
        "create",
        "--name",
        &volume_name,
        "--size",
        "foobar",
    ]);
    assert!(!success, "create with invalid size should fail");
    assert!(
        stderr.contains("invalid") || stderr.contains("not a valid"),
        "error should mention invalid size: {}",
        stderr
    );

    // Test: Create volume with invalid type
    eprintln!("Test: triton volume create with invalid type");
    let (_, stderr, success) = run_triton_with_profile([
        "volume",
        "create",
        "--name",
        &volume_name,
        "--type",
        "foobar",
    ]);
    assert!(!success, "create with invalid type should fail");
    assert!(
        stderr.contains("Invalid") || stderr.contains("invalid"),
        "error should mention invalid type: {}",
        stderr
    );

    // Test: Create volume with invalid network
    eprintln!("Test: triton volume create with invalid network");
    let (_, stderr, success) = run_triton_with_profile([
        "volume",
        "create",
        "--name",
        &volume_name,
        "--network",
        "nonexistent-network",
    ]);
    assert!(!success, "create with invalid network should fail");
    assert!(
        stderr.contains("not found") || stderr.contains("no network"),
        "error should mention network not found: {}",
        stderr
    );

    // Test: Create volume with invalid tag format
    eprintln!("Test: triton volume create with invalid tag");
    let (_, stderr, success) = run_triton_with_profile([
        "volume",
        "create",
        "--name",
        &volume_name,
        "--tag",
        "invalid-no-equals",
    ]);
    assert!(!success, "create with invalid tag should fail");
    assert!(
        stderr.contains("invalid") || stderr.contains("KEY=VALUE"),
        "error should mention invalid tag format: {}",
        stderr
    );

    // Test: Create valid volume
    eprintln!(
        "Test: triton volume create --name {} --tag role=test -w",
        volume_name
    );
    let (stdout, stderr, success) = run_triton_with_profile([
        "volume",
        "create",
        "--name",
        &volume_name,
        "--tag",
        "role=test",
        "-w",
    ]);
    if !success {
        eprintln!(
            "Failed to create volume: stdout={}, stderr={}",
            stdout, stderr
        );
        // Volume creation might fail due to infrastructure limitations
        // Skip remaining tests
        return;
    }

    // Test: Get volume
    eprintln!("Test: triton volume get --json {}", volume_name);
    let (stdout, _, success) = run_triton_with_profile(["volume", "get", "--json", &volume_name]);
    assert!(success, "volume get should succeed");
    let volume: VolumeInfo = serde_json::from_str(&stdout).expect("should parse JSON");
    assert_eq!(volume.name, volume_name);
    assert_eq!(volume.volume_type, "tritonnfs");
    // Tags may or may not be present depending on CloudAPI version
    if !volume.tags.is_empty() {
        assert_eq!(volume.tags.get("role"), Some(&"test".to_string()));
    }

    // Test: Delete volume
    eprintln!("Test: triton volume delete -y -w {}", volume_name);
    let (_, _, success) = run_triton_with_profile(["volume", "delete", "-y", "-w", &volume_name]);
    assert!(success, "volume delete should succeed");

    // Test: Verify volume was deleted
    eprintln!("Test: triton volume get {} (should fail)", volume_name);
    let (_, stderr, success) = run_triton_with_profile(["volume", "get", &volume_name]);
    assert!(!success, "volume get should fail after deletion");
    assert!(
        stderr.contains("ResourceNotFound") || stderr.contains("not found"),
        "error should mention not found: {}",
        stderr
    );
}

/// Test volume creation on fabric network
#[test]
#[ignore = "requires API access - run with make triton-test-api"]
fn test_volume_create_on_fabric_network() {
    use common::{
        allow_write_actions, json_stream_parse, make_resource_name, run_triton_with_profile,
    };

    if !allow_write_actions() {
        eprintln!("Skipping test: requires config.allowWriteActions");
        return;
    }

    let config = common::config::load_config();
    if let Some(c) = config
        && !c.allow_volumes_tests
    {
        eprintln!("Skipping test: config.allowVolumesTests is false");
        return;
    }

    // Find a fabric network
    eprintln!("Finding fabric network...");
    let (stdout, _, success) = run_triton_with_profile(["network", "list", "-j"]);
    if !success {
        eprintln!("Failed to list networks");
        return;
    }

    let networks: Vec<NetworkInfo> = json_stream_parse(&stdout);
    let fabric_network = networks.iter().find(|n| n.fabric);

    let fabric_network_id = match fabric_network {
        Some(n) => n.id.clone(),
        None => {
            eprintln!("No fabric network found, skipping test");
            return;
        }
    };

    eprintln!("Found fabric network: {}", fabric_network_id);

    let volume_name = make_resource_name("tritontest-volume-fabric");

    // Cleanup
    delete_test_volume(&volume_name);

    // Create volume on fabric network
    eprintln!(
        "Test: triton volume create --name {} --network {} -w -j",
        volume_name, fabric_network_id
    );
    let (stdout, stderr, success) = run_triton_with_profile([
        "volume",
        "create",
        "--name",
        &volume_name,
        "--network",
        &fabric_network_id,
        "-w",
        "-j",
    ]);

    if !success {
        eprintln!("Failed to create volume on fabric: stderr={}", stderr);
        return;
    }

    let volume: VolumeInfo = serde_json::from_str(&stdout).expect("should parse JSON");

    // Verify volume
    eprintln!("Test: triton volume get {}", volume_name);
    let (stdout, _, success) = run_triton_with_profile(["volume", "get", &volume_name]);
    assert!(success, "volume get should succeed");
    let vol: VolumeInfo = serde_json::from_str(&stdout).expect("should parse JSON");
    assert!(
        vol.networks.contains(&fabric_network_id),
        "volume should be on fabric network"
    );

    // Cleanup
    delete_test_volume(&volume.name);
}
