// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! End-to-end tests of the `/v1/...` admin surface against a live server.
//!
//! These exercise the load-bearing authz contract: a fleet token manages
//! any realm; a tenant-admin token manages only its own tenant's realm and
//! gets 403 elsewhere; secrets/password hashes never appear in responses.
//!
//! The binary crate can't be linked from an integration test, so we drive
//! it over HTTP only and re-derive the pinned constants the bootstrap uses.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::process::{Child, Command};
use std::time::Duration;

use serial_test::file_serial;

// Pinned wire-contract ids (mirrors `src/identifiers.rs`).
const SYSTEM_REALM: &str = "00000000-0000-4000-8000-000000000000";
const TENANT_REALM: &str = "11111111-1111-4111-8111-111111111111";

// Tenant (Workbench) directory: nwilkens is TenantAdmin there.
const TENANT_CLIENT_ID: &str = "triton-workbench";
const TENANT_CLIENT_SECRET: &str = "dev-secret";
const TENANT_USERNAME: &str = "nwilkens";
const TENANT_PASSWORD: &str = "workbench-demo";

// System (operator) directory: a fleet_admin user via the password grant.
const SYSTEM_CLIENT_ID: &str = "triton-operator";
const SYSTEM_CLIENT_SECRET: &str = "operator-secret";
const OPERATOR_USERNAME: &str = "operator";
const OPERATOR_PASSWORD: &str = "operator-demo";

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

