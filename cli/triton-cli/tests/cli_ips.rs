// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Network IP CLI tests
//!
//! Tests for `triton network ip` commands.
//!
//! Tests are split into:
//! - Offline tests (help, usage) - run without API access
//! - API tests (list, get) - marked with #[ignore], require config.json
//!
//! Ported from node-triton test/integration/cli-ips.test.js

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

/// Test `triton network ip list -h` shows help
#[test]
fn test_network_ip_list_help() {
    triton_cmd()
        .args(["network", "ip", "list", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::is_match(r"Usage:\s+triton network ip list").unwrap());
}

/// Test `triton network ip ls` alias works
#[test]
fn test_network_ip_ls_alias() {
    triton_cmd()
        .args(["network", "ip", "ls", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton network ip list` without args shows error
#[test]
fn test_network_ip_list_no_args() {
    triton_cmd()
        .args(["network", "ip", "list"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("required"));
}

/// Test `triton network ip get -h` shows help
#[test]
fn test_network_ip_get_help() {
    triton_cmd()
        .args(["network", "ip", "get", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::is_match(r"Usage:\s+triton network ip").unwrap());
}

/// Test `triton network ip help get` shows help
#[test]
fn test_network_ip_help_get() {
    triton_cmd()
        .args(["network", "ip", "help", "get"])
        .assert()
        .success()
        .stdout(predicate::str::is_match(r"Usage:\s+triton network ip get").unwrap());
}

/// Test `triton network ip get` without args shows error
#[test]
fn test_network_ip_get_no_args() {
    triton_cmd()
        .args(["network", "ip", "get"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("required"));
}

/// Test `triton network ip update -h` shows help
#[test]
fn test_network_ip_update_help() {
    triton_cmd()
        .args(["network", "ip", "update", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton net ip list -h` alias works
#[test]
fn test_net_ip_list_help() {
    triton_cmd()
        .args(["net", "ip", "list", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

// =============================================================================
// API tests - require config.json
// These tests are ignored by default and run with `make triton-test-api`
// =============================================================================

/// Network info from network list
#[derive(Debug, serde::Deserialize)]
struct NetworkInfo {
    id: String,
    name: String,
    #[allow(dead_code)]
    fabric: Option<bool>,
}

/// IP info from network ip list
#[derive(Debug, serde::Deserialize)]
struct IpInfo {
    ip: String,
    #[allow(dead_code)]
    reserved: bool,
    #[allow(dead_code)]
    managed: Option<bool>,
}

/// Get a fabric network for testing
fn get_fabric_network() -> Option<NetworkInfo> {
    use common::{json_stream_parse, run_triton_with_profile};

    let (stdout, _, success) = run_triton_with_profile(["networks", "-j"]);
    if !success {
        return None;
    }

    let networks: Vec<NetworkInfo> = json_stream_parse(&stdout);
    // Find a fabric network
    networks.into_iter().find(|n| n.fabric == Some(true))
}

/// Test `triton network ip list ID` returns table output
#[test]
#[ignore]
fn test_network_ip_list_table() {
    use common::run_triton_with_profile;

    let network = match get_fabric_network() {
        Some(n) => n,
        None => {
            eprintln!("Skipping test: no fabric networks available");
            return;
        }
    };

    let (stdout, _, success) = run_triton_with_profile(["network", "ip", "list", &network.id]);
    assert!(success, "network ip list should succeed");

    // Check for expected columns (node-triton uses IP and MANAGED)
    assert!(stdout.contains("IP"), "output should contain IP header");
    assert!(
        stdout.contains("MANAGED") || stdout.contains("RESERVED"),
        "output should contain MANAGED or RESERVED header"
    );
}

/// Test `triton network ip list SHORTID` works
#[test]
#[ignore]
fn test_network_ip_list_shortid() {
    use common::{run_triton_with_profile, short_id};

    let network = match get_fabric_network() {
        Some(n) => n,
        None => {
            eprintln!("Skipping test: no fabric networks available");
            return;
        }
    };

    let shortid = short_id(&network.id);
    let (stdout, _, success) = run_triton_with_profile(["network", "ip", "list", &shortid]);
    assert!(success, "network ip list with shortid should succeed");
    assert!(stdout.contains("IP"), "output should contain IP header");
}

/// Test `triton network ip list -j ID` returns JSON
#[test]
#[ignore]
fn test_network_ip_list_json() {
    use common::{json_stream_parse, run_triton_with_profile};

    let network = match get_fabric_network() {
        Some(n) => n,
        None => {
            eprintln!("Skipping test: no fabric networks available");
            return;
        }
    };

    let (stdout, _, success) =
        run_triton_with_profile(["network", "ip", "list", "-j", &network.id]);
    assert!(success, "network ip list -j should succeed");

    let ips: Vec<IpInfo> = json_stream_parse(&stdout);
    // May have no IPs, that's OK - but if we have any, check they have ip field
    if !ips.is_empty() {
        assert!(!ips[0].ip.is_empty(), "IP should have ip field");
    }
}

/// Test `triton network ip get ID IP` returns IP details
#[test]
#[ignore]
fn test_network_ip_get() {
    use common::{json_stream_parse, run_triton_with_profile};

    let network = match get_fabric_network() {
        Some(n) => n,
        None => {
            eprintln!("Skipping test: no fabric networks available");
            return;
        }
    };

    // First get the IP list
    let (stdout, _, success) =
        run_triton_with_profile(["network", "ip", "list", "-j", &network.id]);
    if !success {
        eprintln!("Skipping test: could not list IPs");
        return;
    }

    let ips: Vec<IpInfo> = json_stream_parse(&stdout);
    if ips.is_empty() {
        eprintln!("Skipping test: no IPs in network");
        return;
    }

    let ip = &ips[0];

    // Get by full ID
    let (stdout, _, success) =
        run_triton_with_profile(["network", "ip", "get", &network.id, &ip.ip]);
    assert!(success, "network ip get should succeed");

    let got_ip: IpInfo = serde_json::from_str(&stdout).expect("should parse IP JSON");
    assert_eq!(got_ip.ip, ip.ip, "IP should match");
}

/// Test `triton network ip get SHORTID IP` works
#[test]
#[ignore]
fn test_network_ip_get_shortid() {
    use common::{json_stream_parse, run_triton_with_profile, short_id};

    let network = match get_fabric_network() {
        Some(n) => n,
        None => {
            eprintln!("Skipping test: no fabric networks available");
            return;
        }
    };

    // First get the IP list
    let (stdout, _, success) =
        run_triton_with_profile(["network", "ip", "list", "-j", &network.id]);
    if !success {
        eprintln!("Skipping test: could not list IPs");
        return;
    }

    let ips: Vec<IpInfo> = json_stream_parse(&stdout);
    if ips.is_empty() {
        eprintln!("Skipping test: no IPs in network");
        return;
    }

    let ip = &ips[0];
    let shortid = short_id(&network.id);

    // Get by short ID
    let (stdout, _, success) = run_triton_with_profile(["network", "ip", "get", &shortid, &ip.ip]);
    assert!(success, "network ip get with shortid should succeed");

    let got_ip: IpInfo = serde_json::from_str(&stdout).expect("should parse IP JSON");
    assert_eq!(got_ip.ip, ip.ip, "IP should match");
}

/// Test `triton network ip get NAME IP` works
#[test]
#[ignore]
fn test_network_ip_get_name() {
    use common::{json_stream_parse, run_triton_with_profile};

    let network = match get_fabric_network() {
        Some(n) => n,
        None => {
            eprintln!("Skipping test: no fabric networks available");
            return;
        }
    };

    // First get the IP list
    let (stdout, _, success) =
        run_triton_with_profile(["network", "ip", "list", "-j", &network.id]);
    if !success {
        eprintln!("Skipping test: could not list IPs");
        return;
    }

    let ips: Vec<IpInfo> = json_stream_parse(&stdout);
    if ips.is_empty() {
        eprintln!("Skipping test: no IPs in network");
        return;
    }

    let ip = &ips[0];

    // Get by network name
    let (stdout, _, success) =
        run_triton_with_profile(["network", "ip", "get", &network.name, &ip.ip]);
    assert!(success, "network ip get with name should succeed");

    let got_ip: IpInfo = serde_json::from_str(&stdout).expect("should parse IP JSON");
    assert_eq!(got_ip.ip, ip.ip, "IP should match");
}
