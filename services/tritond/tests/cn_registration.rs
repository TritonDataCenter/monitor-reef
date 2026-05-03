// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

#![allow(clippy::unwrap_used, clippy::expect_used)]

//! End-to-end tests for the CN self-registration + approval flow
//! (`/v2/agent/register`, `/v2/agent/register/status`, and the
//! operator surface at `/v2/cns/*`).
//!
//! Strategy: stand up a tritond, exercise the anonymous registration
//! endpoints from a no-auth client, drive the operator approval
//! through a root-bearer client, and verify the long-poll bridge
//! delivers the per-CN API key exactly once.

use std::sync::Arc;

use chrono::Utc;
use tritond::audit::AuditService;
use tritond::auth::AuthService;
use tritond::{ApiContext, start_server_with_context};
use tritond_audit::MemChain;
use tritond_auth::{JwtKey, RedactedString, hash_password};
use tritond_client::Client;
use tritond_client::types::{
    ApproveCnRequest, CnState, LoginRequest, OpenAutoApproveRequest, RegisterCnRequest,
};
use tritond_store::{MemStore, Store, User};
use uuid::Uuid;

const ROOT_PASSWORD: &str = "correct horse battery staple";

struct TestServer {
    server: dropshot::HttpServer<ApiContext>,
}

impl TestServer {
    async fn start() -> Self {
        let store: Arc<dyn Store> = Arc::new(MemStore::new());
        let user = User {
            id: Uuid::new_v4(),
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
        let auth = Arc::new(AuthService::new(JwtKey::generate()).unwrap());
        let audit = Arc::new(AuditService::new(Arc::new(MemChain::new())));
        let context =
            ApiContext::new(Arc::clone(&store), auth, audit).without_in_process_provisioner();
        let server = start_server_with_context("127.0.0.1:0", context)
            .await
            .unwrap();
        Self { server }
    }

    fn bind(&self) -> std::net::SocketAddr {
        self.server.local_addr()
    }

    fn anonymous_client(&self) -> Client {
        Client::new(&format!("http://{}", self.bind()))
    }

    fn bearer_client(&self, token: &str) -> Client {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {token}").parse().unwrap(),
        );
        let reqwest = reqwest::Client::builder()
            .default_headers(headers)
            .build()
            .unwrap();
        Client::new_with_client(&format!("http://{}", self.bind()), reqwest)
    }

    async fn close(self) {
        self.server.close().await.unwrap();
    }
}

async fn root_session(test: &TestServer) -> Client {
    let anon = test.anonymous_client();
    let token = anon
        .login()
        .body(LoginRequest {
            username: "root".to_string(),
            password: ROOT_PASSWORD.to_string(),
        })
        .send()
        .await
        .unwrap()
        .into_inner();
    test.bearer_client(&token.access_token)
}

fn fixture_sysinfo(uuid: Uuid, hostname: &str) -> serde_json::Value {
    serde_json::json!({
        "UUID": uuid.to_string(),
        "Hostname": hostname,
        "Boot Time": "1700000000",
    })
}

#[tokio::test]
async fn register_creates_pending_with_claim_code() {
    let test = TestServer::start().await;
    let anon = test.anonymous_client();
    let server_uuid = Uuid::new_v4();
    let resp = anon
        .agent_register()
        .body(RegisterCnRequest {
            server_uuid,
            hostname: "cn-a".to_string(),
            admin_ip: Some("10.99.99.7".parse().unwrap()),
            sysinfo: fixture_sysinfo(server_uuid, "cn-a"),
        })
        .send()
        .await
        .unwrap()
        .into_inner();

    assert_eq!(resp.server_uuid, server_uuid);
    assert!(matches!(resp.state, CnState::Pending));
    let claim = resp
        .claim_code
        .expect("Pending registration carries a claim code");
    // Display format: XXX-XXX (six chars + hyphen).
    assert_eq!(claim.len(), 7);
    assert_eq!(&claim[3..4], "-");
    assert_eq!(resp.poll_token.len(), 32);

    test.close().await;
}

