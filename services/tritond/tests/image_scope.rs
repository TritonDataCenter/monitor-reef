// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Visibility / ownership matrix for the multi-scope image
//! catalog (slice F).
//!
//! The other image test files cover sha256 / size validation,
//! name uniqueness within a single scope, and the request-shape
//! happy path. This file is the load-bearing security-test
//! surface — every variant of [`tritond_client::types::ImageScope`]
//! is exercised against multiple principals (root, same-tenant
//! member, cross-tenant member, cross-silo member, anonymous)
//! and the answers are pinned.
//!
//! A wrong filter here is a cross-tenant information leak, so
//! the cases below are intentionally redundant — every scope
//! lists out who *can* see and who *cannot* see, rather than
//! relying on a single "this should fail" assertion.

use std::net::SocketAddr;
use std::sync::Arc;

use chrono::Utc;
use tritond::audit::AuditService;
use tritond::auth::AuthService;
use tritond::{ApiContext, start_server_with_context};
use tritond_audit::MemChain;
use tritond_auth::{JwtKey, RedactedString, hash_password, mint_access};
use tritond_client::types::{
    ImageScope, NewImage, NewProject, NewSilo, NewSshKey, NewSubnet, NewVpc,
};
use tritond_store::{MemStore, Store, User};
use uuid::Uuid;

const ROOT_PASSWORD: &str = "image-scope-test";

// ---------- harness ----------

