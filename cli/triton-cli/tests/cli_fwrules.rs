// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Firewall rule CLI tests
//!
//! Tests for `triton fwrule` commands.
//!
//! Tests are split into:
//! - Offline tests (help, usage) - run without API access
//! - API tests (list, get, create, delete, enable, disable) - marked with #[ignore]

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

/// Test `triton fwrule -h` shows help
#[test]
fn test_fwrule_help_short() {
    triton_cmd()
        .args(["fwrule", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Manage firewall rules"));
}

/// Test `triton fwrule --help` shows help
#[test]
fn test_fwrule_help_long() {
    triton_cmd()
        .args(["fwrule", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Manage firewall rules"));
}

/// Test `triton help fwrule` shows help
#[test]
fn test_help_fwrule() {
    triton_cmd()
        .args(["help", "fwrule"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Manage firewall rules"));
}

/// Test `triton fwrule list -h` shows help
#[test]
fn test_fwrule_list_help() {
    triton_cmd()
        .args(["fwrule", "list", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("List firewall rules"));
}

/// Test `triton fwrule ls` alias works
#[test]
fn test_fwrule_ls_alias() {
    triton_cmd()
        .args(["fwrule", "ls", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("List firewall rules"));
}

/// Test `triton fwrules` shortcut works
#[test]
fn test_fwrules_shortcut() {
    triton_cmd()
        .args(["fwrules", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("List firewall rules"));
}

/// Test `triton fwrule get -h` shows help
#[test]
fn test_fwrule_get_help() {
    triton_cmd()
        .args(["fwrule", "get", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Get firewall rule details"));
}

/// Test `triton fwrule help get` shows help
#[test]
fn test_fwrule_help_get() {
    triton_cmd()
        .args(["fwrule", "help", "get"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Get firewall rule details"));
}

/// Test `triton fwrule get` without args shows error
#[test]
fn test_fwrule_get_no_args() {
    triton_cmd()
        .args(["fwrule", "get"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("required"));
}

/// Test `triton fwrule create -h` shows help
#[test]
fn test_fwrule_create_help() {
    triton_cmd()
        .args(["fwrule", "create", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Create firewall rule"));
}

/// Test `triton fwrule delete -h` shows help
#[test]
fn test_fwrule_delete_help() {
    triton_cmd()
        .args(["fwrule", "delete", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Delete firewall rule"));
}

/// Test `triton fwrule rm` alias works
#[test]
fn test_fwrule_rm_alias() {
    triton_cmd()
        .args(["fwrule", "rm", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Delete firewall rule"));
}

/// Test `triton fwrule enable -h` shows help
#[test]
fn test_fwrule_enable_help() {
    triton_cmd()
        .args(["fwrule", "enable", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Enable firewall rule"));
}

/// Test `triton fwrule disable -h` shows help
#[test]
fn test_fwrule_disable_help() {
    triton_cmd()
        .args(["fwrule", "disable", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Disable firewall rule"));
}

/// Test `triton fwrule update -h` shows help
#[test]
fn test_fwrule_update_help() {
    triton_cmd()
        .args(["fwrule", "update", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Update firewall rule"));
}

/// Test `triton fwrule instances -h` shows help
#[test]
fn test_fwrule_instances_help() {
    triton_cmd()
        .args(["fwrule", "instances", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("List instances affected by rule"));
}

/// Test `triton fwrule insts` alias works
#[test]
fn test_fwrule_insts_alias() {
    triton_cmd()
        .args(["fwrule", "insts", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("List instances affected by rule"));
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
fn test_fwrule_list() {
    if !common::config::has_integration_config() {
        eprintln!("Skipping: no test config found");
        return;
    }

    let output = triton_with_profile()
        .args(["fwrule", "list"])
        .output()
        .expect("Failed to execute command");

    assert!(
        output.status.success(),
        "fwrule list should succeed.\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    // Output can be empty if no rules exist, or show a table with SHORTID column
    let stdout = String::from_utf8_lossy(&output.stdout);
    // If there are rules, should have SHORTID header
    if !stdout.trim().is_empty() {
        assert!(
            stdout.contains("SHORTID") || stdout.contains("No firewall rules"),
            "fwrule list should show SHORTID column or indicate no rules"
        );
    }
}

#[test]
#[ignore = "requires API access - run with make triton-test-api"]
fn test_fwrule_list_json() {
    if !common::config::has_integration_config() {
        eprintln!("Skipping: no test config found");
        return;
    }

    let output = triton_with_profile()
        .args(["fwrule", "list", "-j"])
        .output()
        .expect("Failed to execute command");

    assert!(
        output.status.success(),
        "fwrule list -j should succeed.\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    // JSON output should parse as array (may be empty)
    let rules: Vec<serde_json::Value> = common::json_stream_parse(&stdout);
    // Rules array may be empty, but if not empty should have id field
    if !rules.is_empty() {
        assert!(
            rules[0].get("id").is_some(),
            "Firewall rules should have id field"
        );
    }
}
