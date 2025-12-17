// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Instance snapshot CLI tests
//!
//! Tests for `triton instance snapshot` commands.
//!
//! Tests are split into:
//! - Offline tests (help, usage) - run without API access
//! - API tests (create, list, get, delete) - marked with #[ignore], require config.json
//!   and allowWriteActions: true
//!
//! Ported from node-triton test/integration/cli-snapshots.test.js

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

/// Test `triton instance snapshot -h` shows help
#[test]
fn test_instance_snapshot_help_short() {
    triton_cmd()
        .args(["instance", "snapshot", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton instance snapshot --help` shows help
#[test]
fn test_instance_snapshot_help_long() {
    triton_cmd()
        .args(["instance", "snapshot", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton inst snapshot -h` alias works
#[test]
fn test_inst_snapshot_help() {
    triton_cmd()
        .args(["inst", "snapshot", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton instance snapshot list -h` shows help
#[test]
fn test_instance_snapshot_list_help() {
    triton_cmd()
        .args(["instance", "snapshot", "list", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton instance snapshot ls` alias works
#[test]
fn test_instance_snapshot_ls_alias() {
    triton_cmd()
        .args(["instance", "snapshot", "ls", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton instance snapshot create -h` shows help
#[test]
fn test_instance_snapshot_create_help() {
    triton_cmd()
        .args(["instance", "snapshot", "create", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton instance snapshot get -h` shows help
#[test]
fn test_instance_snapshot_get_help() {
    triton_cmd()
        .args(["instance", "snapshot", "get", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton instance snapshot delete -h` shows help
#[test]
fn test_instance_snapshot_delete_help() {
    triton_cmd()
        .args(["instance", "snapshot", "delete", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton instance snapshot rm` alias works
#[test]
fn test_instance_snapshot_rm_alias() {
    triton_cmd()
        .args(["instance", "snapshot", "rm", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton instance snapshots -h` shows help (for listing)
#[test]
fn test_instance_snapshots_alias() {
    triton_cmd()
        .args(["instance", "snapshots", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

// =============================================================================
// API write tests - require config.json with allowWriteActions: true
// These tests are ignored by default and run with `make triton-test-api`
// =============================================================================

/// Snapshot info returned from `triton instance snapshot get`
#[derive(Debug, serde::Deserialize)]
struct SnapshotInfo {
    name: String,
    state: String,
}

/// Full instance snapshot workflow test
/// This test creates an instance, creates/lists/deletes snapshots, and cleans up.
///
/// Ported from node-triton test/integration/cli-snapshots.test.js
#[test]
#[ignore]
fn test_instance_snapshot_workflow() {
    use common::{
        allow_write_actions, create_test_instance, delete_test_instance, make_resource_name,
        run_triton_with_profile, short_id,
    };

    // Skip if write actions not allowed
    if !allow_write_actions() {
        eprintln!("Skipping test: requires config.allowWriteActions");
        return;
    }

    let inst_alias = make_resource_name("tritontest-snapshots");
    let snap_name = "test-snapshot";
    let snap_name_2 = "test-snapshot-2";

    // Cleanup any existing instance with this alias
    eprintln!("Cleanup: removing any existing instance {}", inst_alias);
    delete_test_instance(&inst_alias);

    // Create test instance
    eprintln!("Setup: creating test instance {}", inst_alias);
    let inst = create_test_instance(&inst_alias, &[]);
    let inst = match inst {
        Some(i) => i,
        None => {
            eprintln!("Failed to create test instance, skipping test");
            return;
        }
    };

    eprintln!("Created instance {} ({})", inst.name, inst.id);
    let inst_short_id = short_id(&inst.id);

    // Test: Create snapshot 2 first (for deletion testing before boot from snapshot)
    // Per node-triton comments: Testing snapshot deletion after rolling back a VM
    // to that snapshot can result in vmadm errors, so we test deletion before boot.
    eprintln!(
        "Test: triton instance snapshot create -w -n {} {}",
        snap_name_2, inst_short_id
    );
    let (stdout, stderr, success) = run_triton_with_profile([
        "instance",
        "snapshot",
        "create",
        "-w",
        "-n",
        snap_name_2,
        &inst_short_id,
    ]);
    if !success {
        eprintln!("Failed to create snapshot: stderr={}", stderr);
        delete_test_instance(&inst.id);
        panic!("snapshot create failed");
    }
    assert!(
        stdout.contains(&format!("Created snapshot \"{}\"", snap_name_2)),
        "stdout should contain created message: {}",
        stdout
    );

    // Test: Get snapshot 2
    eprintln!(
        "Test: triton instance snapshot get {} {}",
        inst_short_id, snap_name_2
    );
    let (stdout, _, success) =
        run_triton_with_profile(["instance", "snapshot", "get", &inst_short_id, snap_name_2]);
    assert!(success, "snapshot get should succeed");
    let snap: SnapshotInfo = serde_json::from_str(&stdout).expect("should parse JSON");
    assert_eq!(snap.name, snap_name_2);
    assert_eq!(snap.state, "created");

    // Test: Delete snapshot 2
    eprintln!(
        "Test: triton instance snapshot delete -w --force {} {}",
        inst_short_id, snap_name_2
    );
    let (stdout, _, success) = run_triton_with_profile([
        "instance",
        "snapshot",
        "delete",
        "-w",
        "--force",
        &inst_short_id,
        snap_name_2,
    ]);
    assert!(success, "snapshot delete should succeed");
    assert!(
        stdout.contains(&format!("Deleting snapshot \"{}\"", snap_name_2)),
        "stdout should contain deleting message"
    );
    assert!(
        stdout.contains(&format!("Deleted snapshot \"{}\"", snap_name_2)),
        "stdout should contain deleted message"
    );

    // Test: Create snapshot 1 (will be used for boot from snapshot)
    eprintln!(
        "Test: triton instance snapshot create -w -n {} {}",
        snap_name, inst_short_id
    );
    let (stdout, _, success) = run_triton_with_profile([
        "instance",
        "snapshot",
        "create",
        "-w",
        "-n",
        snap_name,
        &inst_short_id,
    ]);
    assert!(success, "snapshot create should succeed");
    assert!(
        stdout.contains(&format!("Created snapshot \"{}\"", snap_name)),
        "stdout should contain created message"
    );

    // Test: Get snapshot 1
    eprintln!(
        "Test: triton instance snapshot get {} {}",
        inst_short_id, snap_name
    );
    let (stdout, _, success) =
        run_triton_with_profile(["instance", "snapshot", "get", &inst_short_id, snap_name]);
    assert!(success, "snapshot get should succeed");
    let snap: SnapshotInfo = serde_json::from_str(&stdout).expect("should parse JSON");
    assert_eq!(snap.name, snap_name);
    assert_eq!(snap.state, "created");

    // Test: List snapshots
    eprintln!("Test: triton instance snapshot list {}", inst_short_id);
    let (stdout, _, success) =
        run_triton_with_profile(["instance", "snapshot", "list", &inst_short_id]);
    assert!(success, "snapshot list should succeed");
    // Check header
    assert!(
        stdout.contains("NAME") && stdout.contains("STATE") && stdout.contains("CREATED"),
        "list output should have header"
    );
    // Check our snapshot is listed
    assert!(
        stdout.contains(snap_name),
        "list output should contain our snapshot"
    );

    // Test: Start instance from snapshot
    eprintln!(
        "Test: triton instance start {} -w --snapshot={}",
        inst_short_id, snap_name
    );
    let (stdout, _, success) = run_triton_with_profile([
        "instance",
        "start",
        &inst_short_id,
        "-w",
        &format!("--snapshot={}", snap_name),
    ]);
    assert!(success, "instance start --snapshot should succeed");
    assert!(
        stdout.contains(&format!("Start instance {}", inst_short_id)),
        "stdout should contain start message"
    );

    // Cleanup: delete test instance
    eprintln!("Cleanup: deleting test instance {}", inst.id);
    delete_test_instance(&inst.id);
}

/// Test snapshot list on instance with no snapshots
#[test]
#[ignore]
fn test_instance_snapshot_list_empty() {
    use common::{
        allow_write_actions, create_test_instance, delete_test_instance, make_resource_name,
        run_triton_with_profile,
    };

    if !allow_write_actions() {
        eprintln!("Skipping test: requires config.allowWriteActions");
        return;
    }

    let inst_alias = make_resource_name("tritontest-snapempty");
    delete_test_instance(&inst_alias);

    let inst = match create_test_instance(&inst_alias, &[]) {
        Some(i) => i,
        None => {
            eprintln!("Failed to create test instance");
            return;
        }
    };

    // List snapshots on instance with no snapshots
    let (stdout, _, success) =
        run_triton_with_profile(["instance", "snapshot", "list", &inst.name]);
    assert!(success, "snapshot list should succeed");
    // Should have header but no snapshots
    assert!(
        stdout.contains("NAME") && stdout.contains("STATE"),
        "list output should have header"
    );

    delete_test_instance(&inst.id);
}
