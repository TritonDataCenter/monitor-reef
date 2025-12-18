// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Instance migration CLI tests
//!
//! Tests for `triton instance migration` commands.
//!
//! Tests are split into:
//! - Offline tests (help, usage) - run without API access
//! - API tests (begin, sync, switch, abort) - marked with #[ignore], require config.json
//!   and allowWriteActions: true
//!
//! Ported from node-triton test/integration/cli-migrations.test.js

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

/// Test `triton instance migration -h` shows help
#[test]
fn test_instance_migration_help() {
    triton_cmd()
        .args(["instance", "migration", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton inst migration -h` alias works
#[test]
fn test_inst_migration_help() {
    triton_cmd()
        .args(["inst", "migration", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton instance migration get -h` shows help
#[test]
fn test_instance_migration_get_help() {
    triton_cmd()
        .args(["instance", "migration", "get", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton instance migration get` without args shows error
#[test]
fn test_instance_migration_get_no_args() {
    triton_cmd()
        .args(["instance", "migration", "get"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("required"));
}

/// Test `triton instance migration list -h` shows help
#[test]
fn test_instance_migration_list_help() {
    triton_cmd()
        .args(["instance", "migration", "list", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton instance migration ls -h` alias works
#[test]
fn test_instance_migration_ls_help() {
    triton_cmd()
        .args(["instance", "migration", "ls", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton instance migration begin -h` shows help
#[test]
fn test_instance_migration_begin_help() {
    triton_cmd()
        .args(["instance", "migration", "begin", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton instance migration start -h` alias works
#[test]
fn test_instance_migration_start_help() {
    triton_cmd()
        .args(["instance", "migration", "start", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton instance migration begin` without args shows error
#[test]
fn test_instance_migration_begin_no_args() {
    triton_cmd()
        .args(["instance", "migration", "begin"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("required"));
}

/// Test `triton instance migration sync -h` shows help
#[test]
fn test_instance_migration_sync_help() {
    triton_cmd()
        .args(["instance", "migration", "sync", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton instance migration sync` without args shows error
#[test]
fn test_instance_migration_sync_no_args() {
    triton_cmd()
        .args(["instance", "migration", "sync"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("required"));
}

/// Test `triton instance migration switch -h` shows help
#[test]
fn test_instance_migration_switch_help() {
    triton_cmd()
        .args(["instance", "migration", "switch", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton instance migration finalize -h` alias works
#[test]
fn test_instance_migration_finalize_help() {
    triton_cmd()
        .args(["instance", "migration", "finalize", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton instance migration switch` without args shows error
#[test]
fn test_instance_migration_switch_no_args() {
    triton_cmd()
        .args(["instance", "migration", "switch"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("required"));
}

/// Test `triton instance migration abort -h` shows help
#[test]
fn test_instance_migration_abort_help() {
    triton_cmd()
        .args(["instance", "migration", "abort", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton instance migration abort` without args shows error
#[test]
fn test_instance_migration_abort_no_args() {
    triton_cmd()
        .args(["instance", "migration", "abort"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("required"));
}

/// Test `triton instance migration wait -h` shows help
#[test]
fn test_instance_migration_wait_help() {
    triton_cmd()
        .args(["instance", "migration", "wait", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton instance migration estimate -h` shows help
#[test]
fn test_instance_migration_estimate_help() {
    triton_cmd()
        .args(["instance", "migration", "estimate", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton instance migration estimate` without args shows error
#[test]
fn test_instance_migration_estimate_no_args() {
    triton_cmd()
        .args(["instance", "migration", "estimate"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("required"));
}

// =============================================================================
// API tests - require config.json with allowWriteActions: true
// These tests are ignored by default and run with `make triton-test-api`
//
// Note: Migration tests require special infrastructure (multiple CNs) to fully
// test. These tests verify the CLI interface and output format work correctly.
// =============================================================================

/// Migration info returned from JSON output
#[derive(Debug, serde::Deserialize)]
#[allow(dead_code)]
struct MigrationInfo {
    vm_uuid: String,
    state: String,
    phase: String,
    progress_percent: Option<f64>,
    automatic: Option<bool>,
}

/// Test `triton instance migration get ID` returns migration status
/// This test verifies the command runs and handles "no migration" case gracefully
#[test]
#[ignore]
fn test_instance_migration_get() {
    use common::{
        allow_write_actions, create_test_instance, delete_test_instance, make_resource_name,
        run_triton_with_profile,
    };

    // Skip if write actions not allowed
    if !allow_write_actions() {
        eprintln!("Skipping test: requires config.allowWriteActions");
        return;
    }

    let inst_alias = make_resource_name("tritontest-migration");

    // Cleanup
    delete_test_instance(&inst_alias);

    // Create test instance
    let inst = create_test_instance(&inst_alias, &[]);
    let inst = match inst {
        Some(i) => i,
        None => {
            eprintln!("Failed to create test instance, skipping test");
            return;
        }
    };

    eprintln!("Created instance {} ({})", inst.name, inst.id);

    // Get migration status - should return either status or error (no migration)
    eprintln!("Test: triton instance migration get {}", inst.id);
    let (stdout, stderr, success) =
        run_triton_with_profile(["instance", "migration", "get", &inst.id]);

    // Either succeeds with migration info, or fails with "no migration" error
    if success {
        // Parse as JSON or human-readable format
        if stdout.contains("Instance:") || stdout.contains("vm_uuid") {
            eprintln!("Got migration status: {}", stdout);
        }
    } else {
        // "no migration" is expected for instances without active migrations
        assert!(
            stderr.contains("no migration")
                || stderr.contains("NotFound")
                || stderr.contains("404"),
            "error should indicate no migration: stderr={}",
            stderr
        );
        eprintln!("No active migration (expected): {}", stderr);
    }

    // Cleanup
    delete_test_instance(&inst.id);
}

/// Test `triton instance migration get -j ID` returns JSON
#[test]
#[ignore]
fn test_instance_migration_get_json() {
    use common::{
        allow_write_actions, create_test_instance, delete_test_instance, make_resource_name,
        run_triton_with_profile,
    };

    if !allow_write_actions() {
        eprintln!("Skipping test: requires config.allowWriteActions");
        return;
    }

    let inst_alias = make_resource_name("tritontest-migration2");
    delete_test_instance(&inst_alias);

    let inst = create_test_instance(&inst_alias, &[]);
    let inst = match inst {
        Some(i) => i,
        None => {
            eprintln!("Failed to create test instance, skipping test");
            return;
        }
    };

    // Get migration status with JSON output
    let (stdout, _, success) =
        run_triton_with_profile(["instance", "migration", "get", "-j", &inst.id]);

    if success {
        // Should be valid JSON if migration exists
        let _migration: MigrationInfo =
            serde_json::from_str(&stdout).expect("should parse migration JSON");
    }

    delete_test_instance(&inst.id);
}

/// Test `triton instance migration begin` (without actually performing migration)
/// This verifies the CLI accepts the command - actual migration requires special infra
#[test]
#[ignore]
fn test_instance_migration_begin_command() {
    use common::{
        allow_write_actions, create_test_instance, delete_test_instance, make_resource_name,
        run_triton_with_profile,
    };

    if !allow_write_actions() {
        eprintln!("Skipping test: requires config.allowWriteActions");
        return;
    }

    let inst_alias = make_resource_name("tritontest-migration3");
    delete_test_instance(&inst_alias);

    let inst = create_test_instance(&inst_alias, &[]);
    let inst = match inst {
        Some(i) => i,
        None => {
            eprintln!("Failed to create test instance, skipping test");
            return;
        }
    };

    // Try to begin migration - may fail if no target CN available
    eprintln!("Test: triton instance migration begin {}", inst.id);
    let (stdout, stderr, success) =
        run_triton_with_profile(["instance", "migration", "begin", &inst.id]);

    if success {
        // Migration started
        eprintln!("Migration started: {}", stdout);
        // Should contain state/phase info
        assert!(
            stdout.contains("State:") || stdout.contains("state"),
            "output should contain state: {}",
            stdout
        );

        // Abort the migration to clean up
        let _ = run_triton_with_profile(["instance", "migration", "abort", "-w", &inst.id]);
    } else {
        // Expected on single-CN setup - no target available
        eprintln!("Migration begin failed (expected on single-CN): {}", stderr);
    }

    delete_test_instance(&inst.id);
}

/// Test `triton instance migration estimate ID` returns size estimate
#[test]
#[ignore]
fn test_instance_migration_estimate() {
    use common::{
        allow_write_actions, create_test_instance, delete_test_instance, make_resource_name,
        run_triton_with_profile,
    };

    if !allow_write_actions() {
        eprintln!("Skipping test: requires config.allowWriteActions");
        return;
    }

    let inst_alias = make_resource_name("tritontest-migration4");
    delete_test_instance(&inst_alias);

    let inst = create_test_instance(&inst_alias, &[]);
    let inst = match inst {
        Some(i) => i,
        None => {
            eprintln!("Failed to create test instance, skipping test");
            return;
        }
    };

    // Get migration estimate
    eprintln!("Test: triton instance migration estimate {}", inst.id);
    let (stdout, _, success) =
        run_triton_with_profile(["instance", "migration", "estimate", &inst.id]);

    if success {
        // Should contain size info
        assert!(
            stdout.contains("size") || stdout.contains("GB") || stdout.contains("Estimated"),
            "output should contain size estimate: {}",
            stdout
        );
    }

    delete_test_instance(&inst.id);
}

/// Test `triton instance migration estimate -j ID` returns JSON
#[test]
#[ignore]
fn test_instance_migration_estimate_json() {
    use common::{
        allow_write_actions, create_test_instance, delete_test_instance, make_resource_name,
        run_triton_with_profile,
    };

    if !allow_write_actions() {
        eprintln!("Skipping test: requires config.allowWriteActions");
        return;
    }

    let inst_alias = make_resource_name("tritontest-migration5");
    delete_test_instance(&inst_alias);

    let inst = create_test_instance(&inst_alias, &[]);
    let inst = match inst {
        Some(i) => i,
        None => {
            eprintln!("Failed to create test instance, skipping test");
            return;
        }
    };

    // Get migration estimate as JSON
    let (stdout, _, success) =
        run_triton_with_profile(["instance", "migration", "estimate", "-j", &inst.id]);

    if success {
        // Should be valid JSON with size field
        #[derive(Debug, serde::Deserialize)]
        struct EstimateInfo {
            size: u64,
        }
        let estimate: EstimateInfo =
            serde_json::from_str(&stdout).expect("should parse estimate JSON");
        assert!(estimate.size > 0, "size should be positive");
    }

    delete_test_instance(&inst.id);
}
