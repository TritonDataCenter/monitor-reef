// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

#![allow(clippy::unwrap_used, clippy::expect_used)]

//! End-to-end tests for the agent transport seam (`/v2/agent/*`).
//!
//! Strategy: stand up a tritond with the in-process stub
//! provisioner *disabled* so the test owns the queue. Mint an
//! API key with `ApiKeyScope::Agent`. Drive claim → complete via
//! the generated client and verify (a) empty queue returns
//! `{"job": null}`, (b) a Pending job is claimed and transitions
//! to `InProgress`, (c) `complete` lands the job in `Completed`,
//! (d) a re-claim returns null, and (e) keys with other scopes
//! cannot reach `/v2/agent/*` at all.

use std::sync::Arc;

use chrono::Utc;
use tritond::audit::AuditService;
use tritond::auth::AuthService;
use tritond::{ApiContext, start_server_with_context};
use tritond_audit::MemChain;
use tritond_auth::{JwtKey, RedactedString, hash_password};
use tritond_client::types::{
    ApiKeyScope, ClaimJobRequest, CompleteJobRequest, JobOutcome, JobStatus, LoginRequest,
    NewApiKey,
};
use tritond_store::{JobKind, MemStore, NewJob, Store, User};
use uuid::Uuid;

const ROOT_PASSWORD: &str = "correct horse battery staple";

struct TestServer {
    server: dropshot::HttpServer<ApiContext>,
    store: Arc<dyn Store>,
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
        let context = ApiContext::new(Arc::clone(&store), auth, audit)
            // The agent test owns the queue — disable the stub so
            // the real-agent path is the only consumer.
            .without_in_process_provisioner();
        let server = start_server_with_context("127.0.0.1:0", context)
            .await
            .unwrap();
        Self { server, store }
    }

    fn bind(&self) -> std::net::SocketAddr {
        self.server.local_addr()
    }

    fn anonymous_client(&self) -> tritond_client::Client {
        tritond_client::Client::new(&format!("http://{}", self.bind()))
    }

    fn bearer_client(&self, token: &str) -> tritond_client::Client {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {token}").parse().unwrap(),
        );
        let reqwest = reqwest::Client::builder()
            .default_headers(headers)
            .build()
            .unwrap();
        tritond_client::Client::new_with_client(&format!("http://{}", self.bind()), reqwest)
    }

    async fn close(self) {
        self.server.close().await.unwrap();
    }
}

/// Authenticate as root and mint an API key at the requested scope.
/// Returns the wire-form `tcadm_…` plaintext.
async fn mint_key(test: &TestServer, scope: ApiKeyScope) -> String {
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
    let session = test.bearer_client(&token.access_token);
    let created = session
        .create_api_key()
        .body(NewApiKey {
            description: format!("agent-test-{scope:?}"),
            scope,
        })
        .send()
        .await
        .unwrap()
        .into_inner();
    created.secret
}

#[tokio::test]
async fn agent_claim_returns_null_on_empty_queue() {
    let test = TestServer::start().await;
    let secret = mint_key(&test, ApiKeyScope::Agent).await;
    let client = test.bearer_client(&secret);

    let resp = client
        .agent_claim_job()
        .body(ClaimJobRequest {
            claimed_by: "agent-A".to_string(),
        })
        .send()
        .await
        .expect("Agent claim must succeed even on empty queue");
    assert!(
        resp.into_inner().job.is_none(),
        "empty queue should yield job=None",
    );

    test.close().await;
}

