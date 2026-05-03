// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

#![allow(clippy::unwrap_used, clippy::expect_used)]

//! End-to-end tests for least-privilege API-key scopes.
//!
//! Each test mints an API key at a specific scope (Full, ReadOnly,
//! AuditOnly), then exercises the wire surface to verify that the
//! key can do exactly the actions its scope permits and *no* others.
//! The scope check fires before Cedar so even a key whose owning
//! user is `root` is constrained by the scope.

use std::sync::Arc;

use chrono::Utc;
use tritond::audit::AuditService;
use tritond::auth::AuthService;
use tritond::{ApiContext, start_server_with_context};
use tritond_audit::MemChain;
use tritond_auth::{JwtKey, RedactedString, hash_password};
use tritond_client::types::{ApiKeyScope, LoginRequest, NewApiKey, NewSilo};
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
            tenant_id: None,
            federation: None,
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

/// Authenticate as root and mint an API key at the requested scope.
/// Returns the wire-form `tcadm_…` plaintext.
async fn mint_key(test: &TestServer, scope: ApiKeyScope, description: &str) -> String {
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
            description: description.to_string(),
            scope,
        })
        .send()
        .await
        .unwrap()
        .into_inner();
    assert_eq!(created.scope, scope);
    created.secret
}

/// `Full`-scoped key behaves identically to a JWT session — proves
/// the new code path doesn't regress the default behaviour.
#[tokio::test]
async fn full_scope_key_can_do_writes() {
    let test = TestServer::start().await;
    let secret = mint_key(&test, ApiKeyScope::Full, "ci-pipeline").await;
    let client = test.bearer_client(&secret);

    // Read.
    let _ = client
        .list_api_keys()
        .send()
        .await
        .expect("Full key should list api-keys");

    // Write.
    let silo = client
        .create_silo()
        .body(NewSilo {
            name: "from-full-key".to_string(),
            description: None,
        })
        .send()
        .await
        .expect("Full key should create silos")
        .into_inner();
    assert_eq!(silo.name, "from-full-key");

    test.close().await;
}

#[tokio::test]
async fn read_only_scope_key_blocks_writes() {
    let test = TestServer::start().await;
    let secret = mint_key(&test, ApiKeyScope::ReadOnly, "monitoring").await;
    let client = test.bearer_client(&secret);

    // Reads succeed.
    let _ = client
        .list_api_keys()
        .send()
        .await
        .expect("ReadOnly key should list api-keys");

    // Writes are denied. Fleet-scoped writes return 403.
    let err = client
        .create_silo()
        .body(NewSilo {
            name: "should-not-exist".to_string(),
            description: None,
        })
        .send()
        .await
        .expect_err("ReadOnly key must not create silos");
    let progenitor_client::Error::ErrorResponse(resp) = err else {
        panic!("expected ErrorResponse, got {err:?}");
    };
    assert_eq!(resp.status().as_u16(), 403);

    // Minting another key (write on the auth surface) is also denied.
    let err = client
        .create_api_key()
        .body(NewApiKey {
            description: "second-key".to_string(),
            scope: ApiKeyScope::ReadOnly,
        })
        .send()
        .await
        .expect_err("ReadOnly key must not mint api-keys");
    let progenitor_client::Error::ErrorResponse(resp) = err else {
        panic!("expected ErrorResponse, got {err:?}");
    };
    assert_eq!(resp.status().as_u16(), 403);

    test.close().await;
}

#[tokio::test]
async fn read_only_scope_blocks_silo_scoped_writes_with_404() {
    // Silo-scoped deny returns 404 (not 403) — preserves the
    // cross-tenant probe invariant that scoped keys can't enumerate
    // silos by attempting actions in them.
    let test = TestServer::start().await;

    // Set up: root creates a silo + project the scoped key will try to mutate.
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
    let silo = session
        .create_silo()
        .body(NewSilo {
            name: "acme".to_string(),
            description: None,
        })
        .send()
        .await
        .unwrap()
        .into_inner();

    let secret = mint_key(&test, ApiKeyScope::ReadOnly, "ro").await;
    let scoped = test.bearer_client(&secret);

    // ReadOnly: project_list (read) on a silo should succeed.
    let _ = scoped
        .list_silo_projects()
        .silo_id(silo.id)
        .send()
        .await
        .expect("ReadOnly key should list projects in a silo");

    // ReadOnly: project_create (write) must be denied as 404 since
    // the action is silo-scoped.
    let err = scoped
        .create_silo_project()
        .silo_id(silo.id)
        .body(tritond_client::types::NewProject {
            name: "should-not-exist".to_string(),
            description: None,
        })
        .send()
        .await
        .expect_err("ReadOnly key must not create projects");
    let progenitor_client::Error::ErrorResponse(resp) = err else {
        panic!("expected ErrorResponse, got {err:?}");
    };
    assert_eq!(resp.status().as_u16(), 404);

    test.close().await;
}

#[tokio::test]
async fn audit_only_scope_blocks_everything_except_audit() {
    let test = TestServer::start().await;
    let secret = mint_key(&test, ApiKeyScope::AuditOnly, "compliance").await;
    let client = test.bearer_client(&secret);

    // Audit chain reads succeed.
    let _ = client
        .list_audit_events()
        .send()
        .await
        .expect("AuditOnly key should list audit events");
    let _ = client
        .verify_audit_chain()
        .send()
        .await
        .expect("AuditOnly key should verify audit chain");

    // Resource reads are denied — the key can see who did what but
    // can't see the resources themselves.
    let err = client
        .list_api_keys()
        .send()
        .await
        .expect_err("AuditOnly key must not list api-keys");
    let progenitor_client::Error::ErrorResponse(resp) = err else {
        panic!("expected ErrorResponse, got {err:?}");
    };
    assert_eq!(resp.status().as_u16(), 403);

    // Writes are denied.
    let err = client
        .create_silo()
        .body(NewSilo {
            name: "nope".to_string(),
            description: None,
        })
        .send()
        .await
        .expect_err("AuditOnly key must not create silos");
    let progenitor_client::Error::ErrorResponse(resp) = err else {
        panic!("expected ErrorResponse, got {err:?}");
    };
    assert_eq!(resp.status().as_u16(), 403);

    test.close().await;
}

#[tokio::test]
async fn omitted_scope_field_defaults_to_full() {
    // Operators who mint a key without specifying a scope must keep
    // the pre-scope behaviour where the key acts as the full owner.
    // Verifies the wire-side `#[serde(default)]` defaulting.
    let test = TestServer::start().await;
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

    // Hit the wire directly with a body that omits `scope` entirely.
    let bind = test.bind();
    let url = format!("http://{bind}/v2/auth/api-keys");
    let raw = reqwest::Client::new()
        .post(&url)
        .header(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {}", token.access_token),
        )
        .json(&serde_json::json!({"description": "no-scope"}))
        .send()
        .await
        .unwrap();
    assert_eq!(raw.status().as_u16(), 201);
    let body: serde_json::Value = raw.json().await.unwrap();
    assert_eq!(body["scope"], "full");
    let secret = body["secret"].as_str().unwrap().to_string();

    // The minted key should behave like Full (write succeeds).
    let client = test.bearer_client(&secret);
    let _ = client
        .create_silo()
        .body(NewSilo {
            name: "implicitly-full".to_string(),
            description: None,
        })
        .send()
        .await
        .expect("default-scope key should create silos");

    test.close().await;
}
