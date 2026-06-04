// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

#![allow(clippy::unwrap_used, clippy::expect_used)]

//! End-to-end tests for the tenant-scoped access-key write endpoints
//! (monitor-reef-5fek, Wave 2):
//!
//!   POST   /v1/silos/{sid}/tenants/{tid}/storage/users/{user}/access-keys
//!   DELETE /v1/silos/{sid}/tenants/{tid}/storage/users/{user}/access-keys/{access_key_id}
//!
//! Same shape as the Wave 1 `tenant_storage_access_keys.rs` read-side
//! file. Covers the handler-owned gates only — authn/authz, cross-silo
//! defence, 412 on an unbound tenant, and upstream-error pass-through
//! against an unreachable mantad. The user/access-key path segments
//! are free-form strings (mantad's IAM scheme) — when the resource
//! doesn't exist in the tenant's workspace mantad's gate produces a
//! 404; here we exercise only the gates tritond owns.

use std::sync::Arc;

use chrono::Utc;
use tritond::audit::AuditService;
use tritond::auth::AuthService;
use tritond::{ApiContext, start_server_with_context};
use tritond_audit::MemChain;
use tritond_auth::{JwtKey, RedactedString, hash_password, mint_access};
use tritond_client::types::{NewSilo, NewTenant};
use tritond_store::{MemStore, NewStorageCluster, StorageClusterSurface, Store, User};
use uuid::Uuid;

struct TestServer {
    server: dropshot::HttpServer<ApiContext>,
    store: Arc<dyn Store>,
    bearer_root: String,
    bearer_non_root: String,
}

impl TestServer {
    async fn start() -> Self {
        let store: Arc<dyn Store> = Arc::new(MemStore::new());

        let root_id = Uuid::new_v4();
        let root = User {
            id: root_id,
            username: "root".to_string(),
            password_hash: hash_password(&RedactedString::from("ignored"))
                .await
                .unwrap(),
            is_root: true,
            fleet_admin: true,
            capabilities: Default::default(),
            created_at: Utc::now(),
            tenant_id: None,
            federation: None,
        };
        store.create_user(root).await.unwrap();

        let non_root_id = Uuid::new_v4();
        let non_root = User {
            id: non_root_id,
            username: "ops-user".to_string(),
            password_hash: hash_password(&RedactedString::from("ignored"))
                .await
                .unwrap(),
            is_root: false,
            fleet_admin: false,
            capabilities: Default::default(),
            created_at: Utc::now(),
            tenant_id: None,
            federation: None,
        };
        store.create_user(non_root).await.unwrap();

        let jwt_key = JwtKey::generate();
        let (bearer_root, _) = mint_access(&jwt_key, root_id).unwrap();
        let (bearer_non_root, _) = mint_access(&jwt_key, non_root_id).unwrap();
        let auth = Arc::new(AuthService::new(jwt_key).unwrap());
        let audit = Arc::new(AuditService::new(Arc::new(MemChain::new())));
        let context = ApiContext::new(store.clone(), auth, audit);
        let server = start_server_with_context("127.0.0.1:0", context)
            .await
            .expect("server should start on ephemeral port");
        Self {
            server,
            store,
            bearer_root,
            bearer_non_root,
        }
    }

    fn bind(&self) -> std::net::SocketAddr {
        self.server.local_addr()
    }

    fn root_client(&self) -> tritond_client::Client {
        self.bearer_client(&self.bearer_root)
    }

    fn non_root_client(&self) -> tritond_client::Client {
        self.bearer_client(&self.bearer_non_root)
    }

    fn anonymous_client(&self) -> tritond_client::Client {
        tritond_client::Client::new(&format!("http://{}", self.bind()))
    }

    fn bearer_client(&self, token: &str) -> tritond_client::Client {
        let mut headers = reqwest::header::HeaderMap::new();
        let value = format!("Bearer {token}").parse().unwrap();
        headers.insert(reqwest::header::AUTHORIZATION, value);
        let reqwest = reqwest::Client::builder()
            .default_headers(headers)
            .build()
            .unwrap();
        tritond_client::Client::new_with_client(&format!("http://{}", self.bind()), reqwest)
    }

    async fn close(self) {
        self.server.close().await.expect("server should close");
    }
}

fn unreachable_cluster(name: &str) -> NewStorageCluster {
    NewStorageCluster {
        name: name.to_string(),
        endpoint: "http://127.0.0.1:1".to_string(),
        admin_token: "test-token-not-a-real-secret".to_string(),
        surface: StorageClusterSurface::S3,
        default_region: "us-east-1".to_string(),
        display_name: None,
    }
}

async fn make_silo(client: &tritond_client::Client, name: &str) -> Uuid {
    client
        .create_silo()
        .body(NewSilo {
            name: name.to_string(),
            description: None,
        })
        .send()
        .await
        .expect("create_silo")
        .into_inner()
        .id
}