fn spawn() -> Option<Server> {
    let bin = env!("CARGO_BIN_EXE_identityd");
    let child = Command::new(bin).env("RUST_LOG", "warn").spawn().ok()?;
    Some(Server { child })
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

/// Password-grant against the given pinned realm. Returns the access token.
async fn login(
    client: &reqwest::Client,
    realm: &str,
    client_id: &str,
    client_secret: &str,
    username: &str,
    password: &str,
) -> String {
    let token: serde_json::Value = client
        .post(format!("{}/realms/{}/token", base(), realm))
        .json(&serde_json::json!({
            "grant_type": "password",
            "username": username,
            "password": password,
            "client_id": client_id,
            "client_secret": client_secret,
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    token["access_token"]
        .as_str()
        .unwrap_or_else(|| panic!("no access_token in {token}"))
        .to_string()
}

async fn fleet_token(client: &reqwest::Client) -> String {
    login(
        client,
        SYSTEM_REALM,
        SYSTEM_CLIENT_ID,
        SYSTEM_CLIENT_SECRET,
        OPERATOR_USERNAME,
        OPERATOR_PASSWORD,
    )
    .await
}

async fn tenant_admin_token(client: &reqwest::Client) -> String {
    login(
        client,
        TENANT_REALM,
        TENANT_CLIENT_ID,
        TENANT_CLIENT_SECRET,
        TENANT_USERNAME,
        TENANT_PASSWORD,
    )
    .await
}

/// Resolve the store id of the realm with the given scope tag/tenant via the
/// fleet `GET /v1/realms` listing.
async fn realm_store_id(client: &reqwest::Client, fleet: &str, want: &serde_json::Value) -> String {
    let realms: serde_json::Value = client
        .get(format!("{}/v1/realms", base()))
        .bearer_auth(fleet)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    for r in realms.as_array().unwrap() {
        if &r["scope"] == want {
            return r["id"].as_str().unwrap().to_string();
        }
    }
    panic!("no realm with scope {want} in {realms}");
}

#[tokio::test]
#[file_serial(identityd_port)]
async fn fleet_token_can_crud_users_and_connections_in_any_realm() {
    let Some(_server) = spawn() else {
        eprintln!("SKIP: could not spawn identityd binary");
        return;
    };
    let client = reqwest::Client::new();
    assert!(wait_healthy(&client).await, "identityd did not become healthy");

    let fleet = fleet_token(&client).await;
    let tenant_realm = realm_store_id(
        &client,
        &fleet,
        &serde_json::json!({
            "kind": "tenant",
            "tenant_id": "22222222-2222-4222-8222-222222222222"
        }),
    )
    .await;

    // --- create a user (fleet, in the tenant realm) ---
    let created: serde_json::Value = client
        .post(format!("{}/v1/realms/{}/users", base(), tenant_realm))
        .bearer_auth(&fleet)
        .json(&serde_json::json!({
            "username": "alice",
            "email": "alice@example.com",
            "display_name": "Alice",
            "password": "s3cret-alice",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(created["username"], "alice");
    // No password hash anywhere in the response.
    assert!(
        created.get("password_hash").is_none(),
        "create-user leaked a password_hash field: {created}"
    );
    let alice_id = created["id"].as_str().unwrap().to_string();

    // --- list users includes alice ---
    let users: serde_json::Value = client
        .get(format!("{}/v1/realms/{}/users", base(), tenant_realm))
        .bearer_auth(&fleet)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        users.as_array().unwrap().iter().any(|u| u["id"] == created["id"]),
        "alice not in user list"
    );

    // --- set password + update status ---
    let pw = client
        .post(format!(
            "{}/v1/realms/{}/users/{}/password",
            base(),
            tenant_realm,
            alice_id
        ))
        .bearer_auth(&fleet)
        .json(&serde_json::json!({ "password": "new-s3cret" }))
        .send()
        .await
        .unwrap();
    assert!(pw.status().is_success(), "set password failed");

    let patched: serde_json::Value = client
        .patch(format!(
            "{}/v1/realms/{}/users/{}",
            base(),
            tenant_realm,
            alice_id
        ))
        .bearer_auth(&fleet)
        .json(&serde_json::json!({ "status": "disabled" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(patched["status"], "disabled");

    // --- create a connection (with a secret) and confirm it is redacted ---
    let conn: serde_json::Value = client
        .post(format!("{}/v1/realms/{}/connections", base(), tenant_realm))
        .bearer_auth(&fleet)
        .json(&serde_json::json!({
            "name": "azure",
            "kind": {
                "protocol": "oidc",
                "issuer_url": "https://login.microsoftonline.com/tenant/v2.0",
                "client_id": "azure-client",
                "client_secret": "azure-SUPER-secret",
                "scopes": ["openid", "profile"],
            },
            "enabled": false,
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let conn_str = serde_json::to_string(&conn).unwrap();
    assert!(
        !conn_str.contains("azure-SUPER-secret"),
        "connection response leaked the client_secret: {conn_str}"
    );
    assert_eq!(conn["kind"]["client_secret_set"], true);
    let conn_id = conn["id"].as_str().unwrap().to_string();

    // --- delete alice ---
    let del = client
        .delete(format!(
            "{}/v1/realms/{}/users/{}",
            base(),
            tenant_realm,
            alice_id
        ))
        .bearer_auth(&fleet)
        .send()
        .await
        .unwrap();
    assert!(del.status().is_success(), "delete user failed");

    // --- delete the connection ---
    let del = client
        .delete(format!(
            "{}/v1/realms/{}/connections/{}",
            base(),
            tenant_realm,
            conn_id
        ))
        .bearer_auth(&fleet)
        .send()
        .await
        .unwrap();
    assert!(del.status().is_success(), "delete connection failed");
}

#[tokio::test]
#[file_serial(identityd_port)]
async fn tenant_admin_manages_own_realm_but_not_others() {
    let Some(_server) = spawn() else {
        eprintln!("SKIP: could not spawn identityd binary");
        return;
    };
    let client = reqwest::Client::new();
    assert!(wait_healthy(&client).await, "identityd did not become healthy");

    let fleet = fleet_token(&client).await;
    let admin = tenant_admin_token(&client).await;

    let own_realm = realm_store_id(
        &client,
        &fleet,
        &serde_json::json!({
            "kind": "tenant",
            "tenant_id": "22222222-2222-4222-8222-222222222222"
        }),
    )
    .await;

    // The tenant admin can list users in its OWN realm.
    let ok = client
        .get(format!("{}/v1/realms/{}/users", base(), own_realm))
        .bearer_auth(&admin)
        .send()
        .await
        .unwrap();
    assert!(
        ok.status().is_success(),
        "tenant admin denied on its own realm: {}",
        ok.status()
    );

    // The tenant admin can create a user in its own realm.
    let created = client
        .post(format!("{}/v1/realms/{}/users", base(), own_realm))
        .bearer_auth(&admin)
        .json(&serde_json::json!({
            "username": "bob",
            "password": "bob-s3cret",
        }))
        .send()
        .await
        .unwrap();
    assert!(
        created.status().is_success(),
        "tenant admin could not create a user in its own realm"
    );

    // --- cross-tenant isolation: a fleet-created sibling tenant B realm ---
    let tenant_b = "99999999-9999-4999-8999-999999999999";
    let realm_b: serde_json::Value = client
        .post(format!("{}/v1/tenants/{}/realm", base(), tenant_b))
        .bearer_auth(&fleet)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let realm_b_id = realm_b["id"].as_str().unwrap().to_string();

    // Tenant A's admin must NOT read tenant B's realm.
    let denied = client
        .get(format!("{}/v1/realms/{}/users", base(), realm_b_id))
        .bearer_auth(&admin)
        .send()
        .await
        .unwrap();
    assert_eq!(
        denied.status(),
        reqwest::StatusCode::FORBIDDEN,
        "tenant A admin was NOT blocked from tenant B's realm"
    );

    // Tenant A's admin must NOT touch the System realm either.
    let system_realm = realm_store_id(&client, &fleet, &serde_json::json!({ "kind": "system" })).await;
    let denied_sys = client
        .get(format!("{}/v1/realms/{}/users", base(), system_realm))
        .bearer_auth(&admin)
        .send()
        .await
        .unwrap();
    assert_eq!(
        denied_sys.status(),
        reqwest::StatusCode::FORBIDDEN,
        "tenant A admin was NOT blocked from the System realm"
    );

    // And the tenant admin cannot list all realms (fleet-only).
    let denied_list = client
        .get(format!("{}/v1/realms", base()))
        .bearer_auth(&admin)
        .send()
        .await
        .unwrap();
    assert_eq!(
        denied_list.status(),
        reqwest::StatusCode::FORBIDDEN,
        "tenant admin was allowed to list all realms"
    );
}

#[tokio::test]
#[file_serial(identityd_port)]
async fn enabling_oidc_connection_flips_identity_source_mode() {
    let Some(_server) = spawn() else {
        eprintln!("SKIP: could not spawn identityd binary");
        return;
    };
    let client = reqwest::Client::new();
    assert!(wait_healthy(&client).await, "identityd did not become healthy");

    let fleet = fleet_token(&client).await;
    // Use a fresh tenant realm so the toggle is isolated from other tests
    // racing on the shared tenant realm.
    let tenant_c = "55555555-5555-4555-8555-555555555555";
    let realm_c: serde_json::Value = client
        .post(format!("{}/v1/tenants/{}/realm", base(), tenant_c))
        .bearer_auth(&fleet)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let realm = realm_c["id"].as_str().unwrap().to_string();

    // No connection yet => integrated.
    let src: serde_json::Value = client
        .get(format!("{}/v1/realms/{}/identity-source", base(), realm))
        .bearer_auth(&fleet)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(src["mode"], "integrated");

    // Create an enabled OIDC connection => mode flips to oidc.
    let conn: serde_json::Value = client
        .post(format!("{}/v1/realms/{}/connections", base(), realm))
        .bearer_auth(&fleet)
        .json(&serde_json::json!({
            "name": "okta",
            "kind": {
                "protocol": "oidc",
                "issuer_url": "https://example.okta.com",
                "client_id": "okta-client",
                "client_secret": "okta-SECRET",
                "scopes": ["openid"],
            },
            "enabled": true,
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let conn_id = conn["id"].as_str().unwrap().to_string();

    let src: serde_json::Value = client
        .get(format!("{}/v1/realms/{}/identity-source", base(), realm))
        .bearer_auth(&fleet)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(src["mode"], "oidc");
    assert_eq!(src["connection"]["issuer_url"], "https://example.okta.com");
    assert_eq!(src["connection"]["client_id"], "okta-client");
    // The identity-source summary must not leak the secret either.
    let src_str = serde_json::to_string(&src).unwrap();
    assert!(
        !src_str.contains("okta-SECRET"),
        "identity-source leaked the client_secret: {src_str}"
    );

    // Disable it => back to integrated.
    let patched = client
        .patch(format!(
            "{}/v1/realms/{}/connections/{}",
            base(),
            realm,
            conn_id
        ))
        .bearer_auth(&fleet)
        .json(&serde_json::json!({ "enabled": false }))
        .send()
        .await
        .unwrap();
    assert!(patched.status().is_success());

    let src: serde_json::Value = client
        .get(format!("{}/v1/realms/{}/identity-source", base(), realm))
        .bearer_auth(&fleet)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(src["mode"], "integrated");

    // Listing connections must redact the secret.
    let conns: serde_json::Value = client
        .get(format!("{}/v1/realms/{}/connections", base(), realm))
        .bearer_auth(&fleet)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let conns_str = serde_json::to_string(&conns).unwrap();
    assert!(
        !conns_str.contains("okta-SECRET"),
        "connection list leaked the client_secret: {conns_str}"
    );
}

#[tokio::test]
#[file_serial(identityd_port)]
async fn enabling_second_connection_disables_the_first() {
    // H3: a realm has at most one enabled upstream connection. Enabling B
    // after A leaves only B enabled, and identity-source reflects B.
    let Some(_server) = spawn() else {
        eprintln!("SKIP: could not spawn identityd binary");
        return;
    };
    let client = reqwest::Client::new();
    assert!(wait_healthy(&client).await, "identityd did not become healthy");

    let fleet = fleet_token(&client).await;
    let tenant_d = "66666666-6666-4666-8666-666666666666";
    let realm_d: serde_json::Value = client
        .post(format!("{}/v1/tenants/{}/realm", base(), tenant_d))
        .bearer_auth(&fleet)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let realm = realm_d["id"].as_str().unwrap().to_string();

    let create_conn = |name: &'static str, enabled: bool| {
        let client = client.clone();
        let fleet = fleet.clone();
        let realm = realm.clone();
        async move {
            let conn: serde_json::Value = client
                .post(format!("{}/v1/realms/{}/connections", base(), realm))
                .bearer_auth(&fleet)
                .json(&serde_json::json!({
                    "name": name,
                    "kind": {
                        "protocol": "oidc",
                        "issuer_url": format!("https://{name}.example.com"),
                        "client_id": format!("{name}-client"),
                        "client_secret": "shh-SECRET",
                        "scopes": ["openid"],
                    },
                    "enabled": enabled,
                }))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            conn["id"].as_str().unwrap().to_string()
        }
    };

    // A enabled, B created disabled.
    let a_id = create_conn("conn-a", true).await;
    let b_id = create_conn("conn-b", false).await;

    let list_enabled = || {
        let client = client.clone();
        let fleet = fleet.clone();
        let realm = realm.clone();
        async move {
            let conns: serde_json::Value = client
                .get(format!("{}/v1/realms/{}/connections", base(), realm))
                .bearer_auth(&fleet)
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            conns
                .as_array()
                .unwrap()
                .iter()
                .filter(|c| c["enabled"].as_bool().unwrap_or(false))
                .map(|c| c["id"].as_str().unwrap().to_string())
                .collect::<Vec<_>>()
        }
    };

    assert_eq!(list_enabled().await, vec![a_id.clone()]);

    // Enable B via PATCH; A must auto-disable.
    let patched = client
        .patch(format!("{}/v1/realms/{}/connections/{}", base(), realm, b_id))
        .bearer_auth(&fleet)
        .json(&serde_json::json!({ "enabled": true }))
        .send()
        .await
        .unwrap();
    assert!(patched.status().is_success());

    assert_eq!(
        list_enabled().await,
        vec![b_id.clone()],
        "enabling B must disable A (at-most-one-enabled per realm)",
    );

    // identity-source must reflect B, not A.
    let src: serde_json::Value = client
        .get(format!("{}/v1/realms/{}/identity-source", base(), realm))
        .bearer_auth(&fleet)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(src["mode"], "oidc");
    assert_eq!(src["connection"]["id"], b_id);
    assert_eq!(src["connection"]["issuer_url"], "https://conn-b.example.com");
}

#[tokio::test]
#[file_serial(identityd_port)]
async fn no_token_is_unauthorized() {
    let Some(_server) = spawn() else {
        eprintln!("SKIP: could not spawn identityd binary");
        return;
    };
    let client = reqwest::Client::new();
    assert!(wait_healthy(&client).await, "identityd did not become healthy");

    let resp = client
        .get(format!("{}/v1/realms", base()))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
}
