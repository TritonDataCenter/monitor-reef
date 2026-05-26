// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Visibility / ownership matrix for the multi-scope SSH-key
//! catalog (slice G).
//!
//! Mirrors `image_scope.rs` (slice F): the per-scope happy paths
//! and shape tests live in `ssh_keys.rs`; this file is the
//! load-bearing security-test surface — every variant of
//! [`tritond_client::types::SshKeyScope`] is exercised against
//! multiple principals (root, same-tenant member, cross-tenant
//! member, cross-silo member, anonymous) and the answers are
//! pinned.
//!
//! A wrong filter here is a cross-tenant information leak, so
//! the cases below are intentionally redundant — every scope
//! lists out who *can* see and who *cannot* see, rather than
//! relying on a single "this should fail" assertion.

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
use tritond_client::types::{
    NewImage, NewProject, NewSilo, NewSshKey, NewSubnet, NewVpc, SshKeyScope,
};
use tritond_store::{MemStore, Store, User};
use uuid::Uuid;

const ROOT_PASSWORD: &str = "ssh-key-scope-test";

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
            capabilities: Default::default(),
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
    /// (its silo is derived at auth time via the tenant lookup),
    /// returning the user_id and an access token. Bypasses the
    /// bcrypt password path entirely — the test authenticates via
    /// the JWT directly.
    async fn make_tenant_user(&self, tenant_id: Uuid, username: &str) -> (Uuid, String) {
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

fn fresh_pubkey() -> String {
    let priv_key = PrivateKey::random(&mut OsRng, Algorithm::Ed25519).unwrap();
    priv_key.public_key().to_openssh().unwrap()
}

fn ssh_key_body(name: &str) -> NewSshKey {
    NewSshKey {
        name: name.to_string(),
        description: None,
        public_key: fresh_pubkey(),
    }
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
async fn public_ssh_key_visible_to_anonymous() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let key = root
        .create_public_ssh_key()
        .body(ssh_key_body("public-key"))
        .send()
        .await
        .unwrap()
        .into_inner();
    assert!(matches!(key.scope, SshKeyScope::Public));

    // Anonymous list / get both succeed.
    let anon = test.anonymous_client();
    let listed = anon
        .list_public_ssh_keys()
        .send()
        .await
        .unwrap()
        .into_inner();
    assert!(listed.iter().any(|k| k.id == key.id));
    let fetched = anon
        .get_ssh_key()
        .key_id(key.id)
        .send()
        .await
        .unwrap()
        .into_inner();
    assert_eq!(fetched.id, key.id);

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn public_ssh_key_create_is_root_only() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let silo = make_silo(&root, "members").await;
    let (_user_id, member_token) = test
        .make_tenant_user(silo.default_tenant_id, "member")
        .await;
    let member = test.bearer_client(&member_token);

    let err = member
        .create_public_ssh_key()
        .body(ssh_key_body("would-be-public"))
        .send()
        .await
        .expect_err("non-root must not create public ssh keys");
    assert_status(err, 403);

    let anon = test.anonymous_client();
    let err = anon
        .create_public_ssh_key()
        .body(ssh_key_body("would-be-public-2"))
        .send()
        .await
        .expect_err("anonymous must not create public ssh keys");
    assert_status(err, 403);

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn silo_ssh_key_visibility_matrix() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let silo_a = make_silo(&root, "silo-a").await;
    let silo_b = make_silo(&root, "silo-b").await;
    let key = root
        .create_silo_ssh_key()
        .silo_id(silo_a.id)
        .body(ssh_key_body("silo-a-key"))
        .send()
        .await
        .unwrap()
        .into_inner();
    assert!(matches!(key.scope, SshKeyScope::Silo { silo_id: s } if s == silo_a.id));

    let (_, token_a) = test
        .make_tenant_user(silo_a.default_tenant_id, "alice@a")
        .await;
    let (_, token_b) = test
        .make_tenant_user(silo_b.default_tenant_id, "bob@b")
        .await;
    let alice = test.bearer_client(&token_a);
    let bob = test.bearer_client(&token_b);

    let fetched = alice
        .get_ssh_key()
        .key_id(key.id)
        .send()
        .await
        .unwrap()
        .into_inner();
    assert_eq!(fetched.id, key.id);

    let err = bob
        .get_ssh_key()
        .key_id(key.id)
        .send()
        .await
        .expect_err("cross-silo Silo ssh-key must 404");
    assert_status(err, 404);

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tenant_ssh_key_visibility_matrix() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let silo = make_silo(&root, "shared-silo").await;
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
    let key = root
        .create_tenant_ssh_key()
        .tenant_id(tenant_a)
        .body(ssh_key_body("tenant-a-key"))
        .send()
        .await
        .unwrap()
        .into_inner();
    assert!(matches!(key.scope, SshKeyScope::Tenant { tenant_id: t } if t == tenant_a));

    let (_, token_a) = test.make_tenant_user(tenant_a, "alice@a").await;
    let (_, token_b) = test.make_tenant_user(tenant_b, "bob@b").await;
    let alice = test.bearer_client(&token_a);
    let bob = test.bearer_client(&token_b);

    let fetched = alice
        .get_ssh_key()
        .key_id(key.id)
        .send()
        .await
        .unwrap()
        .into_inner();
    assert_eq!(fetched.id, key.id);

    let err = bob
        .get_ssh_key()
        .key_id(key.id)
        .send()
        .await
        .expect_err("cross-tenant Tenant ssh-key must 404");
    assert_status(err, 404);

    let bob_view = bob
        .list_tenant_ssh_keys()
        .tenant_id(tenant_b)
        .send()
        .await
        .unwrap()
        .into_inner();
    assert!(
        bob_view.iter().all(|k| k.id != key.id),
        "tenant_b list must not leak tenant_a key"
    );

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn project_ssh_key_visibility_matrix() {
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
    let key = root
        .create_project_ssh_key()
        .tenant_id(tenant_a)
        .project_id(project_a)
        .body(ssh_key_body("proj-a-key"))
        .send()
        .await
        .unwrap()
        .into_inner();
    assert!(matches!(key.scope, SshKeyScope::Project { project_id: p } if p == project_a));

    let (_, token_a) = test.make_tenant_user(tenant_a, "alice@p").await;
    let (_, token_b) = test.make_tenant_user(tenant_b, "bob@p").await;
    let alice = test.bearer_client(&token_a);
    let bob = test.bearer_client(&token_b);

    let fetched = alice
        .get_ssh_key()
        .key_id(key.id)
        .send()
        .await
        .unwrap()
        .into_inner();
    assert_eq!(fetched.id, key.id);

    let err = bob
        .get_ssh_key()
        .key_id(key.id)
        .send()
        .await
        .expect_err("cross-tenant Project ssh-key must 404");
    assert_status(err, 404);

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn user_ssh_key_visibility_matrix() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let silo = make_silo(&root, "users-silo").await;
    let tenant = silo.default_tenant_id;
    let (alice_id, alice_token) = test.make_tenant_user(tenant, "alice@u").await;
    let (_bob_id, bob_token) = test.make_tenant_user(tenant, "bob@u").await;
    let alice = test.bearer_client(&alice_token);
    let bob = test.bearer_client(&bob_token);

    let key = alice
        .create_my_ssh_key()
        .body(ssh_key_body("alice-private"))
        .send()
        .await
        .unwrap()
        .into_inner();
    assert!(matches!(key.scope, SshKeyScope::User { user_id: u } if u == alice_id));

    let fetched = alice
        .get_ssh_key()
        .key_id(key.id)
        .send()
        .await
        .unwrap()
        .into_inner();
    assert_eq!(fetched.id, key.id);

    // Bob (same tenant!) cannot see Alice's User-scoped key.
    let err = bob
        .get_ssh_key()
        .key_id(key.id)
        .send()
        .await
        .expect_err("cross-user User ssh-key must 404");
    assert_status(err, 404);

    // Root can.
    let fetched = root
        .get_ssh_key()
        .key_id(key.id)
        .send()
        .await
        .unwrap()
        .into_inner();
    assert_eq!(fetched.id, key.id);
    let _ = test.root_user_id;

    // Alice's /v2/auth/ssh-keys list shows her own key; Bob's
    // shows nothing.
    let alice_list = alice.list_my_ssh_keys().send().await.unwrap().into_inner();
    assert_eq!(alice_list.len(), 1);
    assert_eq!(alice_list[0].id, key.id);
    let bob_list = bob.list_my_ssh_keys().send().await.unwrap().into_inner();
    assert!(bob_list.is_empty());

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn user_ssh_key_delete_is_owner_only() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let silo = make_silo(&root, "u-del").await;
    let tenant = silo.default_tenant_id;
    let (_, alice_token) = test.make_tenant_user(tenant, "alice@d").await;
    let (_, bob_token) = test.make_tenant_user(tenant, "bob@d").await;
    let alice = test.bearer_client(&alice_token);
    let bob = test.bearer_client(&bob_token);

    let key = alice
        .create_my_ssh_key()
        .body(ssh_key_body("alice-del"))
        .send()
        .await
        .unwrap()
        .into_inner();

    let err = bob
        .delete_ssh_key()
        .key_id(key.id)
        .send()
        .await
        .expect_err("non-owner delete must 404");
    assert_status(err, 404);

    alice.delete_ssh_key().key_id(key.id).send().await.unwrap();

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn public_ssh_key_delete_is_root_only() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let silo = make_silo(&root, "pub-del").await;
    let tenant = silo.default_tenant_id;
    let (_, member_token) = test.make_tenant_user(tenant, "member@p").await;
    let member = test.bearer_client(&member_token);

    let key = root
        .create_public_ssh_key()
        .body(ssh_key_body("public-del"))
        .send()
        .await
        .unwrap()
        .into_inner();

    // Tenant member cannot delete a Public key.
    let err = member
        .delete_ssh_key()
        .key_id(key.id)
        .send()
        .await
        .expect_err("non-root must not delete public ssh keys");
    assert_status(err, 404);

    root.delete_ssh_key().key_id(key.id).send().await.unwrap();

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

    let pub_key = root
        .create_public_ssh_key()
        .body(ssh_key_body("p-pub"))
        .send()
        .await
        .unwrap()
        .into_inner();
    let silo_key = root
        .create_silo_ssh_key()
        .silo_id(silo.id)
        .body(ssh_key_body("p-silo"))
        .send()
        .await
        .unwrap()
        .into_inner();
    let tenant_key = root
        .create_tenant_ssh_key()
        .tenant_id(tenant)
        .body(ssh_key_body("p-tenant"))
        .send()
        .await
        .unwrap()
        .into_inner();
    let project_key = root
        .create_project_ssh_key()
        .tenant_id(tenant)
        .project_id(project)
        .body(ssh_key_body("p-proj"))
        .send()
        .await
        .unwrap()
        .into_inner();

    // Add a User-scoped key and confirm it does NOT show up in
    // the project list — User scope is not in the union.
    let (_, user_token) = test.make_tenant_user(tenant, "user@p").await;
    let user_client = test.bearer_client(&user_token);
    let user_key = user_client
        .create_my_ssh_key()
        .body(ssh_key_body("p-user"))
        .send()
        .await
        .unwrap()
        .into_inner();

    let view = root
        .list_project_ssh_keys()
        .tenant_id(tenant)
        .project_id(project)
        .send()
        .await
        .unwrap()
        .into_inner();
    let mut ids: Vec<Uuid> = view.iter().map(|k| k.id).collect();
    ids.sort();
    let mut want = vec![pub_key.id, silo_key.id, tenant_key.id, project_key.id];
    want.sort();
    assert_eq!(ids, want);
    assert!(
        !view.iter().any(|k| k.id == user_key.id),
        "User-scoped ssh key must not leak into project visibility view"
    );

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn instance_create_with_visible_ssh_key_succeeds() {
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
        .create_vpc_v1()
        .tenant(tenant)
        .project(project)
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
        .create_subnet_v1()
        .vpc(vpc.id)
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
        .body(public_image_body("ic-public"))
        .send()
        .await
        .unwrap()
        .into_inner();
    let ssh_key = root
        .create_public_ssh_key()
        .body(ssh_key_body("ic-public-key"))
        .send()
        .await
        .unwrap()
        .into_inner();

    let inst = root
        .create_instance_v1()
        .tenant(tenant)
        .project(project)
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
async fn instance_create_with_invisible_ssh_key_returns_404() {
    let test = TestServer::start().await;
    let root = test.root_client();
    // Two silos with their own tenants; member of silo-A tries
    // to launch with a silo-B-scoped ssh key.
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
        .create_vpc_v1()
        .tenant(tenant_a)
        .project(project_a)
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
        .create_subnet_v1()
        .vpc(vpc.id)
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

    // Image is Public so it doesn't gate the create — only the
    // ssh-key visibility test should fire.
    let img = root
        .create_public_image()
        .body(public_image_body("ic-public-img"))
        .send()
        .await
        .unwrap()
        .into_inner();
    // SSH key lives in silo-B (Silo-scoped).
    let foreign_key = root
        .create_silo_ssh_key()
        .silo_id(silo_b.id)
        .body(ssh_key_body("foreign-silo-key"))
        .send()
        .await
        .unwrap()
        .into_inner();

    let (_, alice_token) = test.make_tenant_user(tenant_a, "alice@ic").await;
    let alice = test.bearer_client(&alice_token);

    let err = alice
        .create_instance_v1()
        .tenant(tenant_a)
        .project(project_a)
        .body(tritond_client::types::NewInstance {
            name: "leak".to_string(),
            description: None,
            image_id: img.id,
            primary_subnet_id: subnet.id,
            ssh_key_ids: vec![foreign_key.id],
            cpu: 2,
            memory_bytes: 2 * 1024 * 1024 * 1024,
            extra_nics: Vec::new(),
            mac: None,
        })
        .send()
        .await
        .expect_err("invisible ssh key must 404");
    assert_status(err, 404);

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn anonymous_silo_ssh_key_list_returns_404() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let silo = make_silo(&root, "anon-silo").await;

    let anon = test.anonymous_client();
    let err = anon
        .list_silo_ssh_keys()
        .silo_id(silo.id)
        .send()
        .await
        .expect_err("anonymous silo-ssh-keys list must 404 (no enumeration leak)");
    assert_status(err, 404);

    test.close().await;
}

/// Spread sha256 across the test image names so the
/// content-addressed image-id derivation doesn't collide across
/// fixtures.
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

fn public_image_body(name: &str) -> NewImage {
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
