// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

#![allow(clippy::unwrap_used, clippy::expect_used)]

//! End-to-end tests for the CN self-registration + approval flow
//! (`/v1/agent/register`, `/v1/agent/register/status`, and the
//! operator surface at `/v1/cns/*`).
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
    AgentStatusRequest, ApproveCnRequest, ClaimJobRequest, CnRole, CnState, LoginRequest,
    OpenAutoApproveRequest, RegisterCnRequest, SetCnRoleRequest,
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
            fleet_admin: false,
            capabilities: Default::default(),
            created_at: Utc::now(),
            tenant_id: None,
            federation: None,
        };
        store.create_user(user).await.unwrap();
        let auth = Arc::new(AuthService::new(JwtKey::generate()).unwrap());
        let audit = Arc::new(AuditService::new(Arc::new(MemChain::new())));
        let context = ApiContext::new(Arc::clone(&store), auth, audit)
            .without_in_process_provisioner()
            .without_saga_wait_for_agent();
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
            console_listen_port: None,
            console_tls_spki_sha256_hex: None,
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
            console_listen_port: None,
            console_tls_spki_sha256_hex: None,
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
            console_listen_port: None,
            console_tls_spki_sha256_hex: None,
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
    let api_key = status
        .api_key
        .expect("auto-approve must wire up a credential");

    let agent = test.bearer_client(&api_key);
    agent
        .agent_heartbeat()
        .send()
        .await
        .expect("auto-approved agent credential must authorize heartbeat");

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
async fn disabled_record_re_registers_back_to_pending() {
    // Disabling a CN is reversible by re-registration: the agent
    // restarting drops the record back to Pending (with a fresh claim
    // code), awaiting re-approval. This is the supported "re-enable
    // with fresh credentials" path; the disable event stays in the
    // audit chain.
    let test = TestServer::start().await;
    let anon = test.anonymous_client();
    let server_uuid = Uuid::new_v4();

    anon.agent_register()
        .body(RegisterCnRequest {
            server_uuid,
            hostname: "cn-x".to_string(),
            admin_ip: None,
            sysinfo: fixture_sysinfo(server_uuid, "cn-x"),
            console_listen_port: None,
            console_tls_spki_sha256_hex: None,
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

    // Re-register: should succeed and re-arm to Pending with a claim
    // code (not 409 / not Disabled / not Approved).
    let reg = anon
        .agent_register()
        .body(RegisterCnRequest {
            server_uuid,
            hostname: "cn-x".to_string(),
            admin_ip: None,
            sysinfo: fixture_sysinfo(server_uuid, "cn-x"),
            console_listen_port: None,
            console_tls_spki_sha256_hex: None,
        })
        .send()
        .await
        .unwrap()
        .into_inner();
    assert!(matches!(reg.state, CnState::Pending));
    assert!(reg.claim_code.is_some());

    // The record is listed under Pending again.
    let pending = session
        .list_system_cns_v1()
        .state(CnState::Pending)
        .send()
        .await
        .unwrap()
        .into_inner()
        .items;
    assert!(pending.iter().any(|c| c.server_uuid == server_uuid));

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
    // Anonymous on the legacy /v1/cn-approvals approve endpoint
    // still 403s - fleet-scoped, no capability gate.
    assert_eq!(approve_err.status().unwrap().as_u16(), 403);
    // Anonymous on /v1/system/cns hits the capability gate which
    // returns the cross-scope-deny 404 shape per RFD 00007 Locked
    // Decision #3 (indistinguishable from "no such path"). 404
    // here, not the legacy 403, is the right shape now.
    let list_err = anon.list_system_cns_v1().send().await.unwrap_err();
    let code = list_err.status().unwrap().as_u16();
    assert!(
        code == 404 || code == 401 || code == 403,
        "anonymous /v1/system/cns must reject; got {code}"
    );

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
            console_listen_port: None,
            console_tls_spki_sha256_hex: None,
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
            console_listen_port: None,
            console_tls_spki_sha256_hex: None,
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
        .list_system_cns_v1()
        .state(CnState::Pending)
        .send()
        .await
        .unwrap()
        .into_inner()
        .items;
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].server_uuid, p_uuid);

    let approved = session
        .list_system_cns_v1()
        .state(CnState::Approved)
        .send()
        .await
        .unwrap()
        .into_inner()
        .items;
    assert_eq!(approved.len(), 1);
    assert_eq!(approved[0].server_uuid, a_uuid);

    let all = session
        .list_system_cns_v1()
        .send()
        .await
        .unwrap()
        .into_inner()
        .items;
    assert_eq!(all.len(), 2);

    test.close().await;
}