async fn make_tenant(client: &tritond_client::Client, silo_id: Uuid, name: &str) -> Uuid {
    client
        .create_silo_tenant()
        .silo_id(silo_id)
        .body(NewTenant {
            name: name.to_string(),
            description: None,
        })
        .send()
        .await
        .expect("create_silo_tenant")
        .into_inner()
        .id
}

// ---------------------------------------------------------------------------
// POST /v1/silos/{sid}/tenants/{tid}/storage/users/{user}/access-keys
// ---------------------------------------------------------------------------

#[tokio::test]
async fn create_anonymous_is_denied() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let silo_id = make_silo(&root, "ops").await;
    let tenant_id = make_tenant(&root, silo_id, "acme").await;

    let anon = test.anonymous_client();
    let err = anon
        .create_silo_tenant_storage_user_access_key()
        .silo_id(silo_id)
        .tenant_id(tenant_id)
        .user("alice")
        .send()
        .await
        .expect_err("anonymous must be denied");
    let progenitor_client::Error::ErrorResponse(resp) = err else {
        panic!("expected ErrorResponse, got {err:?}");
    };
    let status = resp.status().as_u16();
    assert!(
        matches!(status, 401 | 403 | 404),
        "expected 401/403/404 for anonymous, got {status}"
    );

    test.close().await;
}

#[tokio::test]
async fn create_non_root_in_silo_is_forbidden() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let silo_id = make_silo(&root, "ops").await;
    let tenant_id = make_tenant(&root, silo_id, "acme").await;

    let non_root = test.non_root_client();
    let err = non_root
        .create_silo_tenant_storage_user_access_key()
        .silo_id(silo_id)
        .tenant_id(tenant_id)
        .user("alice")
        .send()
        .await
        .expect_err("non-root must be denied");
    let progenitor_client::Error::ErrorResponse(resp) = err else {
        panic!("expected ErrorResponse, got {err:?}");
    };
    let status = resp.status().as_u16();
    assert!(
        status == 401 || status == 403 || status == 404,
        "expected 401/403/404, got {status}"
    );

    test.close().await;
}

#[tokio::test]
async fn create_unbound_tenant_returns_412_tenant_storage_unbound() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let silo_id = make_silo(&root, "ops").await;
    let tenant_id = make_tenant(&root, silo_id, "acme").await;

    let err = root
        .create_silo_tenant_storage_user_access_key()
        .silo_id(silo_id)
        .tenant_id(tenant_id)
        .user("alice")
        .send()
        .await
        .expect_err("unbound tenant should produce 412");
    let progenitor_client::Error::ErrorResponse(resp) = err else {
        panic!("expected ErrorResponse, got {err:?}");
    };
    assert_eq!(resp.status().as_u16(), 412);
    let body = resp.into_inner();
    assert_eq!(
        body.error_code.as_deref(),
        Some("TenantStorageUnbound"),
        "412 must use the same error code as drop_silo_tenant_storage"
    );

    test.close().await;
}

#[tokio::test]
async fn create_cross_silo_probe_returns_404() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let silo_a = make_silo(&root, "silo-a").await;
    let silo_b = make_silo(&root, "silo-b").await;
    let tenant_a = make_tenant(&root, silo_a, "tenant-a").await;

    let err = root
        .create_silo_tenant_storage_user_access_key()
        .silo_id(silo_b)
        .tenant_id(tenant_a)
        .user("alice")
        .send()
        .await
        .expect_err("cross-silo lookup should be 404");
    let progenitor_client::Error::ErrorResponse(resp) = err else {
        panic!("expected ErrorResponse, got {err:?}");
    };
    assert_eq!(resp.status().as_u16(), 404);

    test.close().await;
}

#[tokio::test]
async fn create_bound_tenant_against_unreachable_mantad_surfaces_5xx() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let silo_id = make_silo(&root, "ops").await;
    let tenant_id = make_tenant(&root, silo_id, "acme").await;

    let cluster = test
        .store
        .create_storage_cluster(unreachable_cluster("primary"))
        .await
        .expect("create_storage_cluster");
    let workspace_uuid = Uuid::new_v4();
    test.store
        .set_tenant_storage_binding(tenant_id, workspace_uuid, cluster.id)
        .await
        .expect("set_tenant_storage_binding");

    let err = root
        .create_silo_tenant_storage_user_access_key()
        .silo_id(silo_id)
        .tenant_id(tenant_id)
        .user("alice")
        .send()
        .await
        .expect_err("unreachable mantad should fail");
    let progenitor_client::Error::ErrorResponse(resp) = err else {
        panic!("expected ErrorResponse, got {err:?}");
    };
    assert!(
        resp.status().as_u16() >= 500,
        "upstream failure must be 5xx, got {}",
        resp.status().as_u16()
    );

    test.close().await;
}

