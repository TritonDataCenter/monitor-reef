// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Instance tag CLI tests
//!
//! Tests for `triton inst tag` commands.
//!
//! Tests are split into:
//! - Offline tests (help, usage) - run without API access
//! - API tests (list, get, set, delete) - marked with #[ignore], require config.json
//!   and allowWriteActions: true

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
// API tests - require config.json with valid profile and allowWriteActions
// These tests are ignored by default and run with `make triton-test-api`
// =============================================================================

// NOTE: Full tag API tests that create/modify tags require:
// 1. allowWriteActions: true in config.json
// 2. A running instance to test against
//
// These are more complex integration tests that would need to:
// - Find or create a test instance
// - Set tags on it
// - Verify the tag list output is JSON
// - Clean up the instance
//
// For now, we have the basic offline help tests above.
// Full API tests would look like the node-triton tests in cli-instance-tag.test.js