#[tokio::test]
async fn root_can_set_cn_role_label() {
    let test = TestServer::start().await;
    let anon = test.anonymous_client();
    let root = root_session(&test).await;
    let server_uuid = Uuid::new_v4();
    let registered = anon
        .agent_register()
        .body(RegisterCnRequest {
            server_uuid,
            hostname: "edge-a".to_string(),
            admin_ip: Some("10.99.99.40".parse().unwrap()),
            sysinfo: fixture_sysinfo(server_uuid, "edge-a"),
            console_listen_port: None,
            console_tls_spki_sha256_hex: None,
        })
        .send()
        .await
        .unwrap()
        .into_inner();
    let approved = root
        .approve_cn()
        .body(ApproveCnRequest {
            code: registered.claim_code.unwrap(),
        })
        .send()
        .await
        .unwrap()
        .into_inner();
    assert_eq!(approved.role, CnRole::Tenant);

    let updated = root
        .set_cn_role()
        .server_uuid(server_uuid)
        .body(SetCnRoleRequest { role: CnRole::Edge })
        .send()
        .await
        .unwrap()
        .into_inner();
    assert_eq!(updated.role, CnRole::Edge);

    let shown = root
        .get_system_cn_v1()
        .cn_id(server_uuid)
        .send()
        .await
        .unwrap()
        .into_inner();
    assert_eq!(shown.role, CnRole::Edge);

    test.close().await;
}

#[tokio::test]
async fn bound_api_key_rejects_claim_for_other_cn() {
    let test = TestServer::start().await;
    let anon = test.anonymous_client();
    let session = root_session(&test).await;

    // Register + approve a CN; retrieve its bound api key.
    let cn_a = Uuid::new_v4();
    let registered = anon
        .agent_register()
        .body(RegisterCnRequest {
            server_uuid: cn_a,
            hostname: "cn-a".to_string(),
            admin_ip: None,
            sysinfo: fixture_sysinfo(cn_a, "cn-a"),
            console_listen_port: None,
            console_tls_spki_sha256_hex: None,
        })
        .send()
        .await
        .unwrap()
        .into_inner();
    let claim_code = registered.claim_code.unwrap();
    session
        .approve_cn()
        .body(ApproveCnRequest { code: claim_code })
        .send()
        .await
        .unwrap();
    let api_key = anon
        .agent_register_status()
        .poll_token(&registered.poll_token)
        .send()
        .await
        .unwrap()
        .into_inner()
        .api_key
        .expect("approval delivers a key");

    // Build a client carrying the bound CN-A key.
    let agent = test.bearer_client(&api_key);

    // Claim with claimed_by = OTHER uuid → 403 (binding mismatch).
    let other = Uuid::new_v4();
    let err = agent
        .agent_claim_job()
        .body(ClaimJobRequest {
            claimed_by: other.to_string(),
        })
        .send()
        .await
        .unwrap_err();
    assert_eq!(err.status().unwrap().as_u16(), 403);

    // Claim with claimed_by = bound CN-A → succeeds (queue empty,
    // returns null job, but no 403 from the binding check).
    let resp = agent
        .agent_claim_job()
        .body(ClaimJobRequest {
            claimed_by: cn_a.to_string(),
        })
        .send()
        .await
        .expect("matching claimed_by passes the binding check")
        .into_inner();
    assert!(resp.job.is_none(), "queue empty");

    // Non-uuid claimed_by → 403 (a bound key requires a uuid form).
    let err = agent
        .agent_claim_job()
        .body(ClaimJobRequest {
            claimed_by: "agent-A".to_string(),
        })
        .send()
        .await
        .unwrap_err();
    assert_eq!(err.status().unwrap().as_u16(), 403);

    test.close().await;
}

/// Helper used by Slice D tests: walk register → approve → retrieve
/// the bound API key for `cn_uuid`.
async fn register_and_approve(test: &TestServer, cn_uuid: Uuid, hostname: &str) -> String {
    let anon = test.anonymous_client();
    let registered = anon
        .agent_register()
        .body(RegisterCnRequest {
            server_uuid: cn_uuid,
            hostname: hostname.to_string(),
            admin_ip: None,
            sysinfo: fixture_sysinfo(cn_uuid, hostname),
            console_listen_port: None,
            console_tls_spki_sha256_hex: None,
        })
        .send()
        .await
        .unwrap()
        .into_inner();
    let session = root_session(test).await;
    session
        .approve_cn()
        .body(ApproveCnRequest {
            code: registered.claim_code.unwrap(),
        })
        .send()
        .await
        .unwrap();
    anon.agent_register_status()
        .poll_token(&registered.poll_token)
        .send()
        .await
        .unwrap()
        .into_inner()
        .api_key
        .expect("approval delivers a key")
}

