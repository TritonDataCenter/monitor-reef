// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

#![allow(clippy::unwrap_used, clippy::expect_used)]

//! End-to-end tests for the `/v2/silos` surface, exercised through
//! the auth gate by minting a root-operator JWT and presenting it as
//! a `Bearer` token.

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

struct TestServer {
    server: dropshot::HttpServer<ApiContext>,
    bearer: String,
}

impl TestServer {
    /// Spin up tritond on an ephemeral port with a single root
    /// operator already in the store and a freshly-minted access
    /// token bound to that user.
    async fn start() -> Self {
        let store: Arc<dyn Store> = Arc::new(MemStore::new());
        let user_id = Uuid::new_v4();
        let user = User {
            id: user_id,
            username: "root".to_string(),
            password_hash: hash_password(&RedactedString::from("ignored-by-token-tests"))
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
        let audit = Arc::new(AuditService::new(Arc::new(MemChain::new())));
        let context = ApiContext::new(store, auth, audit);
        let server = start_server_with_context("127.0.0.1:0", context)
            .await
            .expect("server should start on ephemeral port");
        Self {
            server,
            bearer: token,
        }
    }

    fn bind(&self) -> std::net::SocketAddr {
        self.server.local_addr()
    }

    /// Build a generated `tritond_client::Client` that sends our
    /// access token on every call.
    fn authed_client(&self) -> tritond_client::Client {
        let mut headers = reqwest::header::HeaderMap::new();
        let value = format!("Bearer {}", self.bearer)
            .parse()
            .expect("auth header must be valid");
        headers.insert(reqwest::header::AUTHORIZATION, value);
        let reqwest = reqwest::Client::builder()
            .default_headers(headers)
            .build()
            .expect("reqwest client builds");
        tritond_client::Client::new_with_client(&format!("http://{}", self.bind()), reqwest)
    }

    fn anonymous_client(&self) -> tritond_client::Client {
        tritond_client::Client::new(&format!("http://{}", self.bind()))
    }

    async fn close(self) {
        self.server
            .close()
            .await
            .expect("server should close cleanly");
    }
}

#[tokio::test]
async fn create_then_get_round_trips_via_generated_client() {
    let test = TestServer::start().await;
    let client = test.authed_client();

    let created = client
        .create_silo()
        .body(NewSilo {
            name: "operator".to_string(),
            description: Some("the bootstrap silo".to_string()),
        })
        .send()
        .await
        .expect("create_silo should succeed")
        .into_inner();

    assert_eq!(created.name, "operator");
    assert_eq!(created.description, "the bootstrap silo");

    let fetched = client
        .get_silo()
        .silo_id(created.id)
        .send()
        .await
        .expect("get_silo should succeed")
        .into_inner();

    assert_eq!(fetched.id, created.id);
    assert_eq!(fetched.name, "operator");
    assert_eq!(fetched.created_at, created.created_at);

    test.close().await;
}

#[tokio::test]
async fn duplicate_name_returns_409() {
    let test = TestServer::start().await;
    let client = test.authed_client();

    client
        .create_silo()
        .body(NewSilo {
            name: "ops".to_string(),
            description: None,
        })
        .send()
        .await
        .expect("first create should succeed");

    let err = client
        .create_silo()
        .body(NewSilo {
            name: "ops".to_string(),
            description: None,
        })
        .send()
        .await
        .expect_err("second create should fail");

    let progenitor_client::Error::ErrorResponse(response) = err else {
        panic!("expected ErrorResponse, got {err:?}");
    };
    assert_eq!(response.status().as_u16(), 409);

    test.close().await;
}

#[tokio::test]
async fn missing_silo_returns_404() {
    let test = TestServer::start().await;
    let client = test.authed_client();

    let err = client
        .get_silo()
        .silo_id(Uuid::new_v4())
        .send()
        .await
        .expect_err("get_silo on unknown id should fail");

    let progenitor_client::Error::ErrorResponse(response) = err else {
        panic!("expected ErrorResponse, got {err:?}");
    };
    assert_eq!(response.status().as_u16(), 404);

    test.close().await;
}

#[tokio::test]
async fn anonymous_create_silo_is_forbidden() {
    let test = TestServer::start().await;
    let client = test.anonymous_client();

    let err = client
        .create_silo()
        .body(NewSilo {
            name: "intruder".to_string(),
            description: None,
        })
        .send()
        .await
        .expect_err("anonymous create should fail");

    let progenitor_client::Error::ErrorResponse(response) = err else {
        panic!("expected ErrorResponse, got {err:?}");
    };
    assert_eq!(response.status().as_u16(), 403);

    test.close().await;
}
