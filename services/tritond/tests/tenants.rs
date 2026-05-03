// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

#![allow(clippy::unwrap_used, clippy::expect_used)]

//! End-to-end tests for the `/v2/silos/{silo_id}/tenants` surface,
//! exercised through the auth gate by minting a root-operator JWT
//! and presenting it as a `Bearer` token. Mirrors the harness shape
//! used in `silos.rs`.

use std::sync::Arc;

use chrono::Utc;
use tritond::audit::AuditService;
use tritond::auth::AuthService;
use tritond::{ApiContext, start_server_with_context};
use tritond_audit::MemChain;
use tritond_auth::{JwtKey, RedactedString, hash_password, mint_access};
use tritond_client::types::{NewSilo, NewTenant};
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

/// Helper: create a silo and return its id.
async fn make_silo(client: &tritond_client::Client, name: &str) -> Uuid {
    client
        .create_silo()
        .body(NewSilo {
            name: name.to_string(),
            description: None,
        })
        .send()
        .await
        .expect("create_silo should succeed")
        .into_inner()
        .id
}

#[tokio::test]
async fn root_creates_then_lists_tenant_in_silo() {
    let test = TestServer::start().await;
    let client = test.authed_client();

    let silo_id = make_silo(&client, "ops-silo").await;

    let created = client
        .create_silo_tenant()
        .silo_id(silo_id)
        .body(NewTenant {
            name: "acme".to_string(),
            description: Some("the acme tenant".to_string()),
        })
        .send()
        .await
        .expect("create_silo_tenant should succeed")
        .into_inner();

    assert_eq!(created.silo_id, silo_id);
    assert_eq!(created.name, "acme");
    assert_eq!(created.description, "the acme tenant");

    let listed = client
        .list_silo_tenants()
        .silo_id(silo_id)
        .send()
        .await
        .expect("list_silo_tenants should succeed")
        .into_inner();

    // The silo's auto-created `default` tenant plus our new acme.
    assert!(
        listed.iter().any(|t| t.id == created.id && t.name == "acme"),
        "freshly-created tenant should appear in the silo's tenant list"
    );

    test.close().await;
}

#[tokio::test]
async fn cross_silo_tenant_get_returns_404() {
    let test = TestServer::start().await;
    let client = test.authed_client();

    let silo_a = make_silo(&client, "silo-a").await;
    let silo_b = make_silo(&client, "silo-b").await;

    let tenant_a = client
        .create_silo_tenant()
        .silo_id(silo_a)
        .body(NewTenant {
            name: "tenant-a".to_string(),
            description: None,
        })
        .send()
        .await
        .expect("create tenant-a should succeed")
        .into_inner();

    // Probing tenant-a under silo-b's URL must surface as 404 — never
    // 200 (which would leak existence) and never 403 (which would
    // leak that some other silo owns it).
    let err = client
        .get_silo_tenant()
        .silo_id(silo_b)
        .tenant_id(tenant_a.id)
        .send()
        .await
        .expect_err("cross-silo probe must fail");
    let progenitor_client::Error::ErrorResponse(response) = err else {
        panic!("expected ErrorResponse, got {err:?}");
    };
    assert_eq!(
        response.status().as_u16(),
        404,
        "cross-silo tenant probe must return 404, not 200/403"
    );

    test.close().await;
}

#[tokio::test]
async fn duplicate_tenant_name_in_same_silo_returns_409() {
    let test = TestServer::start().await;
    let client = test.authed_client();

    let silo_id = make_silo(&client, "dup-silo").await;

    client
        .create_silo_tenant()
        .silo_id(silo_id)
        .body(NewTenant {
            name: "engineering".to_string(),
            description: None,
        })
        .send()
        .await
        .expect("first create should succeed");

    let err = client
        .create_silo_tenant()
        .silo_id(silo_id)
        .body(NewTenant {
            name: "engineering".to_string(),
            description: None,
        })
        .send()
        .await
        .expect_err("duplicate tenant name should be rejected");

    let progenitor_client::Error::ErrorResponse(response) = err else {
        panic!("expected ErrorResponse, got {err:?}");
    };
    assert_eq!(response.status().as_u16(), 409);

    test.close().await;
}

#[tokio::test]
async fn same_tenant_name_in_different_silos_is_allowed() {
    let test = TestServer::start().await;
    let client = test.authed_client();

    let silo_a = make_silo(&client, "sibling-a").await;
    let silo_b = make_silo(&client, "sibling-b").await;

    let a = client
        .create_silo_tenant()
        .silo_id(silo_a)
        .body(NewTenant {
            name: "shared-name".to_string(),
            description: None,
        })
        .send()
        .await
        .expect("create in silo-a should succeed")
        .into_inner();
    let b = client
        .create_silo_tenant()
        .silo_id(silo_b)
        .body(NewTenant {
            name: "shared-name".to_string(),
            description: None,
        })
        .send()
        .await
        .expect("create in silo-b should succeed")
        .into_inner();

    assert_ne!(a.id, b.id);
    assert_eq!(a.silo_id, silo_a);
    assert_eq!(b.silo_id, silo_b);

    test.close().await;
}

