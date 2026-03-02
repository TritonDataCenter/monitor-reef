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

/// Test that `triton env` emits `unset DOCKER_TLS_VERIFY` when it is null (insecure mode).
#[test]
fn test_env_bash_docker_insecure_unsets_tls_verify() {
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

    // Should have other Docker vars and unset DOCKER_TLS_VERIFY
    assert!(
        stdout.contains("export DOCKER_HOST="),
        "Should still export DOCKER_HOST. Got:\n{}",
        stdout
    );
    assert!(
        stdout.contains("unset DOCKER_TLS_VERIFY"),
        "Should unset DOCKER_TLS_VERIFY when null. Got:\n{}",
        stdout
    );
    assert!(
        !stdout.contains("export DOCKER_TLS_VERIFY"),
        "Should NOT export DOCKER_TLS_VERIFY when null. Got:\n{}",
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

/// Test that `triton env --shell fish` outputs Docker variables in fish syntax.
#[test]
fn test_env_fish_docker_vars() {
    let tmp = setup_docker_env(true);

    let output = triton_cmd()
        .args(["env", "--shell", "fish"])
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

/// Helper: run `triton env` with standard env vars and extra args, return stdout.
fn run_env_with_args(args: &[&str]) -> String {
    run_env_with_args_insecure(args, false)
}

/// Helper: run `triton env` with standard env vars, extra args, and optional
/// TRITON_TLS_INSECURE flag, return stdout.
fn run_env_with_args_insecure(args: &[&str], insecure: bool) -> String {
    let mut cmd = triton_cmd();
    let mut all_args = vec!["env"];
    all_args.extend_from_slice(args);
    let cmd = cmd
        .args(all_args)
        .env("TRITON_URL", "https://cloudapi.test.example.com")
        .env("TRITON_ACCOUNT", "test-account")
        .env(
            "TRITON_KEY_ID",
            "00:11:22:33:44:55:66:77:88:99:aa:bb:cc:dd:ee:ff",
        )
        .env("HOME", "/nonexistent");
    if insecure {
        cmd.env("TRITON_TLS_INSECURE", "true");
    }
    let output = cmd.output().expect("Failed to run command");

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "Command should succeed.\nstdout: {}\nstderr: {}",
        stdout,
        stderr
    );
    stdout
}

/// Test that `--triton` emits only the triton section.
#[test]
fn test_env_triton_section_only() {
    let stdout = run_env_with_args(&["--triton"]);

    assert!(
        stdout.contains("# triton"),
        "Should have triton section. Got:\n{}",
        stdout
    );
    assert!(
        stdout.contains("TRITON_PROFILE"),
        "Should have TRITON_PROFILE. Got:\n{}",
        stdout
    );
    assert!(
        !stdout.contains("# docker"),
        "Should NOT have docker section. Got:\n{}",
        stdout
    );
    assert!(
        !stdout.contains("# smartdc"),
        "Should NOT have smartdc section. Got:\n{}",
        stdout
    );
}

/// Test that `--docker` emits only the docker section when setup.json exists.
#[test]
fn test_env_docker_section_only() {
    let tmp = setup_docker_env(true);

    let output = triton_cmd()
        .args(["env", "--docker"])
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

    assert!(
        stdout.contains("# docker"),
        "Should have docker section. Got:\n{}",
        stdout
    );
    assert!(
        !stdout.contains("# triton"),
        "Should NOT have triton section. Got:\n{}",
        stdout
    );
    assert!(
        !stdout.contains("# smartdc"),
        "Should NOT have smartdc section. Got:\n{}",
        stdout
    );
}

/// Test that `--smartdc` / `-s` emits only the smartdc section.
#[test]
fn test_env_smartdc_section_only() {
    let stdout = run_env_with_args(&["-s"]);

    assert!(
        stdout.contains("# smartdc"),
        "Should have smartdc section. Got:\n{}",
        stdout
    );
    assert!(
        stdout.contains("SDC_URL"),
        "Should have SDC_URL. Got:\n{}",
        stdout
    );
    assert!(
        !stdout.contains("# triton"),
        "Should NOT have triton section. Got:\n{}",
        stdout
    );
    assert!(
        !stdout.contains("# docker"),
        "Should NOT have docker section. Got:\n{}",
        stdout
    );
}

/// Test that combining section flags emits only those sections.
#[test]
fn test_env_combined_sections() {
    let tmp = setup_docker_env(true);

    let output = triton_cmd()
        .args(["env", "--triton", "--docker"])
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

    assert!(
        stdout.contains("# triton"),
        "Should have triton section. Got:\n{}",
        stdout
    );
    assert!(
        stdout.contains("# docker"),
        "Should have docker section. Got:\n{}",
        stdout
    );
    assert!(
        !stdout.contains("# smartdc"),
        "Should NOT have smartdc section. Got:\n{}",
        stdout
    );
}

/// Test that `--unset` emits unset commands for all sections (POSIX).
#[test]
fn test_env_unset_posix() {
    let stdout = run_env_with_args(&["--unset"]);

    // triton section
    assert!(
        stdout.contains("unset TRITON_PROFILE"),
        "Should unset TRITON_PROFILE. Got:\n{}",
        stdout
    );
    assert!(
        stdout.contains("unset TRITON_URL"),
        "Should unset TRITON_URL. Got:\n{}",
        stdout
    );
    assert!(
        stdout.contains("unset TRITON_ACCOUNT"),
        "Should unset TRITON_ACCOUNT. Got:\n{}",
        stdout
    );
    assert!(
        stdout.contains("unset TRITON_USER"),
        "Should unset TRITON_USER. Got:\n{}",
        stdout
    );
    assert!(
        stdout.contains("unset TRITON_KEY_ID"),
        "Should unset TRITON_KEY_ID. Got:\n{}",
        stdout
    );
    assert!(
        stdout.contains("unset TRITON_TLS_INSECURE"),
        "Should unset TRITON_TLS_INSECURE. Got:\n{}",
        stdout
    );
    // docker section
    assert!(
        stdout.contains("unset DOCKER_HOST"),
        "Should unset DOCKER_HOST. Got:\n{}",
        stdout
    );
    assert!(
        stdout.contains("unset DOCKER_CERT_PATH"),
        "Should unset DOCKER_CERT_PATH. Got:\n{}",
        stdout
    );
    // smartdc section
    assert!(
        stdout.contains("unset SDC_URL"),
        "Should unset SDC_URL. Got:\n{}",
        stdout
    );
    assert!(
        stdout.contains("unset SDC_TESTING"),
        "Should unset SDC_TESTING. Got:\n{}",
        stdout
    );

    // Should NOT contain any export statements
    assert!(
        !stdout.contains("export "),
        "Should NOT have any exports in unset mode. Got:\n{}",
        stdout
    );
}

/// Test that `--unset --triton` only unsets triton variables.
#[test]
fn test_env_unset_triton_only() {
    let stdout = run_env_with_args(&["--unset", "--triton"]);

    assert!(
        stdout.contains("unset TRITON_PROFILE"),
        "Should unset TRITON_PROFILE. Got:\n{}",
        stdout
    );
    assert!(
        !stdout.contains("DOCKER_"),
        "Should NOT mention DOCKER_ vars. Got:\n{}",
        stdout
    );
    assert!(
        !stdout.contains("SDC_"),
        "Should NOT mention SDC_ vars. Got:\n{}",
        stdout
    );
}

/// Test that `--unset` with fish shell uses `set -e`.
#[test]
fn test_env_unset_fish() {
    let stdout = run_env_with_args(&["--unset", "--shell", "fish"]);

    assert!(
        stdout.contains("set -e TRITON_PROFILE"),
        "Should use fish unset syntax. Got:\n{}",
        stdout
    );
    assert!(
        stdout.contains("set -e DOCKER_HOST"),
        "Should use fish unset for DOCKER_HOST. Got:\n{}",
        stdout
    );
    assert!(
        stdout.contains("set -e SDC_URL"),
        "Should use fish unset for SDC_URL. Got:\n{}",
        stdout
    );
}

/// Test that `--unset` with powershell uses `Remove-Item`.
#[test]
fn test_env_unset_powershell() {
    let stdout = run_env_with_args(&["--unset", "--shell", "powershell"]);

    assert!(
        stdout.contains("Remove-Item Env:TRITON_PROFILE -ErrorAction SilentlyContinue"),
        "Should use PowerShell unset syntax. Got:\n{}",
        stdout
    );
    assert!(
        stdout.contains("Remove-Item Env:SDC_URL -ErrorAction SilentlyContinue"),
        "Should use PowerShell unset for SDC_URL. Got:\n{}",
        stdout
    );
}

/// Test that bash exports SDC_TESTING="true" when profile is insecure.
#[test]
fn test_env_bash_sdc_testing_insecure() {
    let stdout = run_env_with_args_insecure(&[], true);

    assert!(
        stdout.contains("export SDC_TESTING=\"true\""),
        "Should export SDC_TESTING when insecure. Got:\n{}",
        stdout
    );
}

/// Test that bash unsets SDC_TESTING when profile is not insecure.
#[test]
fn test_env_bash_sdc_testing_secure() {
    let stdout = run_env_with_args_insecure(&[], false);

    assert!(
        stdout.contains("unset SDC_TESTING"),
        "Should unset SDC_TESTING when not insecure. Got:\n{}",
        stdout
    );
    assert!(
        !stdout.contains("export SDC_TESTING"),
        "Should NOT export SDC_TESTING when not insecure. Got:\n{}",
        stdout
    );
}

/// Test that fish exports SDC_TESTING when profile is insecure.
#[test]
fn test_env_fish_sdc_testing_insecure() {
    let stdout = run_env_with_args_insecure(&["--shell", "fish"], true);

    assert!(
        stdout.contains("set -gx SDC_TESTING 'true'"),
        "Should set SDC_TESTING in fish when insecure. Got:\n{}",
        stdout
    );
}

/// Test that fish unsets SDC_TESTING when profile is not insecure.
#[test]
fn test_env_fish_sdc_testing_secure() {
    let stdout = run_env_with_args_insecure(&["--shell", "fish"], false);

    assert!(
        stdout.contains("set -e SDC_TESTING"),
        "Should unset SDC_TESTING in fish when not insecure. Got:\n{}",
        stdout
    );
}

/// Test that powershell exports SDC_TESTING when profile is insecure.
#[test]
fn test_env_powershell_sdc_testing_insecure() {
    let stdout = run_env_with_args_insecure(&["--shell", "powershell"], true);

    assert!(
        stdout.contains("$env:SDC_TESTING = 'true'"),
        "Should set SDC_TESTING in powershell when insecure. Got:\n{}",
        stdout
    );
}

/// Test that powershell unsets SDC_TESTING when profile is not insecure.
#[test]
fn test_env_powershell_sdc_testing_secure() {
    let stdout = run_env_with_args_insecure(&["--shell", "powershell"], false);

    assert!(
        stdout.contains("Remove-Item Env:SDC_TESTING -ErrorAction SilentlyContinue"),
        "Should remove SDC_TESTING in powershell when not insecure. Got:\n{}",
        stdout
    );
}

/// Test that `triton env --shell fish` emits `set -e DOCKER_TLS_VERIFY` when it is null.
#[test]
fn test_env_fish_docker_insecure_unsets_tls_verify() {
    let tmp = setup_docker_env(false);

    let output = triton_cmd()
        .args(["env", "--shell", "fish"])
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
        stdout.contains("set -gx DOCKER_HOST"),
        "Should still set DOCKER_HOST. Got:\n{}",
        stdout
    );
    assert!(
        stdout.contains("set -e DOCKER_TLS_VERIFY"),
        "Should unset DOCKER_TLS_VERIFY in fish when null. Got:\n{}",
        stdout
    );
    assert!(
        !stdout.contains("set -gx DOCKER_TLS_VERIFY"),
        "Should NOT set DOCKER_TLS_VERIFY in fish when null. Got:\n{}",
        stdout
    );
}

/// Test that `triton env --docker` errors when no setup.json exists,
/// telling the user to run `triton profile docker-setup`.
#[test]
fn test_env_docker_explicit_no_setup_errors() {
    let output = triton_cmd()
        .args(["env", "--docker"])
        .env("TRITON_URL", "https://cloudapi.test.example.com")
        .env("TRITON_ACCOUNT", "test-account")
        .env(
            "TRITON_KEY_ID",
            "00:11:22:33:44:55:66:77:88:99:aa:bb:cc:dd:ee:ff",
        )
        .env("HOME", "/nonexistent")
        .output()
        .expect("Failed to run command");

    assert!(
        !output.status.success(),
        "Command should fail when --docker is explicit and no setup.json exists"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("docker-setup"),
        "Error should mention docker-setup. Got stderr:\n{}",
        stderr
    );
}
