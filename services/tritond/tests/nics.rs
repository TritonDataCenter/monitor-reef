// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

#![allow(clippy::unwrap_used, clippy::expect_used)]

//! End-to-end tests for the
//! `/v2/silos/{silo_id}/projects/{project_id}/instances/{instance_id}/nics`
//! surface.
//!
//! Verifies the auto-create + cascade semantics introduced in slice 3
//! of Tier 3:
//!
//! * Instance create produces exactly one primary NIC, with a MAC
//!   and a primary IPv4 inside the parent subnet's CIDR (skipping
//!   network/gateway/broadcast).
//! * Cross-instance get returns 404 (defence-in-depth on the path's
//!   instance_id, even when the NIC belongs to the same silo+project).
//! * Instance delete cascades — the NIC vanishes and its IP is
//!   released back to the subnet pool, which we observe by
//!   re-creating an instance and getting the same IP back.

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
    LifecycleState, NewImage, NewInstance, NewProject, NewSilo, NewSshKey, NewSubnet, NewVpc,
};
use tritond_store::{MemStore, Store, User};
use uuid::Uuid;

const ROOT_PASSWORD: &str = "nics-test";

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
            .get_project_instance()
            .tenant_id(silo_id)
            .project_id(project_id)
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
        .create_project_vpc()
        .tenant_id(silo.default_tenant_id)
        .project_id(project.id)
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
        .tenant_id(silo.default_tenant_id)
        .project_id(project.id)
        .vpc_id(vpc.id)
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
            size_bytes: 1_000_000_000,
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
    }
}