#[tokio::test]
async fn agent_claim_then_complete_drains_queue() {
    let test = TestServer::start().await;
    let secret = mint_key(&test, ApiKeyScope::Agent).await;
    let client = test.bearer_client(&secret);

    // Direct-store enqueue: skips the instance-create flow so the
    // test focuses on the agent surface alone.
    let instance_id = Uuid::new_v4();
    let queued = test
        .store
        .enqueue_job(NewJob {
            kind: JobKind::Provision { instance_id },
        })
        .await
        .unwrap();

    let claimed = client
        .agent_claim_job()
        .body(ClaimJobRequest {
            claimed_by: "agent-B".to_string(),
        })
        .send()
        .await
        .expect("claim should succeed")
        .into_inner()
        .job
        .expect("queue had a job; claim must return Some");
    assert_eq!(claimed.id, queued.id);
    assert!(matches!(claimed.status, JobStatus::InProgress));
    assert_eq!(claimed.claimed_by.as_deref(), Some("agent-B"));

    let completed = client
        .agent_complete_job()
        .job_id(claimed.id)
        .body(CompleteJobRequest {
            outcome: JobOutcome::Completed,
        })
        .send()
        .await
        .expect("complete should succeed")
        .into_inner();
    assert!(matches!(completed.status, JobStatus::Completed));

    // Queue is now drained.
    let next = client
        .agent_claim_job()
        .body(ClaimJobRequest {
            claimed_by: "agent-B".to_string(),
        })
        .send()
        .await
        .unwrap()
        .into_inner();
    assert!(next.job.is_none(), "queue should be empty after complete");

    test.close().await;
}

#[tokio::test]
async fn agent_complete_with_failure_records_reason() {
    let test = TestServer::start().await;
    let secret = mint_key(&test, ApiKeyScope::Agent).await;
    let client = test.bearer_client(&secret);

    let instance_id = Uuid::new_v4();
    test.store
        .enqueue_job(NewJob {
            kind: JobKind::Provision { instance_id },
        })
        .await
        .unwrap();

    let claimed = client
        .agent_claim_job()
        .body(ClaimJobRequest {
            claimed_by: "agent-fail".to_string(),
        })
        .send()
        .await
        .unwrap()
        .into_inner()
        .job
        .unwrap();

    let completed = client
        .agent_complete_job()
        .job_id(claimed.id)
        .body(CompleteJobRequest {
            outcome: JobOutcome::Failed("image not on host".to_string()),
        })
        .send()
        .await
        .expect("complete with failure should succeed")
        .into_inner();
    match completed.status {
        JobStatus::Failed(reason) => assert_eq!(reason, "image not on host"),
        other => panic!("expected Failed, got {other:?}"),
    }

    test.close().await;
}

#[tokio::test]
async fn read_only_scope_cannot_reach_agent_surface() {
    // ReadOnly scope is the highest read-allowed scope; if it can't
    // touch /v2/agent/* then neither can anything narrower.
    let test = TestServer::start().await;
    let secret = mint_key(&test, ApiKeyScope::ReadOnly).await;
    let client = test.bearer_client(&secret);

    let err = client
        .agent_claim_job()
        .body(ClaimJobRequest {
            claimed_by: "should-not-reach".to_string(),
        })
        .send()
        .await
        .expect_err("ReadOnly scope must not authorise agent_claim");
    let progenitor_client::Error::ErrorResponse(resp) = err else {
        panic!("expected ErrorResponse, got {err:?}");
    };
    assert_eq!(resp.status().as_u16(), 403);

    test.close().await;
}

#[tokio::test]
async fn anonymous_cannot_reach_agent_surface() {
    let test = TestServer::start().await;
    let anon = test.anonymous_client();
    let err = anon
        .agent_claim_job()
        .body(ClaimJobRequest {
            claimed_by: "anon".to_string(),
        })
        .send()
        .await
        .expect_err("anonymous principal must not authorise agent_claim");
    let progenitor_client::Error::ErrorResponse(resp) = err else {
        panic!("expected ErrorResponse, got {err:?}");
    };
    // Cedar denies anonymous on agent_* via default-deny — fleet
    // scope returns 403 (matching `forbidden_for`).
    assert_eq!(resp.status().as_u16(), 403);

    test.close().await;
}
