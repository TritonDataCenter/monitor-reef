// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Instance NIC CLI tests
//!
//! Tests for `triton instance nic` commands.
//!
//! Tests are split into:
//! - Offline tests (help, usage) - run without API access
//! - API tests (create, list, get, delete) - marked with #[ignore], require config.json
//!   and allowWriteActions: true
//!
//! Ported from node-triton test/integration/cli-nics.test.js

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

/// Test `triton instance nic -h` shows help
#[test]
fn test_instance_nic_help_short() {
    triton_cmd()
        .args(["instance", "nic", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton instance nic --help` shows help
#[test]
fn test_instance_nic_help_long() {
    triton_cmd()
        .args(["instance", "nic", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton inst nic -h` alias works
#[test]
fn test_inst_nic_help() {
    triton_cmd()
        .args(["inst", "nic", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton instance nic list -h` shows help
#[test]
fn test_instance_nic_list_help() {
    triton_cmd()
        .args(["instance", "nic", "list", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton instance nic ls` alias works
#[test]
fn test_instance_nic_ls_alias() {
    triton_cmd()
        .args(["instance", "nic", "ls", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton instance nic add -h` shows help
#[test]
fn test_instance_nic_add_help() {
    triton_cmd()
        .args(["instance", "nic", "add", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton instance nic create` alias works (node-triton compat)
#[test]
fn test_instance_nic_create_alias() {
    triton_cmd()
        .args(["instance", "nic", "create", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton instance nic get -h` shows help
#[test]
fn test_instance_nic_get_help() {
    triton_cmd()
        .args(["instance", "nic", "get", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton instance nic remove -h` shows help
#[test]
fn test_instance_nic_remove_help() {
    triton_cmd()
        .args(["instance", "nic", "remove", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton instance nic rm` alias works
#[test]
fn test_instance_nic_rm_alias() {
    triton_cmd()
        .args(["instance", "nic", "rm", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton instance nic delete` alias works (node-triton compat)
#[test]
fn test_instance_nic_delete_alias() {
    triton_cmd()
        .args(["instance", "nic", "delete", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

// =============================================================================
// API write tests - require config.json with allowWriteActions: true
// These tests are ignored by default and run with `make triton-test-api`
// =============================================================================

/// NIC info returned from JSON output
#[derive(Debug, serde::Deserialize)]
struct NicInfo {
    ip: String,
    mac: String,
    network: String,
    #[allow(dead_code)]
    state: Option<String>,
    #[allow(dead_code)]
    primary: bool,
}

/// Network info from network list
#[derive(Debug, serde::Deserialize)]
struct NetworkInfo {
    id: String,
    #[allow(dead_code)]
    name: String,
}

/// Full instance NIC workflow test
/// This test creates an instance, adds/lists/deletes NICs, and cleans up.
///
/// Ported from node-triton test/integration/cli-nics.test.js
#[test]
#[ignore]
fn test_instance_nic_workflow() {
    use common::{
        allow_write_actions, create_test_instance, delete_test_instance, make_resource_name,
        run_triton_with_profile, short_id,
    };

    // Skip if write actions not allowed
    if !allow_write_actions() {
        eprintln!("Skipping test: requires config.allowWriteActions");
        return;
    }

    let inst_alias = make_resource_name("tritontest-nics");

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

    // Get a network for tests
    eprintln!("Setup: finding network for tests");
    let (stdout, _, success) = run_triton_with_profile(["network", "list", "-j"]);
    if !success {
        delete_test_instance(&inst.id);
        panic!("network list failed");
    }

    let networks: Vec<NetworkInfo> = stdout
        .lines()
        .filter(|l| !l.is_empty())
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();

    if networks.is_empty() {
        delete_test_instance(&inst.id);
        panic!("no networks available for test");
    }
    let network = &networks[0];
    eprintln!("Using network {} ({})", network.name, network.id);

    // Test: triton instance nic create (add)
    eprintln!(
        "Test: triton instance nic create -j -w {} {}",
        inst_short_id, network.id
    );
    let (stdout, stderr, success) = run_triton_with_profile([
        "instance",
        "nic",
        "create",
        "-j",
        "-w",
        &inst_short_id,
        &network.id,
    ]);
    if !success {
        eprintln!("Failed to create NIC: stderr={}", stderr);
        delete_test_instance(&inst.id);
        panic!("nic create failed");
    }

    let nic: NicInfo = serde_json::from_str(stdout.trim()).expect("should parse NIC JSON");
    eprintln!("Created NIC: {} ({})", nic.mac, nic.ip);

    // Test: triton instance nic get
    eprintln!(
        "Test: triton instance nic get {} {}",
        inst_short_id, nic.mac
    );
    let (stdout, _, success) =
        run_triton_with_profile(["instance", "nic", "get", &inst_short_id, &nic.mac]);
    assert!(success, "nic get should succeed");
    let got_nic: NicInfo = serde_json::from_str(stdout.trim()).expect("should parse NIC JSON");
    assert_eq!(got_nic.mac, nic.mac, "NIC MAC should match");
    assert_eq!(got_nic.ip, nic.ip, "NIC IP should match");
    assert_eq!(got_nic.network, nic.network, "NIC network should match");

    // Test: triton instance nic list (table output)
    eprintln!("Test: triton instance nic list {}", inst_short_id);
    let (stdout, _, success) = run_triton_with_profile(["instance", "nic", "list", &inst_short_id]);
    assert!(success, "nic list should succeed");
    // Check header matches node-triton format
    assert!(
        stdout.contains("IP") && stdout.contains("MAC") && stdout.contains("STATE"),
        "list output should have header: {}",
        stdout
    );
    // Our NIC should be listed
    assert!(
        stdout.contains(&nic.mac),
        "list output should contain our NIC MAC"
    );

    // Test: triton instance nic list -j (JSON output)
    eprintln!("Test: triton instance nic list -j {}", inst_short_id);
    let (stdout, _, success) =
        run_triton_with_profile(["instance", "nic", "list", "-j", &inst_short_id]);
    assert!(success, "nic list -j should succeed");
    // Should be NDJSON (one JSON per line)
    let nics: Vec<NicInfo> = stdout
        .lines()
        .filter(|l| !l.is_empty())
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    assert!(!nics.is_empty(), "should have at least one NIC");
    let found = nics.iter().any(|n| n.mac == nic.mac);
    assert!(found, "our NIC should be in the list");

    // Test: triton instance nic list mac=<mac> filter
    eprintln!(
        "Test: triton instance nic list -j {} mac={}",
        inst_short_id, nic.mac
    );
    let (stdout, _, success) = run_triton_with_profile([
        "instance",
        "nic",
        "list",
        "-j",
        &inst_short_id,
        &format!("mac={}", nic.mac),
    ]);
    assert!(success, "nic list with filter should succeed");
    let filtered_nics: Vec<NicInfo> = stdout
        .lines()
        .filter(|l| !l.is_empty())
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    assert_eq!(
        filtered_nics.len(),
        1,
        "filter should return exactly one NIC"
    );
    assert_eq!(filtered_nics[0].ip, nic.ip, "filtered NIC IP should match");
    assert_eq!(
        filtered_nics[0].network, nic.network,
        "filtered NIC network should match"
    );

    // Test: triton instance nic delete
    eprintln!(
        "Test: triton instance nic delete --force {} {}",
        inst_short_id, nic.mac
    );
    let (stdout, _, success) = run_triton_with_profile([
        "instance",
        "nic",
        "delete",
        "--force",
        &inst_short_id,
        &nic.mac,
    ]);
    assert!(success, "nic delete should succeed");
    // node-triton outputs "Deleted NIC <mac>"
    assert!(
        stdout.contains(&format!("Deleted NIC {}", nic.mac)),
        "stdout should contain 'Deleted NIC' message: {}",
        stdout
    );

    // Test: triton instance nic create with NICOPTS (ipv4_uuid=...)
    eprintln!(
        "Test: triton instance nic create -j -w {} ipv4_uuid={}",
        inst_short_id, network.id
    );
    let (stdout, stderr, success) = run_triton_with_profile([
        "instance",
        "nic",
        "create",
        "-j",
        "-w",
        &inst_short_id,
        &format!("ipv4_uuid={}", network.id),
    ]);
    if !success {
        eprintln!("Failed to create NIC with NICOPTS: stderr={}", stderr);
        delete_test_instance(&inst.id);
        panic!("nic create with NICOPTS failed");
    }

    let nic2: NicInfo = serde_json::from_str(stdout.trim()).expect("should parse NIC JSON");
    eprintln!("Created NIC with NICOPTS: {} ({})", nic2.mac, nic2.ip);

    // Test: Get the NIC created with NICOPTS
    eprintln!(
        "Test: triton instance nic get {} {}",
        inst_short_id, nic2.mac
    );
    let (stdout, _, success) =
        run_triton_with_profile(["instance", "nic", "get", &inst_short_id, &nic2.mac]);
    assert!(success, "nic get should succeed");
    let got_nic2: NicInfo = serde_json::from_str(stdout.trim()).expect("should parse NIC JSON");
    assert_eq!(got_nic2.mac, nic2.mac, "NIC MAC should match");
    assert_eq!(got_nic2.ip, nic2.ip, "NIC IP should match");
    assert_eq!(got_nic2.network, nic2.network, "NIC network should match");

    // Test: Delete the second NIC
    eprintln!(
        "Test: triton instance nic delete --force {} {}",
        inst_short_id, nic2.mac
    );
    let (stdout, _, success) = run_triton_with_profile([
        "instance",
        "nic",
        "delete",
        "--force",
        &inst_short_id,
        &nic2.mac,
    ]);
    assert!(success, "nic delete should succeed");
    assert!(
        stdout.contains(&format!("Deleted NIC {}", nic2.mac)),
        "stdout should contain 'Deleted NIC' message"
    );

    // Cleanup: delete test instance
    eprintln!("Cleanup: deleting test instance {}", inst.id);
    delete_test_instance(&inst.id);
}
