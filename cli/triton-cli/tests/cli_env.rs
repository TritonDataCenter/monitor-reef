// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Tests for `triton env` command output format

#![allow(deprecated, clippy::expect_used)]

mod common;

use assert_cmd::Command;

fn triton_cmd() -> Command {
    Command::cargo_bin("triton").expect("Failed to find triton binary")
}

/// Test that `triton env` uses double quotes for bash export values,
/// matching Node.js triton behavior.
#[test]
fn test_env_bash_uses_double_quotes() {
    let output = triton_cmd()
        .args(["env"])
        .env("TRITON_URL", "https://cloudapi.test.example.com")
        .env("TRITON_ACCOUNT", "test-account")
        .env(
            "TRITON_KEY_ID",
            "00:11:22:33:44:55:66:77:88:99:aa:bb:cc:dd:ee:ff",
        )
        .env("HOME", "/nonexistent")
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

    // Verify double quotes on all export lines
    assert!(
        stdout.contains("export TRITON_PROFILE=\"env\""),
        "TRITON_PROFILE should use double quotes. Got:\n{}",
        stdout
    );
    assert!(
        stdout.contains("export SDC_URL=\"https://cloudapi.test.example.com\""),
        "SDC_URL should use double quotes. Got:\n{}",
        stdout
    );
    assert!(
        stdout.contains("export SDC_ACCOUNT=\"test-account\""),
        "SDC_ACCOUNT should use double quotes. Got:\n{}",
        stdout
    );
    assert!(
        stdout.contains("export SDC_KEY_ID=\"00:11:22:33:44:55:66:77:88:99:aa:bb:cc:dd:ee:ff\""),
        "SDC_KEY_ID should use double quotes. Got:\n{}",
        stdout
    );

    // Verify no single-quoted exports
    for line in stdout.lines() {
        if line.starts_with("export ") {
            assert!(
                !line.contains("='"),
                "Export line should not use single quotes: {}",
                line
            );
        }
    }
}

/// Test that `triton env` includes SDC_USER with double quotes when
/// TRITON_USER is set.
#[test]
fn test_env_bash_user_double_quotes() {
    let output = triton_cmd()
        .args(["env"])
        .env("TRITON_URL", "https://cloudapi.test.example.com")
        .env("TRITON_ACCOUNT", "test-account")
        .env(
            "TRITON_KEY_ID",
            "00:11:22:33:44:55:66:77:88:99:aa:bb:cc:dd:ee:ff",
        )
        .env("TRITON_USER", "subuser")
        .env("HOME", "/nonexistent")
        .output()
        .expect("Failed to run command");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success());

    assert!(
        stdout.contains("export SDC_USER=\"subuser\""),
        "SDC_USER should use double quotes. Got:\n{}",
        stdout
    );
}

/// Test that `triton env` outputs unset SDC_USER when no user is set.
#[test]
fn test_env_bash_unset_user() {
    let output = triton_cmd()
        .args(["env"])
        .env("TRITON_URL", "https://cloudapi.test.example.com")
        .env("TRITON_ACCOUNT", "test-account")
        .env(
            "TRITON_KEY_ID",
            "00:11:22:33:44:55:66:77:88:99:aa:bb:cc:dd:ee:ff",
        )
        .env_remove("TRITON_USER")
        .env_remove("SDC_USER")
        .env("HOME", "/nonexistent")
        .output()
        .expect("Failed to run command");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success());

    assert!(
        stdout.contains("unset SDC_USER"),
        "Should unset SDC_USER when no user. Got:\n{}",
        stdout
    );
}
