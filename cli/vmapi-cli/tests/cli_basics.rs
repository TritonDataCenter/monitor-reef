// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Basic CLI tests for vmapi-cli - help, version, subcommand validation.
//!
//! These tests run offline (no VMAPI server needed) and verify that the
//! CLI argument parsing, help output, and error handling work correctly.

// Allow deprecated - cargo_bin is standard for CLI testing
#![allow(deprecated, clippy::expect_used)]

use assert_cmd::Command;
use predicates::prelude::*;

fn vmapi_cmd() -> Command {
    Command::cargo_bin("vmapi").expect("Failed to find vmapi binary")
}

// ============================================================================
// Version and Help
// ============================================================================

#[test]
fn test_vmapi_version() {
    vmapi_cmd()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::is_match(r"vmapi \d+\.\d+\.\d+").unwrap());
}

#[test]
fn test_vmapi_help_short() {
    vmapi_cmd()
        .arg("-h")
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"))
        .stdout(predicate::str::contains("CLI for Triton VMAPI"));
}

#[test]
fn test_vmapi_help_long() {
    vmapi_cmd()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"))
        .stdout(predicate::str::contains("list-vms"));
}

#[test]
fn test_vmapi_help_subcommand() {
    vmapi_cmd()
        .arg("help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"))
        .stdout(predicate::str::contains("list-vms"));
}

// ============================================================================
// Subcommand Help
// ============================================================================

#[test]
fn test_vmapi_list_vms_help() {
    vmapi_cmd()
        .args(["list-vms", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"))
        .stdout(predicate::str::contains("--owner-uuid"));
}

#[test]
fn test_vmapi_get_vm_help() {
    vmapi_cmd()
        .args(["get-vm", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

#[test]
fn test_vmapi_create_vm_help() {
    vmapi_cmd()
        .args(["create-vm", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--owner-uuid"));
}

#[test]
fn test_vmapi_update_vm_help() {
    vmapi_cmd()
        .args(["update-vm", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

#[test]
fn test_vmapi_delete_vm_help() {
    vmapi_cmd()
        .args(["delete-vm", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

#[test]
fn test_vmapi_start_vm_help() {
    vmapi_cmd()
        .args(["start-vm", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

#[test]
fn test_vmapi_stop_vm_help() {
    vmapi_cmd()
        .args(["stop-vm", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

#[test]
fn test_vmapi_reboot_vm_help() {
    vmapi_cmd()
        .args(["reboot-vm", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

#[test]
fn test_vmapi_ping_help() {
    vmapi_cmd()
        .args(["ping", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

#[test]
fn test_vmapi_add_nics_help() {
    vmapi_cmd()
        .args(["add-nics", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

#[test]
fn test_vmapi_list_jobs_help() {
    vmapi_cmd()
        .args(["list-jobs", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

#[test]
fn test_vmapi_add_role_tags_help() {
    vmapi_cmd()
        .args(["add-role-tags", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

// ============================================================================
// Error Cases
// ============================================================================

#[test]
fn test_vmapi_invalid_subcommand() {
    vmapi_cmd()
        .arg("nonexistent-command")
        .assert()
        .failure()
        .stderr(predicate::str::contains("error:"));
}

#[test]
fn test_vmapi_get_vm_missing_uuid() {
    vmapi_cmd()
        .arg("get-vm")
        .assert()
        .failure()
        .stderr(predicate::str::contains("error:"));
}

#[test]
fn test_vmapi_get_vm_invalid_uuid() {
    vmapi_cmd()
        .args(["get-vm", "not-a-uuid"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("error:"));
}

#[test]
fn test_vmapi_create_vm_missing_owner() {
    vmapi_cmd()
        .arg("create-vm")
        .assert()
        .failure()
        .stderr(predicate::str::contains("--owner-uuid"));
}

#[test]
fn test_vmapi_delete_vm_missing_uuid() {
    vmapi_cmd()
        .arg("delete-vm")
        .assert()
        .failure()
        .stderr(predicate::str::contains("error:"));
}

// ============================================================================
// Value Enum Validation
// ============================================================================

#[test]
fn test_vmapi_list_vms_invalid_state() {
    vmapi_cmd()
        .args(["list-vms", "--state", "invalid-state"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("error:"));
}

#[test]
fn test_vmapi_list_vms_invalid_brand() {
    vmapi_cmd()
        .args(["list-vms", "--brand", "invalid-brand"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("error:"));
}

// ============================================================================
// Base URL Flag
// ============================================================================

#[test]
fn test_vmapi_base_url_flag() {
    // --base-url should be accepted without error (help still works)
    vmapi_cmd()
        .args(["--base-url", "http://vmapi.example.com", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}