struct TestServer {
    server: dropshot::HttpServer<ApiContext>,
    store: Arc<dyn Store>,
    jwt_key_bytes: [u8; 32],
    root_bearer: String,
    root_user_id: Uuid,
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
            created_at: Utc::now(),
            tenant_id: None,
            federation: None,
        };
        store.create_user(user).await.unwrap();
        let jwt_key = JwtKey::generate();
        let jwt_key_bytes = *jwt_key.bytes();
        let (token, _) = mint_access(&jwt_key, user_id).unwrap();
        let auth = Arc::new(AuthService::new(jwt_key).unwrap());
        let audit = Arc::new(AuditService::new(Arc::new(MemChain::new())));
        let context = ApiContext::new(store.clone(), auth, audit);
        let server = start_server_with_context("127.0.0.1:0", context)
            .await
            .unwrap();
        Self {
            server,
            store,
            jwt_key_bytes,
            root_bearer: token,
            root_user_id: user_id,
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

    /// Mint a non-root operator bound to the supplied tenant
    /// (its silo is derived at auth time via the tenant
    /// lookup), returning the user_id and an access token.
    /// Bypasses the bcrypt password path entirely — the test
    /// authenticates via the JWT directly.
    async fn make_tenant_user(&self, tenant_id: Uuid, username: &str) -> (Uuid, String) {
        let id = Uuid::new_v4();
        let user = User {
            id,
            username: username.to_string(),
            password_hash: "$2y$12$placeholder".to_string(),
            is_root: false,
            fleet_admin: false,
            created_at: Utc::now(),
            tenant_id: Some(tenant_id),
            federation: None,
        };
        self.store.create_user(user).await.unwrap();
        // JwtKey doesn't impl Clone (its Drop zeroes the bytes),
        // so reconstruct it from the saved bytes for each mint.
        let key = JwtKey::from_bytes(self.jwt_key_bytes);
        let (token, _) = mint_access(&key, id).unwrap();
        (id, token)
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

fn standard_image(name: &str) -> NewImage {
    NewImage {
        name: name.to_string(),
        description: None,
        os: "linux".to_string(),
        version: "ubuntu-22.04".to_string(),
        size_bytes: 1_000_000_000,
        sha256: image_sha(name),
        source_url: Some("mantafs://images/test".to_string()),
        id: None,
        compatibility: None,
    }
}

/// Spread sha256 across the test image names so the new
/// content-addressed id derivation (per scope + sha256) doesn't
/// collide across fixtures.
fn image_sha(name: &str) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(64);
    for byte in name.as_bytes() {
        write!(&mut s, "{byte:02x}").ok();
    }
    while s.len() < 64 {
        s.push('0');
    }
    s.truncate(64);
    s
}

async fn make_silo(root: &tritond_client::Client, name: &str) -> tritond_client::types::Silo {
    root.create_silo()
        .body(NewSilo {
            name: name.to_string(),
            description: None,
        })
        .send()
        .await
        .unwrap()
        .into_inner()
}

// ---------- tests ----------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn public_image_visible_to_anonymous() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let img = root
        .create_public_image()
        .body(standard_image("public-img"))
        .send()
        .await
        .unwrap()
        .into_inner();
    assert!(matches!(img.scope, ImageScope::Public));

    // Anonymous list / get both succeed.
    let anon = test.anonymous_client();
    let listed = anon.list_public_images().send().await.unwrap().into_inner();
    assert!(listed.iter().any(|i| i.id == img.id));
    let fetched = anon
        .get_image()
        .image_id(img.id)
        .send()
        .await
        .unwrap()
        .into_inner();
    assert_eq!(fetched.id, img.id);

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn public_image_create_is_root_only() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let silo = make_silo(&root, "members").await;
    let (_user_id, member_token) = test
        .make_tenant_user(silo.default_tenant_id, "member")
        .await;
    let member = test.bearer_client(&member_token);

    let err = member
        .create_public_image()
        .body(standard_image("would-be-public"))
        .send()
        .await
        .expect_err("non-root must not create public images");
    assert_status(err, 403);

    // Anonymous is even more restricted (no auth at all).
    let anon = test.anonymous_client();
    let err = anon
        .create_public_image()
        .body(standard_image("would-be-public-2"))
        .send()
        .await
        .expect_err("anonymous must not create public images");
    // Anonymous Cedar deny on image_create at the global resource
    // surfaces as 403 from authenticate_and_authorize.
    assert_status(err, 403);

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn silo_image_visibility_matrix() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let silo_a = make_silo(&root, "silo-a").await;
    let silo_b = make_silo(&root, "silo-b").await;
    let img = root
        .create_silo_image()
        .silo_id(silo_a.id)
        .body(standard_image("silo-a-img"))
        .send()
        .await
        .unwrap()
        .into_inner();
    assert!(matches!(img.scope, ImageScope::Silo { silo_id: s } if s == silo_a.id));

    let (_, token_a) = test
        .make_tenant_user(silo_a.default_tenant_id, "alice@a")
        .await;
    let (_, token_b) = test
        .make_tenant_user(silo_b.default_tenant_id, "bob@b")
        .await;
    let alice = test.bearer_client(&token_a);
    let bob = test.bearer_client(&token_b);

    // Alice (silo-A member) can see the image.
    let fetched = alice
        .get_image()
        .image_id(img.id)
        .send()
        .await
        .unwrap()
        .into_inner();
    assert_eq!(fetched.id, img.id);

    // Bob (silo-B member) gets 404 — cross-silo invisibility.
    let err = bob
        .get_image()
        .image_id(img.id)
        .send()
        .await
        .expect_err("cross-silo Silo image must 404");
    assert_status(err, 404);

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tenant_image_visibility_matrix() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let silo = make_silo(&root, "shared-silo").await;
    // Two tenants in the same silo so the "same silo, different
    // tenant" probe is exercised.
    let tenant_a = silo.default_tenant_id;
    let tenant_b = root
        .create_silo_tenant()
        .silo_id(silo.id)
        .body(tritond_client::types::NewTenant {
            name: "tenant-b".to_string(),
            description: None,
        })
        .send()
        .await
        .unwrap()
        .into_inner()
        .id;
    let img = root
        .create_tenant_image()
        .tenant_id(tenant_a)
        .body(standard_image("tenant-a-img"))
        .send()
        .await
        .unwrap()
        .into_inner();
    assert!(matches!(img.scope, ImageScope::Tenant { tenant_id: t } if t == tenant_a));

    let (_, token_a) = test.make_tenant_user(tenant_a, "alice@a").await;
    let (_, token_b) = test.make_tenant_user(tenant_b, "bob@b").await;
    let alice = test.bearer_client(&token_a);
    let bob = test.bearer_client(&token_b);

    let fetched = alice
        .get_image()
        .image_id(img.id)
        .send()
        .await
        .unwrap()
        .into_inner();
    assert_eq!(fetched.id, img.id);

    let err = bob
        .get_image()
        .image_id(img.id)
        .send()
        .await
        .expect_err("cross-tenant Tenant image must 404");
    assert_status(err, 404);

    // Bob's tenant view should NOT include tenant_a's image.
    let bob_view = bob
        .list_tenant_images()
        .tenant_id(tenant_b)
        .send()
        .await
        .unwrap()
        .into_inner();
    assert!(
        bob_view.iter().all(|i| i.id != img.id),
        "tenant_b list must not leak tenant_a image"
    );

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn project_image_visibility_matrix() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let silo = make_silo(&root, "proj-silo").await;
    let tenant_a = silo.default_tenant_id;
    let tenant_b = root
        .create_silo_tenant()
        .silo_id(silo.id)
        .body(tritond_client::types::NewTenant {
            name: "tenant-b".to_string(),
            description: None,
        })
        .send()
        .await
        .unwrap()
        .into_inner()
        .id;
    let project_a = root
        .create_tenant_project()
        .tenant_id(tenant_a)
        .body(NewProject {
            name: "p".to_string(),
            description: None,
        })
        .send()
        .await
        .unwrap()
        .into_inner()
        .id;
    let img = root
        .create_project_image()
        .tenant_id(tenant_a)
        .project_id(project_a)
        .body(standard_image("proj-a-img"))
        .send()
        .await
        .unwrap()
        .into_inner();
    assert!(matches!(img.scope, ImageScope::Project { project_id: p } if p == project_a));

    let (_, token_a) = test.make_tenant_user(tenant_a, "alice@p").await;
    let (_, token_b) = test.make_tenant_user(tenant_b, "bob@p").await;
    let alice = test.bearer_client(&token_a);
    let bob = test.bearer_client(&token_b);

    // Alice (member of project's tenant) can see it.
    let fetched = alice
        .get_image()
        .image_id(img.id)
        .send()
        .await
        .unwrap()
        .into_inner();
    assert_eq!(fetched.id, img.id);

    // Bob (different tenant in the same silo) cannot.
    let err = bob
        .get_image()
        .image_id(img.id)
        .send()
        .await
        .expect_err("cross-tenant Project image must 404");
    assert_status(err, 404);

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn user_image_visibility_matrix() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let silo = make_silo(&root, "users-silo").await;
    let tenant = silo.default_tenant_id;
    let (alice_id, alice_token) = test.make_tenant_user(tenant, "alice@u").await;
    let (_bob_id, bob_token) = test.make_tenant_user(tenant, "bob@u").await;
    let alice = test.bearer_client(&alice_token);
    let bob = test.bearer_client(&bob_token);

    let img = alice
        .create_my_image()
        .body(standard_image("alice-private"))
        .send()
        .await
        .unwrap()
        .into_inner();
    assert!(matches!(img.scope, ImageScope::User { user_id: u } if u == alice_id));

    let fetched = alice
        .get_image()
        .image_id(img.id)
        .send()
        .await
        .unwrap()
        .into_inner();
    assert_eq!(fetched.id, img.id);

    // Bob (same tenant!) cannot see Alice's User-scoped image.
    let err = bob
        .get_image()
        .image_id(img.id)
        .send()
        .await
        .expect_err("cross-user User image must 404");
    assert_status(err, 404);

    // Root can.
    let fetched = root
        .get_image()
        .image_id(img.id)
        .send()
        .await
        .unwrap()
        .into_inner();
    assert_eq!(fetched.id, img.id);
    let _ = test.root_user_id;

    // Alice's /v2/auth/images list shows her own image; Bob's
    // shows nothing.
    let alice_list = alice.list_my_images().send().await.unwrap().into_inner();
    assert_eq!(alice_list.len(), 1);
    assert_eq!(alice_list[0].id, img.id);
    let bob_list = bob.list_my_images().send().await.unwrap().into_inner();
    assert!(bob_list.is_empty());

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn user_image_delete_is_owner_only() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let silo = make_silo(&root, "u-del").await;
    let tenant = silo.default_tenant_id;
    let (_, alice_token) = test.make_tenant_user(tenant, "alice@d").await;
    let (_, bob_token) = test.make_tenant_user(tenant, "bob@d").await;
    let alice = test.bearer_client(&alice_token);
    let bob = test.bearer_client(&bob_token);

    let img = alice
        .create_my_image()
        .body(standard_image("alice-del"))
        .send()
        .await
        .unwrap()
        .into_inner();

    // Bob can't delete Alice's image (visibility deny → 404).
    let err = bob
        .delete_image()
        .image_id(img.id)
        .send()
        .await
        .expect_err("non-owner delete must 404");
    assert_status(err, 404);

    // Alice can.
    alice.delete_image().image_id(img.id).send().await.unwrap();

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn public_image_delete_is_root_only() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let silo = make_silo(&root, "pub-del").await;
    let tenant = silo.default_tenant_id;
    let (_, member_token) = test.make_tenant_user(tenant, "member@p").await;
    let member = test.bearer_client(&member_token);

    let img = root
        .create_public_image()
        .body(standard_image("public-del"))
        .send()
        .await
        .unwrap()
        .into_inner();

    // Tenant member cannot delete a Public image.
    let err = member
        .delete_image()
        .image_id(img.id)
        .send()
        .await
        .expect_err("non-root must not delete public images");
    assert_status(err, 404);

    // Root can.
    root.delete_image().image_id(img.id).send().await.unwrap();

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn visible_in_project_unions_public_silo_tenant_project() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let silo = make_silo(&root, "visible-p").await;
    let tenant = silo.default_tenant_id;
    let project = root
        .create_tenant_project()
        .tenant_id(tenant)
        .body(NewProject {
            name: "p".to_string(),
            description: None,
        })
        .send()
        .await
        .unwrap()
        .into_inner()
        .id;

    let pub_img = root
        .create_public_image()
        .body(standard_image("p-pub"))
        .send()
        .await
        .unwrap()
        .into_inner();
    let silo_img = root
        .create_silo_image()
        .silo_id(silo.id)
        .body(standard_image("p-silo"))
        .send()
        .await
        .unwrap()
        .into_inner();
    let tenant_img = root
        .create_tenant_image()
        .tenant_id(tenant)
        .body(standard_image("p-tenant"))
        .send()
        .await
        .unwrap()
        .into_inner();
    let project_img = root
        .create_project_image()
        .tenant_id(tenant)
        .project_id(project)
        .body(standard_image("p-proj"))
        .send()
        .await
        .unwrap()
        .into_inner();

    // Add a User-scoped image and confirm it does NOT show up
    // in the project list — User scope is not in the union.
    let (_, user_token) = test.make_tenant_user(tenant, "user@p").await;
    let user_client = test.bearer_client(&user_token);
    let user_img = user_client
        .create_my_image()
        .body(standard_image("p-user"))
        .send()
        .await
        .unwrap()
        .into_inner();

    let view = root
        .list_project_images()
        .tenant_id(tenant)
        .project_id(project)
        .send()
        .await
        .unwrap()
        .into_inner();
    let mut ids: Vec<Uuid> = view.iter().map(|i| i.id).collect();
    ids.sort();
    let mut want = vec![pub_img.id, silo_img.id, tenant_img.id, project_img.id];
    want.sort();
    assert_eq!(ids, want);
    assert!(
        !view.iter().any(|i| i.id == user_img.id),
        "User-scoped image must not leak into project visibility view"
    );

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn instance_create_with_visible_image_succeeds() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let silo = make_silo(&root, "ic-v").await;
    let tenant = silo.default_tenant_id;
    let project = root
        .create_tenant_project()
        .tenant_id(tenant)
        .body(NewProject {
            name: "p".to_string(),
            description: None,
        })
        .send()
        .await
        .unwrap()
        .into_inner()
        .id;
    let vpc = root
        .create_project_vpc()
        .tenant_id(tenant)
        .project_id(project)
        .body(NewVpc {
            name: "v".to_string(),
            description: None,
            ipv4_block: Some("10.0.0.0/16".to_string()),
            ipv6_block: None,
        })
        .send()
        .await
        .unwrap()
        .into_inner();
    let subnet = root
        .create_vpc_subnet()
        .tenant_id(tenant)
        .project_id(project)
        .vpc_id(vpc.id)
        .body(NewSubnet {
            name: "s".to_string(),
            description: None,
            ipv4_block: Some("10.0.1.0/24".to_string()),
            ipv6_block: None,
        })
        .send()
        .await
        .unwrap()
        .into_inner();
    let img = root
        .create_public_image()
        .body(standard_image("ic-public"))
        .send()
        .await
        .unwrap()
        .into_inner();
    let ssh_key = root
        .create_silo_ssh_key()
        .silo_id(silo.id)
        .body(NewSshKey {
            name: "k".to_string(),
            description: None,
            public_key: fresh_pubkey(),
        })
        .send()
        .await
        .unwrap()
        .into_inner();

    let inst = root
        .create_project_instance()
        .tenant_id(tenant)
        .project_id(project)
        .body(tritond_client::types::NewInstance {
            name: "web".to_string(),
            description: None,
            image_id: img.id,
            primary_subnet_id: subnet.id,
            ssh_key_ids: vec![ssh_key.id],
            cpu: 2,
            memory_bytes: 2 * 1024 * 1024 * 1024,
            extra_nics: Vec::new(),
            mac: None,
        })
        .send()
        .await
        .unwrap()
        .into_inner();
    assert_eq!(inst.image_id, img.id);

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn instance_create_with_invisible_image_returns_404() {
    let test = TestServer::start().await;
    let root = test.root_client();
    // Two silos with their own tenants; member of silo-A tries
    // to launch from a silo-B image.
    let silo_a = make_silo(&root, "ic-a").await;
    let silo_b = make_silo(&root, "ic-b").await;
    let tenant_a = silo_a.default_tenant_id;

    let project_a = root
        .create_tenant_project()
        .tenant_id(tenant_a)
        .body(NewProject {
            name: "p".to_string(),
            description: None,
        })
        .send()
        .await
        .unwrap()
        .into_inner()
        .id;
    let vpc = root
        .create_project_vpc()
        .tenant_id(tenant_a)
        .project_id(project_a)
        .body(NewVpc {
            name: "v".to_string(),
            description: None,
            ipv4_block: Some("10.0.0.0/16".to_string()),
            ipv6_block: None,
        })
        .send()
        .await
        .unwrap()
        .into_inner();
    let subnet = root
        .create_vpc_subnet()
        .tenant_id(tenant_a)
        .project_id(project_a)
        .vpc_id(vpc.id)
        .body(NewSubnet {
            name: "s".to_string(),
            description: None,
            ipv4_block: Some("10.0.1.0/24".to_string()),
            ipv6_block: None,
        })
        .send()
        .await
        .unwrap()
        .into_inner();

    // Image lives in silo-B (Silo-scoped).
    let foreign_img = root
        .create_silo_image()
        .silo_id(silo_b.id)
        .body(standard_image("foreign-silo-img"))
        .send()
        .await
        .unwrap()
        .into_inner();
    let ssh_key = root
        .create_silo_ssh_key()
        .silo_id(silo_a.id)
        .body(NewSshKey {
            name: "k".to_string(),
            description: None,
            public_key: fresh_pubkey(),
        })
        .send()
        .await
        .unwrap()
        .into_inner();

    // Mint a non-root member of silo-A's tenant; the visibility
    // predicate rejects the foreign image as invisible.
    let (_, alice_token) = test.make_tenant_user(tenant_a, "alice@ic").await;
    let alice = test.bearer_client(&alice_token);

    let err = alice
        .create_project_instance()
        .tenant_id(tenant_a)
        .project_id(project_a)
        .body(tritond_client::types::NewInstance {
            name: "leak".to_string(),
            description: None,
            image_id: foreign_img.id,
            primary_subnet_id: subnet.id,
            ssh_key_ids: vec![ssh_key.id],
            cpu: 2,
            memory_bytes: 2 * 1024 * 1024 * 1024,
            extra_nics: Vec::new(),
            mac: None,
        })
        .send()
        .await
        .expect_err("invisible image must 404");
    assert_status(err, 404);

    test.close().await;
}

fn fresh_pubkey() -> String {
    use rsa::rand_core::OsRng;
    use ssh_key::{Algorithm, PrivateKey};
    let priv_key = PrivateKey::random(&mut OsRng, Algorithm::Ed25519).unwrap();
    priv_key.public_key().to_openssh().unwrap()
}