#[tokio::test]
async fn approve_by_code_then_status_returns_api_key_once() {
    let test = TestServer::start().await;
    let anon = test.anonymous_client();
    let server_uuid = Uuid::new_v4();
    let registered = anon
        .agent_register()
        .body(RegisterCnRequest {
            server_uuid,
            hostname: "cn-a".to_string(),
            admin_ip: None,
            sysinfo: fixture_sysinfo(server_uuid, "cn-a"),
        })
        .send()
        .await
        .unwrap()
        .into_inner();
    let claim = registered.claim_code.unwrap();

    // Operator approves via the displayed code.
    let session = root_session(&test).await;
    let approved = session
        .approve_cn()
        .body(ApproveCnRequest { code: claim })
        .send()
        .await
        .expect("approve must succeed for a valid Pending claim code")
        .into_inner();
    assert!(matches!(approved.state, CnState::Approved));
    assert!(approved.bound_api_key_id.is_some());
    assert!(approved.claim_code.is_none());

    // Agent's first long-poll retrieves the API key.
    let status = anon
        .agent_register_status()
        .poll_token(&registered.poll_token)
        .send()
        .await
        .unwrap()
        .into_inner();
    assert!(matches!(status.state, CnState::Approved));
    let api_key = status
        .api_key
        .expect("first poll after approve carries the api key");
    assert!(api_key.starts_with("tcadm_"));

    // Second long-poll: state still Approved, but api_key is None
    // (one-shot delivery; if the agent loses the key, operator must
    // disable + re-approve).
    let status_again = anon
        .agent_register_status()
        .poll_token(&registered.poll_token)
        .send()
        .await
        .unwrap()
        .into_inner();
    assert!(matches!(status_again.state, CnState::Approved));
    assert!(status_again.api_key.is_none());

    test.close().await;
}

#[tokio::test]
async fn auto_approve_window_promotes_registration() {
    let test = TestServer::start().await;
    let anon = test.anonymous_client();

    // Operator opens a count-bounded window.
    let session = root_session(&test).await;
    session
        .open_auto_approve_window()
        .body(OpenAutoApproveRequest {
            duration_secs: 600,
            count: Some(5),
        })
        .send()
        .await
        .unwrap();

    let server_uuid = Uuid::new_v4();
    let registered = anon
        .agent_register()
        .body(RegisterCnRequest {
            server_uuid,
            hostname: "cn-bulk".to_string(),
            admin_ip: None,
            sysinfo: fixture_sysinfo(server_uuid, "cn-bulk"),
        })
        .send()
        .await
        .unwrap()
        .into_inner();
    assert!(matches!(registered.state, CnState::Approved));
    assert!(registered.claim_code.is_none());

    // Agent immediately gets its API key on the next long-poll.
    let status = anon
        .agent_register_status()
        .poll_token(&registered.poll_token)
        .send()
        .await
        .unwrap()
        .into_inner();
    assert!(matches!(status.state, CnState::Approved));
    assert!(
        status.api_key.is_some(),
        "auto-approve must wire up a credential"
    );

    test.close().await;
}

#[tokio::test]
async fn auto_approve_window_caps_to_24h() {
    let test = TestServer::start().await;
    let session = root_session(&test).await;

    // Request a year-long window; server clamps to 24h.
    let window = session
        .open_auto_approve_window()
        .body(OpenAutoApproveRequest {
            duration_secs: 365 * 24 * 60 * 60,
            count: None,
        })
        .send()
        .await
        .unwrap()
        .into_inner();
    let span = window.expires_at - window.opened_at;
    assert!(
        span.num_seconds() <= 24 * 60 * 60,
        "expected clamp to 24h, got {} seconds",
        span.num_seconds(),
    );

    test.close().await;
}

#[tokio::test]
async fn invalid_claim_code_returns_400() {
    let test = TestServer::start().await;
    let session = root_session(&test).await;
    let err = session
        .approve_cn()
        .body(ApproveCnRequest {
            // Contains "L" — not in Crockford alphabet.
            code: "ABCDEL".to_string(),
        })
        .send()
        .await
        .unwrap_err();
    let status = err.status().unwrap();
    assert_eq!(status.as_u16(), 400);

    test.close().await;
}

