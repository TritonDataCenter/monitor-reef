// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

#![allow(clippy::unwrap_used, clippy::expect_used)]

//! End-to-end tests for the
//! `/v1/silos/{silo_id}/projects/{project_id}/floating-ips` surface.
//!
//! Verifies the design choices that make our FloatingIp better
//! than typical clouds':
//!
//! * Symmetric IPv4 + IPv6 (same wire shape, picks family by enum).
//! * Allocations come from the documented Phase 0 pools (TEST-NET-3
//!   for v4, 2001:db8::/48 for v6) so the IPs look obviously fake.
//! * Atomic attach with replace semantics — re-attaching to a new
//!   NIC swaps without a detach window.
//! * Project-owned: instance delete auto-detaches but does NOT
//!   release; the IP stays in the project pool for re-use.
//! * Cross-tenant attach target → 404 (defence-in-depth).
//! * delete-while-attached → 409 (explicit detach required).

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
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
    AddressFamily, AttachFloatingIpRequest, LifecycleState, NewFloatingIp, NewImage, NewInstance,
    NewProject, NewSilo, NewSshKey, NewSubnet, NewVpc,
};
use tritond_store::{JobKind, JobOutcome, MemStore, Store, User};
use uuid::Uuid;

const ROOT_PASSWORD: &str = "fip-test";

// ---------- test fixture ----------

struct TestServer {
    server: dropshot::HttpServer<ApiContext>,
    root_bearer: String,
    store: Arc<dyn Store>,
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
        let context = ApiContext::new(Arc::clone(&store), auth, audit);
        let server = start_server_with_context("127.0.0.1:0", context)
            .await
            .unwrap();
        Self {
            server,
            root_bearer: token,
            store,
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
        disk_bytes: None,
        extra_nics: Vec::new(),
        mac: None,
    }
}

/// Pin an instance to a host CN after its create saga finishes. The
/// fixture registers no CNs, so the designate step leaves instances
/// unrouted, and the store-level attach guard refuses to attach a
/// FloatingIp to an unplaced instance (the FipClaim job must pin to
/// a hosting CN).
async fn place_instance(test: &TestServer, instance_id: Uuid, cn: Uuid) {
    test.store
        .set_instance_host_cn(instance_id, Some(cn))
        .await
        .expect("pin instance to a host CN");
}

/// Store-level fake agent for jobs routed to `cn`. The in-process
/// stub provisioner only claims unrouted jobs, so once an instance
/// is pinned the FipClaim / FipRelease / lifecycle jobs would wedge
/// the sagas' await-terminal actions without this loop. Lifecycle
/// jobs mirror the stub's CAS dance before the ack because the
/// store-level complete path does not advance lifecycle.
fn spawn_cn_agent(store: Arc<dyn Store>, cn: Uuid) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match store.claim_next_job("fip-test-agent", Some(cn)).await {
                Ok(job) => {
                    match job.kind {
                        JobKind::Stop { instance_id } => {
                            let _ = store
                                .transition_instance_lifecycle(
                                    instance_id,
                                    &[tritond_store::LifecycleStateKind::Stopping],
                                    tritond_store::LifecycleState::Stopped,
                                )
                                .await;
                        }
                        JobKind::Start { instance_id } => {
                            let _ = store
                                .transition_instance_lifecycle(
                                    instance_id,
                                    &[tritond_store::LifecycleStateKind::Pending],
                                    tritond_store::LifecycleState::Running,
                                )
                                .await;
                        }
                        _ => {}
                    }
                    let _ = store
                        .complete_job(job.id, JobOutcome::Completed, None)
                        .await;
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
            }
        }
    })
}

