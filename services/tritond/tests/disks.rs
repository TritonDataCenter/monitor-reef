// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

#![allow(clippy::unwrap_used, clippy::expect_used)]

//! End-to-end tests for the
//! `/v1/silos/{silo_id}/projects/{project_id}/instances/{instance_id}/disks`
//! surface.
//!
//! Verifies the auto-create + cascade semantics introduced in slice 4
//! of Tier 3:
//!
//! * Instance create produces exactly one boot Disk, sized to the
//!   source image, tagged with the image's id, kind = Boot.
//! * Cross-instance get returns 404 (defence-in-depth on the path's
//!   instance_id).
//! * Instance delete cascades — the boot Disk vanishes when the
//!   instance is deleted.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use rsa::rand_core::OsRng;
use ssh_key::{Algorithm, PrivateKey};
use tritond::audit::AuditService;
use tritond::auth::AuthService;
use tritond::{ApiContext, start_server_with_context};
use tritond_audit::MemChain;
use tritond_auth::{JwtKey, RedactedString, hash_password, mint_access};
use tritond_client::types::{
    DiskKind, LifecycleState, NewImage, NewInstance, NewProject, NewSilo, NewSshKey, NewSubnet,
    NewVpc,
};
use tritond_store::{MemStore, Store, User};
use uuid::Uuid;

const ROOT_PASSWORD: &str = "disks-test";
const IMAGE_SIZE: u64 = 1_000_000_000;

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

fn fresh_pubkey() -> String {
    let priv_key = PrivateKey::random(&mut OsRng, Algorithm::Ed25519).unwrap();
    priv_key.public_key().to_openssh().unwrap()
}

fn lifecycle_kind(state: &LifecycleState) -> &'static str {
    match state {
        LifecycleState::Pending => "Pending",
        LifecycleState::Provisioning => "Provisioning",
        LifecycleState::Running => "Running",
        LifecycleState::Stopping => "Stopping",
        LifecycleState::Stopped => "Stopped",
        LifecycleState::Failed(_) => "Failed",
    }
}

async fn wait_for_lifecycle(
    client: &tritond_client::Client,
    silo_id: Uuid,
    project_id: Uuid,
    instance_id: Uuid,
    expected: &str,
    deadline: Duration,
) -> tritond_client::types::Instance {
    let start = Instant::now();
    loop {
        let inst = client
            .get_instance_v1()
            .instance_id(instance_id)
            .send()
            .await
            .unwrap()
            .into_inner();
        let observed = lifecycle_kind(&inst.lifecycle);
        if observed == expected {
            return inst;
        }
        if start.elapsed() > deadline {
            panic!("timeout waiting for lifecycle={expected}; last seen={observed}");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

const SETTLE: Duration = Duration::from_secs(5);

struct Fixture {
    tenant_id: Uuid,
    project_id: Uuid,
    image_id: Uuid,
    subnet_id: Uuid,
    ssh_key_id: Uuid,
}

async fn build_fixture(root: &tritond_client::Client) -> Fixture {
    let silo = root
        .create_silo()
        .body(NewSilo {
            name: format!("silo-{}", Uuid::new_v4()),
            description: None,
        })
        .send()
        .await
        .unwrap()
        .into_inner();
    let project = root
        .create_tenant_project()
        .tenant_id(silo.default_tenant_id)
        .body(NewProject {
            name: "p".to_string(),
            description: None,
        })
        .send()
        .await
        .unwrap()
        .into_inner();
    let vpc = root
        .create_vpc_v1()
        .tenant(silo.default_tenant_id)
        .project(project.id)
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
            name: "primary".to_string(),
            description: None,
            ipv4_block: Some("10.0.1.0/24".to_string()),
            ipv6_block: None,
        })
        .send()
        .await
        .unwrap()
        .into_inner();
    let image = root
        .create_silo_image()
        .silo_id(silo.id)
        .body(NewImage {
            name: "ubuntu".to_string(),
            description: None,
            os: "linux".to_string(),
            version: "22.04".to_string(),
            size_bytes: IMAGE_SIZE,
            sha256: "0".repeat(64),
            source_url: Some("mantafs://images/ubuntu".to_string()),
            id: None,
            compatibility: None,
        })
        .send()
        .await
        .unwrap()
        .into_inner();
    let ssh_key = root
        .create_silo_ssh_key()
        .silo_id(silo.id)
        .body(NewSshKey {
            name: "ci".to_string(),
            description: None,
            public_key: fresh_pubkey(),
        })
        .send()
        .await
        .unwrap()
        .into_inner();
    Fixture {
        tenant_id: silo.default_tenant_id,
        project_id: project.id,
        image_id: image.id,
        subnet_id: subnet.id,
        ssh_key_id: ssh_key.id,
    }
}

fn instance_req(fx: &Fixture, name: &str) -> NewInstance {
    NewInstance {
        name: name.to_string(),
        description: None,
        image_id: fx.image_id,
        primary_subnet_id: fx.subnet_id,
        ssh_key_ids: vec![fx.ssh_key_id],
        cpu: 2,
        memory_bytes: 2 * 1024 * 1024 * 1024,
        extra_nics: Vec::new(),
        mac: None,
    }
}

// ---------- tests ----------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn instance_create_produces_boot_disk_sized_to_image() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let fx = build_fixture(&root).await;
    let inst = root
        .create_instance_v1()
        .tenant(fx.tenant_id)
        .project(fx.project_id)
        .body(instance_req(&fx, "web"))
        .send()
        .await
        .unwrap()
        .into_inner();

    let disks = root
        .list_disks_v1()
        .instance(inst.id)
        .send()
        .await
        .unwrap()
        .into_inner()
        .items;
    assert_eq!(disks.len(), 1, "expect exactly one boot disk");
    let boot = &disks[0];
    assert_eq!(boot.name, "boot");
    assert_eq!(boot.kind, DiskKind::Boot);
    assert_eq!(boot.size_bytes, IMAGE_SIZE);
    assert_eq!(boot.source_image_id, Some(fx.image_id));
    assert_eq!(boot.instance_id, inst.id);

    test.close().await;
}