#[tokio::test]
async fn unknown_claim_code_returns_404() {
    let test = TestServer::start().await;
    let session = root_session(&test).await;
    let err = session
        .approve_cn()
        .body(ApproveCnRequest {
            code: "K7P-9X2".to_string(),
        })
        .send()
        .await
        .unwrap_err();
    let status = err.status().unwrap();
    assert_eq!(status.as_u16(), 404);

    test.close().await;
}

#[tokio::test]
async fn unknown_poll_token_returns_404() {
    let test = TestServer::start().await;
    let anon = test.anonymous_client();
    let err = anon
        .agent_register_status()
        .poll_token("0".repeat(32))
        .send()
        .await
        .unwrap_err();
    let status = err.status().unwrap();
    assert_eq!(status.as_u16(), 404);

    test.close().await;
}

#[tokio::test]
async fn disabled_record_blocks_re_registration() {
    let test = TestServer::start().await;
    let anon = test.anonymous_client();
    let server_uuid = Uuid::new_v4();

    anon.agent_register()
        .body(RegisterCnRequest {
            server_uuid,
            hostname: "cn-x".to_string(),
            admin_ip: None,
            sysinfo: fixture_sysinfo(server_uuid, "cn-x"),
        })
        .send()
        .await
        .unwrap();

    let session = root_session(&test).await;
    session
        .disable_cn()
        .server_uuid(server_uuid)
        .send()
        .await
        .unwrap();

    let err = anon
        .agent_register()
        .body(RegisterCnRequest {
            server_uuid,
            hostname: "cn-x".to_string(),
            admin_ip: None,
            sysinfo: fixture_sysinfo(server_uuid, "cn-x"),
        })
        .send()
        .await
        .unwrap_err();
    let status = err.status().unwrap();
    // Store-level Conflict surfaces as 409.
    assert_eq!(status.as_u16(), 409);

    test.close().await;
}

#[tokio::test]
async fn anonymous_cannot_approve_or_list() {
    let test = TestServer::start().await;
    let anon = test.anonymous_client();
    let approve_err = anon
        .approve_cn()
        .body(ApproveCnRequest {
            code: "K7P-9X2".to_string(),
        })
        .send()
        .await
        .unwrap_err();
    // CN endpoints are FLEET-scoped; the cross-tenant 404 invariant
    // (which masks Cedar deny so silos can't be enumerated) does not
    // apply. Anonymous on a fleet endpoint gets a real 403.
    assert_eq!(approve_err.status().unwrap().as_u16(), 403);
    let list_err = anon.list_cns().send().await.unwrap_err();
    assert_eq!(list_err.status().unwrap().as_u16(), 403);

    test.close().await;
}

#[tokio::test]
async fn list_cns_filters_by_state() {
    let test = TestServer::start().await;
    let anon = test.anonymous_client();
    let session = root_session(&test).await;

    // Register two; approve one.
    let p_uuid = Uuid::new_v4();
    let a_uuid = Uuid::new_v4();
    anon.agent_register()
        .body(RegisterCnRequest {
            server_uuid: p_uuid,
            hostname: "p".into(),
            admin_ip: None,
            sysinfo: fixture_sysinfo(p_uuid, "p"),
        })
        .send()
        .await
        .unwrap();
    let a_reg = anon
        .agent_register()
        .body(RegisterCnRequest {
            server_uuid: a_uuid,
            hostname: "a".into(),
            admin_ip: None,
            sysinfo: fixture_sysinfo(a_uuid, "a"),
        })
        .send()
        .await
        .unwrap()
        .into_inner();
    session
        .approve_cn()
        .body(ApproveCnRequest {
            code: a_reg.claim_code.unwrap(),
        })
        .send()
        .await
        .unwrap();

    let pending = session
        .list_cns()
        .state(CnState::Pending)
        .send()
        .await
        .unwrap()
        .into_inner();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].server_uuid, p_uuid);

    let approved = session
        .list_cns()
        .state(CnState::Approved)
        .send()
        .await
        .unwrap()
        .into_inner();
    assert_eq!(approved.len(), 1);
    assert_eq!(approved[0].server_uuid, a_uuid);

    let all = session.list_cns().send().await.unwrap().into_inner();
    assert_eq!(all.len(), 2);

    test.close().await;
}
