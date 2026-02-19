// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Tests for `triton env` command output format

#![allow(deprecated, clippy::expect_used, clippy::unwrap_used)]

mod common;

use assert_cmd::Command;
use std::fs;

fn triton_cmd() -> Command {
    Command::cargo_bin("triton").expect("Failed to find triton binary")
}

/// Create a temp dir with a docker/env/setup.json containing Docker env vars.
/// Returns the temp dir (must be kept alive for the duration of the test).
fn setup_docker_env(tls_verify: bool) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("Failed to create temp dir");
    let docker_dir = tmp.path().join("docker").join("env");
    fs::create_dir_all(&docker_dir).expect("Failed to create docker dir");

    let tls_value = if tls_verify {
        serde_json::json!("1")
    } else {
        serde_json::Value::Null
    };

    let setup = serde_json::json!({
        "profile": "env",
        "account": "test-account",
        "time": "2026-01-01T00:00:00Z",
        "env": {
            "DOCKER_CERT_PATH": docker_dir.to_string_lossy(),
            "DOCKER_HOST": "tcp://us-central-1.docker.example.com:2376",
            "DOCKER_TLS_VERIFY": tls_value,
            "COMPOSE_HTTP_TIMEOUT": "300"
        }
    });

    fs::write(
        docker_dir.join("setup.json"),
        serde_json::to_string_pretty(&setup).unwrap(),
    )
    .expect("Failed to write setup.json");

    tmp
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

/// Test that `triton env` outputs Docker variables when setup.json exists.
#[test]
fn test_env_bash_docker_vars() {
    let tmp = setup_docker_env(true);

    let output = triton_cmd()
        .args(["env"])
        .env("TRITON_URL", "https://cloudapi.test.example.com")
        .env("TRITON_ACCOUNT", "test-account")
        .env(
            "TRITON_KEY_ID",
            "00:11:22:33:44:55:66:77:88:99:aa:bb:cc:dd:ee:ff",
        )
        .env("TRITON_CONFIG_DIR", tmp.path().to_str().unwrap())
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

    // Docker cert path contains the temp dir, just check the variable is present
    assert!(
        stdout.contains("export DOCKER_CERT_PATH="),
        "Should export DOCKER_CERT_PATH. Got:\n{}",
        stdout
    );
    assert!(
        stdout.contains("export DOCKER_HOST=\"tcp://us-central-1.docker.example.com:2376\""),
        "Should export DOCKER_HOST. Got:\n{}",
        stdout
    );
    assert!(
        stdout.contains("export DOCKER_TLS_VERIFY=\"1\""),
        "Should export DOCKER_TLS_VERIFY. Got:\n{}",
        stdout
    );
    assert!(
        stdout.contains("export COMPOSE_HTTP_TIMEOUT=\"300\""),
        "Should export COMPOSE_HTTP_TIMEOUT. Got:\n{}",
        stdout
    );
}

/// Test that `triton env` omits DOCKER_TLS_VERIFY when it is null (insecure mode).
#[test]
fn test_env_bash_docker_insecure_omits_tls_verify() {
    let tmp = setup_docker_env(false);

    let output = triton_cmd()
        .args(["env"])
        .env("TRITON_URL", "https://cloudapi.test.example.com")
        .env("TRITON_ACCOUNT", "test-account")
        .env(
            "TRITON_KEY_ID",
            "00:11:22:33:44:55:66:77:88:99:aa:bb:cc:dd:ee:ff",
        )
        .env("TRITON_CONFIG_DIR", tmp.path().to_str().unwrap())
        .output()
        .expect("Failed to run command");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success());

    // Should have other Docker vars but NOT DOCKER_TLS_VERIFY
    assert!(
        stdout.contains("export DOCKER_HOST="),
        "Should still export DOCKER_HOST. Got:\n{}",
        stdout
    );
    assert!(
        !stdout.contains("DOCKER_TLS_VERIFY"),
        "Should NOT export DOCKER_TLS_VERIFY when insecure. Got:\n{}",
        stdout
    );
}

/// Test that `triton env` outputs empty docker section when no setup.json exists.
#[test]
fn test_env_bash_no_docker_setup() {
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
    assert!(output.status.success());

    // Should have the docker comment but no DOCKER_ exports
    assert!(
        stdout.contains("# docker"),
        "Should have docker section header. Got:\n{}",
        stdout
    );
    assert!(
        !stdout.contains("DOCKER_"),
        "Should NOT have any DOCKER_ variables without setup.json. Got:\n{}",
        stdout
    );
}

/// Test that `triton env -s fish` outputs Docker variables in fish syntax.
#[test]
fn test_env_fish_docker_vars() {
    let tmp = setup_docker_env(true);

    let output = triton_cmd()
        .args(["env", "-s", "fish"])
        .env("TRITON_URL", "https://cloudapi.test.example.com")
        .env("TRITON_ACCOUNT", "test-account")
        .env(
            "TRITON_KEY_ID",
            "00:11:22:33:44:55:66:77:88:99:aa:bb:cc:dd:ee:ff",
        )
        .env("TRITON_CONFIG_DIR", tmp.path().to_str().unwrap())
        .output()
        .expect("Failed to run command");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success());

    assert!(
        stdout.contains("set -gx DOCKER_HOST 'tcp://us-central-1.docker.example.com:2376'"),
        "Should use fish syntax for DOCKER_HOST. Got:\n{}",
        stdout
    );
    assert!(
        stdout.contains("set -gx DOCKER_TLS_VERIFY '1'"),
        "Should use fish syntax for DOCKER_TLS_VERIFY. Got:\n{}",
        stdout
    );
}
