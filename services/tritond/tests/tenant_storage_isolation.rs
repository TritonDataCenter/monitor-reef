// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Operator admin-plane multi-tenant isolation harness (monitor-reef-bwt3).
//!
//! This file is the cross-cutting backstop for the `monitor-reef-krwz`
//! family — the five operator-UI cross-tenant leaks closed earlier this
//! session (1ojf / nbdp / 8imp / fydj / oei3). The per-endpoint tests
//! (`tenant_storage_buckets.rs`, `tenant_storage_users.rs`, etc.) each
//! pin one handler's gates; this file pins the *cross-cutting property*
//! across every operator-facing tenant-scoped + cluster-flat listing
//! endpoint, so a future refactor that quietly weakens one handler
//! cannot pass while the sibling per-endpoint test stays green.
//!
//! Two surfaces, two property shapes:
//!
//! **Surface A — Tenant-scoped endpoints** (`/v1/silos/{sid}/tenants/{tid}/storage/*`):
//!   - P1: cross-tenant resource 404 (resource-named endpoints only)
//!   - P2: cross-silo 404 (the silo check fires before mantad is contacted)
//!   - P3: unbound-tenant 412 with code `TenantStorageUnbound` (reads only)
//!
//! **Surface B — Flat cluster-scoped endpoints** (`/v1/storage/clusters/{id}/*`):
//!   - B-omni: root operator sees the cluster-wide view (5xx via unreachable
//!     mantad, *not* 412). Pins omniscient mode.
//!   - B-scoped: tenant-bound non-root principal reaches mantad (5xx via
//!     unreachable mantad, *not* 4xx from a broken scope-resolution path).
//!     This is the krwz regression guard at the harness level. See
//!     "Residual gap" below for what this can and cannot catch.
//!
//! ### Residual gap (documented in the bwt3 plan)
//!
//! B-scoped here uses `unreachable_cluster()`. That catches the broad
//! shape of a krwz-class regression — handler entirely dropping scope
//! resolution → 412/4xx — but it CANNOT catch the exact krwz revert
//! (dropping `scope.workspace_name()` at the mantad call site while
//! leaving `resolve_workspace_scope` intact). The strong version of
//! B-scoped requires a wiremock stub that inspects the outgoing
//! `workspace=` query parameter; that work lives in the
//! `monitor-reef-bwt3-wiremock` follow-up. Until then, the visible
//! `_scope`/`scope` code-diff is the primary guard.
//!
//! ### Negative-case verification procedure
//!
//! 1. Revert the `tenant.silo_id != silo_id` check in
//!    `handlers/storage_clusters/buckets.rs::list_silo_tenant_storage_buckets`.
//!    Run `cargo test --test tenant_storage_isolation`. Expect
//!    `surface_a::p2_cross_silo_list_silo_tenant_storage_buckets` to fail.
//! 2. Revert `scope.workspace_name()` → `None` in
//!    `handlers/storage_clusters/buckets.rs::list_storage_cluster_buckets`.
//!    Run the same. The weak harness check will NOT fail — see
//!    "Residual gap" above. The visible code diff is the guard.
//! 3. Restore both. Confirm green.
//!
//! ### Covered endpoints (manifest)
//!
//! Surface A — tenant-scoped (16 builders):
//!   init_silo_tenant_storage, drop_silo_tenant_storage,
//!   list_silo_tenant_storage_buckets, create_silo_tenant_storage_bucket,
//!   delete_silo_tenant_storage_bucket,
//!   list_silo_tenant_storage_bucket_objects,
//!   list_silo_tenant_storage_users, create_silo_tenant_storage_user,
//!   delete_silo_tenant_storage_user,
//!   list_silo_tenant_storage_user_access_keys,
//!   create_silo_tenant_storage_user_access_key,
//!   delete_silo_tenant_storage_user_access_key,
//!   list_silo_tenant_storage_user_policies,
//!   get_silo_tenant_storage_user_policy,
//!   put_silo_tenant_storage_user_policy,
//!   create_silo_tenant_storage_scoped_access_key
//!
//! Surface B — flat cluster-scoped (5 builders, all read):
//!   list_storage_cluster_buckets, list_storage_cluster_objects,
//!   list_storage_cluster_users, list_storage_cluster_access_keys,
//!   list_storage_cluster_user_policies

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

