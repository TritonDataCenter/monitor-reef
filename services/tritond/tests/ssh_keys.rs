// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

#![allow(clippy::unwrap_used, clippy::expect_used)]

//! End-to-end tests for the `Silo`-scoped slice of the multi-scope
//! `/v1/silos/{silo_id}/ssh-keys` surface and the cross-cutting
//! concerns that aren't visibility (fingerprint validation,
//! openssh parsing, name + fingerprint uniqueness within a single
//! scope).
//!
//! The visibility / ownership matrix across all five scopes lives in
//! `tests/ssh_key_scope.rs` (added in slice G); this file is the
//! per-scope happy-path / wire-shape test surface.

use std::net::SocketAddr;
use std::sync::Arc;

use chrono::Utc;
use rsa::rand_core::OsRng;
use ssh_key::{Algorithm, PrivateKey};
use tritond::audit::AuditService;
use tritond::auth::AuthService;
use tritond::{ApiContext, start_server_with_context};
use tritond_audit::MemChain;
use tritond_auth::{JwtKey, RedactedString, hash_password, mint_access};
use tritond_client::types::{NewSilo, NewSshKey};
use tritond_store::{MemStore, Store, User};
use uuid::Uuid;

const ROOT_PASSWORD: &str = "ssh-keys-test";

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

/// Generate a fresh ed25519 keypair and return the openssh-formatted
/// public key. Different on every call.
fn fresh_pubkey() -> String {
    let priv_key = PrivateKey::random(&mut OsRng, Algorithm::Ed25519).unwrap();
    priv_key.public_key().to_openssh().unwrap()
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

// ---------- tests ----------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ssh_key_round_trip_within_silo() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let silo_id = make_silo(&root).await;

    let pk = fresh_pubkey();
    let key = root
        .create_silo_ssh_key()
        .silo_id(silo_id)
        .body(NewSshKey {
            name: "ci".to_string(),
            description: Some("ci pipeline".to_string()),
            public_key: pk.clone(),
        })
        .send()
        .await
        .unwrap()
        .into_inner();
    assert!(matches!(
        key.scope,
        tritond_client::types::SshKeyScope::Silo { silo_id: s } if s == silo_id,
    ));
    assert_eq!(key.public_key, pk);
    assert!(
        key.fingerprint.starts_with("SHA256:"),
        "fingerprint should start with SHA256:, got {}",
        key.fingerprint
    );
    assert!(
        !key.fingerprint.ends_with('='),
        "ssh-key crate strips trailing padding from fingerprints, got {}",
        key.fingerprint
    );

    let listed = root
        .list_silo_ssh_keys()
        .silo_id(silo_id)
        .send()
        .await
        .unwrap()
        .into_inner();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, key.id);

    // Global by-id get works regardless of scope.
    let fetched = root
        .get_ssh_key()
        .key_id(key.id)
        .send()
        .await
        .unwrap()
        .into_inner();
    assert_eq!(fetched.fingerprint, key.fingerprint);

    root.delete_ssh_key().key_id(key.id).send().await.unwrap();
    let err = root
        .get_ssh_key()
        .key_id(key.id)
        .send()
        .await
        .expect_err("post-delete get must 404");
    assert_status(err, 404);

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn malformed_public_key_returns_400() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let silo_id = make_silo(&root).await;

    let err = root
        .create_silo_ssh_key()
        .silo_id(silo_id)
        .body(NewSshKey {
            name: "bad".to_string(),
            description: None,
            public_key: "this is not a valid openssh key".to_string(),
        })
        .send()
        .await
        .expect_err("malformed openssh must 400");
    assert_status(err, 400);

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn duplicate_fingerprint_within_silo_returns_409() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let silo_id = make_silo(&root).await;

    let pk = fresh_pubkey();
    root.create_silo_ssh_key()
        .silo_id(silo_id)
        .body(NewSshKey {
            name: "alice".to_string(),
            description: None,
            public_key: pk.clone(),
        })
        .send()
        .await
        .unwrap();

    // Same key, different name → fingerprint collision → 409.
    let err = root
        .create_silo_ssh_key()
        .silo_id(silo_id)
        .body(NewSshKey {
            name: "bob".to_string(),
            description: None,
            public_key: pk,
        })
        .send()
        .await
        .expect_err("re-uploading same key under new name must 409");
    assert_status(err, 409);

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn duplicate_name_within_silo_returns_409() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let silo_id = make_silo(&root).await;

    root.create_silo_ssh_key()
        .silo_id(silo_id)
        .body(NewSshKey {
            name: "ci".to_string(),
            description: None,
            public_key: fresh_pubkey(),
        })
        .send()
        .await
        .unwrap();
    let err = root
        .create_silo_ssh_key()
        .silo_id(silo_id)
        .body(NewSshKey {
            name: "ci".to_string(),
            description: None,
            public_key: fresh_pubkey(),
        })
        .send()
        .await
        .expect_err("duplicate name must 409");
    assert_status(err, 409);

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn same_key_in_different_silos_does_not_conflict() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let silo_a = make_silo(&root).await;
    let silo_b = make_silo(&root).await;
    let pk = fresh_pubkey();

    root.create_silo_ssh_key()
        .silo_id(silo_a)
        .body(NewSshKey {
            name: "ci".to_string(),
            description: None,
            public_key: pk.clone(),
        })
        .send()
        .await
        .unwrap();
    root.create_silo_ssh_key()
        .silo_id(silo_b)
        .body(NewSshKey {
            name: "ci".to_string(),
            description: None,
            public_key: pk,
        })
        .send()
        .await
        .expect("same key + name in a different silo must succeed");

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn anonymous_cannot_reach_silo_ssh_key_endpoints() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let silo_id = make_silo(&root).await;

    let anon = test.anonymous_client();
    let err = anon
        .list_silo_ssh_keys()
        .silo_id(silo_id)
        .send()
        .await
        .expect_err("anonymous list must be denied");
    assert_status(err, 404);

    test.close().await;
}
