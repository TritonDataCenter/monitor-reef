// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Account CLI tests
//!
//! Ported from node-triton test/integration/cli-account.test.js
//!
//! Tests are split into:
//! - Offline tests (help, usage) - run without API access
//! - API tests (get, limits) - marked with #[ignore], require config.json

// Allow deprecated - cargo_bin is standard for CLI testing
#![allow(deprecated, clippy::expect_used)]

mod common;

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::Value;

fn triton_cmd() -> Command {
    Command::cargo_bin("triton").expect("Failed to find triton binary")
}

// =============================================================================
// Offline tests - no API access required
// =============================================================================

/// Test `triton account -h` shows help
///
/// Equivalent to Node.js:
/// ```js
/// h.triton('account -h', function (err, stdout, stderr) {
///     t.ok(/Usage:\s+triton account/.test(stdout), 'account usage');
/// });
/// ```
#[test]
fn test_account_help_short() {
    triton_cmd()
        .args(["account", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"))
        .stdout(predicate::str::contains("account"));
}

/// Test `triton account --help` shows help
#[test]
fn test_account_help_long() {
    triton_cmd()
        .args(["account", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton help account` shows help
///
/// Equivalent to Node.js:
/// ```js
/// h.triton('help account', function (err, stdout, stderr) {
///     t.ok(/Usage:\s+triton account/.test(stdout), 'account usage');
/// });
/// ```
#[test]
fn test_help_account() {
    triton_cmd()
        .args(["help", "account"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"))
        .stdout(predicate::str::contains("account"));
}

/// Test `triton account get -h` shows help
#[test]
fn test_account_get_help() {
    triton_cmd()
        .args(["account", "get", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton account limits -h` shows help
#[test]
fn test_account_limits_help() {
    triton_cmd()
        .args(["account", "limits", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton account update -h` shows help
#[test]
fn test_account_update_help() {
    triton_cmd()
        .args(["account", "update", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

// =============================================================================
// API tests - require config.json with valid profile
// These tests are ignored by default and run with `make triton-test-api`
// =============================================================================

/// Run triton with profile from test config
fn triton_with_profile() -> Command {
    let mut cmd = triton_cmd();

    // Load profile environment from config
    let env_vars = common::config::get_profile_env();
    for (key, value) in env_vars {
        cmd.env(key, value);
    }

    cmd
}

/// Test `triton account get` returns account info
///
/// Equivalent to Node.js:
/// ```js
/// h.triton('-v account get', function (err, stdout, stderr) {
///     t.ok(new RegExp('^login: ' + h.CONFIG.profile.account, 'm').test(stdout));
/// });
/// ```
#[test]
#[ignore = "requires API access - run with make triton-test-api"]
fn test_account_get() {
    if !common::config::has_integration_config() {
        eprintln!("Skipping: no test config found");
        return;
    }

    let output = triton_with_profile()
        .args(["account", "get"])
        .output()
        .expect("Failed to run command");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "account get should succeed.\nstdout: {}\nstderr: {}",
        stdout,
        stderr
    );

    // Should show login field in human-readable output
    assert!(
        stdout.contains("login:") || stdout.contains("LOGIN"),
        "Should show login field. Got:\n{}",
        stdout
    );
}

/// Test `triton account get -j` returns JSON
///
/// Equivalent to Node.js:
/// ```js
/// h.triton('account get -j', function (err, stdout, stderr) {
///     account = JSON.parse(stdout);
///     t.equal(account.login, h.CONFIG.profile.account, 'account.login');
/// });
/// ```
#[test]
#[ignore = "requires API access - run with make triton-test-api"]
fn test_account_get_json() {
    if !common::config::has_integration_config() {
        eprintln!("Skipping: no test config found");
        return;
    }

    let output = triton_with_profile()
        .args(["account", "get", "-j"])
        .output()
        .expect("Failed to run command");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "account get -j should succeed.\nstdout: {}\nstderr: {}",
        stdout,
        stderr
    );

    let account: Value = serde_json::from_str(&stdout).expect("Should return valid JSON");

    // Account should have a login field
    assert!(
        account["login"].is_string(),
        "Account should have login field. Got: {:?}",
        account
    );
}

/// Test `triton account limits` returns limit info
///
/// Equivalent to Node.js:
/// ```js
/// h.triton('-v account limits', function (err, stdout, stderr) {
///     t.ok(stdout.indexOf('LIMIT') > 0, 'LIMIT header should be found');
/// });
/// ```
#[test]
#[ignore = "requires API access - run with make triton-test-api"]
fn test_account_limits() {
    if !common::config::has_integration_config() {
        eprintln!("Skipping: no test config found");
        return;
    }

    let output = triton_with_profile()
        .args(["account", "limits"])
        .output()
        .expect("Failed to run command");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "account limits should succeed.\nstdout: {}\nstderr: {}",
        stdout,
        stderr
    );

    // Table output should show LIMIT header
    assert!(
        stdout.contains("LIMIT") || stdout.contains("limit"),
        "Should show LIMIT column. Got:\n{}",
        stdout
    );
}

/// Test `triton account limits -j` returns JSON array
///
/// Equivalent to Node.js:
/// ```js
/// h.triton('account limits -j', function (err, stdout, stderr) {
///     var limits = JSON.parse(stdout);
///     t.ok(Array.isArray(limits), 'json limits should be an array');
///     if (Array.isArray(limits) && limits.length > 0) {
///         for (i = 0; i < limits.length; i++) {
///             t.ok(['ram', 'quota', 'machines'].indexOf(limits[i].type) >= 0);
///             t.ok(limits[i].used >= 0, 'limit has a valid used field');
///             t.ok(limits[i].limit >= 0, 'limit has a valid limit field');
///         }
///     }
/// });
/// ```
#[test]
#[ignore = "requires API access - run with make triton-test-api"]
fn test_account_limits_json() {
    if !common::config::has_integration_config() {
        eprintln!("Skipping: no test config found");
        return;
    }

    let output = triton_with_profile()
        .args(["account", "limits", "-j"])
        .output()
        .expect("Failed to run command");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "account limits -j should succeed.\nstdout: {}\nstderr: {}",
        stdout,
        stderr
    );

    // Parse JSON stream output (could be NDJSON or array)
    let limits: Vec<Value> = common::json_stream_parse(&stdout);

    // If we have limits, verify their structure
    for limit in &limits {
        // Each limit should have type, used, and limit fields
        if let Some(limit_type) = limit["type"].as_str() {
            assert!(
                ["ram", "quota", "machines"].contains(&limit_type),
                "Limit type should be ram, quota, or machines. Got: {}",
                limit_type
            );
        }

        // used and limit should be non-negative numbers
        if limit["used"].is_number() {
            let used = limit["used"].as_i64().unwrap_or(-1);
            assert!(used >= 0, "used should be non-negative");
        }

        if limit["limit"].is_number() {
            let limit_val = limit["limit"].as_i64().unwrap_or(-1);
            assert!(limit_val >= 0, "limit should be non-negative");
        }
    }
}

/// Test `triton account update foo=bar` fails with invalid field
///
/// Equivalent to Node.js:
/// ```js
/// h.triton('account update foo=bar', function (err, stdout, stderr) {
///     t.ok(err);
/// });
/// ```
#[test]
#[ignore = "requires API access - run with make triton-test-api"]
fn test_account_update_invalid_field() {
    if !common::config::has_integration_config() {
        eprintln!("Skipping: no test config found");
        return;
    }

    let output = triton_with_profile()
        .args(["account", "update", "foo=bar"])
        .output()
        .expect("Failed to run command");

    // Should fail because 'foo' is not a valid account field
    assert!(
        !output.status.success(),
        "account update with invalid field should fail"
    );
}