// ---------------------------------------------------------------------------
// DELETE /v1/silos/{sid}/tenants/{tid}/storage/users/{user}/access-keys/{access_key_id}
// ---------------------------------------------------------------------------

#[tokio::test]
async fn delete_anonymous_is_denied() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let silo_id = make_silo(&root, "ops").await;
    let tenant_id = make_tenant(&root, silo_id, "acme").await;

    let anon = test.anonymous_client();
    let err = anon
        .delete_silo_tenant_storage_user_access_key()
        .silo_id(silo_id)
        .tenant_id(tenant_id)
        .user("alice")
        .access_key_id("AKIDEXAMPLE")
        .send()
        .await
        .expect_err("anonymous must be denied");
    let progenitor_client::Error::ErrorResponse(resp) = err else {
        panic!("expected ErrorResponse, got {err:?}");
    };
    let status = resp.status().as_u16();
    assert!(
        matches!(status, 401 | 403 | 404),
        "expected 401/403/404 for anonymous, got {status}"
    );

    test.close().await;
}

#[tokio::test]
async fn delete_non_root_in_silo_is_forbidden() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let silo_id = make_silo(&root, "ops").await;
    let tenant_id = make_tenant(&root, silo_id, "acme").await;

    let non_root = test.non_root_client();
    let err = non_root
        .delete_silo_tenant_storage_user_access_key()
        .silo_id(silo_id)
        .tenant_id(tenant_id)
        .user("alice")
        .access_key_id("AKIDEXAMPLE")
        .send()
        .await
        .expect_err("non-root must be denied");
    let progenitor_client::Error::ErrorResponse(resp) = err else {
        panic!("expected ErrorResponse, got {err:?}");
    };
    let status = resp.status().as_u16();
    assert!(
        status == 401 || status == 403 || status == 404,
        "expected 401/403/404, got {status}"
    );

    test.close().await;
}

#[tokio::test]
async fn delete_unbound_tenant_returns_412_tenant_storage_unbound() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let silo_id = make_silo(&root, "ops").await;
    let tenant_id = make_tenant(&root, silo_id, "acme").await;

    let err = root
        .delete_silo_tenant_storage_user_access_key()
        .silo_id(silo_id)
        .tenant_id(tenant_id)
        .user("alice")
        .access_key_id("AKIDEXAMPLE")
        .send()
        .await
        .expect_err("unbound tenant should produce 412");
    let progenitor_client::Error::ErrorResponse(resp) = err else {
        panic!("expected ErrorResponse, got {err:?}");
    };
    assert_eq!(resp.status().as_u16(), 412);
    let body = resp.into_inner();
    assert_eq!(
        body.error_code.as_deref(),
        Some("TenantStorageUnbound"),
        "412 must use the same error code as drop_silo_tenant_storage"
    );

    test.close().await;
}

#[tokio::test]
async fn delete_cross_silo_probe_returns_404() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let silo_a = make_silo(&root, "silo-a").await;
    let silo_b = make_silo(&root, "silo-b").await;
    let tenant_a = make_tenant(&root, silo_a, "tenant-a").await;

    let err = root
        .delete_silo_tenant_storage_user_access_key()
        .silo_id(silo_b)
        .tenant_id(tenant_a)
        .user("alice")
        .access_key_id("AKIDEXAMPLE")
        .send()
        .await
        .expect_err("cross-silo lookup should be 404");
    let progenitor_client::Error::ErrorResponse(resp) = err else {
        panic!("expected ErrorResponse, got {err:?}");
    };
    assert_eq!(resp.status().as_u16(), 404);

    test.close().await;
}

#[tokio::test]
async fn delete_bound_tenant_against_unreachable_mantad_surfaces_5xx() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let silo_id = make_silo(&root, "ops").await;
    let tenant_id = make_tenant(&root, silo_id, "acme").await;

    let cluster = test
        .store
        .create_storage_cluster(unreachable_cluster("primary"))
        .await
        .expect("create_storage_cluster");
    let workspace_uuid = Uuid::new_v4();
    test.store
        .set_tenant_storage_binding(tenant_id, workspace_uuid, cluster.id)
        .await
        .expect("set_tenant_storage_binding");

    let err = root
        .delete_silo_tenant_storage_user_access_key()
        .silo_id(silo_id)
        .tenant_id(tenant_id)
        .user("alice")
        .access_key_id("AKIDEXAMPLE")
        .send()
        .await
        .expect_err("unreachable mantad should fail");
    let progenitor_client::Error::ErrorResponse(resp) = err else {
        panic!("expected ErrorResponse, got {err:?}");
    };
    assert!(
        resp.status().as_u16() >= 500,
        "upstream failure must be 5xx, got {}",
        resp.status().as_u16()
    );

    test.close().await;
}
