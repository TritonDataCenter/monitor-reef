// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

#![allow(clippy::unwrap_used, clippy::expect_used)]

//! End-to-end tests for the operator-auth surface
//! (`/v2/auth/login`, `/v2/auth/refresh`, `/v2/auth/api-keys`).

use std::sync::Arc;

use chrono::Utc;
use tritond::audit::AuditService;
use tritond::auth::AuthService;
use tritond::{ApiContext, start_server_with_context};
use tritond_audit::MemChain;
use tritond_auth::{JwtKey, RedactedString, hash_password};
use tritond_client::types::{LoginRequest, NewApiKey, NewSilo, RefreshRequest};
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
        };
        store.create_user(user).await.unwrap();
        let auth = Arc::new(AuthService::new(JwtKey::generate()).unwrap());
        let audit = Arc::new(AuditService::new(Arc::new(MemChain::new())));
        let context = ApiContext::new(store, auth, audit);
        let server = start_server_with_context("127.0.0.1:0", context)
            .await
            .unwrap();
        Self { server }
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

#[tokio::test]
async fn login_with_correct_password_returns_token_pair_and_unlocks_silos() {
    let test = TestServer::start().await;
    let anon = test.anonymous_client();

    let response = anon
        .login()
        .body(LoginRequest {
            username: "root".to_string(),
            password: ROOT_PASSWORD.to_string(),
        })
        .send()
        .await
        .expect("login should succeed")
        .into_inner();

    assert!(!response.access_token.is_empty());
    assert!(!response.refresh_token.is_empty());
    assert!(response.access_expires_at > response.refresh_expires_at - chrono::Duration::hours(24));

    // Use the access token to create a silo — proves the issued JWT
    // is accepted by the auth middleware end-to-end.
    let authed = test.bearer_client(&response.access_token);
    let silo = authed
        .create_silo()
        .body(NewSilo {
            name: "after-login".to_string(),
            description: None,
        })
        .send()
        .await
        .expect("authed create_silo should succeed")
        .into_inner();
    assert_eq!(silo.name, "after-login");

    test.close().await;
}

#[tokio::test]
async fn login_with_wrong_password_returns_401() {
    let test = TestServer::start().await;
    let anon = test.anonymous_client();

    let err = anon
        .login()
        .body(LoginRequest {
            username: "root".to_string(),
            password: "definitely-not-the-password".to_string(),
        })
        .send()
        .await
        .expect_err("login with bad password should fail");

    let progenitor_client::Error::ErrorResponse(response) = err else {
        panic!("expected ErrorResponse, got {err:?}");
    };
    assert_eq!(response.status().as_u16(), 401);

    test.close().await;
}

#[tokio::test]
async fn login_for_unknown_user_also_returns_401() {
    let test = TestServer::start().await;
    let anon = test.anonymous_client();

    let err = anon
        .login()
        .body(LoginRequest {
            username: "nobody".to_string(),
            password: "anything".to_string(),
        })
        .send()
        .await
        .expect_err("login for unknown user should fail");

    let progenitor_client::Error::ErrorResponse(response) = err else {
        panic!("expected ErrorResponse, got {err:?}");
    };
    assert_eq!(response.status().as_u16(), 401);

    test.close().await;
}

#[tokio::test]
async fn refresh_returns_new_access_token() {
    let test = TestServer::start().await;
    let anon = test.anonymous_client();

    let initial = anon
        .login()
        .body(LoginRequest {
            username: "root".to_string(),
            password: ROOT_PASSWORD.to_string(),
        })
        .send()
        .await
        .unwrap()
        .into_inner();

    let refreshed = anon
        .refresh()
        .body(RefreshRequest {
            refresh_token: initial.refresh_token.clone(),
        })
        .send()
        .await
        .expect("refresh should succeed")
        .into_inner();

    assert!(!refreshed.access_token.is_empty());
    // It is allowed for a fast refresh to produce the same token if
    // the second-resolution `iat` matches; we only require the token
    // is usable, not that it differs lexically.
    let authed = test.bearer_client(&refreshed.access_token);
    authed
        .create_silo()
        .body(NewSilo {
            name: "after-refresh".to_string(),
            description: None,
        })
        .send()
        .await
        .expect("refreshed token should still authenticate");

    test.close().await;
}

#[tokio::test]
async fn refresh_with_garbage_token_returns_401() {
    let test = TestServer::start().await;
    let anon = test.anonymous_client();

    let err = anon
        .refresh()
        .body(RefreshRequest {
            refresh_token: "not.a.token".to_string(),
        })
        .send()
        .await
        .expect_err("refresh with bad token should fail");

    let progenitor_client::Error::ErrorResponse(response) = err else {
        panic!("expected ErrorResponse, got {err:?}");
    };
    assert_eq!(response.status().as_u16(), 401);

    test.close().await;
}

#[tokio::test]
async fn api_key_create_then_use_round_trip() {
    let test = TestServer::start().await;
    let anon = test.anonymous_client();

    let token_response = anon
        .login()
        .body(LoginRequest {
            username: "root".to_string(),
            password: ROOT_PASSWORD.to_string(),
        })
        .send()
        .await
        .unwrap()
        .into_inner();

    let session_client = test.bearer_client(&token_response.access_token);
    let created = session_client
        .create_api_key()
        .body(NewApiKey {
            description: "ci-pipeline".to_string(),
        })
        .send()
        .await
        .expect("api-key create should succeed")
        .into_inner();
    assert!(created.secret.starts_with("tcadm_"));
    assert_eq!(created.description, "ci-pipeline");

    // List confirms the key is there but the response shape carries
    // no secret material.
    let listed = session_client
        .list_api_keys()
        .send()
        .await
        .expect("list should succeed")
        .into_inner();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, created.id);

    // Use the API-key plaintext directly: it should authenticate
    // independently of the JWT session.
    let api_key_client = test.bearer_client(&created.secret);
    api_key_client
        .create_silo()
        .body(NewSilo {
            name: "from-api-key".to_string(),
            description: None,
        })
        .send()
        .await
        .expect("api-key bearer should authenticate");

    // Delete the key and confirm it no longer authenticates.
    session_client
        .delete_api_key()
        .api_key_id(created.id)
        .send()
        .await
        .expect("delete should succeed");
    let err = api_key_client
        .create_silo()
        .body(NewSilo {
            name: "after-delete".to_string(),
            description: None,
        })
        .send()
        .await
        .expect_err("deleted api-key should not authenticate");
    let progenitor_client::Error::ErrorResponse(response) = err else {
        panic!("expected ErrorResponse, got {err:?}");
    };
    assert_eq!(response.status().as_u16(), 403);

    test.close().await;
}
