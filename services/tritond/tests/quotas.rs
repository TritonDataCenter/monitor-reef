// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

#![allow(clippy::unwrap_used, clippy::expect_used)]

//! End-to-end tests for the
//! `/v2/silos/{silo_id}/projects/{project_id}/quota` surface.
//!
//! Exercises the singleton-per-project semantics:
//!
//! * GET on an unset quota → 404 (no record means "unlimited").
//! * PUT replaces any existing record.
//! * DELETE on an unset quota → 404 (idempotent? no — explicit
//!   "nothing to remove" surfaces).
//! * Cross-silo PUT/GET/DELETE → 404 (defence-in-depth on
//!   project_id, since the URL still has a silo_id).
//! * Cross-project: each project's quota is independent; setting
//!   one doesn't affect the other.

use std::net::SocketAddr;
use std::sync::Arc;

use chrono::Utc;
use tritond::audit::AuditService;
use tritond::auth::AuthService;
use tritond::{ApiContext, start_server_with_context};
use tritond_audit::MemChain;
use tritond_auth::{JwtKey, RedactedString, hash_password, mint_access};
use tritond_client::types::{NewProject, NewQuota, NewSilo};
use tritond_store::{MemStore, Store, User};
use uuid::Uuid;

const ROOT_PASSWORD: &str = "quotas-test";

// ---------- test fixture ----------

struct TestServer {
    server: dropshot::HttpServer<ApiContext>,
    root_bearer: String,
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
        let (token, _) = mint_access(&jwt_key, user_id).unwrap();
        let auth = Arc::new(AuthService::new(jwt_key).unwrap());
        let audit = Arc::new(AuditService::new(Arc::new(MemChain::new())));
        let context = ApiContext::new(store, auth, audit);
        let server = start_server_with_context("127.0.0.1:0", context)
            .await
            .unwrap();
        Self {
            server,
            root_bearer: token,
        }
    }

    fn bind(&self) -> SocketAddr {
        self.server.local_addr()
    }

    fn root_client(&self) -> tritond_client::Client {
        self.bearer_client(&self.root_bearer)
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

fn assert_status(err: progenitor_client::Error<tritond_client::types::Error>, want: u16) {
    let progenitor_client::Error::ErrorResponse(response) = err else {
        panic!("expected ErrorResponse, got {err:?}");
    };
    assert_eq!(response.status().as_u16(), want);
}

async fn make_silo_and_project(root: &tritond_client::Client) -> (Uuid, Uuid) {
    let silo = root
        .create_silo()
        .body(NewSilo {
            name: format!("silo-{}", Uuid::new_v4()),
            description: None,
        })
        .send()
        .await
        .unwrap()
        .into_inner();
    let project = root
        .create_silo_project()
        .silo_id(silo.id)
        .body(NewProject {
            name: "p".to_string(),
            description: None,
        })
        .send()
        .await
        .unwrap()
        .into_inner();
    (silo.id, project.id)
}

fn standard_quota() -> NewQuota {
    NewQuota {
        cpu_limit: 16,
        memory_bytes: 32 * 1024 * 1024 * 1024,
        disk_bytes: 1024 * 1024 * 1024 * 1024,
        instance_limit: 8,
    }
}

// ---------- tests ----------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn quota_round_trip_within_project() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let (silo_id, project_id) = make_silo_and_project(&root).await;

    // Initial GET → 404 (no quota set).
    let err = root
        .get_project_quota()
        .silo_id(silo_id)
        .project_id(project_id)
        .send()
        .await
        .expect_err("unset quota must 404");
    assert_status(err, 404);

    let quota = root
        .put_project_quota()
        .silo_id(silo_id)
        .project_id(project_id)
        .body(standard_quota())
        .send()
        .await
        .unwrap()
        .into_inner();
    assert_eq!(quota.cpu_limit, 16);

    let read = root
        .get_project_quota()
        .silo_id(silo_id)
        .project_id(project_id)
        .send()
        .await
        .unwrap()
        .into_inner();
    assert_eq!(read.cpu_limit, 16);

    // Re-PUT replaces.
    let mut req = standard_quota();
    req.cpu_limit = 32;
    let updated = root
        .put_project_quota()
        .silo_id(silo_id)
        .project_id(project_id)
        .body(req)
        .send()
        .await
        .unwrap()
        .into_inner();
    assert_eq!(updated.cpu_limit, 32);

    root.delete_project_quota()
        .silo_id(silo_id)
        .project_id(project_id)
        .send()
        .await
        .unwrap();
    let err = root
        .get_project_quota()
        .silo_id(silo_id)
        .project_id(project_id)
        .send()
        .await
        .expect_err("post-delete is 404");
    assert_status(err, 404);

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn delete_unset_quota_returns_404() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let (silo_id, project_id) = make_silo_and_project(&root).await;

    let err = root
        .delete_project_quota()
        .silo_id(silo_id)
        .project_id(project_id)
        .send()
        .await
        .expect_err("delete with no quota must 404");
    assert_status(err, 404);

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cross_silo_quota_put_returns_404() {
    // Project lives in silo_a; PUT against silo_b's path → 404.
    let test = TestServer::start().await;
    let root = test.root_client();
    let (silo_a, project_id) = make_silo_and_project(&root).await;
    let silo_b = root
        .create_silo()
        .body(NewSilo {
            name: "other".to_string(),
            description: None,
        })
        .send()
        .await
        .unwrap()
        .into_inner();
    let _ = silo_a;

    let err = root
        .put_project_quota()
        .silo_id(silo_b.id)
        .project_id(project_id)
        .body(standard_quota())
        .send()
        .await
        .expect_err("cross-silo PUT must 404");
    assert_status(err, 404);

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn quota_in_unknown_project_returns_404() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let silo = root
        .create_silo()
        .body(NewSilo {
            name: "x".to_string(),
            description: None,
        })
        .send()
        .await
        .unwrap()
        .into_inner();

    let err = root
        .put_project_quota()
        .silo_id(silo.id)
        .project_id(Uuid::new_v4())
        .body(standard_quota())
        .send()
        .await
        .expect_err("unknown project must 404");
    assert_status(err, 404);

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn quotas_are_independent_per_project() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let silo = root
        .create_silo()
        .body(NewSilo {
            name: "x".to_string(),
            description: None,
        })
        .send()
        .await
        .unwrap()
        .into_inner();
    let proj_a = root
        .create_silo_project()
        .silo_id(silo.id)
        .body(NewProject {
            name: "a".to_string(),
            description: None,
        })
        .send()
        .await
        .unwrap()
        .into_inner();
    let proj_b = root
        .create_silo_project()
        .silo_id(silo.id)
        .body(NewProject {
            name: "b".to_string(),
            description: None,
        })
        .send()
        .await
        .unwrap()
        .into_inner();

    let mut req_a = standard_quota();
    req_a.cpu_limit = 8;
    root.put_project_quota()
        .silo_id(silo.id)
        .project_id(proj_a.id)
        .body(req_a)
        .send()
        .await
        .unwrap();

    // proj_b still has no quota.
    let err = root
        .get_project_quota()
        .silo_id(silo.id)
        .project_id(proj_b.id)
        .send()
        .await
        .expect_err("proj_b quota must still be unset");
    assert_status(err, 404);

    // proj_a's quota is the value we set.
    let read = root
        .get_project_quota()
        .silo_id(silo.id)
        .project_id(proj_a.id)
        .send()
        .await
        .unwrap()
        .into_inner();
    assert_eq!(read.cpu_limit, 8);

    test.close().await;
}
