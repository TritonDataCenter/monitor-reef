// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! VLAN CLI tests
//!
//! Tests for `triton vlan` commands.
//!
//! Tests are split into:
//! - Offline tests (help, usage) - run without API access
//! - API tests (list, get, create, delete) - marked with #[ignore], require config.json
//!   and allowWriteActions: true for write tests
//!
//! Ported from node-triton test/integration/cli-vlans.test.js

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

/// Test `triton vlan list -h` shows help
#[test]
fn test_vlan_list_help() {
    triton_cmd()
        .args(["vlan", "list", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::is_match(r"Usage:\s+triton vlan list").unwrap());
}

/// Test `triton vlan ls` alias works
#[test]
fn test_vlan_ls_alias() {
    triton_cmd()
        .args(["vlan", "ls", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton vlan get -h` shows help
#[test]
fn test_vlan_get_help() {
    triton_cmd()
        .args(["vlan", "get", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::is_match(r"Usage:\s+triton vlan").unwrap());
}

/// Test `triton vlan help get` shows help
#[test]
fn test_vlan_help_get() {
    triton_cmd()
        .args(["vlan", "help", "get"])
        .assert()
        .success()
        .stdout(predicate::str::is_match(r"Usage:\s+triton vlan get").unwrap());
}

/// Test `triton vlan get` without args shows error
#[test]
fn test_vlan_get_no_args() {
    triton_cmd()
        .args(["vlan", "get"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("required"));
}

/// Test `triton vlan networks -h` shows help
#[test]
fn test_vlan_networks_help() {
    triton_cmd()
        .args(["vlan", "networks", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::is_match(r"Usage:\s+triton vlan networks").unwrap());
}

/// Test `triton vlan help networks` shows help
#[test]
fn test_vlan_help_networks() {
    triton_cmd()
        .args(["vlan", "help", "networks"])
        .assert()
        .success()
        .stdout(predicate::str::is_match(r"Usage:\s+triton vlan networks").unwrap());
}

/// Test `triton vlan networks` without args shows error
#[test]
fn test_vlan_networks_no_args() {
    triton_cmd()
        .args(["vlan", "networks"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("required"));
}

/// Test `triton vlan create -h` shows help
#[test]
fn test_vlan_create_help() {
    triton_cmd()
        .args(["vlan", "create", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::is_match(r"Usage:\s+triton vlan").unwrap());
}

/// Test `triton vlan help create` shows help
#[test]
fn test_vlan_help_create() {
    triton_cmd()
        .args(["vlan", "help", "create"])
        .assert()
        .success()
        .stdout(predicate::str::is_match(r"Usage:\s+triton vlan create").unwrap());
}

/// Test `triton vlan create` without args shows error
#[test]
fn test_vlan_create_no_args() {
    triton_cmd()
        .args(["vlan", "create"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("required"));
}

/// Test `triton vlan delete -h` shows help
#[test]
fn test_vlan_delete_help() {
    triton_cmd()
        .args(["vlan", "delete", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::is_match(r"Usage:\s+triton vlan").unwrap());
}

/// Test `triton vlan help delete` shows help
#[test]
fn test_vlan_help_delete() {
    triton_cmd()
        .args(["vlan", "help", "delete"])
        .assert()
        .success()
        .stdout(predicate::str::is_match(r"Usage:\s+triton vlan delete").unwrap());
}

/// Test `triton vlan delete` without args shows error
#[test]
fn test_vlan_delete_no_args() {
    triton_cmd()
        .args(["vlan", "delete"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("required"));
}

/// Test `triton vlan rm` alias works
#[test]
fn test_vlan_rm_alias() {
    triton_cmd()
        .args(["vlan", "rm", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton vlan update -h` shows help
#[test]
fn test_vlan_update_help() {
    triton_cmd()
        .args(["vlan", "update", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

// =============================================================================
// API tests - require config.json
// These tests are ignored by default and run with `make triton-test-api`
// =============================================================================

/// VLAN info returned from JSON output
#[derive(Debug, serde::Deserialize)]
struct VlanInfo {
    vlan_id: u16,
    name: String,
    #[allow(dead_code)]
    description: Option<String>,
}

/// Test `triton vlan list` returns table output
#[test]
#[ignore]
fn test_vlan_list_table() {
    use common::run_triton_with_profile;

    let (stdout, _, success) = run_triton_with_profile(["vlan", "list"]);
    assert!(success, "vlan list should succeed");

    // Check for expected columns
    assert!(
        stdout.contains("VLAN_ID"),
        "output should contain VLAN_ID header"
    );
    assert!(stdout.contains("NAME"), "output should contain NAME header");
}

/// Test `triton vlan list -j` returns JSON
#[test]
#[ignore]
fn test_vlan_list_json() {
    use common::{json_stream_parse, run_triton_with_profile};

    let (stdout, _, success) = run_triton_with_profile(["vlan", "list", "-j"]);
    assert!(success, "vlan list -j should succeed");

    let vlans: Vec<VlanInfo> = json_stream_parse(&stdout);
    // May have no VLANs, that's OK
    if !vlans.is_empty() {
        let vlan = &vlans[0];
        assert!(vlan.vlan_id > 0, "VLAN should have valid vlan_id");
        assert!(!vlan.name.is_empty(), "VLAN should have name");
    }
}

/// Test `triton vlan get ID` returns VLAN details
#[test]
#[ignore]
fn test_vlan_get_by_id() {
    use common::{json_stream_parse, run_triton_with_profile};

    // First get a VLAN to test with
    let (stdout, _, success) = run_triton_with_profile(["vlan", "list", "-j"]);
    if !success {
        eprintln!("Skipping test: could not list VLANs");
        return;
    }

    let vlans: Vec<VlanInfo> = json_stream_parse(&stdout);
    if vlans.is_empty() {
        eprintln!("Skipping test: no VLANs available");
        return;
    }

    let vlan = &vlans[0];
    let vlan_id_str = vlan.vlan_id.to_string();

    // Get by ID
    let (stdout, _, success) = run_triton_with_profile(["vlan", "get", &vlan_id_str]);
    assert!(success, "vlan get should succeed");

    let got_vlan: VlanInfo = serde_json::from_str(&stdout).expect("should parse VLAN JSON");
    assert_eq!(got_vlan.vlan_id, vlan.vlan_id, "VLAN ID should match");
}

/// Test `triton vlan get NAME` returns VLAN details
#[test]
#[ignore]
fn test_vlan_get_by_name() {
    use common::{json_stream_parse, run_triton_with_profile};

    // First get a VLAN to test with
    let (stdout, _, success) = run_triton_with_profile(["vlan", "list", "-j"]);
    if !success {
        eprintln!("Skipping test: could not list VLANs");
        return;
    }

    let vlans: Vec<VlanInfo> = json_stream_parse(&stdout);
    if vlans.is_empty() {
        eprintln!("Skipping test: no VLANs available");
        return;
    }

    let vlan = &vlans[0];

    // Get by name
    let (stdout, _, success) = run_triton_with_profile(["vlan", "get", &vlan.name]);
    assert!(success, "vlan get by name should succeed");

    let got_vlan: VlanInfo = serde_json::from_str(&stdout).expect("should parse VLAN JSON");
    assert_eq!(got_vlan.vlan_id, vlan.vlan_id, "VLAN ID should match");
}

/// Test `triton vlan networks ID` lists networks on VLAN
#[test]
#[ignore]
fn test_vlan_networks() {
    use common::{json_stream_parse, run_triton_with_profile};

    // First get a VLAN to test with
    let (stdout, _, success) = run_triton_with_profile(["vlan", "list", "-j"]);
    if !success {
        eprintln!("Skipping test: could not list VLANs");
        return;
    }

    let vlans: Vec<VlanInfo> = json_stream_parse(&stdout);
    if vlans.is_empty() {
        eprintln!("Skipping test: no VLANs available");
        return;
    }

    let vlan = &vlans[0];
    let vlan_id_str = vlan.vlan_id.to_string();

    // Get networks
    let (stdout, _, success) = run_triton_with_profile(["vlan", "networks", "-j", &vlan_id_str]);
    assert!(success, "vlan networks should succeed");

    // Parse and verify each network has the correct vlan_id
    #[derive(Debug, serde::Deserialize)]
    struct NetworkInfo {
        #[allow(dead_code)]
        id: String,
        #[allow(dead_code)]
        name: String,
        vlan_id: Option<u16>,
    }

    let networks: Vec<NetworkInfo> = json_stream_parse(&stdout);
    for net in &networks {
        if let Some(net_vlan_id) = net.vlan_id {
            assert_eq!(
                net_vlan_id, vlan.vlan_id,
                "network vlan_id should match requested VLAN"
            );
        }
    }
}

/// Test `triton vlan networks NAME` lists networks by VLAN name
#[test]
#[ignore]
fn test_vlan_networks_by_name() {
    use common::{json_stream_parse, run_triton_with_profile};

    // First get a VLAN to test with
    let (stdout, _, success) = run_triton_with_profile(["vlan", "list", "-j"]);
    if !success {
        eprintln!("Skipping test: could not list VLANs");
        return;
    }

    let vlans: Vec<VlanInfo> = json_stream_parse(&stdout);
    if vlans.is_empty() {
        eprintln!("Skipping test: no VLANs available");
        return;
    }

    let vlan = &vlans[0];

    // Get networks by VLAN name
    let (stdout, _, success) = run_triton_with_profile(["vlan", "networks", "-j", &vlan.name]);
    assert!(success, "vlan networks by name should succeed");

    // Parse and verify
    #[derive(Debug, serde::Deserialize)]
    struct NetworkInfo {
        vlan_id: Option<u16>,
    }

    let networks: Vec<NetworkInfo> = json_stream_parse(&stdout);
    for net in &networks {
        if let Some(net_vlan_id) = net.vlan_id {
            assert_eq!(
                net_vlan_id, vlan.vlan_id,
                "network vlan_id should match requested VLAN"
            );
        }
    }
}

/// Test `triton vlan list` with filters
#[test]
#[ignore]
fn test_vlan_list_with_filters() {
    use common::{json_stream_parse, run_triton_with_profile};

    // First get a VLAN to test with
    let (stdout, _, success) = run_triton_with_profile(["vlan", "list", "-j"]);
    if !success {
        eprintln!("Skipping test: could not list VLANs");
        return;
    }

    let vlans: Vec<VlanInfo> = json_stream_parse(&stdout);
    if vlans.is_empty() {
        eprintln!("Skipping test: no VLANs available");
        return;
    }

    let vlan = &vlans[0];
    let vlan_id_filter = format!("vlan_id={}", vlan.vlan_id);

    // Filter by vlan_id
    let (stdout, _, success) = run_triton_with_profile(["vlan", "list", "-j", &vlan_id_filter]);
    assert!(success, "vlan list with filter should succeed");

    let filtered: Vec<VlanInfo> = json_stream_parse(&stdout);
    assert_eq!(filtered.len(), 1, "filter should return exactly one VLAN");
    assert_eq!(
        filtered[0].vlan_id, vlan.vlan_id,
        "filtered VLAN ID should match"
    );
}

// =============================================================================
// API write tests - require config.json with allowWriteActions: true
// =============================================================================

/// Full VLAN create/delete workflow test
#[test]
#[ignore]
fn test_vlan_create_delete_workflow() {
    use common::{allow_write_actions, make_resource_name, run_triton_with_profile};

    // Skip if write actions not allowed
    if !allow_write_actions() {
        eprintln!("Skipping test: requires config.allowWriteActions");
        return;
    }

    let vlan_name = make_resource_name("tritontest-vlan");
    // Use a high VLAN ID to avoid conflicts (3197 from node-triton tests)
    let vlan_id = "3197";

    // Cleanup any existing VLAN with this name
    eprintln!("Cleanup: removing any existing VLAN {}", vlan_name);
    let _ = run_triton_with_profile(["vlan", "delete", &vlan_name, "--force"]);

    // Create VLAN
    eprintln!(
        "Test: triton vlan create -j --name={} {}",
        vlan_name, vlan_id
    );
    let (stdout, stderr, success) = run_triton_with_profile([
        "vlan",
        "create",
        "-j",
        &format!("--name={}", vlan_name),
        vlan_id,
    ]);

    if !success {
        eprintln!("Failed to create VLAN: stderr={}", stderr);
        // May fail if VLAN ID already exists
        if stderr.contains("already exists") || stderr.contains("in use") {
            eprintln!("Skipping test: VLAN ID {} already in use", vlan_id);
            return;
        }
        panic!("vlan create failed");
    }

    // Parse created VLAN
    let vlan: VlanInfo = serde_json::from_str(stdout.trim()).expect("should parse VLAN JSON");
    assert_eq!(vlan.name, vlan_name, "created VLAN name should match");
    assert_eq!(
        vlan.vlan_id.to_string(),
        vlan_id,
        "created VLAN ID should match"
    );

    eprintln!("Created VLAN {} ({})", vlan.name, vlan.vlan_id);

    // Delete by ID
    eprintln!("Test: triton vlan delete --force {}", vlan.vlan_id);
    let (_, _, success) =
        run_triton_with_profile(["vlan", "delete", "--force", &vlan.vlan_id.to_string()]);
    assert!(success, "vlan delete should succeed");

    // Verify deleted
    let (_, _, success) = run_triton_with_profile(["vlan", "get", &vlan.vlan_id.to_string()]);
    assert!(!success, "vlan should be gone after delete");

    eprintln!("Test passed: VLAN create/delete workflow");
}

/// Test VLAN delete by name
#[test]
#[ignore]
fn test_vlan_delete_by_name() {
    use common::{allow_write_actions, make_resource_name, run_triton_with_profile};

    // Skip if write actions not allowed
    if !allow_write_actions() {
        eprintln!("Skipping test: requires config.allowWriteActions");
        return;
    }

    let vlan_name = make_resource_name("tritontest-vlan2");
    let vlan_id = "3198";

    // Cleanup any existing VLAN
    let _ = run_triton_with_profile(["vlan", "delete", &vlan_name, "--force"]);
    let _ = run_triton_with_profile(["vlan", "delete", vlan_id, "--force"]);

    // Create VLAN
    let (stdout, stderr, success) = run_triton_with_profile([
        "vlan",
        "create",
        "-j",
        &format!("--name={}", vlan_name),
        vlan_id,
    ]);

    if !success {
        if stderr.contains("already exists") || stderr.contains("in use") {
            eprintln!("Skipping test: VLAN ID {} already in use", vlan_id);
            return;
        }
        panic!("vlan create failed: {}", stderr);
    }

    let vlan: VlanInfo = serde_json::from_str(stdout.trim()).expect("should parse VLAN JSON");

    // Delete by name
    eprintln!("Test: triton vlan delete --force {}", vlan.name);
    let (_, _, success) = run_triton_with_profile(["vlan", "delete", "--force", &vlan.name]);
    assert!(success, "vlan delete by name should succeed");

    // Verify deleted
    let (_, _, success) = run_triton_with_profile(["vlan", "get", &vlan.vlan_id.to_string()]);
    assert!(!success, "vlan should be gone after delete");

    eprintln!("Test passed: VLAN delete by name");
}
