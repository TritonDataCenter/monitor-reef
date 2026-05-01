// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Integration tests for the `tcadm` operator-auth surface.
//!
//! Each test spins up `tritond` in-process on an ephemeral port,
//! seeds a root user with a known password, then runs `tcadm` as a
//! subprocess against that endpoint. The on-disk config is rerouted
//! into a tempdir via `TCADM_CONFIG_DIR` so the user's real
//! `~/.config/tcadm` is never touched.

use std::sync::Arc;

use assert_cmd::Command;
use chrono::Utc;
use tempfile::TempDir;
use tritond::audit::AuditService;
use tritond::auth::AuthService;
use tritond::{ApiContext, start_server_with_context};
use tritond_audit::MemChain;
use tritond_auth::{JwtKey, RedactedString, hash_password, mint_access};
use tritond_store::{MemStore, Store, User};
use uuid::Uuid;

const ROOT_PASSWORD: &str = "rosebud";

struct TestServer {
    server: dropshot::HttpServer<ApiContext>,
    user_id: Uuid,
    jwt_key: JwtKey,
}

impl TestServer {
    async fn start() -> Self {
        let store: Arc<dyn Store> = Arc::new(MemStore::new());
        let user_id = Uuid::new_v4();
        let user = User {
            id: user_id,
            username: "root".to_string(),
            password_hash: hash_password(&RedactedString::from(ROOT_PASSWORD))
                .await
                .unwrap(),
            is_root: true,
            created_at: Utc::now(),
            silo_id: None,
            federation: None,
        };
        store.create_user(user).await.unwrap();
        let jwt_key = JwtKey::generate();
        let auth_service =
            Arc::new(AuthService::new(JwtKey::from_bytes(*jwt_key.bytes())).unwrap());
        let audit = Arc::new(AuditService::new(Arc::new(MemChain::new())));
        let context = ApiContext::new(store, auth_service, audit);
        let server = start_server_with_context("127.0.0.1:0", context)
            .await
            .unwrap();
        Self {
            server,
            user_id,
            jwt_key,
        }
    }

    fn endpoint(&self) -> String {
        format!("http://{}", self.server.local_addr())
    }

    fn access_token(&self) -> String {
        let (token, _) = mint_access(&self.jwt_key, self.user_id).unwrap();
        token
    }

    async fn close(self) {
        self.server.close().await.unwrap();
    }
}

/// Build a `Command::cargo_bin("tcadm")` with the config dir routed
/// to `tmp` and TCADM_* env vars cleared so each test starts from a
/// clean slate.
fn tcadm_cmd(tmp: &TempDir) -> Command {
    let mut cmd = Command::cargo_bin("tcadm").unwrap();
    cmd.env("TCADM_CONFIG_DIR", tmp.path())
        .env_remove("TCADM_ENDPOINT")
        .env_remove("TCADM_API_KEY")
        .env_remove("TCADM_ACCESS_TOKEN");
    cmd
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn configure_persists_tokens_and_subsequent_call_uses_them() {
    let tmp = TempDir::new().unwrap();
    let test = TestServer::start().await;
    let endpoint = test.endpoint();

    // tcadm configure --endpoint X --username root --password-stdin
    tcadm_cmd(&tmp)
        .args([
            "configure",
            "--endpoint",
            &endpoint,
            "--username",
            "root",
            "--password-stdin",
        ])
        .write_stdin(format!("{ROOT_PASSWORD}\n"))
        .assert()
        .success();

    // Now `tcadm api-key list` should succeed using the persisted
    // tokens — no flag needed.
    tcadm_cmd(&tmp)
        .args(["api-key", "list"])
        .assert()
        .success()
        .stdout(predicates::str::contains("(no api keys)"));

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn login_with_wrong_password_fails() {
    let tmp = TempDir::new().unwrap();
    let test = TestServer::start().await;
    let endpoint = test.endpoint();

    tcadm_cmd(&tmp)
        .args([
            "configure",
            "--endpoint",
            &endpoint,
            "--username",
            "root",
            "--password-stdin",
        ])
        .write_stdin("definitely-not-the-password\n")
        .assert()
        .failure();

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn access_token_env_var_authenticates() {
    let tmp = TempDir::new().unwrap();
    let test = TestServer::start().await;
    let endpoint = test.endpoint();
    let token = test.access_token();

    // No `configure` happened; we authenticate purely via env.
    tcadm_cmd(&tmp)
        .args(["--endpoint", &endpoint, "api-key", "list"])
        .env("TCADM_ACCESS_TOKEN", &token)
        .assert()
        .success();

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_credentials_returns_403_via_anonymous_call() {
    let tmp = TempDir::new().unwrap();
    let test = TestServer::start().await;
    let endpoint = test.endpoint();

    // No config, no env, `api-key list` requires auth → server
    // refuses with 403, tcadm bubbles that up as a non-zero exit.
    tcadm_cmd(&tmp)
        .args(["--endpoint", &endpoint, "api-key", "list"])
        .assert()
        .failure();

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn api_key_create_then_use_via_api_key_flag() {
    let tmp = TempDir::new().unwrap();
    let test = TestServer::start().await;
    let endpoint = test.endpoint();

    // First: configure (which mints a session JWT).
    tcadm_cmd(&tmp)
        .args([
            "configure",
            "--endpoint",
            &endpoint,
            "--username",
            "root",
            "--password-stdin",
        ])
        .write_stdin(format!("{ROOT_PASSWORD}\n"))
        .assert()
        .success();

    // Mint an api key with --json so we can parse the secret.
    let create_output = tcadm_cmd(&tmp)
        .args(["api-key", "create", "--description", "ci", "--json"])
        .output()
        .unwrap();
    assert!(
        create_output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&create_output.stderr)
    );
    let parsed: serde_json::Value = serde_json::from_slice(&create_output.stdout).unwrap();
    let secret = parsed["secret"].as_str().expect("secret should be present");
    assert!(secret.starts_with("tcadm_"));

    // Use the api-key bearer to list — this proves the API-key path
    // works without using the stored JWT session.
    let list_output = tcadm_cmd(&tmp)
        .args([
            "--endpoint",
            &endpoint,
            "--api-key",
            secret,
            "api-key",
            "list",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        list_output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&list_output.stderr)
    );
    let listed: serde_json::Value = serde_json::from_slice(&list_output.stdout).unwrap();
    assert_eq!(listed.as_array().map(|a| a.len()), Some(1));

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn env_subcommand_emits_endpoint_export() {
    let tmp = TempDir::new().unwrap();
    let test = TestServer::start().await;
    let endpoint = test.endpoint();
    let token = test.access_token();

    let output = tcadm_cmd(&tmp)
        .args(["--endpoint", &endpoint, "env"])
        .env("TCADM_ACCESS_TOKEN", &token)
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("TCADM_ENDPOINT="));
    assert!(stdout.contains("TCADM_ACCESS_TOKEN="));

    test.close().await;
}
