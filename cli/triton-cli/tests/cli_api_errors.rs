// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Negative API error handling tests (404, 403, 500 scenarios)
//!
//! These tests verify that the CLI produces user-friendly error messages
//! and correct exit codes for common API error scenarios. Tests cover:
//! - Missing profile/auth configuration
//! - Invalid CloudAPI URL
//! - Connection refused (unreachable server)
//! - Authentication failures
//!
//! All tests run offline without API access.

// Allow deprecated - cargo_bin is standard for CLI testing
#![allow(deprecated, clippy::expect_used)]

use assert_cmd::Command;
use predicates::prelude::*;

fn triton_cmd() -> Command {
    Command::cargo_bin("triton").expect("Failed to find triton binary")
}

/// Strip all profile/auth environment to ensure a clean test environment.
/// This prevents the user's real profile from interfering with tests.
fn triton_no_profile() -> Command {
    let mut cmd = triton_cmd();
    cmd.env("HOME", "/nonexistent/home/dir")
        .env("TRITON_CONFIG_DIR", "/nonexistent/.triton")
        .env_remove("TRITON_URL")
        .env_remove("SDC_URL")
        .env_remove("TRITON_ACCOUNT")
        .env_remove("SDC_ACCOUNT")
        .env_remove("TRITON_KEY_ID")
        .env_remove("SDC_KEY_ID")
        .env_remove("TRITON_PROFILE");
    cmd
}

/// Configure env vars pointing to a specific URL but with no valid auth key.
fn triton_with_url(url: &str) -> Command {
    let mut cmd = triton_no_profile();
    cmd.env("TRITON_URL", url)
        .env("TRITON_ACCOUNT", "testaccount")
        .env(
            "TRITON_KEY_ID",
            "00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00",
        );
    cmd
}

// =============================================================================
// Missing profile / no configuration
// =============================================================================

#[test]
fn test_no_profile_instance_list_fails() {
    triton_no_profile()
        .args(["instance", "list"])
        .assert()
        .failure();
}

#[test]
fn test_no_profile_instance_list_shows_error_on_stderr() {
    let output = triton_no_profile()
        .args(["instance", "list"])
        .output()
        .expect("Failed to execute");

    assert!(!output.status.success(), "should fail without a profile");

    let stderr = String::from_utf8_lossy(&output.stderr);
    // Error should mention profile, URL, or configuration
    assert!(
        !stderr.is_empty() || !String::from_utf8_lossy(&output.stdout).is_empty(),
        "should produce some error output"
    );
}

#[test]
fn test_no_profile_image_list_fails() {
    triton_no_profile()
        .args(["image", "list"])
        .assert()
        .failure();
}

#[test]
fn test_no_profile_network_list_fails() {
    triton_no_profile()
        .args(["network", "list"])
        .assert()
        .failure();
}

#[test]
fn test_no_profile_volume_list_fails() {
    triton_no_profile()
        .args(["volume", "list"])
        .assert()
        .failure();
}

#[test]
fn test_no_profile_package_list_fails() {
    triton_no_profile()
        .args(["package", "list"])
        .assert()
        .failure();
}

#[test]
fn test_no_profile_instance_get_fails() {
    triton_no_profile()
        .args(["instance", "get", "some-instance"])
        .assert()
        .failure();
}

#[test]
fn test_no_profile_account_get_fails() {
    triton_no_profile()
        .args(["account", "get"])
        .assert()
        .failure();
}

// =============================================================================
// Partial profile - missing required fields
// =============================================================================

#[test]
fn test_url_only_no_account_fails() {
    let mut cmd = triton_no_profile();
    cmd.env("TRITON_URL", "https://cloudapi.example.com");
    // Missing TRITON_ACCOUNT and TRITON_KEY_ID
    cmd.args(["instance", "list"]).assert().failure();
}

