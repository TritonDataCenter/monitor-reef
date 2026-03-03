// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Offline error-path tests for CLI commands
//!
//! These tests verify that the CLI properly rejects invalid input,
//! missing required arguments, and conflicting flags. All tests run
//! offline without API access.
//!
//! Note: Some commands (delete, start, stop, reboot, fwrule enable/disable,
//! key delete) accept variadic args and succeed silently with zero args.
//! Those are documented but not tested for failure here.

// Allow deprecated - cargo_bin is standard for CLI testing
#![allow(deprecated, clippy::expect_used)]

use assert_cmd::Command;
use predicates::prelude::*;

fn triton_cmd() -> Command {
    Command::cargo_bin("triton").expect("Failed to find triton binary")
}

// =============================================================================
// Instance core commands - missing required args
// =============================================================================

#[test]
fn test_instance_get_no_args() {
    triton_cmd()
        .args(["instance", "get"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("required"));
}

#[test]
fn test_instance_create_no_args() {
    triton_cmd()
        .args(["instance", "create"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("required"));
}

#[test]
fn test_instance_rename_no_args() {
    triton_cmd()
        .args(["instance", "rename"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("required"));
}

#[test]
fn test_instance_resize_no_args() {
    triton_cmd()
        .args(["instance", "resize"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("required"));
}

#[test]
fn test_instance_ssh_no_args() {
    triton_cmd()
        .args(["instance", "ssh"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("required"));
}

// Commands that accept variadic IDs succeed with zero args (no-op behavior).
// Verify they at least don't crash:
#[test]
fn test_instance_delete_zero_args_succeeds() {
    triton_cmd()
        .args(["instance", "delete"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::is_empty());
}

#[test]
fn test_instance_start_zero_args_succeeds() {
    triton_cmd()
        .args(["instance", "start"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::is_empty());
}

#[test]
fn test_instance_stop_zero_args_succeeds() {
    triton_cmd()
        .args(["instance", "stop"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::is_empty());
}

#[test]
fn test_instance_reboot_zero_args_succeeds() {
    triton_cmd()
        .args(["instance", "reboot"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::is_empty());
}

// =============================================================================
// Instance sub-resource commands - missing required args
// =============================================================================

#[test]
fn test_instance_snapshot_create_no_args() {
    triton_cmd()
        .args(["instance", "snapshot", "create"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("required"));
}

#[test]
fn test_instance_snapshot_get_no_args() {
    triton_cmd()
        .args(["instance", "snapshot", "get"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("required"));
}

#[test]
fn test_instance_snapshot_delete_no_args() {
    triton_cmd()
        .args(["instance", "snapshot", "delete"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("required"));
}

#[test]
fn test_instance_disk_list_no_args() {
    triton_cmd()
        .args(["instance", "disk", "list"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("required"));
}

#[test]
fn test_instance_disk_get_no_args() {
    triton_cmd()
        .args(["instance", "disk", "get"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("required"));
}

#[test]
fn test_instance_disk_resize_no_args() {
    triton_cmd()
        .args(["instance", "disk", "resize"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("required"));
}

#[test]
fn test_instance_nic_add_no_args() {
    triton_cmd()
        .args(["instance", "nic", "add"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("required"));
}

#[test]
fn test_instance_nic_get_no_args() {
    triton_cmd()
        .args(["instance", "nic", "get"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("required"));
}

#[test]
fn test_instance_nic_remove_no_args() {
    triton_cmd()
        .args(["instance", "nic", "remove"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("required"));
}

#[test]
fn test_instance_metadata_get_no_args() {
    triton_cmd()
        .args(["instance", "metadata", "get"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("required"));
}

#[test]
fn test_instance_metadata_set_no_args() {
    triton_cmd()
        .args(["instance", "metadata", "set"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("required"));
}

#[test]
fn test_instance_metadata_delete_no_args() {
    triton_cmd()
        .args(["instance", "metadata", "delete"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("required"));
}

// =============================================================================
// Other resource commands - missing required args
// =============================================================================

#[test]
fn test_volume_create_fails_without_profile() {
    // volume create has no required clap args (name is auto-generated),
    // but it fails when no profile/auth is available
    triton_cmd()
        .args(["volume", "create"])
        .env("HOME", "/nonexistent")
        .env("TRITON_CONFIG_DIR", "/nonexistent/.triton")
        .env_remove("TRITON_URL")
        .env_remove("SDC_URL")
        .env_remove("TRITON_ACCOUNT")
        .env_remove("SDC_ACCOUNT")
        .env_remove("TRITON_KEY_ID")
        .env_remove("SDC_KEY_ID")
        .assert()
        .failure()
        .stderr(predicate::str::contains("triton: error:"));
}

// volume delete accepts variadic args - zero is a no-op
#[test]
fn test_volume_delete_zero_args_succeeds() {
    triton_cmd()
        .args(["volume", "delete"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::is_empty());
}

#[test]
fn test_key_add_fails_without_matching_key() {
    // key add defaults to adding the current SSH key; fails if key not found
    triton_cmd()
        .args(["key", "add"])
        .env("HOME", "/nonexistent")
        .assert()
        .failure()
        .stderr(predicate::str::contains("triton: error:"));
}

// key delete accepts variadic args - zero is a no-op
#[test]
fn test_key_delete_zero_args_succeeds() {
    triton_cmd()
        .args(["key", "delete"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::is_empty());
}

#[test]
fn test_fwrule_create_no_args() {
    triton_cmd()
        .args(["fwrule", "create"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("required"));
}

// fwrule delete/enable/disable accept variadic args - zero is a no-op
#[test]
fn test_fwrule_delete_zero_args_succeeds() {
    triton_cmd()
        .args(["fwrule", "delete"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::is_empty());
}

#[test]
fn test_fwrule_enable_zero_args_succeeds() {
    triton_cmd()
        .args(["fwrule", "enable"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::is_empty());
}

#[test]
fn test_fwrule_disable_zero_args_succeeds() {
    triton_cmd()
        .args(["fwrule", "disable"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::is_empty());
}

#[test]
fn test_fwrule_update_no_args() {
    triton_cmd()
        .args(["fwrule", "update"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("required"));
}

#[test]
fn test_network_delete_no_args() {
    triton_cmd()
        .args(["network", "delete"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("required"));
}

// =============================================================================
// Aliases - verify required args still enforced through aliases
// =============================================================================

#[test]
fn test_create_alias_no_args() {
    triton_cmd()
        .args(["create"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("required"));
}

#[test]
fn test_ssh_alias_no_args() {
    triton_cmd()
        .args(["ssh"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("required"));
}

// Variadic aliases also succeed with zero args
#[test]
fn test_delete_alias_zero_args_succeeds() {
    triton_cmd()
        .args(["delete"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::is_empty());
}

#[test]
fn test_start_alias_zero_args_succeeds() {
    triton_cmd()
        .args(["start"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::is_empty());
}

#[test]
fn test_stop_alias_zero_args_succeeds() {
    triton_cmd()
        .args(["stop"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::is_empty());
}

#[test]
fn test_reboot_alias_zero_args_succeeds() {
    triton_cmd()
        .args(["reboot"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::is_empty());
}

// =============================================================================
// Invalid argument values
// =============================================================================

#[test]
fn test_completion_invalid_shell() {
    triton_cmd()
        .args(["completion", "invalid_shell"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid value"));
}

// =============================================================================
// Exit code verification for common error patterns
// =============================================================================

#[test]
fn test_unknown_flag_exits_nonzero() {
    triton_cmd()
        .args(["instance", "list", "--nonexistent-flag"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unexpected argument"));
}

#[test]
fn test_instance_create_missing_image_and_package() {
    triton_cmd()
        .args(["instance", "create", "-n", "test"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("required"));
}
