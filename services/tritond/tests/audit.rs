// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

#![allow(clippy::unwrap_used, clippy::expect_used)]

//! End-to-end tests for the audit-log surface (`/v1/audit/events`,
//! `/v1/audit/verify`) and the emission rules driven by the request
//! lifecycle.

use std::sync::Arc;

use chrono::Utc;
use tritond::audit::AuditService;
use tritond::auth::AuthService;
use tritond::{ApiContext, start_server_with_context};
use tritond_audit::MemChain;
use tritond_auth::{JwtKey, RedactedString, hash_password, mint_access};
use tritond_client::types::NewSilo;
use tritond_store::{MemStore, Store, User};
use uuid::Uuid;

const ROOT_PASSWORD: &str = "audit-test-pass";

struct TestServer {
    server: dropshot::HttpServer<ApiContext>,
    bearer: String,
    /// Direct handle on the chain so we can inspect appended events
    /// without going through the HTTP surface.
    chain: Arc<dyn tritond_audit::Chain>,
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
            fleet_admin: false,
            capabilities: Default::default(),
            created_at: Utc::now(),
            tenant_id: None,
            federation: None,
        };
        store.create_user(user).await.unwrap();

        let jwt_key = JwtKey::generate();
        let (token, _) = mint_access(&jwt_key, user_id).unwrap();
        let auth = Arc::new(AuthService::new(jwt_key).unwrap());
        let chain: Arc<dyn tritond_audit::Chain> = Arc::new(MemChain::new());
        let audit = Arc::new(AuditService::new(chain.clone()));
        let context = ApiContext::new(store, auth, audit);
        let server = start_server_with_context("127.0.0.1:0", context)
            .await
            .unwrap();
        Self {
            server,
            bearer: token,
            chain,
        }
    }

    fn bind(&self) -> std::net::SocketAddr {
        self.server.local_addr()
    }

    fn authed_client(&self) -> tritond_client::Client {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {}", self.bearer).parse().unwrap(),
        );
        let reqwest = reqwest::Client::builder()
            .default_headers(headers)
            .build()
            .unwrap();
        tritond_client::Client::new_with_client(&format!("http://{}", self.bind()), reqwest)
    }

    fn anonymous_client(&self) -> tritond_client::Client {
        tritond_client::Client::new(&format!("http://{}", self.bind()))
    }

    async fn close(self) {
        self.server.close().await.unwrap();
    }
}

/// Helper: snapshot the entire chain by iterating get(seq) from 0 to head.
/// `Chain::list` uses exclusive `after_seq` semantics (next-page friendly)
/// so it can't return seq=0; tests want a full inclusive view.
async fn dump_all(chain: &Arc<dyn tritond_audit::Chain>) -> Vec<tritond_audit::AuditEvent> {
    let head = chain.head().await.unwrap();
    let Some(head) = head else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity((head.seq + 1) as usize);
    for s in 0..=head.seq {
        out.push(chain.get(s).await.unwrap());
    }
    out
}

#[tokio::test]
async fn create_silo_emits_decision_plus_mutation_events() {
    let test = TestServer::start().await;
    let client = test.authed_client();

    client
        .create_silo()
        .body(NewSilo {
            name: "audited".to_string(),
            description: None,
        })
        .send()
        .await
        .unwrap();

    // One event for the Cedar Allow, one for the create_silo mutation
    // outcome. Both carry the same action name; their decision/outcome
    // shape distinguishes them.
    let events = dump_all(&test.chain).await;
    assert!(
        events.len() >= 2,
        "expected ≥2 events, got {} ({:#?})",
        events.len(),
        events
    );
    let actions: Vec<&str> = events.iter().map(|e| e.action.as_str()).collect();
    assert!(actions.contains(&"create_silo"));

    test.close().await;
}

#[tokio::test]
async fn anonymous_deny_does_not_pollute_chain() {
    let test = TestServer::start().await;
    let client = test.anonymous_client();

    // Anonymous probe of /v1/silos → 403; per design, no event.
    let _err = client
        .create_silo()
        .body(NewSilo {
            name: "intruder".to_string(),
            description: None,
        })
        .send()
        .await
        .expect_err("anonymous should be forbidden");

    // Health is anonymous-allowed, so it produces an Allow event.
    client.health().send().await.unwrap();

    let events = dump_all(&test.chain).await;
    // The only event should be the health check; the anonymous deny
    // must not have logged.
    assert_eq!(events.len(), 1, "got {events:#?}");
    assert_eq!(events[0].action, "health");

    test.close().await;
}

#[tokio::test]
async fn login_failure_records_unauthenticated_event() {
    let test = TestServer::start().await;
    let anon = test.anonymous_client();

    let _err = anon
        .login()
        .body(tritond_client::types::LoginRequest {
            username: "root".to_string(),
            password: "definitely-wrong".to_string(),
        })
        .send()
        .await
        .expect_err("bad password should fail");

    let events = dump_all(&test.chain).await;
    // Expected: Cedar Allow event for `login` (it's a public action),
    // plus the auth-event for the failure with username="root".
    assert!(
        events
            .iter()
            .any(|e| matches!(&e.outcome, tritond_audit::Outcome::Unauthenticated { .. }))
    );
    assert!(
        events
            .iter()
            .any(|e| e.action == "login" && e.payload["username"] == "root")
    );

    test.close().await;
}

#[tokio::test]
async fn audit_endpoints_round_trip_via_http() {
    let test = TestServer::start().await;
    let client = test.authed_client();

    // Generate a few events first.
    for name in ["a", "b", "c"] {
        client
            .create_silo()
            .body(NewSilo {
                name: name.to_string(),
                description: None,
            })
            .send()
            .await
            .unwrap();
    }

    // List
    let listed = client
        .list_audit_events()
        .send()
        .await
        .unwrap()
        .into_inner();
    assert!(!listed.events.is_empty());
    assert!(listed.head.is_some());

    // Fetch one
    let first = &listed.events[0];
    let one = client
        .get_audit_event()
        .seq(first.seq)
        .send()
        .await
        .unwrap()
        .into_inner();
    assert_eq!(one.seq, first.seq);

    // Verify clean
    let v = client
        .verify_audit_chain()
        .send()
        .await
        .unwrap()
        .into_inner();
    use tritond_client::types::VerifyOutcome;
    assert!(matches!(v.outcome, VerifyOutcome::Ok { .. }));

    test.close().await;
}

#[tokio::test]
async fn anonymous_cannot_access_audit_endpoints() {
    let test = TestServer::start().await;
    let anon = test.anonymous_client();

    let err = anon
        .list_audit_events()
        .send()
        .await
        .expect_err("anon list should be forbidden");
    let progenitor_client::Error::ErrorResponse(response) = err else {
        panic!("expected ErrorResponse, got {err:?}");
    };
    assert_eq!(response.status().as_u16(), 403);

    test.close().await;
}