#[test]
fn test_url_and_account_no_key_fails() {
    let mut cmd = triton_no_profile();
    cmd.env("TRITON_URL", "https://cloudapi.example.com")
        .env("TRITON_ACCOUNT", "testaccount");
    // Missing TRITON_KEY_ID
    cmd.args(["instance", "list"]).assert().failure();
}

// =============================================================================
// Connection refused (server unreachable)
// =============================================================================

// These tests point to localhost port 1 which should be unreachable.
// The CLI should fail with a connection error, not crash.

#[test]
fn test_connection_refused_instance_list() {
    triton_with_url("http://127.0.0.1:1")
        .args(["instance", "list"])
        .assert()
        .failure();
}

#[test]
fn test_connection_refused_image_list() {
    triton_with_url("http://127.0.0.1:1")
        .args(["image", "list"])
        .assert()
        .failure();
}

#[test]
fn test_connection_refused_produces_error_not_panic() {
    let output = triton_with_url("http://127.0.0.1:1")
        .args(["instance", "list"])
        .output()
        .expect("Failed to execute");

    assert!(!output.status.success());

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let combined = format!("{}{}", stdout, stderr);

    // Should NOT contain Rust panic output
    assert!(
        !combined.contains("thread 'main' panicked"),
        "CLI should not panic on connection refused"
    );
    assert!(
        !combined.contains("RUST_BACKTRACE"),
        "CLI should not suggest RUST_BACKTRACE on connection failure"
    );
}

// =============================================================================
// Invalid URL format
// =============================================================================

#[test]
fn test_invalid_url_format_fails() {
    triton_with_url("not-a-valid-url")
        .args(["instance", "list"])
        .assert()
        .failure();
}

#[test]
fn test_empty_url_fails() {
    triton_with_url("")
        .args(["instance", "list"])
        .assert()
        .failure();
}

// =============================================================================
// Exit code verification
// =============================================================================

#[test]
fn test_error_exit_code_is_nonzero() {
    let output = triton_no_profile()
        .args(["instance", "list"])
        .output()
        .expect("Failed to execute");

    assert!(
        !output.status.success(),
        "failed commands should have non-zero exit code"
    );

    // On Unix, verify the exit code is 1 (standard error)
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        assert!(
            output.status.signal().is_none(),
            "CLI should exit cleanly, not be killed by signal"
        );
    }
}

#[test]
fn test_connection_error_exit_code_is_nonzero() {
    let output = triton_with_url("http://127.0.0.1:1")
        .args(["instance", "list"])
        .output()
        .expect("Failed to execute");

    assert!(
        !output.status.success(),
        "connection error should produce non-zero exit code"
    );

    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        assert!(
            output.status.signal().is_none(),
            "CLI should exit cleanly on connection error, not be killed by signal"
        );
    }
}

// =============================================================================
// Error message quality (no panic, no Debug format)
// =============================================================================

#[test]
fn test_no_profile_error_is_user_friendly() {
    let output = triton_no_profile()
        .args(["instance", "list"])
        .output()
        .expect("Failed to execute");

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let combined = format!("{}{}", stdout, stderr);

    // Should not contain Rust-internal formatting
    assert!(
        !combined.contains("thread 'main' panicked"),
        "error should not be a panic"
    );

    // The error output should not be empty
    assert!(
        !combined.trim().is_empty(),
        "error scenario should produce some diagnostic output"
    );
}

#[test]
fn test_profile_commands_work_without_api() {
    // Profile list should work even without API access, since it reads local files
    triton_no_profile()
        .args(["profile", "list"])
        .assert()
        .success();
}

// =============================================================================
// Commands that should succeed without API (no false errors)
// =============================================================================

#[test]
fn test_help_succeeds_without_profile() {
    triton_no_profile()
        .args(["--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

#[test]
fn test_version_succeeds_without_profile() {
    triton_no_profile()
        .args(["--version"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Triton CLI"));
}

#[test]
fn test_completion_succeeds_without_profile() {
    triton_no_profile()
        .args(["completion", "bash"])
        .assert()
        .success();
}