#[tokio::test]
async fn anonymous_is_rejected_on_every_endpoint() {
    let test = TestServer::start().await;
    let authed = test.authed_client();
    let anon = test.anonymous_client();

    let silo_id = make_silo(&authed, "anon-probe-silo").await;
    let tenant = authed
        .create_silo_tenant()
        .silo_id(silo_id)
        .body(NewTenant {
            name: "probe-target".to_string(),
            description: None,
        })
        .send()
        .await
        .expect("setup tenant should succeed")
        .into_inner();

    // List: anonymous → 404 (silo-scoped not-found shape).
    let err = anon
        .list_silo_tenants()
        .silo_id(silo_id)
        .send()
        .await
        .expect_err("anonymous list should fail");
    let progenitor_client::Error::ErrorResponse(response) = err else {
        panic!("expected ErrorResponse, got {err:?}");
    };
    assert_eq!(response.status().as_u16(), 404);

    // Create: anonymous → 404 (cross-silo deny conflated with not-found).
    let err = anon
        .create_silo_tenant()
        .silo_id(silo_id)
        .body(NewTenant {
            name: "intruder".to_string(),
            description: None,
        })
        .send()
        .await
        .expect_err("anonymous create should fail");
    let progenitor_client::Error::ErrorResponse(response) = err else {
        panic!("expected ErrorResponse, got {err:?}");
    };
    assert_eq!(response.status().as_u16(), 404);

    // Get: anonymous → 404.
    let err = anon
        .get_silo_tenant()
        .silo_id(silo_id)
        .tenant_id(tenant.id)
        .send()
        .await
        .expect_err("anonymous get should fail");
    let progenitor_client::Error::ErrorResponse(response) = err else {
        panic!("expected ErrorResponse, got {err:?}");
    };
    assert_eq!(response.status().as_u16(), 404);

    // Delete: anonymous → 404.
    let err = anon
        .delete_silo_tenant()
        .silo_id(silo_id)
        .tenant_id(tenant.id)
        .send()
        .await
        .expect_err("anonymous delete should fail");
    let progenitor_client::Error::ErrorResponse(response) = err else {
        panic!("expected ErrorResponse, got {err:?}");
    };
    assert_eq!(response.status().as_u16(), 404);

    test.close().await;
}

#[tokio::test]
async fn fresh_silo_has_listable_default_tenant() {
    // E-2 made silo creation atomically also create a `default`
    // tenant. Surface that here through the wire-level list so a
    // future regression in either the store or the wire would
    // trip immediately.
    let test = TestServer::start().await;
    let client = test.authed_client();

    let silo_id = make_silo(&client, "fresh-silo").await;

    let listed = client
        .list_silo_tenants()
        .silo_id(silo_id)
        .send()
        .await
        .expect("list_silo_tenants on fresh silo should succeed")
        .into_inner();

    assert!(
        listed.iter().any(|t| t.name == "default"),
        "freshly-created silo must have a `default` tenant immediately listable, got {:?}",
        listed.iter().map(|t| &t.name).collect::<Vec<_>>()
    );

    test.close().await;
}

#[tokio::test]
async fn delete_tenant_removes_it_from_list() {
    let test = TestServer::start().await;
    let client = test.authed_client();

    let silo_id = make_silo(&client, "delete-silo").await;
    let tenant = client
        .create_silo_tenant()
        .silo_id(silo_id)
        .body(NewTenant {
            name: "ephemeral".to_string(),
            description: None,
        })
        .send()
        .await
        .expect("create should succeed")
        .into_inner();

    client
        .delete_silo_tenant()
        .silo_id(silo_id)
        .tenant_id(tenant.id)
        .send()
        .await
        .expect("delete should succeed");

    let listed = client
        .list_silo_tenants()
        .silo_id(silo_id)
        .send()
        .await
        .expect("list should succeed")
        .into_inner();
    assert!(
        !listed.iter().any(|t| t.id == tenant.id),
        "deleted tenant must not appear in subsequent list"
    );

    // And the get becomes a 404.
    let err = client
        .get_silo_tenant()
        .silo_id(silo_id)
        .tenant_id(tenant.id)
        .send()
        .await
        .expect_err("get on deleted tenant should fail");
    let progenitor_client::Error::ErrorResponse(response) = err else {
        panic!("expected ErrorResponse, got {err:?}");
    };
    assert_eq!(response.status().as_u16(), 404);

    test.close().await;
}

#[tokio::test]
async fn delete_nonexistent_tenant_returns_404() {
    let test = TestServer::start().await;
    let client = test.authed_client();

    let silo_id = make_silo(&client, "void-silo").await;

    let err = client
        .delete_silo_tenant()
        .silo_id(silo_id)
        .tenant_id(Uuid::new_v4())
        .send()
        .await
        .expect_err("delete of nonexistent tenant should fail");

    let progenitor_client::Error::ErrorResponse(response) = err else {
        panic!("expected ErrorResponse, got {err:?}");
    };
    assert_eq!(response.status().as_u16(), 404);

    test.close().await;
}
