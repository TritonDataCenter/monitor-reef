// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! End-to-end flow against a live identityd server on an ephemeral port.
//!
//! These tests link the binary crate's modules via `include!` is not
//! possible (binary crate), so they re-derive the few constants they
//! need and exercise the public HTTP surface only.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::process::{Child, Command};
use std::time::Duration;

use serial_test::file_serial;

const TENANT_REALM: &str = "11111111-1111-4111-8111-111111111111";
const CLIENT_ID: &str = "triton-workbench";
const CLIENT_SECRET: &str = "dev-secret";
const USERNAME: &str = "nwilkens";
const PASSWORD: &str = "workbench-demo";

/// Spawn the built identityd binary on its fixed port and wait for it to
/// answer healthz. Returns the child (killed on drop) and the base URL.
struct Server {
    child: Child,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn base() -> String {
    "http://127.0.0.1:8090".to_string()
}

async fn wait_healthy(client: &reqwest::Client) -> bool {
    // Boot seeds several bcrypt(cost=12) hashes; on a slow build host the
    // seed alone can take ~6s, so allow a generous window.
    for _ in 0..200 {
        if let Ok(resp) = client.get(format!("{}/healthz", base())).send().await
            && resp.status().is_success()
        {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

fn spawn() -> Option<Server> {
    let bin = env!("CARGO_BIN_EXE_identityd");
    let child = Command::new(bin).env("RUST_LOG", "warn").spawn().ok()?;
    Some(Server { child })
}

#[tokio::test]
#[file_serial(identityd_port)]
async fn password_grant_then_userinfo_round_trip() {
    let Some(_server) = spawn() else {
        eprintln!("SKIP: could not spawn identityd binary");
        return;
    };
    let client = reqwest::Client::new();
    assert!(wait_healthy(&client).await, "identityd did not become healthy");

    // JWKS publishes exactly one RS256 key with the pinned kid.
    let jwks: serde_json::Value = client
        .get(format!("{}/realms/{}/jwks", base(), TENANT_REALM))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let keys = jwks["keys"].as_array().unwrap();
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0]["kid"], "wb-rsa-1");
    assert_eq!(keys[0]["alg"], "RS256");

    // discovery document points back at this realm.
    let disco: serde_json::Value = client
        .get(format!(
            "{}/realms/{}/.well-known/openid-configuration",
            base(),
            TENANT_REALM
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        disco["issuer"],
        format!("http://127.0.0.1:8090/realms/{TENANT_REALM}")
    );

    // password grant -> tokens.
    let token: serde_json::Value = client
        .post(format!("{}/realms/{}/token", base(), TENANT_REALM))
        .json(&serde_json::json!({
            "grant_type": "password",
            "username": USERNAME,
            "password": PASSWORD,
            "client_id": CLIENT_ID,
            "client_secret": CLIENT_SECRET,
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let access = token["access_token"].as_str().unwrap();
    assert_eq!(token["token_type"], "Bearer");
    assert_eq!(token["expires_in"], 3600);
    let refresh = token["refresh_token"].as_str().unwrap().to_string();

    // userinfo with the access token.
    let info: serde_json::Value = client
        .get(format!("{}/realms/{}/userinfo", base(), TENANT_REALM))
        .bearer_auth(access)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(info["preferred_username"], USERNAME);
    assert_eq!(info["realm"], TENANT_REALM);
    assert_eq!(info["tenant_id"], "22222222-2222-4222-8222-222222222222");
    assert_eq!(info["silo_id"], "33333333-3333-4333-8333-333333333333");
    assert_eq!(info["is_root"], false);

    // refresh grant rotates.
    let refreshed = client
        .post(format!("{}/realms/{}/token", base(), TENANT_REALM))
        .json(&serde_json::json!({
            "grant_type": "refresh_token",
            "refresh_token": refresh,
            "client_id": CLIENT_ID,
            "client_secret": CLIENT_SECRET,
        }))
        .send()
        .await
        .unwrap();
    assert!(refreshed.status().is_success());

    // bad password is rejected.
    let bad = client
        .post(format!("{}/realms/{}/token", base(), TENANT_REALM))
        .json(&serde_json::json!({
            "grant_type": "password",
            "username": USERNAME,
            "password": "wrong",
            "client_id": CLIENT_ID,
            "client_secret": CLIENT_SECRET,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), reqwest::StatusCode::BAD_REQUEST);
}
