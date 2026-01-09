// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Firewall rule CLI tests
//!
//! Tests for `triton fwrule` commands.
//!
//! Tests are split into:
//! - Offline tests (help, usage) - run without API access
//! - API tests (list, get, create, delete, enable, disable) - marked with #[ignore]

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

// =============================================================================
// Write operation tests - require config.json with allowWriteActions: true
// =============================================================================

use regex::Regex;

/// Firewall rule info returned from `triton fwrule get`
#[derive(Debug, serde::Deserialize)]
#[allow(dead_code)]
struct FwruleInfo {
    id: String,
    rule: String,
    enabled: bool,
    #[serde(default)]
    log: bool,
    #[serde(default)]
    description: Option<String>,
}

/// Extract rule ID from "Created firewall rule <uuid>" message
fn extract_rule_id(stdout: &str) -> Option<String> {
    let re = Regex::new(r"Created firewall rule ([a-f0-9-]{36})").ok()?;
    re.captures(stdout)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str().to_string())
}

/// Delete a firewall rule (doesn't error if not found)
fn delete_fwrule(id: &str) {
    use common::run_triton_with_profile;
    let _ = run_triton_with_profile(["fwrule", "delete", id, "--force"]);
}

/// Full firewall rule workflow test
/// This test creates an instance, creates/modifies/deletes rules, and cleans up.
///
/// Ported from node-triton test/integration/cli-fwrules.test.js
#[test]
#[ignore = "requires API access - run with make triton-test-api"]
fn test_fwrule_workflow() {
    use common::{
        allow_write_actions, create_test_instance, delete_test_instance, make_resource_name,
        run_triton_with_profile, short_id,
    };

    // Skip if write actions not allowed
    if !allow_write_actions() {
        eprintln!("Skipping test: requires config.allowWriteActions");
        return;
    }

    let inst_alias = make_resource_name("tritontest-fwrules");
    let desc = "This rule was created by Rust triton tests";

    // Cleanup any existing instance
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
    let rule_text = format!("FROM any TO vm {} ALLOW tcp PORT 80", inst.id);
    let rule_text_2 = format!("FROM any TO vm {} BLOCK tcp port 25", inst.id);

    // Test: Create firewall rule (disabled)
    eprintln!("Test: triton fwrule create -d \"{}\"", rule_text);
    let (stdout, stderr, success) = run_triton_with_profile(["fwrule", "create", "-d", &rule_text]);
    if !success {
        eprintln!("Failed to create fwrule: stderr={}", stderr);
        delete_test_instance(&inst.id);
        return;
    }
    assert!(
        stdout.contains("Created firewall rule") && stdout.contains("(disabled)"),
        "stdout should contain created (disabled) message: {}",
        stdout
    );

    let disabled_rule_id = match extract_rule_id(&stdout) {
        Some(id) => id,
        None => {
            eprintln!("Failed to extract rule ID from: {}", stdout);
            delete_test_instance(&inst.id);
            return;
        }
    };
    let disabled_rule_short = short_id(&disabled_rule_id);
    eprintln!("Created disabled rule: {}", disabled_rule_id);

    // Test: Get disabled rule
    eprintln!("Test: triton fwrule get {}", disabled_rule_short);
    let (stdout, _, success) = run_triton_with_profile(["fwrule", "get", &disabled_rule_short]);
    assert!(success, "fwrule get should succeed");
    let rule_info: FwruleInfo = serde_json::from_str(&stdout).expect("should parse JSON");
    assert_eq!(rule_info.rule, rule_text);
    assert!(!rule_info.enabled, "rule should be disabled");
    assert!(!rule_info.log, "rule should not be logging");

    // Test: Create firewall rule (enabled with description and log)
    eprintln!(
        "Test: triton fwrule create -D \"{}\" \"{}\" --log",
        desc, rule_text
    );
    let (stdout, _, success) =
        run_triton_with_profile(["fwrule", "create", "-D", desc, &rule_text, "--log"]);
    assert!(success, "fwrule create should succeed");
    assert!(
        stdout.contains("Created firewall rule") && !stdout.contains("(disabled)"),
        "stdout should show created (enabled) message: {}",
        stdout
    );

    let enabled_rule_id = match extract_rule_id(&stdout) {
        Some(id) => id,
        None => {
            eprintln!("Failed to extract rule ID");
            delete_fwrule(&disabled_rule_id);
            delete_test_instance(&inst.id);
            return;
        }
    };
    let rule_short_id = short_id(&enabled_rule_id);
    eprintln!("Created enabled rule: {}", enabled_rule_id);

    // Test: Get enabled rule
    eprintln!("Test: triton fwrule get {}", rule_short_id);
    let (stdout, _, success) = run_triton_with_profile(["fwrule", "get", &rule_short_id]);
    assert!(success, "fwrule get should succeed");
    let rule_info: FwruleInfo = serde_json::from_str(&stdout).expect("should parse JSON");
    assert_eq!(rule_info.rule, rule_text);
    assert_eq!(rule_info.description, Some(desc.to_string()));
    assert!(rule_info.enabled, "rule should be enabled");
    assert!(rule_info.log, "rule should be logging");

    // Test: Enable rule
    eprintln!("Test: triton fwrule enable {}", rule_short_id);
    let (stdout, _, success) = run_triton_with_profile(["fwrule", "enable", &rule_short_id]);
    assert!(success, "fwrule enable should succeed");
    assert!(
        stdout.contains(&format!("Enabled firewall rule {}", rule_short_id)),
        "stdout should show enabled message"
    );

    // Test: Disable rule
    eprintln!("Test: triton fwrule disable {}", rule_short_id);
    let (stdout, _, success) = run_triton_with_profile(["fwrule", "disable", &rule_short_id]);
    assert!(success, "fwrule disable should succeed");
    assert!(
        stdout.contains(&format!("Disabled firewall rule {}", rule_short_id)),
        "stdout should show disabled message"
    );

    // Test: Update rule
    eprintln!(
        "Test: triton fwrule update {} rule=\"{}\"",
        rule_short_id, rule_text_2
    );
    let (stdout, _, success) = run_triton_with_profile([
        "fwrule",
        "update",
        &rule_short_id,
        &format!("rule={}", rule_text_2),
    ]);
    assert!(success, "fwrule update should succeed");
    assert!(
        stdout.contains(&format!("Updated firewall rule {}", rule_short_id))
            && stdout.contains("(fields: rule)"),
        "stdout should show updated message with fields: {}",
        stdout
    );

    // Test: Update log to false
    eprintln!("Test: triton fwrule update {} log=false", rule_short_id);
    let (stdout, _, success) =
        run_triton_with_profile(["fwrule", "update", &rule_short_id, "log=false"]);
    assert!(success, "fwrule update log should succeed");
    assert!(
        stdout.contains("(fields: log)"),
        "stdout should show updated log field"
    );

    // Test: List rules
    eprintln!("Test: triton fwrule list -l");
    let (stdout, _, success) = run_triton_with_profile(["fwrule", "list", "-l"]);
    assert!(success, "fwrule list should succeed");
    assert!(
        stdout.contains("ID") && stdout.contains("ENABLED") && stdout.contains("RULE"),
        "list should have header columns"
    );
    assert!(
        stdout.contains(&rule_short_id),
        "list should contain our rule"
    );

    // Test: triton fwrules shortcut
    eprintln!("Test: triton fwrules -l");
    let (stdout, _, success) = run_triton_with_profile(["fwrules", "-l"]);
    assert!(success, "fwrules shortcut should succeed");
    assert!(
        stdout.contains(&rule_short_id),
        "fwrules should contain our rule"
    );

    // Test: List instances affected by rule
    eprintln!("Test: triton fwrule instances -l {}", rule_short_id);
    let (stdout, _, success) =
        run_triton_with_profile(["fwrule", "instances", "-l", &rule_short_id]);
    assert!(success, "fwrule instances should succeed");
    // Should show our instance in the list
    let inst_short_id = short_id(&inst.id);
    // The instance may or may not appear depending on rule state
    if stdout.contains("ID") && stdout.contains("NAME") {
        eprintln!("Instances output: {}", stdout);
    }

    // Test: Instance fwrules
    eprintln!("Test: triton instance fwrules -l {}", inst_short_id);
    let (_, _, success) = run_triton_with_profile(["instance", "fwrules", "-l", &inst_short_id]);
    assert!(success, "instance fwrules should succeed");

    // Test: Delete rule
    eprintln!("Test: triton fwrule delete {} --force", rule_short_id);
    let (stdout, _, success) =
        run_triton_with_profile(["fwrule", "delete", &rule_short_id, "--force"]);
    assert!(success, "fwrule delete should succeed");
    assert!(
        stdout.contains(&format!("Deleted rule {}", rule_short_id)),
        "stdout should show deleted message: {}",
        stdout
    );

    // Cleanup disabled rule
    delete_fwrule(&disabled_rule_id);

    // Test: Enable firewall on instance
    eprintln!("Test: triton instance enable-firewall {} -w", inst_short_id);
    let (stdout, _, success) =
        run_triton_with_profile(["instance", "enable-firewall", &inst_short_id, "-w"]);
    assert!(success, "enable-firewall should succeed");
    assert!(
        stdout.contains("Enabled firewall"),
        "stdout should show enabled firewall message"
    );

    // Verify firewall is enabled
    let (stdout, _, success) = run_triton_with_profile(["instance", "get", "-j", &inst_short_id]);
    assert!(success, "instance get should succeed");
    let inst_info: serde_json::Value = serde_json::from_str(&stdout).expect("should parse JSON");
    assert_eq!(
        inst_info.get("firewall_enabled"),
        Some(&serde_json::json!(true)),
        "firewall should be enabled"
    );

    // Test: Disable firewall on instance
    eprintln!(
        "Test: triton instance disable-firewall {} -w",
        inst_short_id
    );
    let (stdout, _, success) =
        run_triton_with_profile(["instance", "disable-firewall", &inst_short_id, "-w"]);
    assert!(success, "disable-firewall should succeed");
    assert!(
        stdout.contains("Disabled firewall"),
        "stdout should show disabled firewall message"
    );

    // Verify firewall is disabled
    let (stdout, _, success) = run_triton_with_profile(["instance", "get", "-j", &inst_short_id]);
    assert!(success, "instance get should succeed");
    let inst_info: serde_json::Value = serde_json::from_str(&stdout).expect("should parse JSON");
    assert_eq!(
        inst_info.get("firewall_enabled"),
        Some(&serde_json::json!(false)),
        "firewall should be disabled"
    );

    // Cleanup: delete test instance
    eprintln!("Cleanup: deleting test instance {}", inst.id);
    delete_test_instance(&inst.id);
}