#[tokio::test]
async fn agent_heartbeat_updates_last_seen() {
    let test = TestServer::start().await;
    let cn_uuid = Uuid::new_v4();
    let api_key = register_and_approve(&test, cn_uuid, "cn-hb").await;
    let agent = test.bearer_client(&api_key);

    // Sanity: last_seen is currently `Some(...)` from registration
    // (set on the register write). Capture it.
    let session = root_session(&test).await;
    let before = session
        .get_system_cn_v1()
        .cn_id(cn_uuid)
        .send()
        .await
        .unwrap()
        .into_inner()
        .last_seen
        .expect("registration sets last_seen");

    // Wait a moment so the post-heartbeat timestamp is strictly later.
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    agent.agent_heartbeat().send().await.unwrap();

    let after = session
        .get_system_cn_v1()
        .cn_id(cn_uuid)
        .send()
        .await
        .unwrap()
        .into_inner()
        .last_seen
        .unwrap();
    assert!(after > before, "heartbeat must bump last_seen");

    test.close().await;
}

#[tokio::test]
async fn agent_status_replaces_last_status_and_bumps_last_seen() {
    let test = TestServer::start().await;
    let cn_uuid = Uuid::new_v4();
    let api_key = register_and_approve(&test, cn_uuid, "cn-status").await;
    let agent = test.bearer_client(&api_key);

    let payload = serde_json::json!({
        "vms": {
            "11111111-1111-1111-1111-111111111111": {
                "state": "running",
                "brand": "joyent-minimal",
            }
        },
        "zpoolStatus": {
            "zones": { "bytes_available": 100, "bytes_used": 50 },
        },
        "meminfo": {
            "availrmem_bytes": 1234,
            "arcsize_bytes": 5678,
            "total_bytes": 9999,
        },
        "timestamp": "2026-05-03T12:00:00Z",
    });
    agent
        .agent_status()
        .body(AgentStatusRequest {
            payload: payload.clone(),
        })
        .send()
        .await
        .unwrap();

    let session = root_session(&test).await;
    let view = session
        .get_system_cn_v1()
        .cn_id(cn_uuid)
        .send()
        .await
        .unwrap()
        .into_inner();
    assert_eq!(view.last_status.as_ref().expect("status present"), &payload,);
    assert!(view.last_seen.is_some());

    // Posting a different payload replaces (not merges) the field.
    let next_payload = serde_json::json!({"vms": {}, "round": 2});
    agent
        .agent_status()
        .body(AgentStatusRequest {
            payload: next_payload.clone(),
        })
        .send()
        .await
        .unwrap();
    let view2 = session
        .get_system_cn_v1()
        .cn_id(cn_uuid)
        .send()
        .await
        .unwrap()
        .into_inner();
    assert_eq!(view2.last_status.as_ref().unwrap(), &next_payload);

    test.close().await;
}

#[tokio::test]
async fn unbound_agent_key_cannot_heartbeat_or_status() {
    use tritond_client::types::{ApiKeyScope, NewApiKey};
    let test = TestServer::start().await;
    let session = root_session(&test).await;

    // Operator mints a generic Agent-scoped key (NOT bound to any CN).
    // This is the legacy path; bound keys come only from approval.
    let secret = session
        .create_api_key()
        .body(NewApiKey {
            description: "unbound-agent".to_string(),
            scope: ApiKeyScope::Agent,
        })
        .send()
        .await
        .unwrap()
        .into_inner()
        .secret;
    let agent = test.bearer_client(&secret);

    // Heartbeat: 403 (no bound CN).
    let err = agent.agent_heartbeat().send().await.unwrap_err();
    assert_eq!(err.status().unwrap().as_u16(), 403);

    // Status: 403 too.
    let err = agent
        .agent_status()
        .body(AgentStatusRequest {
            payload: serde_json::json!({}),
        })
        .send()
        .await
        .unwrap_err();
    assert_eq!(err.status().unwrap().as_u16(), 403);

    test.close().await;
}

/// Fetch the published nic_tag inventory for a single CN via the
/// operator aggregate endpoint, or `None` if the CN has never
/// published.
async fn published_inventory_for(
    session: &Client,
    cn: Uuid,
) -> Option<tritond_client::types::CnNicTagInventory> {
    session
        .list_system_cn_nic_tags_v1()
        .send()
        .await
        .unwrap()
        .into_inner()
        .items
        .into_iter()
        .find(|inv| inv.cn == cn)
}

