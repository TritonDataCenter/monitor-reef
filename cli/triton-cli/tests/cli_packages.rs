// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Package CLI tests
//!
//! Tests for `triton package` commands.
//!
//! Tests are split into:
//! - Offline tests (help, usage) - run without API access
//! - API tests (list, get) - marked with #[ignore], require config.json

// Allow deprecated - cargo_bin is standard for CLI testing
#![allow(deprecated)]

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

/// Test `triton package -h` shows help
#[test]
fn test_package_help_short() {
    triton_cmd()
        .args(["package", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"))
        .stdout(predicate::str::contains("package"));
}

/// Test `triton package --help` shows help
#[test]
fn test_package_help_long() {
    triton_cmd()
        .args(["package", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton help package` shows help
#[test]
fn test_help_package() {
    triton_cmd()
        .args(["help", "package"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"))
        .stdout(predicate::str::contains("package"));
}

/// Test `triton package list -h` shows help
#[test]
fn test_package_list_help() {
    triton_cmd()
        .args(["package", "list", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton package get -h` shows help
#[test]
fn test_package_get_help() {
    triton_cmd()
        .args(["package", "get", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton package help get` shows help
#[test]
fn test_package_help_get() {
    triton_cmd()
        .args(["package", "help", "get"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton package get` without args shows error
#[test]
fn test_package_get_no_args() {
    triton_cmd()
        .args(["package", "get"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("required"));
}

/// Test `triton pkg` alias for package
#[test]
fn test_pkg_alias() {
    triton_cmd()
        .args(["pkg", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton pkg ls` alias
#[test]
fn test_pkg_ls_alias() {
    triton_cmd()
        .args(["pkg", "ls", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton pkgs` shortcut alias
#[test]
fn test_pkgs_shortcut() {
    triton_cmd()
        .args(["pkgs", "--help"])
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

/// Test `triton packages` lists packages (table output)
#[test]
#[ignore = "requires API access - run with make triton-test-api"]
fn test_packages_list_table() {
    if !common::config::has_integration_config() {
        eprintln!("Skipping: no test config found");
        return;
    }

    let output = triton_with_profile()
        .args(["packages"])
        .output()
        .expect("Failed to run command");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "Command should succeed.\nstdout: {}\nstderr: {}",
        stdout,
        stderr
    );

    // Table output should have column headers
    assert!(
        stdout.contains("SHORTID") || stdout.contains("ID") || stdout.contains("NAME"),
        "Should show ID or NAME column. Got:\n{}",
        stdout
    );
}

/// Test `triton package list` lists packages
#[test]
#[ignore = "requires API access - run with make triton-test-api"]
fn test_package_list() {
    if !common::config::has_integration_config() {
        eprintln!("Skipping: no test config found");
        return;
    }

    let output = triton_with_profile()
        .args(["package", "list"])
        .output()
        .expect("Failed to run command");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "Command should succeed");
    assert!(
        stdout.contains("SHORTID") || stdout.contains("ID") || stdout.contains("NAME"),
        "Should show package columns"
    );
}

/// Test `triton packages -j` returns JSON
#[test]
#[ignore = "requires API access - run with make triton-test-api"]
fn test_packages_json() {
    if !common::config::has_integration_config() {
        eprintln!("Skipping: no test config found");
        return;
    }

    let output = triton_with_profile()
        .args(["packages", "-j"])
        .output()
        .expect("Failed to run command");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "Command should succeed.\nstdout: {}\nstderr: {}",
        stdout,
        stderr
    );

    // Parse JSON stream output
    let packages: Vec<Value> = common::json_stream_parse(&stdout);

    assert!(
        !packages.is_empty(),
        "Should have at least one package. Got stdout:\n{}",
        stdout
    );

    // First package should have an id field that looks like a UUID
    let first_id = packages[0]["id"]
        .as_str()
        .expect("Package should have id field");
    assert!(
        first_id.contains('-'),
        "Package id should be a UUID: {}",
        first_id
    );
}

/// Test `triton package get ID` returns package details
#[test]
#[ignore = "requires API access - run with make triton-test-api"]
fn test_package_get_by_id() {
    if !common::config::has_integration_config() {
        eprintln!("Skipping: no test config found");
        return;
    }

    // First, get a list of packages to find one to get
    let list_output = triton_with_profile()
        .args(["packages", "-j"])
        .output()
        .expect("Failed to list packages");

    let stdout = String::from_utf8_lossy(&list_output.stdout);
    let packages: Vec<Value> = common::json_stream_parse(&stdout);

    if packages.is_empty() {
        eprintln!("Skipping: no packages available");
        return;
    }

    let package_id = packages[0]["id"].as_str().expect("Package should have id");

    // Now get that specific package
    let get_output = triton_with_profile()
        .args(["package", "get", package_id])
        .output()
        .expect("Failed to get package");

    let get_stdout = String::from_utf8_lossy(&get_output.stdout);
    let get_stderr = String::from_utf8_lossy(&get_output.stderr);

    assert!(
        get_output.status.success(),
        "package get should succeed.\nstdout: {}\nstderr: {}",
        get_stdout,
        get_stderr
    );

    let package: Value = serde_json::from_str(&get_stdout).expect("Should return valid JSON");
    assert_eq!(
        package["id"].as_str(),
        Some(package_id),
        "Returned package should match requested ID"
    );
}

/// Test `triton package get SHORTID` returns package details
#[test]
#[ignore = "requires API access - run with make triton-test-api"]
fn test_package_get_by_shortid() {
    if !common::config::has_integration_config() {
        eprintln!("Skipping: no test config found");
        return;
    }

    // First, get a list of packages
    let list_output = triton_with_profile()
        .args(["packages", "-j"])
        .output()
        .expect("Failed to list packages");

    let stdout = String::from_utf8_lossy(&list_output.stdout);
    let packages: Vec<Value> = common::json_stream_parse(&stdout);

    if packages.is_empty() {
        eprintln!("Skipping: no packages available");
        return;
    }

    let full_id = packages[0]["id"].as_str().expect("Package should have id");
    let short_id = full_id.split('-').next().expect("ID should have parts");

    // Get by short ID
    let get_output = triton_with_profile()
        .args(["package", "get", short_id])
        .output()
        .expect("Failed to get package");

    let get_stdout = String::from_utf8_lossy(&get_output.stdout);

    assert!(
        get_output.status.success(),
        "package get by shortid should succeed"
    );

    let package: Value = serde_json::from_str(&get_stdout).expect("Should return valid JSON");
    assert_eq!(
        package["id"].as_str(),
        Some(full_id),
        "Returned package should match the full ID"
    );
}

/// Test `triton package get NAME` returns package details
#[test]
#[ignore = "requires API access - run with make triton-test-api"]
fn test_package_get_by_name() {
    if !common::config::has_integration_config() {
        eprintln!("Skipping: no test config found");
        return;
    }

    // First, get a list of packages
    let list_output = triton_with_profile()
        .args(["packages", "-j"])
        .output()
        .expect("Failed to list packages");

    let stdout = String::from_utf8_lossy(&list_output.stdout);
    let packages: Vec<Value> = common::json_stream_parse(&stdout);

    if packages.is_empty() {
        eprintln!("Skipping: no packages available");
        return;
    }

    let package_name = match packages[0]["name"].as_str() {
        Some(name) => name,
        None => {
            eprintln!("Skipping: package has no name");
            return;
        }
    };

    let full_id = packages[0]["id"].as_str().expect("Package should have id");

    // Get by name
    let get_output = triton_with_profile()
        .args(["package", "get", package_name])
        .output()
        .expect("Failed to get package");

    let get_stdout = String::from_utf8_lossy(&get_output.stdout);

    assert!(
        get_output.status.success(),
        "package get by name should succeed"
    );

    let package: Value = serde_json::from_str(&get_stdout).expect("Should return valid JSON");
    assert_eq!(
        package["id"].as_str(),
        Some(full_id),
        "Returned package should match the expected ID"
    );
}