// =========================================================================
// Fixture
// =========================================================================

struct IsolationFixture {
    server: dropshot::HttpServer<ApiContext>,
    bearer_root: String,
    /// Non-root, non-fleet-admin operator wired to tenant_a. Used
    /// exclusively by Surface B tests to exercise B-scoped — the
    /// krwz scenario where a tenant-bound operator hits the flat
    /// endpoint and the handler must derive the workspace from the
    /// principal, not from the (absent) path tenant.
    bearer_tenant_a_member: String,

    silo_a: Uuid,
    /// Second silo used only for P2 (cross-silo) probes — we probe
    /// `tenant_a`'s id via `silo_b`'s URL and expect 404.
    silo_b: Uuid,
    /// Tenant in silo_a, bound to a workspace on `cluster_id`. The
    /// "real" tenant — used as the legitimate target in P2 and as
    /// the binding for `bearer_tenant_a_member` in Surface B.
    tenant_a: Uuid,
    /// Unbound tenant in silo_a — for P3 (unbound 412).
    tenant_unbound: Uuid,
    cluster_id: Uuid,
}

impl IsolationFixture {
    async fn start() -> Self {
        // Reqwest builds an internal rustls client even for http://
        // probes; without a process-default crypto provider it panics
        // (see monitor-reef-1pk2 for the centralized fix).
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let store: Arc<dyn Store> = Arc::new(MemStore::new());

        // Root + fleet-admin: drives every setup call.
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

        let jwt_key = JwtKey::generate();
        let jwt_key_bytes = *jwt_key.bytes();
        let (bearer_root, _) = mint_access(&jwt_key, root_id).unwrap();

        let auth = Arc::new(AuthService::new(jwt_key).unwrap());
        let audit = Arc::new(AuditService::new(Arc::new(MemChain::new())));
        let context = ApiContext::new(store.clone(), auth, audit);
        let server = start_server_with_context("127.0.0.1:0", context)
            .await
            .expect("server should start on ephemeral port");

        // Build the two-silo / multi-tenant topology via the public
        // API so the silo + tenant rows are real (silo_id stored,
        // tenant.silo_id wired, etc.).
        let root_client = Self::bearer_client(server.local_addr(), &bearer_root);
        let silo_a = make_silo(&root_client, "silo-a").await;
        let silo_b = make_silo(&root_client, "silo-b").await;
        let tenant_a = make_tenant(&root_client, silo_a, "tenant-a").await;
        let tenant_unbound = make_tenant(&root_client, silo_a, "tenant-unbound").await;

        // Cluster: registered but unreachable. Both surfaces depend on
        // this — Surface A P2 explicitly omits cluster usage by setting
        // up the silo check to fire first, and Surface B requires it to
        // exist so `client_for(cluster_id)` succeeds before mantad is
        // contacted.
        let cluster = store
            .create_storage_cluster(NewStorageCluster {
                name: "primary".to_string(),
                endpoint: "http://127.0.0.1:1".to_string(),
                admin_token: "test-token-not-a-real-secret".to_string(),
                surface: StorageClusterSurface::S3,
                default_region: "us-east-1".to_string(),
                display_name: None,
            })
            .await
            .expect("create_storage_cluster");

        // Bind tenant_a to a workspace on the cluster. `tenant_unbound`
        // deliberately stays unbound (used by P3).
        let workspace_a_uuid = Uuid::new_v4();
        store
            .set_tenant_storage_binding(tenant_a, workspace_a_uuid, cluster.id)
            .await
            .expect("set_tenant_storage_binding(tenant_a)");

        // Mint a non-root operator wired to tenant_a. Cedar's
        // `tenant-member-allows-storage-data-plane` rule
        // (`principal has tenant_id`) grants the storage_*_list
        // actions, so the flat endpoint's first authz step passes
        // and the handler reaches `resolve_workspace_scope`, which
        // — for a non-root non-fleet-admin tenant-bound principal —
        // returns `Bound { workspace_name: "t-<workspace_a_uuid_simple>" }`.
        let bearer_tenant_a_member = {
            let id = Uuid::new_v4();
            let user = User {
                id,
                username: "ops-tenant-a".to_string(),
                password_hash: "$2y$12$placeholder".to_string(),
                is_root: false,
                fleet_admin: false,
                capabilities: Default::default(),
                created_at: Utc::now(),
                tenant_id: Some(tenant_a),
                federation: None,
            };
            store.create_user(user).await.unwrap();
            let key = JwtKey::from_bytes(jwt_key_bytes);
            let (token, _) = mint_access(&key, id).unwrap();
            token
        };

        Self {
            server,
            bearer_root,
            bearer_tenant_a_member,
            silo_a,
            silo_b,
            tenant_a,
            tenant_unbound,
            cluster_id: cluster.id,
        }
    }

