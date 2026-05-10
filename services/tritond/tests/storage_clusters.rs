// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

#![allow(clippy::unwrap_used, clippy::expect_used)]

//! End-to-end tests for the `/v2/storage/clusters` surface
//! (Stage 3.3 + 3.4 CRUD, Stage 3.5 health probe + forwarder gate).
//!
//! Forwarder tests pointed at a real mantad endpoint live elsewhere
//! (out-of-process integration); this file exercises only what's
//! reachable with an in-memory store + an unreachable mantad URL,
//! which still covers:
//!
//! * registry CRUD round-trip via the generated client,
//! * `StorageClusterView` redaction of the bearer token,
//! * Cedar gating (anonymous + non-root denied, root allowed),
//! * idempotent delete + name reuse,
//! * `Fs` / `Block` surface registration succeeds but forwarder
//!   endpoints reject with 409,
//! * health probe against an unreachable cluster transitions
//!   `status` to `Unreachable` and persists `last_observed_at`.

use std::sync::Arc;

use chrono::Utc;
use tritond::audit::AuditService;
use tritond::auth::AuthService;
use tritond::{ApiContext, start_server_with_context};
use tritond_audit::MemChain;
use tritond_auth::{JwtKey, RedactedString, hash_password, mint_access};
use tritond_client::types::{NewStorageCluster, StorageClusterStatus, StorageClusterSurface};
use tritond_store::{MemStore, Store, User};
use uuid::Uuid;

struct TestServer {
    server: dropshot::HttpServer<ApiContext>,
    bearer_root: String,
    bearer_non_root: String,
}

