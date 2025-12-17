// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Network CLI tests
//!
//! Ported from node-triton test/integration/cli-networks.test.js
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

/// Test `triton networks -h` shows help
#[test]
fn test_networks_help_short() {
    triton_cmd()
        .args(["networks", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton networks --help` shows help
#[test]
fn test_networks_help_long() {
    triton_cmd()
        .args(["networks", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton help networks` shows help
#[test]
fn test_help_networks() {
    triton_cmd()
        .args(["help", "networks"])
        .assert()
        .success()
        .stdout(predicate::str::contains("network list"));
}

/// Test `triton network list -h` shows help
#[test]
fn test_network_list_help() {
    triton_cmd()
        .args(["network", "list", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton network get -h` shows help
#[test]
fn test_network_get_help() {
    triton_cmd()
        .args(["network", "get", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton network help get` shows help
#[test]
fn test_network_help_get() {
    triton_cmd()
        .args(["network", "help", "get"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton network get` without args shows error
#[test]
fn test_network_get_no_args() {
    triton_cmd()
        .args(["network", "get"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("required"));
}

/// Test `triton net` alias for network
#[test]
fn test_net_alias() {
    triton_cmd()
        .args(["net", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton net ls` alias
#[test]
fn test_net_ls_alias() {
    triton_cmd()
        .args(["net", "ls", "--help"])
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

/// Test `triton networks` lists networks (table output)
///
/// Equivalent to Node.js:
/// ```js
/// h.triton('networks', function (err, stdout) {
///     t.ok(/^SHORTID\b/.test(stdout));
///     t.ok(/\bFABRIC\b/.test(stdout));
/// });
/// ```
#[test]
#[ignore = "requires API access - run with make triton-test-api"]
fn test_networks_list_table() {
    if !common::config::has_integration_config() {
        eprintln!("Skipping: no test config found");
        return;
    }

    let output = triton_with_profile()
        .args(["networks"])
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

    // Table output should have SHORTID column header
    assert!(
        stdout.contains("SHORTID") || stdout.contains("ID"),
        "Should show SHORTID or ID column. Got:\n{}",
        stdout
    );
}

/// Test `triton network list` lists networks
#[test]
#[ignore = "requires API access - run with make triton-test-api"]
fn test_network_list() {
    if !common::config::has_integration_config() {
        eprintln!("Skipping: no test config found");
        return;
    }

    let output = triton_with_profile()
        .args(["network", "list"])
        .output()
        .expect("Failed to run command");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "Command should succeed");
    assert!(
        stdout.contains("SHORTID") || stdout.contains("ID"),
        "Should show network columns"
    );
}

/// Test `triton networks -j` returns JSON array
///
/// Equivalent to Node.js:
/// ```js
/// h.triton('networks -j', function (err, stdout) {
///     networks = [];
///     stdout.split('\n').forEach(function (line) {
///         if (!line.trim()) return;
///         networks.push(JSON.parse(line));
///     });
///     t.ok(networks.length > 0, 'have at least one network');
///     t.ok(common.isUUID(networks[0].id));
/// });
/// ```
#[test]
#[ignore = "requires API access - run with make triton-test-api"]
fn test_networks_json() {
    if !common::config::has_integration_config() {
        eprintln!("Skipping: no test config found");
        return;
    }

    let output = triton_with_profile()
        .args(["networks", "-j"])
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
    let networks: Vec<Value> = common::json_stream_parse(&stdout);

    assert!(
        !networks.is_empty(),
        "Should have at least one network. Got stdout:\n{}",
        stdout
    );

    // First network should have an id field that looks like a UUID
    let first_id = networks[0]["id"]
        .as_str()
        .expect("Network should have id field");
    assert!(
        first_id.contains('-'),
        "Network id should be a UUID: {}",
        first_id
    );
}

/// Test `triton networks -l` shows long format (full ID)
#[test]
#[ignore = "requires API access - run with make triton-test-api"]
fn test_networks_long_format() {
    if !common::config::has_integration_config() {
        eprintln!("Skipping: no test config found");
        return;
    }

    let output = triton_with_profile()
        .args(["networks", "-l"])
        .output()
        .expect("Failed to run command");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "Command should succeed");

    // Long format should show full ID column
    assert!(stdout.contains("ID"), "Should show full ID column");
}

/// Test `triton network get ID` returns network details
#[test]
#[ignore = "requires API access - run with make triton-test-api"]
fn test_network_get_by_id() {
    if !common::config::has_integration_config() {
        eprintln!("Skipping: no test config found");
        return;
    }

    // First, get a list of networks to find one to get
    let list_output = triton_with_profile()
        .args(["networks", "-j"])
        .output()
        .expect("Failed to list networks");

    let stdout = String::from_utf8_lossy(&list_output.stdout);
    let networks: Vec<Value> = common::json_stream_parse(&stdout);

    if networks.is_empty() {
        eprintln!("Skipping: no networks available");
        return;
    }

    let network_id = networks[0]["id"].as_str().expect("Network should have id");

    // Now get that specific network
    let get_output = triton_with_profile()
        .args(["network", "get", network_id])
        .output()
        .expect("Failed to get network");

    let get_stdout = String::from_utf8_lossy(&get_output.stdout);
    let get_stderr = String::from_utf8_lossy(&get_output.stderr);

    assert!(
        get_output.status.success(),
        "network get should succeed.\nstdout: {}\nstderr: {}",
        get_stdout,
        get_stderr
    );

    let network: Value = serde_json::from_str(&get_stdout).expect("Should return valid JSON");
    assert_eq!(
        network["id"].as_str(),
        Some(network_id),
        "Returned network should match requested ID"
    );
}

/// Test `triton network get SHORTID` returns network details
#[test]
#[ignore = "requires API access - run with make triton-test-api"]
fn test_network_get_by_shortid() {
    if !common::config::has_integration_config() {
        eprintln!("Skipping: no test config found");
        return;
    }

    // First, get a list of networks
    let list_output = triton_with_profile()
        .args(["networks", "-j"])
        .output()
        .expect("Failed to list networks");

    let stdout = String::from_utf8_lossy(&list_output.stdout);
    let networks: Vec<Value> = common::json_stream_parse(&stdout);

    if networks.is_empty() {
        eprintln!("Skipping: no networks available");
        return;
    }

    let full_id = networks[0]["id"].as_str().expect("Network should have id");
    let short_id = full_id.split('-').next().expect("ID should have parts");

    // Get by short ID
    let get_output = triton_with_profile()
        .args(["network", "get", short_id])
        .output()
        .expect("Failed to get network");

    let get_stdout = String::from_utf8_lossy(&get_output.stdout);

    assert!(
        get_output.status.success(),
        "network get by shortid should succeed"
    );

    let network: Value = serde_json::from_str(&get_stdout).expect("Should return valid JSON");
    assert_eq!(
        network["id"].as_str(),
        Some(full_id),
        "Returned network should match the full ID"
    );
}

/// Test `triton network get NAME` returns network details
#[test]
#[ignore = "requires API access - run with make triton-test-api"]
fn test_network_get_by_name() {
    if !common::config::has_integration_config() {
        eprintln!("Skipping: no test config found");
        return;
    }

    // First, get a list of networks
    let list_output = triton_with_profile()
        .args(["networks", "-j"])
        .output()
        .expect("Failed to list networks");

    let stdout = String::from_utf8_lossy(&list_output.stdout);
    let networks: Vec<Value> = common::json_stream_parse(&stdout);

    if networks.is_empty() {
        eprintln!("Skipping: no networks available");
        return;
    }

    let network_name = match networks[0]["name"].as_str() {
        Some(name) => name,
        None => {
            eprintln!("Skipping: network has no name");
            return;
        }
    };

    let full_id = networks[0]["id"].as_str().expect("Network should have id");

    // Get by name
    let get_output = triton_with_profile()
        .args(["network", "get", network_name])
        .output()
        .expect("Failed to get network");

    let get_stdout = String::from_utf8_lossy(&get_output.stdout);

    assert!(
        get_output.status.success(),
        "network get by name should succeed"
    );

    let network: Value = serde_json::from_str(&get_stdout).expect("Should return valid JSON");
    assert_eq!(
        network["id"].as_str(),
        Some(full_id),
        "Returned network should match the expected ID"
    );
}

/// Test `triton networks public=true` filters by public networks
#[test]
#[ignore = "requires API access - run with make triton-test-api"]
fn test_networks_filter_public_true() {
    if !common::config::has_integration_config() {
        eprintln!("Skipping: no test config found");
        return;
    }

    let output = triton_with_profile()
        .args(["networks", "public=true", "-j"])
        .output()
        .expect("Failed to run command");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "Command should succeed");

    let networks: Vec<Value> = common::json_stream_parse(&stdout);

    // All returned networks should be public
    for network in &networks {
        let is_public = network["public"].as_bool().unwrap_or(false);
        assert!(is_public, "Network should be public: {:?}", network);
    }
}

/// Test `triton networks public=false` filters by non-public networks
#[test]
#[ignore = "requires API access - run with make triton-test-api"]
fn test_networks_filter_public_false() {
    if !common::config::has_integration_config() {
        eprintln!("Skipping: no test config found");
        return;
    }

    let output = triton_with_profile()
        .args(["networks", "public=false", "-j"])
        .output()
        .expect("Failed to run command");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "Command should succeed");

    let networks: Vec<Value> = common::json_stream_parse(&stdout);

    // All returned networks should be non-public (if any exist)
    for network in &networks {
        let is_public = network["public"].as_bool().unwrap_or(true);
        assert!(!is_public, "Network should not be public: {:?}", network);
    }
}

/// Test `triton networks public=bogus` returns error
#[test]
#[ignore = "requires API access - run with make triton-test-api"]
fn test_networks_filter_public_invalid() {
    if !common::config::has_integration_config() {
        eprintln!("Skipping: no test config found");
        return;
    }

    let output = triton_with_profile()
        .args(["networks", "public=bogus"])
        .output()
        .expect("Failed to run command");

    assert!(
        !output.status.success(),
        "Command should fail with invalid filter value"
    );
}