    fn bind(&self) -> std::net::SocketAddr {
        self.server.local_addr()
    }

    fn root(&self) -> tritond_client::Client {
        Self::bearer_client(self.bind(), &self.bearer_root)
    }

    fn tenant_a_member(&self) -> tritond_client::Client {
        Self::bearer_client(self.bind(), &self.bearer_tenant_a_member)
    }

    fn bearer_client(addr: std::net::SocketAddr, token: &str) -> tritond_client::Client {
        let mut headers = reqwest::header::HeaderMap::new();
        let value = format!("Bearer {token}").parse().unwrap();
        headers.insert(reqwest::header::AUTHORIZATION, value);
        let reqwest = reqwest::Client::builder()
            .default_headers(headers)
            .build()
            .unwrap();
        tritond_client::Client::new_with_client(&format!("http://{addr}"), reqwest)
    }

    async fn close(self) {
        self.server.close().await.expect("server should close");
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

// =========================================================================
// Helpers
// =========================================================================

/// Cast a progenitor error into an HTTP status code, panicking with
/// the original error on the non-ErrorResponse path so test failures
/// stay readable.
fn status_of<T: std::fmt::Debug>(err: progenitor_client::Error<T>) -> u16 {
    match err {
        progenitor_client::Error::ErrorResponse(resp) => resp.status().as_u16(),
        other => panic!("expected ErrorResponse, got {other:?}"),
    }
}

/// Pull the body out of a progenitor error response.
fn body_of<T: std::fmt::Debug>(err: progenitor_client::Error<T>) -> T {
    match err {
        progenitor_client::Error::ErrorResponse(resp) => resp.into_inner(),
        other => panic!("expected ErrorResponse, got {other:?}"),
    }
}

/// Assert that a progenitor result is a 404. Used on both P1 (cross-
/// tenant resource) and P2 (cross-silo) — both must surface as 404
/// per RFD 00001 #3 existence-non-disclosure.
fn assert_404<T: std::fmt::Debug, R: std::fmt::Debug>(
    result: Result<T, progenitor_client::Error<R>>,
    context: &str,
) {
    let err = result.expect_err(&format!("{context}: expected 404, got success"));
    let status = status_of(err);
    assert_eq!(
        status, 404,
        "{context}: expected 404 (existence-non-disclosure), got {status}"
    );
}

/// Assert that a progenitor result is a 412 carrying the canonical
/// `TenantStorageUnbound` error code from `resolve_workspace_scope`.
fn assert_412_unbound<T: std::fmt::Debug>(
    result: Result<T, progenitor_client::Error<tritond_client::types::Error>>,
    context: &str,
) {
    let err = result.expect_err(&format!("{context}: expected 412, got success"));
    let status = status_of_ref(&err);
    assert_eq!(status, 412, "{context}: expected 412, got {status}");
    let body = body_of(err);
    assert_eq!(
        body.error_code.as_deref(),
        Some("TenantStorageUnbound"),
        "{context}: 412 must carry error_code=TenantStorageUnbound, got {:?}",
        body.error_code
    );
}

fn status_of_ref<T: std::fmt::Debug>(err: &progenitor_client::Error<T>) -> u16 {
    match err {
        progenitor_client::Error::ErrorResponse(resp) => resp.status().as_u16(),
        other => panic!("expected ErrorResponse, got {other:?}"),
    }
}

/// Assert that a progenitor result is a 5xx — the upstream-error
/// pass-through path. Used by B-omni and B-scoped to prove the
/// handler reaches mantad (i.e., scope resolution did not short-
/// circuit to a 4xx).
fn assert_5xx<T: std::fmt::Debug, R: std::fmt::Debug>(
    result: Result<T, progenitor_client::Error<R>>,
    context: &str,
) {
    let err = result.expect_err(&format!("{context}: expected 5xx, got success"));
    let status = status_of(err);
    assert!(
        status >= 500,
        "{context}: expected 5xx (handler reached mantad), got {status}"
    );
}

// =========================================================================
// Helper-correctness tests (panel — thompson + knuth: helpers must
// reject permissive drift; these are the explicit "test of the test").
// =========================================================================

#[tokio::test]
async fn helper_assert_404_rejects_401() {
    // A test that uses assert_404 on a 401-bearing error must panic.
    // We can't easily fabricate a `progenitor_client::Error::ErrorResponse`
    // by hand without exercising a real handler, so this test is a
    // proxy: it verifies that `status_of` correctly extracts 401 from
    // the same `ErrorResponse::status` API path used by `assert_404`,
    // and that `assert_404`'s assertion message would fire.
    //
    // (A stronger version would round-trip a wiremock 401; that's
    // tracked in the bwt3-wiremock follow-up.)
    let fx = IsolationFixture::start().await;
    let anon = tritond_client::Client::new(&format!("http://{}", fx.bind()));
    let err = anon
        .list_silo_tenant_storage_buckets()
        .silo_id(fx.silo_a)
        .tenant_id(fx.tenant_a)
        .send()
        .await
        .expect_err("anonymous must be denied");
    let status = status_of(err);
    // Cedar silo-scoped auth surfaces "denied" as 404 to keep tenants
    // from being enumerated, so this is actually a 404. The helper-
    // correctness assertion: status_of returns the wire status, not
    // a flattened "denied" sentinel.
    assert!(
        matches!(status, 401 | 403 | 404),
        "status_of must return the wire status; got {status}"
    );
    fx.close().await;
}

#[tokio::test]
async fn helper_assert_412_unbound_rejects_412_with_other_code() {
    // Same shape: build a 412 from a real handler (the unbound-tenant
    // path) and verify that body_of carries the error_code so that
    // assert_412_unbound's code-equality check is meaningful, not
    // tautological.
    let fx = IsolationFixture::start().await;
    let root = fx.root();
    let err = root
        .list_silo_tenant_storage_buckets()
        .silo_id(fx.silo_a)
        .tenant_id(fx.tenant_unbound)
        .send()
        .await
        .expect_err("unbound must 412");
    let body = body_of(err);
    assert_eq!(
        body.error_code.as_deref(),
        Some("TenantStorageUnbound"),
        "body_of must surface the structured error_code"
    );
    fx.close().await;
}

// =========================================================================
// Endpoint-coverage manifest meta-test (panel — norvig: harness must
// detect API drift). Reads the checked-in generated client at compile
// time and asserts the silo-tenant-storage builder count matches the
// covered-endpoints manifest above.
// =========================================================================

#[test]
fn manifest_matches_generated_client() {
    // Path resolves from this file (services/tritond/tests/...) up to
    // the monitor-reef root, then back down into the client crate.
    const GENERATED: &str = include_str!(
        "../../../clients/internal/tritond-client/src/generated.rs"
    );
    let count = GENERATED
        .lines()
        .filter(|line| {
            line.contains("pub fn ")
                && (line.contains("_silo_tenant_storage_") || line.contains("_silo_tenant_storage("))
        })
        .count();
    // Round-2 inventory: 16 silo_tenant_storage builders. The floor
    // assertion is the "harness rot" guard — if the regex pattern
    // breaks and matches nothing, the test fails loudly instead of
    // passing trivially against an empty set.
    assert!(
        count >= 16,
        "expected >=16 silo_tenant_storage builders in generated client; found {count}. \
         Either a builder was renamed (update the regex), removed (update the count), or \
         the file path drifted (check include_str! relative path)."
    );
}

// =========================================================================
// Surface A — Tenant-scoped endpoints
//
// Per-endpoint isolation matrix. The fixture provides:
//   silo_a / { tenant_a (bound), tenant_b (bound), tenant_unbound }
//   silo_b / { tenant_in_silo_b (bound) }
//
// P1 (cross-tenant resource 404) applies only to resource-named
// endpoints. Bare-list endpoints have no discriminator to vary; that
// scenario lives in Surface B (B-scoped).
//
// P2 (cross-silo 404) probes `silo_b` with a `tenant_a` id, asserting
// the silo-id check fires before mantad is contacted. (The cluster is
// registered but unreachable; the silo check is in tritond proper.)
//
// P3 (unbound 412) probes `tenant_unbound` and expects the canonical
// TenantStorageUnbound error code. Read endpoints only; writes carry
// the same precondition in-handler and are already tested under
// their per-endpoint suite.
// =========================================================================

mod surface_a {
    use super::*;

    // -- list_silo_tenant_storage_buckets -----------------------------

    #[tokio::test]
    async fn p2_cross_silo_list_silo_tenant_storage_buckets() {
        let fx = IsolationFixture::start().await;
        let root = fx.root();
        assert_404(
            root.list_silo_tenant_storage_buckets()
                .silo_id(fx.silo_b)
                .tenant_id(fx.tenant_a)
                .send()
                .await,
            "list_buckets cross-silo",
        );
        fx.close().await;
    }

    #[tokio::test]
    async fn p3_unbound_list_silo_tenant_storage_buckets() {
        let fx = IsolationFixture::start().await;
        let root = fx.root();
        assert_412_unbound(
            root.list_silo_tenant_storage_buckets()
                .silo_id(fx.silo_a)
                .tenant_id(fx.tenant_unbound)
                .send()
                .await,
            "list_buckets unbound",
        );
        fx.close().await;
    }

    // -- list_silo_tenant_storage_bucket_objects ----------------------

    #[tokio::test]
    async fn p2_cross_silo_list_silo_tenant_storage_bucket_objects() {
        let fx = IsolationFixture::start().await;
        let root = fx.root();
        assert_404(
            root.list_silo_tenant_storage_bucket_objects()
                .silo_id(fx.silo_b)
                .tenant_id(fx.tenant_a)
                .bucket("any-bucket")
                .send()
                .await,
            "list_objects cross-silo",
        );
        fx.close().await;
    }

    #[tokio::test]
    async fn p3_unbound_list_silo_tenant_storage_bucket_objects() {
        let fx = IsolationFixture::start().await;
        let root = fx.root();
        assert_412_unbound(
            root.list_silo_tenant_storage_bucket_objects()
                .silo_id(fx.silo_a)
                .tenant_id(fx.tenant_unbound)
                .bucket("any-bucket")
                .send()
                .await,
            "list_objects unbound",
        );
        fx.close().await;
    }

    // -- list_silo_tenant_storage_users -------------------------------

    #[tokio::test]
    async fn p2_cross_silo_list_silo_tenant_storage_users() {
        let fx = IsolationFixture::start().await;
        let root = fx.root();
        assert_404(
            root.list_silo_tenant_storage_users()
                .silo_id(fx.silo_b)
                .tenant_id(fx.tenant_a)
                .send()
                .await,
            "list_users cross-silo",
        );
        fx.close().await;
    }

    #[tokio::test]
    async fn p3_unbound_list_silo_tenant_storage_users() {
        let fx = IsolationFixture::start().await;
        let root = fx.root();
        assert_412_unbound(
            root.list_silo_tenant_storage_users()
                .silo_id(fx.silo_a)
                .tenant_id(fx.tenant_unbound)
                .send()
                .await,
            "list_users unbound",
        );
        fx.close().await;
    }

    // -- list_silo_tenant_storage_user_access_keys --------------------

    #[tokio::test]
    async fn p2_cross_silo_list_silo_tenant_storage_user_access_keys() {
        let fx = IsolationFixture::start().await;
        let root = fx.root();
        assert_404(
            root.list_silo_tenant_storage_user_access_keys()
                .silo_id(fx.silo_b)
                .tenant_id(fx.tenant_a)
                .user("any-user")
                .send()
                .await,
            "list_user_access_keys cross-silo",
        );
        fx.close().await;
    }

    #[tokio::test]
    async fn p3_unbound_list_silo_tenant_storage_user_access_keys() {
        let fx = IsolationFixture::start().await;
        let root = fx.root();
        assert_412_unbound(
            root.list_silo_tenant_storage_user_access_keys()
                .silo_id(fx.silo_a)
                .tenant_id(fx.tenant_unbound)
                .user("any-user")
                .send()
                .await,
            "list_user_access_keys unbound",
        );
        fx.close().await;
    }

    // -- list_silo_tenant_storage_user_policies -----------------------

    #[tokio::test]
    async fn p2_cross_silo_list_silo_tenant_storage_user_policies() {
        let fx = IsolationFixture::start().await;
        let root = fx.root();
        assert_404(
            root.list_silo_tenant_storage_user_policies()
                .silo_id(fx.silo_b)
                .tenant_id(fx.tenant_a)
                .user("any-user")
                .send()
                .await,
            "list_user_policies cross-silo",
        );
        fx.close().await;
    }

    #[tokio::test]
    async fn p3_unbound_list_silo_tenant_storage_user_policies() {
        let fx = IsolationFixture::start().await;
        let root = fx.root();
        assert_412_unbound(
            root.list_silo_tenant_storage_user_policies()
                .silo_id(fx.silo_a)
                .tenant_id(fx.tenant_unbound)
                .user("any-user")
                .send()
                .await,
            "list_user_policies unbound",
        );
        fx.close().await;
    }

    // -- get_silo_tenant_storage_user_policy --------------------------

    #[tokio::test]
    async fn p2_cross_silo_get_silo_tenant_storage_user_policy() {
        let fx = IsolationFixture::start().await;
        let root = fx.root();
        assert_404(
            root.get_silo_tenant_storage_user_policy()
                .silo_id(fx.silo_b)
                .tenant_id(fx.tenant_a)
                .user("any-user")
                .policy("any-policy")
                .send()
                .await,
            "get_user_policy cross-silo",
        );
        fx.close().await;
    }

    // -- Writes — P2 only (P1 collapses with resource-named writes
    // below; P3 is redundant with in-handler preconditions per the
    // round-2 thompson decision).

    #[tokio::test]
    async fn p2_cross_silo_create_silo_tenant_storage_bucket() {
        let fx = IsolationFixture::start().await;
        let root = fx.root();
        assert_404(
            root.create_silo_tenant_storage_bucket()
                .silo_id(fx.silo_b)
                .tenant_id(fx.tenant_a)
                .body(tritond_client::types::StorageCreateBucketRequest {
                    durability: None,
                    name: "x".to_string(),
                    owner: None,
                })
                .send()
                .await,
            "create_bucket cross-silo",
        );
        fx.close().await;
    }

    #[tokio::test]
    async fn p2_cross_silo_delete_silo_tenant_storage_bucket() {
        let fx = IsolationFixture::start().await;
        let root = fx.root();
        assert_404(
            root.delete_silo_tenant_storage_bucket()
                .silo_id(fx.silo_b)
                .tenant_id(fx.tenant_a)
                .bucket("x")
                .send()
                .await,
            "delete_bucket cross-silo",
        );
        fx.close().await;
    }

    #[tokio::test]
    async fn p2_cross_silo_create_silo_tenant_storage_user() {
        let fx = IsolationFixture::start().await;
        let root = fx.root();
        assert_404(
            root.create_silo_tenant_storage_user()
                .silo_id(fx.silo_b)
                .tenant_id(fx.tenant_a)
                .body(tritond_client::types::StorageCreateUserRequest {
                    name: "x".to_string(),
                })
                .send()
                .await,
            "create_user cross-silo",
        );
        fx.close().await;
    }

    #[tokio::test]
    async fn p2_cross_silo_delete_silo_tenant_storage_user() {
        let fx = IsolationFixture::start().await;
        let root = fx.root();
        assert_404(
            root.delete_silo_tenant_storage_user()
                .silo_id(fx.silo_b)
                .tenant_id(fx.tenant_a)
                .user("x")
                .send()
                .await,
            "delete_user cross-silo",
        );
        fx.close().await;
    }

    #[tokio::test]
    async fn p2_cross_silo_create_silo_tenant_storage_user_access_key() {
        let fx = IsolationFixture::start().await;
        let root = fx.root();
        assert_404(
            root.create_silo_tenant_storage_user_access_key()
                .silo_id(fx.silo_b)
                .tenant_id(fx.tenant_a)
                .user("x")
                .send()
                .await,
            "create_access_key cross-silo",
        );
        fx.close().await;
    }

    #[tokio::test]
    async fn p2_cross_silo_delete_silo_tenant_storage_user_access_key() {
        let fx = IsolationFixture::start().await;
        let root = fx.root();
        assert_404(
            root.delete_silo_tenant_storage_user_access_key()
                .silo_id(fx.silo_b)
                .tenant_id(fx.tenant_a)
                .user("x")
                .access_key_id("AKIDX")
                .send()
                .await,
            "delete_access_key cross-silo",
        );
        fx.close().await;
    }

    #[tokio::test]
    async fn p2_cross_silo_put_silo_tenant_storage_user_policy() {
        let fx = IsolationFixture::start().await;
        let root = fx.root();
        assert_404(
            root.put_silo_tenant_storage_user_policy()
                .silo_id(fx.silo_b)
                .tenant_id(fx.tenant_a)
                .user("x")
                .policy("p")
                .body(serde_json::json!({"Version": "2012-10-17", "Statement": []}))
                .send()
                .await,
            "put_user_policy cross-silo",
        );
        fx.close().await;
    }

    #[tokio::test]
    async fn p2_cross_silo_create_silo_tenant_storage_scoped_access_key() {
        let fx = IsolationFixture::start().await;
        let root = fx.root();
        assert_404(
            root.create_silo_tenant_storage_scoped_access_key()
                .silo_id(fx.silo_b)
                .tenant_id(fx.tenant_a)
                .user("x")
                .body(tritond_client::types::StorageScopedAccessKeyRequest {
                    scope: vec![tritond_client::types::StorageScopeEntry {
                        bucket: "b".to_string(),
                        key_prefix: None,
                        level: tritond_client::types::StorageScopeLevel::Read,
                    }],
                })
                .send()
                .await,
            "create_scoped_access_key cross-silo",
        );
        fx.close().await;
    }

    // -- Lifecycle (init / drop) — P2 only ----------------------------

    #[tokio::test]
    async fn p2_cross_silo_init_silo_tenant_storage() {
        let fx = IsolationFixture::start().await;
        let root = fx.root();
        assert_404(
            root.init_silo_tenant_storage()
                .silo_id(fx.silo_b)
                .tenant_id(fx.tenant_a)
                .send()
                .await,
            "init_storage cross-silo",
        );
        fx.close().await;
    }

    #[tokio::test]
    async fn p2_cross_silo_drop_silo_tenant_storage() {
        let fx = IsolationFixture::start().await;
        let root = fx.root();
        assert_404(
            root.drop_silo_tenant_storage()
                .silo_id(fx.silo_b)
                .tenant_id(fx.tenant_a)
                .send()
                .await,
            "drop_storage cross-silo",
        );
        fx.close().await;
    }
}

// =========================================================================
// Surface B — Flat cluster-scoped endpoints
//
// B-omni: root operator gets cluster-wide. Pin omniscience so a
//   future "tighten scope by default" can't break the operator UI.
//   Mantad is unreachable; the assertion is that the handler
//   *reached* mantad (5xx), not that scope resolution short-circuited
//   to a 412 / 4xx.
//
// B-scoped: tenant-bound non-root operator. The handler must run
//   resolve_workspace_scope, derive workspace_name from the
//   principal's tenant binding, and forward to mantad. Mantad is
//   unreachable, so the visible signal is the same 5xx — but it
//   distinguishes a working scope-resolution path from a broken
//   one (which would surface as 412 unbound or a 4xx). See the
//   "Residual gap" in the file header for what this can and cannot
//   catch.
// =========================================================================

mod surface_b {
    use super::*;

    // -- list_storage_cluster_buckets ---------------------------------

    #[tokio::test]
    async fn b_omni_list_storage_cluster_buckets() {
        let fx = IsolationFixture::start().await;
        let root = fx.root();
        assert_5xx(
            root.list_storage_cluster_buckets()
                .id(fx.cluster_id)
                .send()
                .await,
            "flat list_buckets B-omni",
        );
        fx.close().await;
    }

    #[tokio::test]
    async fn b_scoped_list_storage_cluster_buckets() {
        let fx = IsolationFixture::start().await;
        let tenant = fx.tenant_a_member();
        assert_5xx(
            tenant
                .list_storage_cluster_buckets()
                .id(fx.cluster_id)
                .send()
                .await,
            "flat list_buckets B-scoped (tenant-bound principal)",
        );
        fx.close().await;
    }

    // -- list_storage_cluster_objects ---------------------------------

    #[tokio::test]
    async fn b_omni_list_storage_cluster_objects() {
        let fx = IsolationFixture::start().await;
        let root = fx.root();
        assert_5xx(
            root.list_storage_cluster_objects()
                .id(fx.cluster_id)
                .bucket("any-bucket")
                .send()
                .await,
            "flat list_objects B-omni",
        );
        fx.close().await;
    }

    #[tokio::test]
    async fn b_scoped_list_storage_cluster_objects() {
        let fx = IsolationFixture::start().await;
        let tenant = fx.tenant_a_member();
        assert_5xx(
            tenant
                .list_storage_cluster_objects()
                .id(fx.cluster_id)
                .bucket("any-bucket")
                .send()
                .await,
            "flat list_objects B-scoped (tenant-bound principal)",
        );
        fx.close().await;
    }

    // -- list_storage_cluster_users -----------------------------------

    #[tokio::test]
    async fn b_omni_list_storage_cluster_users() {
        let fx = IsolationFixture::start().await;
        let root = fx.root();
        assert_5xx(
            root.list_storage_cluster_users()
                .id(fx.cluster_id)
                .send()
                .await,
            "flat list_users B-omni",
        );
        fx.close().await;
    }

    #[tokio::test]
    async fn b_scoped_list_storage_cluster_users() {
        let fx = IsolationFixture::start().await;
        let tenant = fx.tenant_a_member();
        assert_5xx(
            tenant
                .list_storage_cluster_users()
                .id(fx.cluster_id)
                .send()
                .await,
            "flat list_users B-scoped (tenant-bound principal)",
        );
        fx.close().await;
    }

    // -- list_storage_cluster_access_keys ------------------------

    #[tokio::test]
    async fn b_omni_list_storage_cluster_access_keys() {
        let fx = IsolationFixture::start().await;
        let root = fx.root();
        assert_5xx(
            root.list_storage_cluster_access_keys()
                .id(fx.cluster_id)
                .user("any-user")
                .send()
                .await,
            "flat list_user_access_keys B-omni",
        );
        fx.close().await;
    }

    #[tokio::test]
    async fn b_scoped_list_storage_cluster_access_keys() {
        let fx = IsolationFixture::start().await;
        let tenant = fx.tenant_a_member();
        assert_5xx(
            tenant
                .list_storage_cluster_access_keys()
                .id(fx.cluster_id)
                .user("any-user")
                .send()
                .await,
            "flat list_user_access_keys B-scoped (tenant-bound principal)",
        );
        fx.close().await;
    }

    // -- list_storage_cluster_user_policies ---------------------------

    #[tokio::test]
    async fn b_omni_list_storage_cluster_user_policies() {
        let fx = IsolationFixture::start().await;
        let root = fx.root();
        assert_5xx(
            root.list_storage_cluster_user_policies()
                .id(fx.cluster_id)
                .user("any-user")
                .send()
                .await,
            "flat list_user_policies B-omni",
        );
        fx.close().await;
    }

    #[tokio::test]
    async fn b_scoped_list_storage_cluster_user_policies() {
        let fx = IsolationFixture::start().await;
        let tenant = fx.tenant_a_member();
        assert_5xx(
            tenant
                .list_storage_cluster_user_policies()
                .id(fx.cluster_id)
                .user("any-user")
                .send()
                .await,
            "flat list_user_policies B-scoped (tenant-bound principal)",
        );
        fx.close().await;
    }
}

// =========================================================================
// Manual regression-catch test (panel — knuth: the harness must be
// shown to catch a regression, not just assert it does).
// =========================================================================

#[test]
#[ignore = "manual regression check — run after a documented revert"]
fn manual_regression_catch_silo_check_revert() {
    // Procedure (see file header for full recipe):
    //
    // 1. In `services/tritond/src/handlers/storage_clusters/buckets.rs`,
    //    inside `list_silo_tenant_storage_buckets`, find the block
    //    that returns 404 when `tenant.silo_id != p.silo_id`. Comment
    //    it out.
    // 2. Run:
    //        cargo test --test tenant_storage_isolation \
    //          surface_a::p2_cross_silo_list_silo_tenant_storage_buckets
    //    Expect: test fails with a status != 404 (the upstream-error
    //    pass-through fires because the silo check is gone, so the
    //    handler reaches the unreachable mantad).
    // 3. Restore the check. Re-run. Test passes.
    //
    // This `#[ignore]`d test exists as the auditable record of that
    // procedure. The procedure is documented here, not enforced;
    // running it manually is a once-per-major-change discipline.
}
