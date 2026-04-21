// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! End-to-end tests for `triton login`, `triton whoami`, `triton logout`.
//!
//! These tests spin up a minimal axum gateway on a random port that
//! honours the three `/v1/auth/*` endpoints the CLI depends on. No
//! outbound network traffic; no real gateway required.
//!
//! We test against HTTP (not HTTPS) to avoid a self-signed-cert dance;
//! `--insecure` isn't exercised here. The cargo-level unit test
//! `insecure_mode_accepts_self_signed_cert` in `main.rs` covers that
//! pathway independently.

#![allow(deprecated, clippy::expect_used, clippy::unwrap_used)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use assert_cmd::Command;
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use predicates::prelude::*;
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio::sync::Mutex;

/// A minimal token store shared between login and whoami/logout handlers
/// so a logout revokes the token live.
#[derive(Default, Clone)]
struct MockState {
    inner: Arc<Mutex<MockInner>>,
}

#[derive(Default)]
struct MockInner {
    /// currently-valid access token (opaque string); None after logout.
    current_token: Option<String>,
    /// currently-valid refresh token.
    current_refresh: Option<String>,
    /// How many logins have we handled? Used to make tokens unique.
    login_count: u64,
}

#[derive(Serialize, Deserialize)]
struct LoginReq {
    username: String,
    password: String,
}

#[derive(Serialize)]
struct UserInfo {
    id: String,
    username: String,
    is_admin: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    company: Option<String>,
}

#[derive(Serialize)]
struct LoginResp {
    token: String,
    refresh_token: String,
    user: UserInfo,
}

#[derive(Serialize)]
struct SessionResp {
    user: UserInfo,
}

#[derive(Serialize)]
struct LogoutResp {
    ok: bool,
}

#[derive(Serialize)]
struct ErrorBody {
    error_code: String,
    message: String,
    request_id: String,
}

fn make_jwt(username: &str, exp_secs_from_now: i64) -> String {
    let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"ES256","typ":"JWT"}"#);
    let exp = (chrono::Utc::now() + chrono::Duration::seconds(exp_secs_from_now)).timestamp();
    let payload = serde_json::json!({ "sub": username, "exp": exp }).to_string();
    let payload = URL_SAFE_NO_PAD.encode(payload.as_bytes());
    let sig = URL_SAFE_NO_PAD.encode(b"mock-signature");
    format!("{header}.{payload}.{sig}")
}

async fn login(
    State(state): State<MockState>,
    Json(body): Json<LoginReq>,
) -> Result<Json<LoginResp>, (StatusCode, Json<ErrorBody>)> {
    if body.password != "joypass123" {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(ErrorBody {
                error_code: "InvalidCredentials".into(),
                message: "bad password".into(),
                request_id: "mock-req".into(),
            }),
        ));
    }
    let mut st = state.inner.lock().await;
    st.login_count += 1;
    let token = make_jwt(&body.username, 3600);
    let refresh = format!("refresh-{}", st.login_count);
    st.current_token = Some(token.clone());
    st.current_refresh = Some(refresh.clone());
    Ok(Json(LoginResp {
        token,
        refresh_token: refresh,
        user: UserInfo {
            id: "00000000-0000-0000-0000-000000000001".into(),
            username: body.username,
            is_admin: true,
            email: Some("admin@example.com".into()),
            name: None,
            company: None,
        },
    }))
}

async fn session(
    State(state): State<MockState>,
    headers: HeaderMap,
) -> Result<Json<SessionResp>, (StatusCode, Json<ErrorBody>)> {
    let auth = headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    let st = state.inner.lock().await;
    match &st.current_token {
        Some(expected) if auth == format!("Bearer {expected}") => Ok(Json(SessionResp {
            user: UserInfo {
                id: "00000000-0000-0000-0000-000000000001".into(),
                username: "admin".into(),
                is_admin: true,
                email: None,
                name: None,
                company: None,
            },
        })),
        _ => Err((
            StatusCode::UNAUTHORIZED,
            Json(ErrorBody {
                error_code: "InvalidToken".into(),
                message: "token invalid or revoked".into(),
                request_id: "mock-req".into(),
            }),
        )),
    }
}

async fn logout(State(state): State<MockState>) -> Json<LogoutResp> {
    let mut st = state.inner.lock().await;
    st.current_token = None;
    st.current_refresh = None;
    Json(LogoutResp { ok: true })
}

async fn spawn_mock_gateway() -> (String, tokio::task::JoinHandle<()>) {
    let state = MockState::default();
    let app = Router::new()
        .route("/v1/auth/login", post(login))
        .route("/v1/auth/session", get(session))
        .route("/v1/auth/logout", post(logout))
        .with_state(state);

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr: SocketAddr = listener.local_addr().expect("addr");
    let url = format!("http://{addr}");

    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });

    (url, handle)
}

