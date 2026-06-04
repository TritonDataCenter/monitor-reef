// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

#![allow(clippy::unwrap_used, clippy::expect_used)]

//! End-to-end tests for
//! `GET /v1/silos/{silo_id}/tenants/{tenant_id}/storage/buckets/{bucket}/objects`
//! (monitor-reef-oei3). Sibling of `tenant_storage_buckets.rs` and
//! `tenant_storage_users.rs`. Covers the gates the handler owns —
//! authn/authz, cross-silo defence, 412 on an unbound tenant, and
//! upstream-error pass-through against an unreachable mantad. Object
//! listing / paging / filter correctness is mantad's responsibility.
//!
//! The last test in the file (`flat_endpoint_principal_resolution_smokes`)
//! is the regression-guard for the flat-endpoint workspace-scope leak:
//! `GET /v1/storage/clusters/{id}/buckets/{bucket}/objects` resolved a
//! workspace `scope` but discarded `scope.workspace_name()` before
//! forwarding to mantad, so a tenant-bound operator could enumerate
//! every workspace on the cluster. The handler now passes
//! `scope.workspace_name()` through; the test below pins the
//! observable shape — a tenant-bound operator calling the flat
//! endpoint with an unreachable cluster surfaces a 5xx (mantad
//! unreachable), not a 412/4xx from a broken scope-resolution path.
//! The real regression guard is the visible code change in
//! `handlers/storage_clusters/buckets.rs::list_storage_cluster_objects`.

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
    jwt_key_bytes: [u8; 32],
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
        let jwt_key_bytes = *jwt_key.bytes();
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
            jwt_key_bytes,
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

    /// Mint a non-root operator bound to the supplied tenant
    /// (`tenant_id = Some(t)`), returning the bearer token. Used by
    /// the flat-endpoint regression smoke test to exercise the path
    /// where `scope.workspace_name()` is `Some("t-...")`.
    async fn make_tenant_bound_bearer(&self, tenant_id: Uuid, username: &str) -> String {
        let id = Uuid::new_v4();
        let user = User {
            id,
            username: username.to_string(),
            password_hash: "$2y$12$placeholder".to_string(),
            is_root: false,
            fleet_admin: false,
            capabilities: Default::default(),
            created_at: Utc::now(),
            tenant_id: Some(tenant_id),
            federation: None,
        };
        self.store.create_user(user).await.unwrap();
        // JwtKey doesn't impl Clone (its Drop zeroes the bytes), so
        // reconstruct it from the saved bytes for each mint.
        let key = JwtKey::from_bytes(self.jwt_key_bytes);
        let (token, _) = mint_access(&key, id).unwrap();
        token
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

#[tokio::test]
async fn anonymous_is_denied() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let silo_id = make_silo(&root, "ops").await;
    let tenant_id = make_tenant(&root, silo_id, "acme").await;

    let anon = test.anonymous_client();
    let err = anon
        .list_silo_tenant_storage_bucket_objects()
        .silo_id(silo_id)
        .tenant_id(tenant_id)
        .bucket("some-bucket")
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
async fn non_root_in_silo_is_forbidden() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let silo_id = make_silo(&root, "ops").await;
    let tenant_id = make_tenant(&root, silo_id, "acme").await;

    let non_root = test.non_root_client();
    let err = non_root
        .list_silo_tenant_storage_bucket_objects()
        .silo_id(silo_id)
        .tenant_id(tenant_id)
        .bucket("some-bucket")
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
async fn unbound_tenant_returns_412_tenant_storage_unbound() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let silo_id = make_silo(&root, "ops").await;
    let tenant_id = make_tenant(&root, silo_id, "acme").await;

    let err = root
        .list_silo_tenant_storage_bucket_objects()
        .silo_id(silo_id)
        .tenant_id(tenant_id)
        .bucket("some-bucket")
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
async fn cross_silo_probe_returns_404() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let silo_a = make_silo(&root, "silo-a").await;
    let silo_b = make_silo(&root, "silo-b").await;
    let tenant_a = make_tenant(&root, silo_a, "tenant-a").await;

    let err = root
        .list_silo_tenant_storage_bucket_objects()
        .silo_id(silo_b)
        .tenant_id(tenant_a)
        .bucket("some-bucket")
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
async fn bound_tenant_against_unreachable_mantad_surfaces_5xx() {
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
        .list_silo_tenant_storage_bucket_objects()
        .silo_id(silo_id)
        .tenant_id(tenant_id)
        .bucket("some-bucket")
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
// Regression guard for the flat endpoint
// `GET /v1/storage/clusters/{id}/buckets/{bucket}/objects`.
//
// The previous handler resolved `scope = resolve_workspace_scope(..)`
// then ignored `scope.workspace_name()` when calling
// `client.list_objects(&bucket, &q)`, leaking sibling workspaces to
// any tenant-bound operator. The fix forwards
// `scope.workspace_name()` to mantad.
//
// A clean behavioural assertion would mock mantad and inspect the
// outgoing query, but mantad-client is not currently mockable from a
// tritond integration test without invasive surgery. The pragmatic
// smoke below proves the scope-resolution path stays live for a
// tenant-bound principal: it calls the flat endpoint with an
// operator whose `tenant_id = Some(t)` (so `scope.workspace_name()`
// is `Some("t-...")`), the bound cluster is unreachable, and the
// response surfaces as 5xx. The real regression guard is the
// visible code diff in
// `handlers/storage_clusters/buckets.rs::list_storage_cluster_objects`.
// ---------------------------------------------------------------------------
#[tokio::test]
async fn flat_endpoint_principal_resolution_smokes() {
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

    // Operator with `tenant_id = Some(tenant_id)` — Cedar's
    // `principal has tenant_id` rule grants `storage_object_list`,
    // so authn/authz succeeds and the handler reaches
    // `resolve_workspace_scope`, which returns
    // `scope.workspace_name() == Some("t-...")`.
    let bearer = test.make_tenant_bound_bearer(tenant_id, "ops-tenant").await;
    let tenant_bound = test.bearer_client(&bearer);

    let err = tenant_bound
        .list_storage_cluster_objects()
        .id(cluster.id)
        .bucket("some-bucket")
        .send()
        .await
        .expect_err("unreachable mantad should fail");
    let progenitor_client::Error::ErrorResponse(resp) = err else {
        panic!("expected ErrorResponse, got {err:?}");
    };
    let status = resp.status().as_u16();
    assert!(
        status >= 500,
        "tenant-bound operator hitting the flat endpoint with an \
         unreachable mantad must surface as 5xx (mantad-unreachable), \
         not 4xx from a broken scope-resolution path; got {status}"
    );

    test.close().await;
}