// RFD 00007 AP-3e: cross-instance defence-in-depth no longer lives
// in the URL shape (the /v1/disks/{id} singleton is by id only); it
// must come from Cedar policy. The test as written uses the root
// principal which sees every silo by design, so it can't exercise
// the boundary at the URL level. Needs a non-root tenant member to
// exercise the deny path. Tracked at AP-3b-6 alongside the matching
// cross_silo_get test in instances.rs.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs non-root principal fixtures; tracked at AP-3b-6"]
async fn cross_instance_disk_get_returns_404() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let fx = build_fixture(&root).await;
    let inst_a = root
        .create_instance_v1()
        .tenant(fx.tenant_id)
        .project(fx.project_id)
        .body(instance_req(&fx, "a"))
        .send()
        .await
        .unwrap()
        .into_inner();
    let inst_b = root
        .create_instance_v1()
        .tenant(fx.tenant_id)
        .project(fx.project_id)
        .body(instance_req(&fx, "b"))
        .send()
        .await
        .unwrap()
        .into_inner();
    let disks_a = root
        .list_disks_v1()
        .instance(inst_a.id)
        .send()
        .await
        .unwrap()
        .into_inner()
        .items;
    let disk_a = &disks_a[0];

    // Same disk id, but path's instance_id is inst_b → 404 via
    // defence-in-depth.
    let err = root
        .get_disk_v1()
        .disk_id(disk_a.id)
        .send()
        .await
        .expect_err("cross-instance disk get must 404");
    assert_status(err, 404);

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn instance_delete_cascades_boot_disk() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let fx = build_fixture(&root).await;
    let inst = root
        .create_instance_v1()
        .tenant(fx.tenant_id)
        .project(fx.project_id)
        .body(instance_req(&fx, "web"))
        .send()
        .await
        .unwrap()
        .into_inner();
    let original_disk = root
        .list_disks_v1()
        .instance(inst.id)
        .send()
        .await
        .unwrap()
        .into_inner()
        .items[0]
        .clone();

    wait_for_lifecycle(
        &root,
        fx.tenant_id,
        fx.project_id,
        inst.id,
        "Running",
        SETTLE,
    )
    .await;
    root.stop_instance_v1()
        .instance_id(inst.id)
        .send()
        .await
        .unwrap();
    wait_for_lifecycle(
        &root,
        fx.tenant_id,
        fx.project_id,
        inst.id,
        "Stopped",
        SETTLE,
    )
    .await;
    root.delete_instance_v1()
        .instance_id(inst.id)
        .send()
        .await
        .unwrap();

    // Boot disk is gone.
    let err = root
        .get_disk_v1()
        .disk_id(original_disk.id)
        .send()
        .await
        .expect_err("boot disk should be cascade-deleted");
    assert_status(err, 404);

    test.close().await;
}