// ---------- tests ----------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn instance_create_produces_primary_nic_with_ip_and_mac() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let fx = build_fixture(&root).await;
    let inst = root
        .create_project_instance()
        .tenant_id(fx.tenant_id)
        .project_id(fx.project_id)
        .body(instance_req(&fx, "web"))
        .send()
        .await
        .unwrap()
        .into_inner();

    let nics = root
        .list_instance_nics()
        .tenant_id(fx.tenant_id)
        .project_id(fx.project_id)
        .instance_id(inst.id)
        .send()
        .await
        .unwrap()
        .into_inner();
    assert_eq!(nics.len(), 1, "expect exactly one primary NIC");
    let primary = &nics[0];
    assert_eq!(primary.name, "primary");
    assert_eq!(primary.instance_id, inst.id);
    assert_eq!(primary.subnet_id, fx.subnet_id);
    // Subnet is 10.0.1.0/24 → first available is .2 (after network
    // .0 and gateway .1).
    let v4 = primary.primary_ipv4.expect("ipv4 expected");
    assert_eq!(v4.octets(), [10, 0, 1, 2]);
    assert!(primary.primary_ipv6.is_none());
    assert!(primary.mac.starts_with("02:"));

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cross_instance_nic_get_returns_404() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let fx = build_fixture(&root).await;
    let inst_a = root
        .create_project_instance()
        .tenant_id(fx.tenant_id)
        .project_id(fx.project_id)
        .body(instance_req(&fx, "a"))
        .send()
        .await
        .unwrap()
        .into_inner();
    let inst_b = root
        .create_project_instance()
        .tenant_id(fx.tenant_id)
        .project_id(fx.project_id)
        .body(instance_req(&fx, "b"))
        .send()
        .await
        .unwrap()
        .into_inner();
    let nic_a = &root
        .list_instance_nics()
        .tenant_id(fx.tenant_id)
        .project_id(fx.project_id)
        .instance_id(inst_a.id)
        .send()
        .await
        .unwrap()
        .into_inner()[0]
        .clone();

    // Same NIC id, but path's instance_id is inst_b → 404 via
    // defence-in-depth.
    let err = root
        .get_instance_nic()
        .tenant_id(fx.tenant_id)
        .project_id(fx.project_id)
        .instance_id(inst_b.id)
        .nic_id(nic_a.id)
        .send()
        .await
        .expect_err("cross-instance nic get must 404");
    assert_status(err, 404);

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn instance_delete_cascades_nic_and_frees_ip() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let fx = build_fixture(&root).await;
    let inst = root
        .create_project_instance()
        .tenant_id(fx.tenant_id)
        .project_id(fx.project_id)
        .body(instance_req(&fx, "web"))
        .send()
        .await
        .unwrap()
        .into_inner();
    let original_nic = root
        .list_instance_nics()
        .tenant_id(fx.tenant_id)
        .project_id(fx.project_id)
        .instance_id(inst.id)
        .send()
        .await
        .unwrap()
        .into_inner()[0]
        .clone();
    let original_ip = original_nic.primary_ipv4.expect("ipv4 expected");

    // Wait for the agent to drive Pending → Running, then stop +
    // wait for Stopped, then delete.
    wait_for_lifecycle(
        &root,
        fx.tenant_id,
        fx.project_id,
        inst.id,
        "Running",
        SETTLE,
    )
    .await;
    root.stop_project_instance()
        .tenant_id(fx.tenant_id)
        .project_id(fx.project_id)
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
    root.delete_project_instance()
        .tenant_id(fx.tenant_id)
        .project_id(fx.project_id)
        .instance_id(inst.id)
        .send()
        .await
        .unwrap();

    // NIC is gone.
    let err = root
        .get_instance_nic()
        .tenant_id(fx.tenant_id)
        .project_id(fx.project_id)
        .instance_id(inst.id)
        .nic_id(original_nic.id)
        .send()
        .await
        .expect_err("nic should be cascade-deleted");
    assert_status(err, 404);

    // Re-create an instance under the same subnet → it picks up
    // the freed IP.
    let inst2 = root
        .create_project_instance()
        .tenant_id(fx.tenant_id)
        .project_id(fx.project_id)
        .body(instance_req(&fx, "web2"))
        .send()
        .await
        .unwrap()
        .into_inner();
    let new_nic = root
        .list_instance_nics()
        .tenant_id(fx.tenant_id)
        .project_id(fx.project_id)
        .instance_id(inst2.id)
        .send()
        .await
        .unwrap()
        .into_inner()[0]
        .clone();
    assert_eq!(
        new_nic.primary_ipv4,
        Some(original_ip),
        "freed IP should be reused"
    );

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn second_instance_in_same_subnet_gets_next_ip() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let fx = build_fixture(&root).await;
    let inst1 = root
        .create_project_instance()
        .tenant_id(fx.tenant_id)
        .project_id(fx.project_id)
        .body(instance_req(&fx, "a"))
        .send()
        .await
        .unwrap()
        .into_inner();
    let inst2 = root
        .create_project_instance()
        .tenant_id(fx.tenant_id)
        .project_id(fx.project_id)
        .body(instance_req(&fx, "b"))
        .send()
        .await
        .unwrap()
        .into_inner();
    let nic1 = &root
        .list_instance_nics()
        .tenant_id(fx.tenant_id)
        .project_id(fx.project_id)
        .instance_id(inst1.id)
        .send()
        .await
        .unwrap()
        .into_inner()[0]
        .clone();
    let nic2 = &root
        .list_instance_nics()
        .tenant_id(fx.tenant_id)
        .project_id(fx.project_id)
        .instance_id(inst2.id)
        .send()
        .await
        .unwrap()
        .into_inner()[0]
        .clone();
    let ip1 = nic1.primary_ipv4.unwrap();
    let ip2 = nic2.primary_ipv4.unwrap();
    assert_ne!(ip1, ip2);
    // First gets .2, second gets .3 (the allocator hands out the
    // lowest available and we never delete in this test).
    assert_eq!(ip1.octets(), [10, 0, 1, 2]);
    assert_eq!(ip2.octets(), [10, 0, 1, 3]);

    test.close().await;
}