/// End-to-end: login -> whoami -> logout -> whoami fails.
#[test]
fn login_whoami_logout_round_trip() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let (url, _handle) = spawn_mock_gateway().await;
        // Give the server a moment to bind + start.
        tokio::time::sleep(Duration::from_millis(50)).await;

        let tmp = tempfile::tempdir().unwrap();
        let config_dir = tmp.path();

        // Write a tritonapi profile pointing at the mock gateway.
        let profiles_dir = config_dir.join("profiles.d");
        tokio::fs::create_dir_all(&profiles_dir).await.unwrap();
        let profile_path = profiles_dir.join("mockgw.json");
        tokio::fs::write(
            &profile_path,
            serde_json::to_string_pretty(&serde_json::json!({
                "auth": "tritonapi",
                "url": url,
                "account": "admin",
            }))
            .unwrap(),
        )
        .await
        .unwrap();

        // login: empty line on stdin => username defaults to "admin";
        // password supplied via TRITON_PASSWORD to avoid needing a tty.
        let login_out = Command::cargo_bin("triton")
            .unwrap()
            .env("TRITON_CONFIG_DIR", config_dir)
            .env("TRITON_PASSWORD", "joypass123")
            .args(["-p", "mockgw", "login"])
            .write_stdin("\n")
            .assert()
            .success()
            .stderr(predicate::str::contains("Logged in as admin"))
            .stderr(predicate::str::contains("Token expires"))
            .get_output()
            .clone();
        // stdout must be empty: all user-facing output goes to stderr.
        assert!(
            login_out.stdout.is_empty(),
            "stdout should be empty, was: {:?}",
            String::from_utf8_lossy(&login_out.stdout)
        );

        // Token file should exist at mode 0600.
        let token_path = config_dir.join("tokens").join("mockgw.json");
        assert!(tokio::fs::try_exists(&token_path).await.unwrap());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let meta = tokio::fs::metadata(&token_path).await.unwrap();
            let mode = meta.permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "token file mode should be 0600, was {mode:o}");
        }

        // whoami prints the username + id.
        Command::cargo_bin("triton")
            .unwrap()
            .env("TRITON_CONFIG_DIR", config_dir)
            .args(["-p", "mockgw", "whoami"])
            .assert()
            .success()
            .stdout(predicate::str::contains("username: admin"))
            .stdout(predicate::str::contains(
                "id:       00000000-0000-0000-0000-000000000001",
            ))
            .stdout(predicate::str::contains("is_admin: true"));

        // logout deletes the token file.
        Command::cargo_bin("triton")
            .unwrap()
            .env("TRITON_CONFIG_DIR", config_dir)
            .args(["-p", "mockgw", "logout"])
            .assert()
            .success()
            .stderr(predicate::str::contains("Logged out"));
        assert!(!tokio::fs::try_exists(&token_path).await.unwrap());

        // whoami now fails with an actionable message.
        Command::cargo_bin("triton")
            .unwrap()
            .env("TRITON_CONFIG_DIR", config_dir)
            .args(["-p", "mockgw", "whoami"])
            .assert()
            .failure()
            .stderr(predicate::str::contains("Not logged in"));

        // logout on a logged-out profile is idempotent.
        Command::cargo_bin("triton")
            .unwrap()
            .env("TRITON_CONFIG_DIR", config_dir)
            .args(["-p", "mockgw", "logout"])
            .assert()
            .success()
            .stderr(predicate::str::contains("Not logged in"));
    });
}

/// Login against an SSH-kind profile must fail with a "wrong auth kind"
/// message; the token file must NOT be written.
#[test]
fn login_refuses_ssh_profile() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let tmp = tempfile::tempdir().unwrap();
        let config_dir = tmp.path();
        let profiles_dir = config_dir.join("profiles.d");
        tokio::fs::create_dir_all(&profiles_dir).await.unwrap();
        let profile_path = profiles_dir.join("ssh.json");
        // No "auth" tag -> SSH-kind (backward compat).
        tokio::fs::write(
            &profile_path,
            serde_json::to_string_pretty(&serde_json::json!({
                "url": "https://cloudapi.example.com",
                "account": "alice",
                "keyId": "aa:bb:cc:dd:ee:ff:00:11:22:33:44:55:66:77:88:99",
            }))
            .unwrap(),
        )
        .await
        .unwrap();

        Command::cargo_bin("triton")
            .unwrap()
            .env("TRITON_CONFIG_DIR", config_dir)
            .args(["-p", "ssh", "login"])
            .assert()
            .failure()
            .stderr(predicate::str::contains("tritonapi"));
        assert!(
            !tokio::fs::try_exists(config_dir.join("tokens").join("ssh.json"))
                .await
                .unwrap()
        );
    });
}