/// The nic_tag inventory publish is authenticated to the bound CN: an
/// unauthenticated caller — and an unbound (operator-minted) Agent key
/// — are both rejected, while the correctly-bound CN can publish its
/// own. Regression for the C-5 blocker: the inventory is a floating-IP
/// placement input, so it must never be settable anonymously.
#[tokio::test]
async fn nic_tag_inventory_publish_requires_bound_cn() {
    use tritond_client::types::{
        ApiKeyScope, NewApiKey, NewNicTag, NicTagInventoryReport, RegisterNicTagProvision,
    };
    let test = TestServer::start().await;
    let session = root_session(&test).await;

    // Operator registers the fleet-wide `external` nic_tag so a
    // reported name resolves.
    session
        .create_system_nic_tag_v1()
        .body(NewNicTag {
            name: "external".to_string(),
            description: None,
            mtu: 1500,
        })
        .send()
        .await
        .expect("create external nic_tag");

    let report = || NicTagInventoryReport {
        nic_tags: vec![RegisterNicTagProvision {
            name: "external".to_string(),
            physical_nic: "igb2".to_string(),
            vlan_id: 0,
            mtu: 1500,
        }],
    };

    // Anonymous: rejected (the action is not in the public-actions
    // list), and nothing is published.
    let anon = test.anonymous_client();
    let err = anon
        .agent_report_nic_tags()
        .body(report())
        .send()
        .await
        .unwrap_err();
    assert_eq!(err.status().unwrap().as_u16(), 403);

    // Unbound (operator-minted) Agent key: rejected — there is no CN
    // to attribute the write to.
    let unbound = session
        .create_api_key()
        .body(NewApiKey {
            description: "unbound-agent".to_string(),
            scope: ApiKeyScope::Agent,
        })
        .send()
        .await
        .unwrap()
        .into_inner()
        .secret;
    let err = test
        .bearer_client(&unbound)
        .agent_report_nic_tags()
        .body(report())
        .send()
        .await
        .unwrap_err();
    assert_eq!(err.status().unwrap().as_u16(), 403);

    let cn_a = Uuid::new_v4();
    assert!(
        published_inventory_for(&session, cn_a).await.is_none(),
        "no inventory should exist before any authenticated publish",
    );

    // Correctly-bound CN: publishes its own inventory.
    let key_a = register_and_approve(&test, cn_a, "cn-a").await;
    test.bearer_client(&key_a)
        .agent_report_nic_tags()
        .body(report())
        .send()
        .await
        .expect("a bound CN can publish its own inventory");
    let inv = published_inventory_for(&session, cn_a)
        .await
        .expect("CN-A inventory must be published");
    assert_eq!(inv.cn, cn_a);
    assert_eq!(inv.provides.len(), 1);
    assert_eq!(inv.provides[0].physical_nic, "igb2");

    test.close().await;
}

/// A CN authenticated as CN-A can never write CN-B's inventory: the
/// row is keyed by the credential's bound CN, not the request body, so
/// the body carries no CN to spoof. Publishing as CN-A leaves CN-B's
/// row untouched.
#[tokio::test]
async fn bound_cn_cannot_write_another_cns_inventory() {
    use tritond_client::types::{NewNicTag, NicTagInventoryReport, RegisterNicTagProvision};
    let test = TestServer::start().await;
    let session = root_session(&test).await;

    session
        .create_system_nic_tag_v1()
        .body(NewNicTag {
            name: "external".to_string(),
            description: None,
            mtu: 1500,
        })
        .send()
        .await
        .expect("create external nic_tag");

    let cn_a = Uuid::new_v4();
    let cn_b = Uuid::new_v4();
    let key_a = register_and_approve(&test, cn_a, "cn-a").await;
    let _key_b = register_and_approve(&test, cn_b, "cn-b").await;

    // CN-A publishes. The endpoint takes no CN in the body, so CN-A
    // has no way to name CN-B.
    test.bearer_client(&key_a)
        .agent_report_nic_tags()
        .body(NicTagInventoryReport {
            nic_tags: vec![RegisterNicTagProvision {
                name: "external".to_string(),
                physical_nic: "igb2".to_string(),
                vlan_id: 0,
                mtu: 1500,
            }],
        })
        .send()
        .await
        .expect("CN-A publishes its own inventory");

    // CN-A's row is set; CN-B's row is still absent.
    assert!(published_inventory_for(&session, cn_a).await.is_some());
    assert!(
        published_inventory_for(&session, cn_b).await.is_none(),
        "publishing as CN-A must not create or touch CN-B's inventory",
    );

    test.close().await;
}
