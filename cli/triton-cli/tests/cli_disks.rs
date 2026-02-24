// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Instance disk CLI tests
//!
//! Tests for `triton instance disk` commands.

// Allow deprecated - cargo_bin is standard for CLI testing
#![allow(deprecated, clippy::expect_used)]

mod common;

use assert_cmd::Command;
use predicates::prelude::*;

fn triton_cmd() -> Command {
    Command::cargo_bin("triton").expect("Failed to find triton binary")
}

// =============================================================================
// Offline tests - no API access required
// =============================================================================

/// Test `triton instance disk -h` shows help
#[test]
fn test_instance_disk_help() {
    triton_cmd()
        .args(["instance", "disk", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton instance disk add -h` shows help
#[test]
fn test_instance_disk_add_help() {
    triton_cmd()
        .args(["instance", "disk", "add", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

// =============================================================================
// Payload tests - verify wire format via --emit-payload
// =============================================================================

/// Test `triton instance disk add INST SIZE` accepts positional size
#[test]
fn test_disk_add_positional_size_payload() {
    let output = triton_cmd()
        .args([
            "--emit-payload",
            "instance",
            "disk",
            "add",
            "00000000-0000-0000-0000-000000000001",
            "10240",
        ])
        .output()
        .expect("Failed to run command");

    assert!(output.status.success(), "command should succeed");
    let stdout = String::from_utf8_lossy(&output.stdout);

    let envelopes: Vec<serde_json::Value> = stdout
        .lines()
        .collect::<Vec<_>>()
        .join("\n")
        .split("\n}\n")
        .filter_map(|chunk| {
            let trimmed = chunk.trim();
            if trimmed.is_empty() {
                return None;
            }
            let json_str = if trimmed.ends_with('}') {
                trimmed.to_string()
            } else {
                format!("{trimmed}\n}}")
            };
            serde_json::from_str(&json_str).ok()
        })
        .collect();

    let post = envelopes
        .iter()
        .find(|e| e["method"] == "POST")
        .expect("should have a POST envelope");

    assert_eq!(post["body"]["size"], 10240);
}