// ---------- tests ----------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn allocate_v4_from_test_net_3_pool() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let fx = build_fixture(&root).await;

    let fip = root
        .create_floating_ip_v1()
        .tenant(fx.tenant_id)
        .project(fx.project_id)
        .body(NewFloatingIp {
            name: "public".to_string(),
            description: None,
            family: Some(AddressFamily::V4),
            network_id: None,
            pool_id: None,
        })
        .send()
        .await
        .unwrap()
        .into_inner();
    let want: Ipv4Addr = "203.0.113.2".parse().unwrap();
    assert_eq!(
        fip.address,
        IpAddr::V4(want),
        "first v4 alloc skips network+gateway"
    );
    assert!(fip.attached_to.is_none());

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn allocate_v6_from_documentation_pool() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let fx = build_fixture(&root).await;

    let fip = root
        .create_floating_ip_v1()
        .tenant(fx.tenant_id)
        .project(fx.project_id)
        .body(NewFloatingIp {
            name: "v6".to_string(),
            description: None,
            family: Some(AddressFamily::V6),
            network_id: None,
            pool_id: None,
        })
        .send()
        .await
        .unwrap()
        .into_inner();
    let want: Ipv6Addr = "2001:db8::2".parse().unwrap();
    assert_eq!(fip.address, IpAddr::V6(want));

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn attach_replaces_existing_attachment_atomically() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let fx = build_fixture(&root).await;

    // Two instances, two NICs, one FloatingIp.
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
    let host_cn = Uuid::new_v4();
    place_instance(&test, inst_a.id, host_cn).await;
    place_instance(&test, inst_b.id, host_cn).await;
    let agent = spawn_cn_agent(Arc::clone(&test.store), host_cn);
    let nic_a = root
        .list_nics_v1()
        .instance(inst_a.id)
        .send()
        .await
        .unwrap()
        .into_inner()
        .items[0]
        .clone();
    let nic_b = root
        .list_nics_v1()
        .instance(inst_b.id)
        .send()
        .await
        .unwrap()
        .into_inner()
        .items[0]
        .clone();
    let fip = root
        .create_floating_ip_v1()
        .tenant(fx.tenant_id)
        .project(fx.project_id)
        .body(NewFloatingIp {
            name: "public".to_string(),
            description: None,
            family: Some(AddressFamily::V4),
            network_id: None,
            pool_id: None,
        })
        .send()
        .await
        .unwrap()
        .into_inner();

    let attached_a = root
        .attach_project_floating_ip()
        .tenant_id(fx.tenant_id)
        .project_id(fx.project_id)
        .floating_ip_id(fip.id)
        .body(AttachFloatingIpRequest { nic_id: nic_a.id })
        .send()
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        attached_a.attached_to.as_ref().map(|a| a.nic_id),
        Some(nic_a.id)
    );

    // Re-attach to inst_b's NIC without an explicit detach. Replace
    // semantics: a single observable state change.
    let attached_b = root
        .attach_project_floating_ip()
        .tenant_id(fx.tenant_id)
        .project_id(fx.project_id)
        .floating_ip_id(fip.id)
        .body(AttachFloatingIpRequest { nic_id: nic_b.id })
        .send()
        .await
        .unwrap()
        .into_inner();
    let attach = attached_b
        .attached_to
        .as_ref()
        .expect("should still be attached");
    assert_eq!(attach.nic_id, nic_b.id);
    assert_eq!(attach.instance_id, inst_b.id);

    agent.abort();
    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn delete_while_attached_returns_409() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let fx = build_fixture(&root).await;
    let inst = root
        .create_instance_v1()
        .tenant(fx.tenant_id)
        .project(fx.project_id)
        .body(instance_req(&fx, "a"))
        .send()
        .await
        .unwrap()
        .into_inner();
    let host_cn = Uuid::new_v4();
    place_instance(&test, inst.id, host_cn).await;
    let agent = spawn_cn_agent(Arc::clone(&test.store), host_cn);
    let nic = root
        .list_nics_v1()
        .instance(inst.id)
        .send()
        .await
        .unwrap()
        .into_inner()
        .items[0]
        .clone();
    let fip = root
        .create_floating_ip_v1()
        .tenant(fx.tenant_id)
        .project(fx.project_id)
        .body(NewFloatingIp {
            name: "public".to_string(),
            description: None,
            family: Some(AddressFamily::V4),
            network_id: None,
            pool_id: None,
        })
        .send()
        .await
        .unwrap()
        .into_inner();
    root.attach_project_floating_ip()
        .tenant_id(fx.tenant_id)
        .project_id(fx.project_id)
        .floating_ip_id(fip.id)
        .body(AttachFloatingIpRequest { nic_id: nic.id })
        .send()
        .await
        .unwrap();

    let err = root
        .delete_floating_ip_v1()
        .floating_ip_id(fip.id)
        .send()
        .await
        .expect_err("delete-while-attached must 409");
    assert_status(err, 409);

    // Detach + delete works.
    root.detach_project_floating_ip()
        .tenant_id(fx.tenant_id)
        .project_id(fx.project_id)
        .floating_ip_id(fip.id)
        .send()
        .await
        .unwrap();
    root.delete_floating_ip_v1()
        .floating_ip_id(fip.id)
        .send()
        .await
        .unwrap();

    agent.abort();
    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn instance_delete_auto_detaches_but_does_not_release() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let fx = build_fixture(&root).await;
    let inst = root
        .create_instance_v1()
        .tenant(fx.tenant_id)
        .project(fx.project_id)
        .body(instance_req(&fx, "a"))
        .send()
        .await
        .unwrap()
        .into_inner();
    let host_cn = Uuid::new_v4();
    place_instance(&test, inst.id, host_cn).await;
    let agent = spawn_cn_agent(Arc::clone(&test.store), host_cn);
    let nic = root
        .list_nics_v1()
        .instance(inst.id)
        .send()
        .await
        .unwrap()
        .into_inner()
        .items[0]
        .clone();
    let fip = root
        .create_floating_ip_v1()
        .tenant(fx.tenant_id)
        .project(fx.project_id)
        .body(NewFloatingIp {
            name: "public".to_string(),
            description: None,
            family: Some(AddressFamily::V4),
            network_id: None,
            pool_id: None,
        })
        .send()
        .await
        .unwrap()
        .into_inner();
    root.attach_project_floating_ip()
        .tenant_id(fx.tenant_id)
        .project_id(fx.project_id)
        .floating_ip_id(fip.id)
        .body(AttachFloatingIpRequest { nic_id: nic.id })
        .send()
        .await
        .unwrap();
    let original_address = fip.address;

    // Run instance to Running, stop, wait Stopped, delete.
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

    // The FloatingIp persists, just detached.
    let after = root
        .get_project_floating_ip()
        .tenant_id(fx.tenant_id)
        .project_id(fx.project_id)
        .floating_ip_id(fip.id)
        .send()
        .await
        .unwrap()
        .into_inner();
    assert!(after.attached_to.is_none(), "should auto-detach");
    assert_eq!(after.address, original_address, "address preserved");
    assert_eq!(
        after.project_id, fx.project_id,
        "project ownership preserved"
    );

    agent.abort();
    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cross_project_attach_target_returns_404() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let fx_a = build_fixture(&root).await;
    let fx_b = build_fixture(&root).await;
    // Instance + NIC under project B.
    let inst_b = root
        .create_instance_v1()
        .tenant(fx_b.tenant_id)
        .project(fx_b.project_id)
        .body(instance_req(&fx_b, "b"))
        .send()
        .await
        .unwrap()
        .into_inner();
    let nic_b = root
        .list_nics_v1()
        .instance(inst_b.id)
        .send()
        .await
        .unwrap()
        .into_inner()
        .items[0]
        .clone();
    // FloatingIp under project A.
    let fip_a = root
        .create_floating_ip_v1()
        .tenant(fx_a.tenant_id)
        .project(fx_a.project_id)
        .body(NewFloatingIp {
            name: "public".to_string(),
            description: None,
            family: Some(AddressFamily::V4),
            network_id: None,
            pool_id: None,
        })
        .send()
        .await
        .unwrap()
        .into_inner();

    // Attempt to attach project A's FloatingIp to project B's NIC.
    // Cross-tenant — must 404 (defence-in-depth on the NIC's
    // silo + project matching the FloatingIp's).
    let err = root
        .attach_project_floating_ip()
        .tenant_id(fx_a.tenant_id)
        .project_id(fx_a.project_id)
        .floating_ip_id(fip_a.id)
        .body(AttachFloatingIpRequest { nic_id: nic_b.id })
        .send()
        .await
        .expect_err("cross-project attach must 404");
    assert_status(err, 404);

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn detach_is_idempotent() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let fx = build_fixture(&root).await;
    let fip = root
        .create_floating_ip_v1()
        .tenant(fx.tenant_id)
        .project(fx.project_id)
        .body(NewFloatingIp {
            name: "public".to_string(),
            description: None,
            family: Some(AddressFamily::V4),
            network_id: None,
            pool_id: None,
        })
        .send()
        .await
        .unwrap()
        .into_inner();

    // Detach an already-detached IP — no error.
    let after = root
        .detach_project_floating_ip()
        .tenant_id(fx.tenant_id)
        .project_id(fx.project_id)
        .floating_ip_id(fip.id)
        .send()
        .await
        .unwrap()
        .into_inner();
    assert!(after.attached_to.is_none());

    test.close().await;
}