impl TestServer {
    /// Spin up tritond with two pre-loaded operators: a root user
    /// (for the happy path) and a tenant-scoped user (for the
    /// "non-root is denied" gate test).
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
            created_at: Utc::now(),
            tenant_id: None,
            federation: None,
        };
        store.create_user(root).await.unwrap();

        let non_root_id = Uuid::new_v4();
        let non_root = User {
            id: non_root_id,
            username: "tenant-admin".to_string(),
            password_hash: hash_password(&RedactedString::from("ignored"))
                .await
                .unwrap(),
            is_root: false,
            fleet_admin: false,
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
        let context = ApiContext::new(store, auth, audit);
        let server = start_server_with_context("127.0.0.1:0", context)
            .await
            .expect("server should start on ephemeral port");
        Self {
            server,
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

/// Build a registration body that points at a deliberately
/// unreachable address (loopback + a privileged port that nothing
/// is listening on). Forwarder calls fail fast with `ECONNREFUSED`
/// rather than waiting out mantad-client's 30s connect timeout the
/// way an unroutable WAN IP (RFC 5737) would.
fn unreachable_cluster(name: &str) -> NewStorageCluster {
    NewStorageCluster {
        name: name.to_string(),
        endpoint: "http://127.0.0.1:1".to_string(),
        admin_token: "test-token-not-a-real-secret".to_string(),
        surface: StorageClusterSurface::S3,
        default_region: "us-east-1".to_string(),
        display_name: Some(format!("{name} display")),
    }
}

#[tokio::test]
async fn create_then_get_round_trips_and_redacts_token() {
    let test = TestServer::start().await;
    let client = test.root_client();

    let created = client
        .create_storage_cluster()
        .body(unreachable_cluster("primary"))
        .send()
        .await
        .expect("create should succeed")
        .into_inner();
    assert_eq!(created.name, "primary");
    assert_eq!(created.surface, StorageClusterSurface::S3);
    assert_eq!(created.status, StorageClusterStatus::Unknown);
    assert!(created.last_observed_at.is_none());

    let fetched = client
        .get_storage_cluster()
        .id(created.id)
        .send()
        .await
        .expect("get should succeed")
        .into_inner();
    assert_eq!(fetched.id, created.id);
    assert_eq!(fetched.endpoint, created.endpoint);

    // Token redaction: `StorageClusterView` deliberately has no
    // `admin_token` field. Re-serialise the wire payload and
    // confirm the literal token string never appears.
    let serialised = serde_json::to_string(&fetched).unwrap();
    assert!(
        !serialised.contains("test-token-not-a-real-secret"),
        "wire payload leaked the bearer token: {serialised}"
    );

    test.close().await;
}

#[tokio::test]
async fn duplicate_name_returns_409() {
    let test = TestServer::start().await;
    let client = test.root_client();

    client
        .create_storage_cluster()
        .body(unreachable_cluster("primary"))
        .send()
        .await
        .expect("first create should succeed");

    let err = client
        .create_storage_cluster()
        .body(unreachable_cluster("primary"))
        .send()
        .await
        .expect_err("duplicate name should fail");
    let progenitor_client::Error::ErrorResponse(response) = err else {
        panic!("expected ErrorResponse, got {err:?}");
    };
    assert_eq!(response.status().as_u16(), 409);

    test.close().await;
}

#[tokio::test]
async fn get_unknown_id_returns_404() {
    let test = TestServer::start().await;
    let client = test.root_client();

    let err = client
        .get_storage_cluster()
        .id(Uuid::new_v4())
        .send()
        .await
        .expect_err("missing id should fail");
    let progenitor_client::Error::ErrorResponse(response) = err else {
        panic!("expected ErrorResponse, got {err:?}");
    };
    assert_eq!(response.status().as_u16(), 404);

    test.close().await;
}

#[tokio::test]
async fn list_is_sorted_by_name() {
    let test = TestServer::start().await;
    let client = test.root_client();

    for n in ["zulu", "alpha", "mike"] {
        client
            .create_storage_cluster()
            .body(unreachable_cluster(n))
            .send()
            .await
            .expect("create should succeed");
    }
    let listed = client
        .list_storage_clusters()
        .send()
        .await
        .expect("list should succeed")
        .into_inner();
    let names: Vec<&str> = listed.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, vec!["alpha", "mike", "zulu"]);

    test.close().await;
}

#[tokio::test]
async fn delete_is_idempotent_and_frees_the_name() {
    let test = TestServer::start().await;
    let client = test.root_client();

    let created = client
        .create_storage_cluster()
        .body(unreachable_cluster("primary"))
        .send()
        .await
        .expect("first create should succeed")
        .into_inner();

    client
        .delete_storage_cluster()
        .id(created.id)
        .send()
        .await
        .expect("first delete should succeed");
    // Idempotent — second delete is also a 204.
    client
        .delete_storage_cluster()
        .id(created.id)
        .send()
        .await
        .expect("second delete should also succeed");

    // Name is freed: re-creating with the same name now works.
    client
        .create_storage_cluster()
        .body(unreachable_cluster("primary"))
        .send()
        .await
        .expect("re-create after delete should succeed");

    test.close().await;
}

#[tokio::test]
async fn anonymous_create_is_forbidden() {
    let test = TestServer::start().await;
    let client = test.anonymous_client();

    let err = client
        .create_storage_cluster()
        .body(unreachable_cluster("intruder"))
        .send()
        .await
        .expect_err("anonymous create should fail");
    let progenitor_client::Error::ErrorResponse(response) = err else {
        panic!("expected ErrorResponse, got {err:?}");
    };
    assert_eq!(response.status().as_u16(), 403);

    test.close().await;
}

#[tokio::test]
async fn non_root_operator_is_forbidden() {
    let test = TestServer::start().await;
    let client = test.non_root_client();

    let err = client
        .create_storage_cluster()
        .body(unreachable_cluster("not-mine"))
        .send()
        .await
        .expect_err("non-root create should fail");
    let progenitor_client::Error::ErrorResponse(response) = err else {
        panic!("expected ErrorResponse, got {err:?}");
    };
    assert_eq!(response.status().as_u16(), 403);

    let err = client
        .list_storage_clusters()
        .send()
        .await
        .expect_err("non-root list should fail");
    let progenitor_client::Error::ErrorResponse(response) = err else {
        panic!("expected ErrorResponse, got {err:?}");
    };
    assert_eq!(response.status().as_u16(), 403);

    test.close().await;
}

#[tokio::test]
async fn fs_surface_registration_succeeds_but_forwarder_rejects_with_409() {
    // The `Fs` surface (mantafs) is accepted by the registry — operators
    // can record the cluster — but every forwarder endpoint returns 409
    // because Stage 3.5 implements only the S3 forwarder family.
    let test = TestServer::start().await;
    let client = test.root_client();

    let mut req = unreachable_cluster("mantafs-cluster");
    req.surface = StorageClusterSurface::Fs;
    let created = client
        .create_storage_cluster()
        .body(req)
        .send()
        .await
        .expect("Fs registration should succeed")
        .into_inner();
    assert_eq!(created.surface, StorageClusterSurface::Fs);

    // Forwarder call against the same cluster: 409 (NOT 502/500),
    // because the gate runs before tritond ever tries to dial mantad.
    let err = client
        .get_storage_cluster_summary()
        .id(created.id)
        .send()
        .await
        .expect_err("forwarder against Fs surface must be rejected");
    let progenitor_client::Error::ErrorResponse(response) = err else {
        panic!("expected ErrorResponse, got {err:?}");
    };
    assert_eq!(response.status().as_u16(), 409);

    test.close().await;
}

#[tokio::test]
async fn health_probe_on_unreachable_cluster_marks_unreachable() {
    let test = TestServer::start().await;
    let client = test.root_client();

    let created = client
        .create_storage_cluster()
        .body(unreachable_cluster("primary"))
        .send()
        .await
        .expect("create should succeed")
        .into_inner();
    assert_eq!(created.status, StorageClusterStatus::Unknown);
    assert!(created.last_observed_at.is_none());

    let probed = client
        .probe_storage_cluster_health()
        .id(created.id)
        .send()
        .await
        .expect("probe should succeed (registers Unreachable, doesn't error)")
        .into_inner();
    assert_eq!(probed.status, StorageClusterStatus::Unreachable);
    assert!(probed.last_observed_at.is_some());

    // Status is persisted: re-reading the cluster shows the new
    // observed-at timestamp.
    let refetched = client
        .get_storage_cluster()
        .id(created.id)
        .send()
        .await
        .expect("get should succeed")
        .into_inner();
    assert_eq!(refetched.status, StorageClusterStatus::Unreachable);
    assert_eq!(refetched.last_observed_at, probed.last_observed_at);

    test.close().await;
}

#[tokio::test]
async fn forwarder_against_unreachable_cluster_surfaces_upstream_error() {
    // S3-surface cluster pointed at an unreachable port: the
    // forwarder gate passes (S3 is supported), then the actual
    // mantad call fails. Should bubble up as a 5xx (not a 4xx),
    // because the failure is upstream and not the operator's fault.
    let test = TestServer::start().await;
    let client = test.root_client();

    let created = client
        .create_storage_cluster()
        .body(unreachable_cluster("primary"))
        .send()
        .await
        .expect("create should succeed")
        .into_inner();

    let err = client
        .get_storage_cluster_summary()
        .id(created.id)
        .send()
        .await
        .expect_err("upstream connect-refused should fail");
    let progenitor_client::Error::ErrorResponse(response) = err else {
        panic!("expected ErrorResponse, got {err:?}");
    };
    assert!(
        response.status().as_u16() >= 500,
        "expected 5xx, got {}",
        response.status().as_u16()
    );

    test.close().await;
}
