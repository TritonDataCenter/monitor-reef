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
