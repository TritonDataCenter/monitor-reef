// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Basic CLI tests - help, version, etc.
//!
//! Ported from node-triton test/integration/cli-basics.test.js

// Allow deprecated - cargo_bin is standard for CLI testing
#![allow(deprecated)]

mod common;

use assert_cmd::Command;
use predicates::prelude::*;

fn triton_cmd() -> Command {
    Command::cargo_bin("triton").expect("Failed to find triton binary")
}

#[test]
fn test_triton_version() {
    triton_cmd()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains("triton"));
}

#[test]
fn test_triton_help_short() {
    triton_cmd()
        .arg("-h")
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"))
        .stdout(predicate::str::contains("instance"));
}

#[test]
fn test_triton_help_long() {
    triton_cmd()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"))
        .stdout(predicate::str::contains("instance"));
}

#[test]
fn test_triton_help_subcommand() {
    triton_cmd()
        .arg("help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"))
        .stdout(predicate::str::contains("instance"));
}

#[test]
fn test_triton_instance_help() {
    triton_cmd()
        .args(["instance", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"))
        .stdout(predicate::str::contains("list"));
}

#[test]
fn test_triton_instance_list_help() {
    triton_cmd()
        .args(["instance", "list", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

#[test]
fn test_triton_volume_help() {
    triton_cmd()
        .args(["volume", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"))
        .stdout(predicate::str::contains("list"));
}

#[test]
fn test_triton_network_help() {
    triton_cmd()
        .args(["network", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

#[test]
fn test_triton_package_help() {
    triton_cmd()
        .args(["package", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

#[test]
fn test_triton_image_help() {
    triton_cmd()
        .args(["image", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

#[test]
fn test_triton_profile_help() {
    triton_cmd()
        .args(["profile", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

#[test]
fn test_triton_invalid_subcommand() {
    triton_cmd()
        .arg("nonexistent-subcommand")
        .assert()
        .failure()
        .stderr(predicate::str::contains("error:"));
}

#[test]
fn test_triton_completion_bash() {
    triton_cmd()
        .args(["completion", "bash"])
        .assert()
        .success()
        .stdout(predicate::str::contains("_triton"));
}

#[test]
fn test_triton_completion_zsh() {
    triton_cmd()
        .args(["completion", "zsh"])
        .assert()
        .success()
        .stdout(predicate::str::contains("#compdef triton"));
}

#[test]
fn test_triton_completion_fish() {
    triton_cmd()
        .args(["completion", "fish"])
        .assert()
        .success()
        .stdout(predicate::str::contains("complete"));
}

#[test]
fn test_triton_env_help() {
    triton_cmd()
        .args(["env", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

#[test]
fn test_triton_key_help() {
    triton_cmd()
        .args(["key", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

#[test]
fn test_triton_fwrule_help() {
    triton_cmd()
        .args(["fwrule", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

#[test]
fn test_triton_account_help() {
    triton_cmd()
        .args(["account", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

// Test aliases
#[test]
fn test_triton_inst_alias() {
    triton_cmd()
        .args(["inst", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

#[test]
fn test_triton_ls_alias() {
    // ls is an alias for instance list
    triton_cmd()
        .args(["ls", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

#[test]
fn test_triton_pkg_alias() {
    triton_cmd()
        .args(["pkg", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

#[test]
fn test_triton_img_alias() {
    triton_cmd()
        .args(["img", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

#[test]
fn test_triton_net_alias() {
    triton_cmd()
        .args(["net", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

#[test]
fn test_triton_vol_alias() {
    triton_cmd()
        .args(["vol", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}
