// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Instance deletion protection CLI tests
//!
//! Tests for `triton instance enable-deletion-protection` and
//! `triton instance disable-deletion-protection` commands.
//!
//! Tests are split into:
//! - Offline tests (help, usage) - run without API access
//! - API tests (enable, disable, verify) - marked with #[ignore], require config.json
//!   and allowWriteActions: true
//!
//! Ported from node-triton test/integration/cli-deletion-protection.test.js

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

/// Test `triton instance enable-deletion-protection -h` shows help
#[test]
fn test_enable_deletion_protection_help() {
    triton_cmd()
        .args(["instance", "enable-deletion-protection", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton instance disable-deletion-protection -h` shows help
#[test]
fn test_disable_deletion_protection_help() {
    triton_cmd()
        .args(["instance", "disable-deletion-protection", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton inst enable-deletion-protection -h` alias works
#[test]
fn test_inst_enable_deletion_protection_help() {
    triton_cmd()
        .args(["inst", "enable-deletion-protection", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton inst disable-deletion-protection -h` alias works
#[test]
fn test_inst_disable_deletion_protection_help() {
    triton_cmd()
        .args(["inst", "disable-deletion-protection", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton instance enable-deletion-protection` without args shows error
#[test]
fn test_enable_deletion_protection_no_args() {
    triton_cmd()
        .args(["instance", "enable-deletion-protection"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("required"));
}

/// Test `triton instance disable-deletion-protection` without args shows error
#[test]
fn test_disable_deletion_protection_no_args() {
    triton_cmd()
        .args(["instance", "disable-deletion-protection"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("required"));
}

/// Test `-w` flag is accepted for enable
#[test]
fn test_enable_deletion_protection_wait_flag() {
    triton_cmd()
        .args(["instance", "enable-deletion-protection", "-w", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `-w` flag is accepted for disable
#[test]
fn test_disable_deletion_protection_wait_flag() {
    triton_cmd()
        .args(["instance", "disable-deletion-protection", "-w", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

// =============================================================================
// API tests - require config.json with allowWriteActions: true
// These tests are ignored by default and run with `make triton-test-api`
// =============================================================================

/// Instance info returned from JSON output
#[derive(Debug, serde::Deserialize)]
struct InstanceInfo {
    #[allow(dead_code)]
    id: String,
    #[allow(dead_code)]
    name: String,
    #[allow(dead_code)]
    state: String,
    deletion_protection: Option<bool>,
}

/// Full deletion protection workflow test
/// This test creates an instance with deletion protection, tries to delete it
/// (which should fail), disables protection, and then deletes it.
///
/// Ported from node-triton test/integration/cli-deletion-protection.test.js
#[test]
#[ignore]
fn test_deletion_protection_workflow() {
    use common::{
        allow_write_actions, create_test_instance, delete_test_instance, make_resource_name,
        run_triton_with_profile,
    };

    // Skip if write actions not allowed
    if !allow_write_actions() {
        eprintln!("Skipping test: requires config.allowWriteActions");
        return;
    }

    let inst_alias = make_resource_name("tritontest-delprotect");

    // Cleanup any existing instance with this alias
    eprintln!("Cleanup: removing any existing instance {}", inst_alias);
    let _ = run_triton_with_profile(["instance", "disable-deletion-protection", &inst_alias, "-w"]);
    delete_test_instance(&inst_alias);

    // Create test instance with --deletion-protection flag
    eprintln!(
        "Setup: creating test instance {} with deletion protection",
        inst_alias
    );
    let inst = create_test_instance(&inst_alias, &["--deletion-protection"]);
    let inst = match inst {
        Some(i) => i,
        None => {
            eprintln!("Failed to create test instance, skipping test");
            return;
        }
    };

    eprintln!("Created instance {} ({})", inst.name, inst.id);

    // Verify deletion protection is enabled
    eprintln!("Test: verify deletion protection is enabled");
    let (stdout, _, success) = run_triton_with_profile(["instance", "get", "-j", &inst.id]);
    assert!(success, "instance get should succeed");
    let got_inst: InstanceInfo = serde_json::from_str(&stdout).expect("should parse instance JSON");
    assert_eq!(
        got_inst.deletion_protection,
        Some(true),
        "deletion_protection should be enabled"
    );

    // Attempt to delete deletion-protected instance (should fail)
    eprintln!("Test: attempt to delete deletion-protected instance");
    let (_, stderr, success) = run_triton_with_profile(["instance", "rm", &inst.id, "-w", "-f"]);
    assert!(
        !success,
        "delete should fail with deletion protection enabled"
    );
    assert!(
        stderr.contains("deletion_protection") || stderr.contains("DeletionProtection"),
        "error should mention deletion_protection: {}",
        stderr
    );

    // Disable deletion protection
    eprintln!("Test: triton instance disable-deletion-protection");
    let (stdout, _, success) =
        run_triton_with_profile(["instance", "disable-deletion-protection", &inst.id, "-w"]);
    assert!(success, "disable-deletion-protection should succeed");
    assert!(
        stdout.contains(&format!(
            "Disabled deletion protection for instance \"{}\"",
            inst.id
        )),
        "output should confirm deletion protection disabled: {}",
        stdout
    );

    // Verify deletion protection is disabled
    let (stdout, _, success) = run_triton_with_profile(["instance", "get", "-j", &inst.id]);
    assert!(success, "instance get should succeed");
    let got_inst: InstanceInfo = serde_json::from_str(&stdout).expect("should parse instance JSON");
    assert!(
        got_inst.deletion_protection != Some(true),
        "deletion_protection should be disabled"
    );

    // Disable again (idempotent - should still succeed)
    eprintln!("Test: triton instance disable-deletion-protection (already disabled)");
    let (stdout, _, success) =
        run_triton_with_profile(["instance", "disable-deletion-protection", &inst.id, "-w"]);
    assert!(
        success,
        "disable-deletion-protection should succeed even when already disabled"
    );
    assert!(
        stdout.contains(&format!(
            "Disabled deletion protection for instance \"{}\"",
            inst.id
        )),
        "output should confirm deletion protection disabled"
    );

    // Enable deletion protection
    eprintln!("Test: triton instance enable-deletion-protection");
    let (stdout, _, success) =
        run_triton_with_profile(["instance", "enable-deletion-protection", &inst.id, "-w"]);
    assert!(success, "enable-deletion-protection should succeed");
    assert!(
        stdout.contains(&format!(
            "Enabled deletion protection for instance \"{}\"",
            inst.id
        )),
        "output should confirm deletion protection enabled: {}",
        stdout
    );

    // Verify deletion protection is enabled again
    let (stdout, _, success) = run_triton_with_profile(["instance", "get", "-j", &inst.id]);
    assert!(success, "instance get should succeed");
    let got_inst: InstanceInfo = serde_json::from_str(&stdout).expect("should parse instance JSON");
    assert_eq!(
        got_inst.deletion_protection,
        Some(true),
        "deletion_protection should be enabled"
    );

    // Enable again (idempotent - should still succeed)
    eprintln!("Test: triton instance enable-deletion-protection (already enabled)");
    let (stdout, _, success) =
        run_triton_with_profile(["instance", "enable-deletion-protection", &inst.id, "-w"]);
    assert!(
        success,
        "enable-deletion-protection should succeed even when already enabled"
    );
    assert!(
        stdout.contains(&format!(
            "Enabled deletion protection for instance \"{}\"",
            inst.id
        )),
        "output should confirm deletion protection enabled"
    );

    // Cleanup: disable deletion protection and delete instance
    eprintln!("Cleanup: disabling deletion protection and deleting instance");
    let _ = run_triton_with_profile(["instance", "disable-deletion-protection", &inst.id, "-w"]);
    delete_test_instance(&inst.id);
}

/// Test that `triton create --deletion-protection` creates instance with protection enabled
#[test]
#[ignore]
fn test_create_with_deletion_protection() {
    use common::{
        allow_write_actions, create_test_instance, delete_test_instance, make_resource_name,
        run_triton_with_profile,
    };

    // Skip if write actions not allowed
    if !allow_write_actions() {
        eprintln!("Skipping test: requires config.allowWriteActions");
        return;
    }

    let inst_alias = make_resource_name("tritontest-delprotect2");

    // Cleanup
    let _ = run_triton_with_profile(["instance", "disable-deletion-protection", &inst_alias, "-w"]);
    delete_test_instance(&inst_alias);

    // Create with --deletion-protection
    let inst = create_test_instance(&inst_alias, &["--deletion-protection"]);
    let inst = match inst {
        Some(i) => i,
        None => {
            eprintln!("Failed to create test instance, skipping test");
            return;
        }
    };

    // Verify deletion protection is enabled
    let (stdout, _, success) = run_triton_with_profile(["instance", "get", "-j", &inst.id]);
    assert!(success, "instance get should succeed");
    let got_inst: InstanceInfo = serde_json::from_str(&stdout).expect("should parse instance JSON");
    assert_eq!(
        got_inst.deletion_protection,
        Some(true),
        "deletion_protection should be enabled on create"
    );

    // Cleanup
    let _ = run_triton_with_profile(["instance", "disable-deletion-protection", &inst.id, "-w"]);
    delete_test_instance(&inst.id);
}
