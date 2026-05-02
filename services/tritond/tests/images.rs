// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

#![allow(clippy::unwrap_used, clippy::expect_used)]

//! End-to-end tests for the `/v2/silos/{silo_id}/images` surface.
//!
//! Mirrors `tests/projects.rs` for the auth fixture and exercises:
//!
//! * Server-side sha256 format validation (must be 64 lowercase hex).
//! * Server-side size_bytes > 0 validation.
//! * Cross-silo 404 on get.
//! * Name uniqueness within silo (409).
//! * Same name in different silos OK.

use std::net::SocketAddr;
use std::sync::Arc;

use chrono::Utc;
use tritond::audit::AuditService;
use tritond::auth::AuthService;
use tritond::{ApiContext, start_server_with_context};
use tritond_audit::MemChain;
use tritond_auth::{JwtKey, RedactedString, hash_password, mint_access};
use tritond_client::types::{NewImage, NewSilo};
use tritond_store::{MemStore, Store, User};
use uuid::Uuid;

const ROOT_PASSWORD: &str = "images-test";

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

fn assert_status(err: progenitor_client::Error<tritond_client::types::Error>, want: u16) {
    let progenitor_client::Error::ErrorResponse(response) = err else {
        panic!("expected ErrorResponse, got {err:?}");
    };
    assert_eq!(response.status().as_u16(), want);
}

async fn make_silo(root: &tritond_client::Client) -> Uuid {
    root.create_silo()
        .body(NewSilo {
            name: format!("silo-{}", Uuid::new_v4()),
            description: None,
        })
        .send()
        .await
        .unwrap()
        .into_inner()
        .id
}

fn standard_image(name: &str) -> NewImage {
    NewImage {
        name: name.to_string(),
        description: None,
        os: "linux".to_string(),
        version: "ubuntu-22.04".to_string(),
        size_bytes: 1_000_000_000,
        sha256: "0".repeat(64),
        source_url: Some("mantafs://images/test".to_string()),
        id: None,
    }
}

// ---------- tests ----------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn image_round_trip_within_silo() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let silo_id = make_silo(&root).await;

    let img = root
        .create_silo_image()
        .silo_id(silo_id)
        .body(standard_image("ubuntu-base"))
        .send()
        .await
        .unwrap()
        .into_inner();
    assert_eq!(img.silo_id, silo_id);
    assert_eq!(img.size_bytes, 1_000_000_000);

    let listed = root
        .list_silo_images()
        .silo_id(silo_id)
        .send()
        .await
        .unwrap()
        .into_inner();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, img.id);

    root.delete_silo_image()
        .silo_id(silo_id)
        .image_id(img.id)
        .send()
        .await
        .unwrap();

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn invalid_sha256_returns_400() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let silo_id = make_silo(&root).await;

    // Wrong length.
    let mut req = standard_image("short-hash");
    req.sha256 = "deadbeef".to_string();
    let err = root
        .create_silo_image()
        .silo_id(silo_id)
        .body(req)
        .send()
        .await
        .expect_err("short sha256 must 400");
    assert_status(err, 400);

    // Right length, wrong charset (uppercase).
    let mut req = standard_image("upper-hash");
    req.sha256 = "A".repeat(64);
    let err = root
        .create_silo_image()
        .silo_id(silo_id)
        .body(req)
        .send()
        .await
        .expect_err("uppercase sha256 must 400");
    assert_status(err, 400);

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn zero_size_image_returns_400() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let silo_id = make_silo(&root).await;

    let mut req = standard_image("empty");
    req.size_bytes = 0;
    let err = root
        .create_silo_image()
        .silo_id(silo_id)
        .body(req)
        .send()
        .await
        .expect_err("zero-byte image must 400");
    assert_status(err, 400);

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn duplicate_name_within_silo_returns_409() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let silo_id = make_silo(&root).await;

    root.create_silo_image()
        .silo_id(silo_id)
        .body(standard_image("ubuntu-base"))
        .send()
        .await
        .unwrap();
    let err = root
        .create_silo_image()
        .silo_id(silo_id)
        .body(standard_image("ubuntu-base"))
        .send()
        .await
        .expect_err("duplicate name must 409");
    assert_status(err, 409);

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn same_name_in_different_silos_does_not_conflict() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let silo_a = make_silo(&root).await;
    let silo_b = make_silo(&root).await;

    root.create_silo_image()
        .silo_id(silo_a)
        .body(standard_image("ubuntu-base"))
        .send()
        .await
        .unwrap();
    root.create_silo_image()
        .silo_id(silo_b)
        .body(standard_image("ubuntu-base"))
        .send()
        .await
        .expect("same name in a different silo must succeed");

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cross_silo_get_returns_404() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let silo_a = make_silo(&root).await;
    let silo_b = make_silo(&root).await;

    let img = root
        .create_silo_image()
        .silo_id(silo_a)
        .body(standard_image("ubuntu-base"))
        .send()
        .await
        .unwrap()
        .into_inner();

    let err = root
        .get_silo_image()
        .silo_id(silo_b)
        .image_id(img.id)
        .send()
        .await
        .expect_err("cross-silo get must 404");
    assert_status(err, 404);

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn anonymous_cannot_reach_image_endpoints() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let silo_id = make_silo(&root).await;

    let anon = test.anonymous_client();
    let err = anon
        .list_silo_images()
        .silo_id(silo_id)
        .send()
        .await
        .expect_err("anonymous list must be denied");
    assert_status(err, 404);

    test.close().await;
}
