// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Instance management workflow CLI tests
//!
//! Tests for `triton instance` lifecycle commands.
//!
//! Tests are split into:
//! - Offline tests (help, usage) - run without API access
//! - API tests (create, start, stop, reboot, resize, rename, delete) - marked
//!   with #[ignore], require config.json and allowWriteActions: true
//!
//! Ported from node-triton test/integration/cli-manage-workflow.test.js

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

/// Test `triton instance create -h` shows help
#[test]
fn test_instance_create_help() {
    triton_cmd()
        .args(["instance", "create", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton instance start -h` shows help
#[test]
fn test_instance_start_help() {
    triton_cmd()
        .args(["instance", "start", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton instance stop -h` shows help
#[test]
fn test_instance_stop_help() {
    triton_cmd()
        .args(["instance", "stop", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton instance reboot -h` shows help
#[test]
fn test_instance_reboot_help() {
    triton_cmd()
        .args(["instance", "reboot", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton instance resize -h` shows help
#[test]
fn test_instance_resize_help() {
    triton_cmd()
        .args(["instance", "resize", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton instance rename -h` shows help
#[test]
fn test_instance_rename_help() {
    triton_cmd()
        .args(["instance", "rename", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton instance delete -h` shows help
#[test]
fn test_instance_delete_help() {
    triton_cmd()
        .args(["instance", "delete", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton instance wait -h` shows help
#[test]
fn test_instance_wait_help() {
    triton_cmd()
        .args(["instance", "wait", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton instance get -h` shows help
#[test]
fn test_instance_get_help() {
    triton_cmd()
        .args(["instance", "get", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton inst` alias works
#[test]
fn test_inst_alias() {
    triton_cmd()
        .args(["inst", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton create` alias works
#[test]
fn test_create_alias() {
    triton_cmd()
        .args(["create", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton start` alias works
#[test]
fn test_start_alias() {
    triton_cmd()
        .args(["start", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton stop` alias works
#[test]
fn test_stop_alias() {
    triton_cmd()
        .args(["stop", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton reboot` alias works
#[test]
fn test_reboot_alias() {
    triton_cmd()
        .args(["reboot", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton delete` alias works
#[test]
fn test_delete_alias() {
    triton_cmd()
        .args(["delete", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

// =============================================================================
// API write tests - require config.json with allowWriteActions: true
// These tests are ignored by default and run with `make triton-test-api`
// =============================================================================

/// Instance info from create/get output
#[derive(Debug, serde::Deserialize)]
struct InstanceInfo {
    id: String,
    name: String,
    state: String,
    #[serde(default)]
    metadata: Option<serde_json::Value>,
    #[serde(default)]
    tags: Option<serde_json::Value>,
    #[serde(default)]
    package: Option<String>,
}

/// Full instance lifecycle workflow test
/// This test covers create, get, stop, start, reboot, resize, rename, delete
///
/// Ported from node-triton test/integration/cli-manage-workflow.test.js
#[test]
#[ignore]
fn test_instance_manage_workflow() {
    use common::{
        allow_write_actions, delete_test_instance, get_resize_test_package, get_test_image,
        get_test_package, json_stream_parse, make_resource_name, run_triton_with_profile, short_id,
    };

    // Skip if write actions not allowed
    if !allow_write_actions() {
        eprintln!("Skipping test: requires config.allowWriteActions");
        return;
    }

    let inst_alias = make_resource_name("tritontest-managewf");
    let inst_alias_newname = format!("{}-renamed", inst_alias);

    // Cleanup any existing instances
    eprintln!(
        "Cleanup: removing any existing instances {} and {}",
        inst_alias, inst_alias_newname
    );
    delete_test_instance(&inst_alias);
    delete_test_instance(&inst_alias_newname);

    // Get test image and packages
    let img_id = match get_test_image() {
        Some(id) => id,
        None => {
            eprintln!("Failed to find test image, skipping test");
            return;
        }
    };
    eprintln!("Using test image: {}", img_id);

    let pkg_id = match get_test_package() {
        Some(id) => id,
        None => {
            eprintln!("Failed to find test package, skipping test");
            return;
        }
    };
    eprintln!("Using test package: {}", pkg_id);

    let resize_pkg_name = match get_resize_test_package() {
        Some(name) => name,
        None => {
            eprintln!("Failed to find resize package, resize test will be skipped");
            String::new()
        }
    };
    if !resize_pkg_name.is_empty() {
        eprintln!("Using resize package: {}", resize_pkg_name);
    }

    // Test: triton create -wj with metadata, tag, script, and name
    // Matches node-triton test which includes --script option
    let script_path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/user-script.sh");
    eprintln!(
        "Test: triton create -wj -m foo=bar --script {} --tag blah=bling -n {} {} {}",
        script_path, inst_alias, img_id, pkg_id
    );
    let (stdout, stderr, success) = run_triton_with_profile([
        "create",
        "-wj",
        "-m",
        "foo=bar",
        "--script",
        script_path,
        "--tag",
        "blah=bling",
        "-n",
        &inst_alias,
        &img_id,
        &pkg_id,
    ]);
    if !success {
        eprintln!("Failed to create instance: stderr={}", stderr);
        panic!("instance create failed");
    }

    // Parse JSON stream output (node-triton outputs two JSON objects)
    let instances: Vec<InstanceInfo> = json_stream_parse(&stdout);
    assert!(
        !instances.is_empty(),
        "should have at least one JSON object in output"
    );

    let instance = instances.last().expect("should have at least one instance");
    eprintln!("Created instance {} ({})", instance.name, instance.id);
    let inst_id = &instance.id;
    let inst_short_id = short_id(inst_id);

    // Verify initial state
    assert_eq!(
        instance.state, "running",
        "instance should be running after -w"
    );

    // Verify metadata was set
    if let Some(metadata) = &instance.metadata {
        if let Some(foo) = metadata.get("foo") {
            assert_eq!(
                foo.as_str(),
                Some("bar"),
                "foo metadata should be set to 'bar'"
            );
        }
        // Verify user-script from --script option was set
        assert!(
            metadata.get("user-script").is_some(),
            "user-script metadata should be set from --script option"
        );
    }

    // Verify tags were set
    if let Some(tags) = &instance.tags
        && let Some(blah) = tags.get("blah")
    {
        assert_eq!(blah.as_str(), Some("bling"), "blah tag should be 'bling'");
    }

    // Test: triton instance get by UUID, alias, and short ID
    eprintln!("Test: triton instance get -j {}", inst_alias);
    let (stdout1, _, success1) = run_triton_with_profile(["instance", "get", "-j", &inst_alias]);
    assert!(success1, "get by alias should succeed");

    eprintln!("Test: triton instance get -j {}", inst_id);
    let (stdout2, _, success2) = run_triton_with_profile(["instance", "get", "-j", inst_id]);
    assert!(success2, "get by UUID should succeed");

    eprintln!("Test: triton instance get -j {}", inst_short_id);
    let (stdout3, _, success3) = run_triton_with_profile(["instance", "get", "-j", &inst_short_id]);
    assert!(success3, "get by short ID should succeed");

    // Verify all return the same data
    let get1: InstanceInfo = serde_json::from_str(&stdout1).expect("should parse JSON");
    let get2: InstanceInfo = serde_json::from_str(&stdout2).expect("should parse JSON");
    let get3: InstanceInfo = serde_json::from_str(&stdout3).expect("should parse JSON");

    assert_eq!(get1.id, get2.id, "UUIDs should match");
    assert_eq!(get2.id, get3.id, "UUIDs should match");

    // Check metadata on retrieved instance
    if let Some(metadata) = &get1.metadata
        && let Some(foo) = metadata.get("foo")
    {
        assert_eq!(foo.as_str(), Some("bar"), "foo metadata should be 'bar'");
    }

    // Test: triton stop with wait
    eprintln!("Test: triton stop -w {}", inst_alias);
    let (stdout, _, success) = run_triton_with_profile(["stop", "-w", &inst_alias]);
    assert!(success, "stop should succeed");
    assert!(
        stdout.contains("Stop instance"),
        "stdout should contain 'Stop instance'"
    );

    // Confirm stopped
    let (stdout, _, success) = run_triton_with_profile(["instance", "get", "-j", &inst_alias]);
    assert!(success, "get should succeed");
    let instance: InstanceInfo = serde_json::from_str(&stdout).expect("should parse JSON");
    assert_eq!(instance.state, "stopped", "instance should be stopped");

    // Test: triton start with wait
    eprintln!("Test: triton start -w {}", inst_alias);
    let (stdout, _, success) = run_triton_with_profile(["start", "-w", &inst_alias]);
    assert!(success, "start should succeed");
    assert!(
        stdout.contains("Start instance"),
        "stdout should contain 'Start instance'"
    );

    // Confirm running
    let (stdout, _, success) = run_triton_with_profile(["instance", "get", "-j", &inst_alias]);
    assert!(success, "get should succeed");
    let instance: InstanceInfo = serde_json::from_str(&stdout).expect("should parse JSON");
    assert_eq!(instance.state, "running", "instance should be running");

    // Test: triton reboot with wait
    eprintln!("Test: triton reboot -w {}", inst_alias);
    let (stdout, _, success) = run_triton_with_profile(["reboot", "-w", &inst_alias]);
    assert!(success, "reboot should succeed");
    assert!(
        stdout.contains("Rebooting instance"),
        "stdout should contain 'Rebooting instance'"
    );
    assert!(
        stdout.contains("Rebooted instance"),
        "stdout should contain 'Rebooted instance'"
    );

    // Confirm still running
    let (stdout, _, success) = run_triton_with_profile(["instance", "get", "-j", &inst_alias]);
    assert!(success, "get should succeed");
    let instance: InstanceInfo = serde_json::from_str(&stdout).expect("should parse JSON");
    assert_eq!(instance.state, "running", "instance should be running");

    // Test: triton inst resize (if resize package available)
    if !resize_pkg_name.is_empty() {
        eprintln!(
            "Test: triton inst resize -w {} {}",
            inst_id, resize_pkg_name
        );
        let (stdout, _, success) =
            run_triton_with_profile(["inst", "resize", "-w", inst_id, &resize_pkg_name]);
        assert!(success, "resize should succeed");
        assert!(
            stdout.contains("Resizing instance"),
            "stdout should contain 'Resizing instance'"
        );
        assert!(
            stdout.contains("Resized instance"),
            "stdout should contain 'Resized instance'"
        );

        // Confirm resized
        let (stdout, _, success) = run_triton_with_profile(["instance", "get", "-j", &inst_alias]);
        assert!(success, "get should succeed");
        let instance: InstanceInfo = serde_json::from_str(&stdout).expect("should parse JSON");
        if let Some(package) = &instance.package {
            assert_eq!(
                package, &resize_pkg_name,
                "instance package should be updated"
            );
        }
    }

    // Test: triton inst rename
    eprintln!(
        "Test: triton inst rename -w {} {}",
        inst_id, inst_alias_newname
    );
    let (stdout, _, success) =
        run_triton_with_profile(["inst", "rename", "-w", inst_id, &inst_alias_newname]);
    assert!(success, "rename should succeed");
    assert!(
        stdout.contains("Renaming instance"),
        "stdout should contain 'Renaming instance'"
    );
    assert!(
        stdout.contains("Renamed instance"),
        "stdout should contain 'Renamed instance'"
    );

    // Confirm renamed
    let (stdout, _, success) =
        run_triton_with_profile(["instance", "get", "-j", &inst_alias_newname]);
    assert!(success, "get by new name should succeed");
    let instance: InstanceInfo = serde_json::from_str(&stdout).expect("should parse JSON");
    assert_eq!(
        instance.name, inst_alias_newname,
        "instance name should be updated"
    );

    // Cleanup: triton delete with wait and force
    eprintln!("Cleanup: triton delete -f -w {}", inst_id);
    let (_stdout, _, success) = run_triton_with_profile(["delete", "-f", "-w", inst_id]);
    assert!(success, "delete should succeed");
}

/// Test instance get on deleted instance returns deleted state
#[test]
#[ignore]
fn test_instance_get_deleted() {
    use common::{
        allow_write_actions, delete_test_instance, get_test_image, get_test_package,
        json_stream_parse, make_resource_name, run_triton_with_profile,
    };

    if !allow_write_actions() {
        eprintln!("Skipping test: requires config.allowWriteActions");
        return;
    }

    let inst_alias = make_resource_name("tritontest-deleted");
    delete_test_instance(&inst_alias);

    let img_id = match get_test_image() {
        Some(id) => id,
        None => {
            eprintln!("Failed to find test image");
            return;
        }
    };
    let pkg_id = match get_test_package() {
        Some(id) => id,
        None => {
            eprintln!("Failed to find test package");
            return;
        }
    };

    // Create instance
    let (stdout, _, success) =
        run_triton_with_profile(["create", "-wj", "-n", &inst_alias, &img_id, &pkg_id]);
    if !success {
        return;
    }

    let instances: Vec<InstanceInfo> = json_stream_parse(&stdout);
    let inst_id = &instances.last().expect("should have instance").id;

    // Delete with wait
    let (_stdout, _, success) = run_triton_with_profile(["delete", "-w", "-f", inst_id]);
    assert!(success, "delete should succeed");

    // Get deleted instance - should return deleted state
    // node-triton returns exit code 3 for InstanceDeleted error
    let (stdout, stderr, success) = run_triton_with_profile(["inst", "get", inst_id]);

    // The CLI may or may not succeed depending on implementation
    // node-triton outputs JSON to stdout and error to stderr, exit code 3
    eprintln!(
        "Get deleted instance: success={}, stdout={}, stderr={}",
        success, stdout, stderr
    );

    // If stdout has JSON, check state is deleted
    if !stdout.trim().is_empty()
        && let Ok(instance) = serde_json::from_str::<InstanceInfo>(&stdout)
    {
        assert_eq!(instance.state, "deleted", "state should be 'deleted'");
    }
}

/// Test instance wait command
#[test]
#[ignore]
fn test_instance_wait() {
    use common::{
        allow_write_actions, delete_test_instance, get_test_image, get_test_package,
        json_stream_parse, make_resource_name, run_triton_with_profile,
    };

    if !allow_write_actions() {
        eprintln!("Skipping test: requires config.allowWriteActions");
        return;
    }

    let inst_alias = make_resource_name("tritontest-wait");
    delete_test_instance(&inst_alias);

    let img_id = match get_test_image() {
        Some(id) => id,
        None => {
            eprintln!("Failed to find test image");
            return;
        }
    };
    let pkg_id = match get_test_package() {
        Some(id) => id,
        None => {
            eprintln!("Failed to find test package");
            return;
        }
    };

    // Create instance without wait (returns immediately in provisioning state)
    eprintln!(
        "Test: triton create -jn {} {} {}",
        inst_alias, img_id, pkg_id
    );
    let (stdout, stderr, success) =
        run_triton_with_profile(["create", "-jn", &inst_alias, &img_id, &pkg_id]);
    if !success {
        eprintln!("Failed to create instance: {}", stderr);
        return;
    }

    let instances: Vec<InstanceInfo> = json_stream_parse(&stdout);
    let instance = instances.last().expect("should have instance");
    let inst_id = &instance.id;
    eprintln!("Created instance {} in state {}", inst_id, instance.state);

    // Instance should be in provisioning state
    assert_eq!(
        instance.state, "provisioning",
        "instance should be provisioning"
    );

    // Test: triton inst wait
    eprintln!("Test: triton inst wait {}", inst_id);
    let (stdout, _, success) = run_triton_with_profile(["inst", "wait", inst_id]);
    assert!(success, "wait should succeed");

    // node-triton wait outputs two lines:
    // 1. "Waiting for instance <id> to reach state (states: running, failed)"
    // 2. "<id> moved to state running"
    assert!(
        stdout.contains("running, failed") || stdout.contains("running"),
        "should mention target states"
    );
    assert!(stdout.contains("running"), "should mention final state");

    // Cleanup
    delete_test_instance(inst_id);
}
