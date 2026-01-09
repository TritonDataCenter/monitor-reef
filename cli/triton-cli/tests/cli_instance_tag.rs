// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Instance tag CLI tests
//!
//! Tests for `triton inst tag` commands.
//!
//! Tests are split into:
//! - Offline tests (help, usage) - run without API access
//! - API tests (list, get, set, delete) - marked with #[ignore], require config.json
//!   and allowWriteActions: true

// Allow deprecated - cargo_bin is standard for CLI testing
#![allow(deprecated, clippy::expect_used)]

mod common;

use assert_cmd::Command;
use predicates::prelude::*;
use std::collections::HashMap;

fn triton_cmd() -> Command {
    Command::cargo_bin("triton").expect("Failed to find triton binary")
}

// =============================================================================
// Offline tests - no API access required
// =============================================================================

/// Test `triton inst tag -h` shows help
#[test]
fn test_inst_tag_help_short() {
    triton_cmd()
        .args(["inst", "tag", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton inst tag --help` shows help
#[test]
fn test_inst_tag_help_long() {
    triton_cmd()
        .args(["inst", "tag", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton inst tag list -h` shows help
#[test]
fn test_inst_tag_list_help() {
    triton_cmd()
        .args(["inst", "tag", "list", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton inst tag ls` alias works
#[test]
fn test_inst_tag_ls_alias() {
    triton_cmd()
        .args(["inst", "tag", "ls", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton inst tag get -h` shows help
#[test]
fn test_inst_tag_get_help() {
    triton_cmd()
        .args(["inst", "tag", "get", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton inst tag set -h` shows help
#[test]
fn test_inst_tag_set_help() {
    triton_cmd()
        .args(["inst", "tag", "set", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton inst tag delete -h` shows help
#[test]
fn test_inst_tag_delete_help() {
    triton_cmd()
        .args(["inst", "tag", "delete", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton inst tag rm` alias works
#[test]
fn test_inst_tag_rm_alias() {
    triton_cmd()
        .args(["inst", "tag", "rm", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton inst tag replace-all -h` shows help
#[test]
fn test_inst_tag_replace_all_help() {
    triton_cmd()
        .args(["inst", "tag", "replace-all", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton inst tags -h` shows help (shortcut)
#[test]
fn test_inst_tags_shortcut_help() {
    triton_cmd()
        .args(["inst", "tags", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

// =============================================================================
// API write tests - require config.json with allowWriteActions: true
// These tests are ignored by default and run with `make triton-test-api`
// =============================================================================

/// Full instance tag workflow test
/// This test creates an instance, performs tag operations, and cleans up.
///
/// Ported from node-triton test/integration/cli-instance-tag.test.js
#[test]
#[ignore]
#[allow(clippy::approx_constant)]
fn test_instance_tag_workflow() {
    use common::{
        allow_write_actions, create_test_instance, delete_test_instance, fixture_path,
        make_resource_name, run_triton_with_profile, short_id,
    };

    // Skip if write actions not allowed
    if !allow_write_actions() {
        eprintln!("Skipping test: requires config.allowWriteActions");
        return;
    }

    let inst_alias = make_resource_name("tritontest-insttag");

    // Cleanup any existing instance with this alias
    eprintln!("Cleanup: removing any existing instance {}", inst_alias);
    delete_test_instance(&inst_alias);

    // Create test instance with initial tag
    eprintln!("Setup: creating test instance {}", inst_alias);
    let inst = create_test_instance(&inst_alias, &["--tag", "blah=bling"]);
    let inst = match inst {
        Some(i) => i,
        None => {
            eprintln!("Failed to create test instance, skipping test");
            return;
        }
    };

    eprintln!("Created instance {} ({})", inst.name, inst.id);
    let inst_short_id = short_id(&inst.id);

    // Test: triton inst tag ls INST
    eprintln!("Test: triton inst tag ls {}", inst.name);
    let (stdout, _, success) = run_triton_with_profile(["inst", "tag", "ls", &inst.name]);
    assert!(success, "tag ls should succeed");
    let tags: HashMap<String, serde_json::Value> =
        serde_json::from_str(&stdout).expect("should parse JSON");
    assert_eq!(tags.get("blah"), Some(&serde_json::json!("bling")));

    // Test: triton inst tags INST (shortcut)
    eprintln!("Test: triton inst tags {}", inst.name);
    let (stdout, _, success) = run_triton_with_profile(["inst", "tags", &inst.name]);
    assert!(success, "tags shortcut should succeed");
    let tags: HashMap<String, serde_json::Value> =
        serde_json::from_str(&stdout).expect("should parse JSON");
    assert_eq!(tags.get("blah"), Some(&serde_json::json!("bling")));

    // Test: triton inst tag set -w INST name=value (with type coercion)
    eprintln!(
        "Test: triton inst tag set -w {} foo=bar pi=3.14 really=true",
        inst.id
    );
    let (stdout, _, success) = run_triton_with_profile([
        "inst",
        "tag",
        "set",
        "-w",
        &inst.id,
        "foo=bar",
        "pi=3.14",
        "really=true",
    ]);
    assert!(success, "tag set should succeed");
    let tags: HashMap<String, serde_json::Value> =
        serde_json::from_str(&stdout).expect("should parse JSON");
    assert_eq!(tags.get("blah"), Some(&serde_json::json!("bling")));
    assert_eq!(tags.get("foo"), Some(&serde_json::json!("bar")));
    assert_eq!(tags.get("pi"), Some(&serde_json::json!(3.14)));
    assert_eq!(tags.get("really"), Some(&serde_json::json!(true)));

    // Test: triton inst tag get INST foo (using short ID)
    eprintln!("Test: triton inst tag get {} foo", inst_short_id);
    let (stdout, _, success) =
        run_triton_with_profile(["inst", "tag", "get", &inst_short_id, "foo"]);
    assert!(success, "tag get should succeed");
    assert_eq!(stdout.trim(), "bar");

    // Test: triton inst tag get INST foo -j
    eprintln!("Test: triton inst tag get {} foo -j", inst.id);
    let (stdout, _, success) =
        run_triton_with_profile(["inst", "tag", "get", &inst.id, "foo", "-j"]);
    assert!(success, "tag get -j should succeed");
    assert_eq!(stdout.trim(), "\"bar\"");

    // Test: triton inst tag get INST really -j (boolean)
    eprintln!("Test: triton inst tag get {} really -j", inst.name);
    let (stdout, _, success) =
        run_triton_with_profile(["inst", "tag", "get", &inst.name, "really", "-j"]);
    assert!(success, "tag get -j should succeed");
    assert_eq!(stdout.trim(), "true");

    // Test: triton inst tag set -w INST -f tags.json
    let tags_json_path = fixture_path("tags.json");
    eprintln!(
        "Test: triton inst tag set -w {} -f {:?}",
        inst.name, tags_json_path
    );
    let (stdout, _, success) = run_triton_with_profile([
        "inst",
        "tag",
        "set",
        "-w",
        &inst.name,
        "-f",
        tags_json_path.to_str().unwrap(),
    ]);
    assert!(success, "tag set -f json should succeed");
    let tags: HashMap<String, serde_json::Value> =
        serde_json::from_str(&stdout).expect("should parse JSON");
    assert_eq!(tags.get("blah"), Some(&serde_json::json!("bling")));
    assert_eq!(tags.get("foo"), Some(&serde_json::json!("bling"))); // overwritten by tags.json
    assert_eq!(tags.get("pi"), Some(&serde_json::json!(3.14)));
    assert_eq!(tags.get("really"), Some(&serde_json::json!(true)));

    // Test: triton inst tag set -w INST -f tags.kv
    let tags_kv_path = fixture_path("tags.kv");
    eprintln!(
        "Test: triton inst tag set -w {} -f {:?}",
        inst.name, tags_kv_path
    );
    let (stdout, _, success) = run_triton_with_profile([
        "inst",
        "tag",
        "set",
        "-w",
        &inst.name,
        "-f",
        tags_kv_path.to_str().unwrap(),
    ]);
    assert!(success, "tag set -f kv should succeed");
    let tags: HashMap<String, serde_json::Value> =
        serde_json::from_str(&stdout).expect("should parse JSON");
    assert_eq!(tags.get("blah"), Some(&serde_json::json!("bling")));
    assert_eq!(tags.get("foo"), Some(&serde_json::json!("bling")));
    assert_eq!(tags.get("pi"), Some(&serde_json::json!(3.14)));
    assert_eq!(tags.get("really"), Some(&serde_json::json!(true)));
    assert_eq!(tags.get("key"), Some(&serde_json::json!("value")));
    assert_eq!(tags.get("beep"), Some(&serde_json::json!("boop")));

    // Test: triton inst tag rm -w INST key really
    eprintln!("Test: triton inst tag rm -w {} key really", inst.name);
    let (stdout, _, success) =
        run_triton_with_profile(["inst", "tag", "rm", "-w", &inst.name, "key", "really"]);
    assert!(success, "tag rm should succeed");
    let lines: Vec<&str> = stdout.trim().lines().collect();
    assert!(
        lines.iter().any(|l| l.starts_with("Deleted tag key")),
        "should show deleted key"
    );
    assert!(
        lines.iter().any(|l| l.starts_with("Deleted tag really")),
        "should show deleted really"
    );

    // Test: triton inst tag list INST (verify deletions)
    eprintln!("Test: triton inst tag list {}", inst.name);
    let (stdout, _, success) = run_triton_with_profile(["inst", "tag", "list", &inst.name]);
    assert!(success, "tag list should succeed");
    let tags: HashMap<String, serde_json::Value> =
        serde_json::from_str(&stdout).expect("should parse JSON");
    assert_eq!(tags.get("blah"), Some(&serde_json::json!("bling")));
    assert_eq!(tags.get("foo"), Some(&serde_json::json!("bling")));
    assert_eq!(tags.get("pi"), Some(&serde_json::json!(3.14)));
    assert_eq!(tags.get("beep"), Some(&serde_json::json!("boop")));
    assert!(!tags.contains_key("key"), "key should be deleted");
    assert!(!tags.contains_key("really"), "really should be deleted");

    // Test: triton inst tag replace-all -w INST whoa=there
    eprintln!(
        "Test: triton inst tag replace-all -w {} whoa=there",
        inst.name
    );
    let (stdout, _, success) =
        run_triton_with_profile(["inst", "tag", "replace-all", "-w", &inst.name, "whoa=there"]);
    assert!(success, "tag replace-all should succeed");
    let tags: HashMap<String, serde_json::Value> =
        serde_json::from_str(&stdout).expect("should parse JSON");
    assert_eq!(tags.len(), 1, "should have exactly one tag");
    assert_eq!(tags.get("whoa"), Some(&serde_json::json!("there")));

    // Test: triton inst tag delete -w -a INST (delete all)
    eprintln!("Test: triton inst tag delete -w -a {}", inst.name);
    let (stdout, _, success) =
        run_triton_with_profile(["inst", "tag", "delete", "-w", "-a", &inst.name]);
    assert!(success, "tag delete -a should succeed");
    assert!(
        stdout.contains(&format!("Deleted all tags on instance {}", inst.name)),
        "should show deleted all tags message"
    );

    // Test: triton inst tags INST (verify empty)
    eprintln!("Test: triton inst tags {} (verify empty)", inst.name);
    let (stdout, _, success) = run_triton_with_profile(["inst", "tags", &inst.name]);
    assert!(success, "tags should succeed");
    assert_eq!(stdout.trim(), "{}", "tags should be empty object");

    // Cleanup: delete test instance
    eprintln!("Cleanup: deleting test instance {}", inst.id);
    delete_test_instance(&inst.id);
}

/// Test tag get with non-existent key
#[test]
#[ignore]
fn test_instance_tag_get_nonexistent() {
    use common::{
        allow_write_actions, create_test_instance, delete_test_instance, make_resource_name,
        run_triton_with_profile,
    };

    if !allow_write_actions() {
        eprintln!("Skipping test: requires config.allowWriteActions");
        return;
    }

    let inst_alias = make_resource_name("tritontest-tagget");
    delete_test_instance(&inst_alias);

    let inst = match create_test_instance(&inst_alias, &[]) {
        Some(i) => i,
        None => {
            eprintln!("Failed to create test instance");
            return;
        }
    };

    // Try to get a non-existent tag
    let (_, stderr, success) =
        run_triton_with_profile(["inst", "tag", "get", &inst.name, "nonexistent"]);
    assert!(!success, "getting nonexistent tag should fail");
    assert!(
        stderr.contains("not found") || stderr.contains("ResourceNotFound"),
        "error should mention not found"
    );

    delete_test_instance(&inst.id);
}
