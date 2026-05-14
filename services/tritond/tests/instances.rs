// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

#![allow(clippy::unwrap_used, clippy::expect_used)]

//! End-to-end tests for the
//! `/v2/silos/{silo_id}/projects/{project_id}/instances` surface,
//! including the lifecycle state machine.
//!
//! Phase 0 collapses Pending → Provisioning → Running into a
//! synchronous transition inside the create handler, so these tests
//! observe `Running` immediately after a successful create. The
//! intent-queue slice will introduce the intermediate states; tests
//! that care about transitions will be updated then.

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

const ROOT_PASSWORD: &str = "instances-test";

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

/// Bring up a full fixture: silo + project + vpc + subnet + image
/// + ssh key. Returns ids for use in instance-create tests.
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
            name: "ubuntu-base".to_string(),
            description: None,
            os: "linux".to_string(),
            version: "ubuntu-22.04".to_string(),
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
        mac: None,
    }
}

fn lifecycle_state(state: &LifecycleState) -> &'static str {
    // Progenitor flattens the server's externally-tagged enum:
    // unit variants for the data-less states + Failed(String) for
    // the one variant that carries a reason.
    match state {
        LifecycleState::Pending => "Pending",
        LifecycleState::Provisioning => "Provisioning",
        LifecycleState::Running => "Running",
        LifecycleState::Stopping => "Stopping",
        LifecycleState::Stopped => "Stopped",
        LifecycleState::Failed(_) => "Failed",
    }
}

/// Poll the get-instance endpoint until the lifecycle reaches the
/// expected state, with a deadline. The stub provisioner runs at
/// 50ms; tests typically settle in 100-200ms with worker_threads=4.
async fn wait_for_lifecycle(
    client: &tritond_client::Client,
    tenant_id: Uuid,
    project_id: Uuid,
    instance_id: Uuid,
    expected: &str,
    deadline: Duration,
) -> tritond_client::types::Instance {
    let start = Instant::now();
    loop {
        let inst = client
            .get_project_instance()
            .tenant_id(tenant_id)
            .project_id(project_id)
            .instance_id(instance_id)
            .send()
            .await
            .unwrap()
            .into_inner();
        let observed = lifecycle_state(&inst.lifecycle);
        if observed == expected {
            return inst;
        }
        if start.elapsed() > deadline {
            panic!("timeout waiting for lifecycle={expected}; last seen={observed}");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Reasonable settle deadline for the stub provisioner. Each
/// provision drives 2 transitions (Pending → Provisioning →
/// Running); restart drives 4. Even on a loaded CI machine this
/// should land in well under a second.
const SETTLE: Duration = Duration::from_secs(5);

// ---------- tests ----------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn instance_create_settles_at_running_via_queue() {
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
    // Create handler returns Pending; the stub provisioner drives
    // Pending → Provisioning → Running asynchronously.
    assert_eq!(lifecycle_state(&inst.lifecycle), "Pending");
    assert_eq!(inst.tenant_id, fx.tenant_id);
    assert_eq!(inst.project_id, fx.project_id);

    let settled = wait_for_lifecycle(
        &root,
        fx.tenant_id,
        fx.project_id,
        inst.id,
        "Running",
        SETTLE,
    )
    .await;
    assert_eq!(lifecycle_state(&settled.lifecycle), "Running");

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn instance_lifecycle_start_stop_restart() {
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
    wait_for_lifecycle(
        &root,
        fx.tenant_id,
        fx.project_id,
        inst.id,
        "Running",
        SETTLE,
    )
    .await;

    // Running → Stopping → Stopped (handler returns Stopping; agent
    // drives the rest).
    let stop_response = root
        .stop_project_instance()
        .tenant_id(fx.tenant_id)
        .project_id(fx.project_id)
        .instance_id(inst.id)
        .send()
        .await
        .unwrap()
        .into_inner();
    assert_eq!(lifecycle_state(&stop_response.lifecycle), "Stopping");
    wait_for_lifecycle(
        &root,
        fx.tenant_id,
        fx.project_id,
        inst.id,
        "Stopped",
        SETTLE,
    )
    .await;

    // Stop while already stopped → 409 (CAS rejects). NB: there's a
    // brief window during Stopping where the CAS would also reject;
    // we guard the test by waiting for Stopped first.
    let err = root
        .stop_project_instance()
        .tenant_id(fx.tenant_id)
        .project_id(fx.project_id)
        .instance_id(inst.id)
        .send()
        .await
        .expect_err("stop a stopped instance must 409");
    assert_status(err, 409);

    // Stopped → Pending → Running (start enqueues a Provision; agent
    // drives Pending → Provisioning → Running).
    let start_response = root
        .start_project_instance()
        .tenant_id(fx.tenant_id)
        .project_id(fx.project_id)
        .instance_id(inst.id)
        .send()
        .await
        .unwrap()
        .into_inner();
    assert_eq!(lifecycle_state(&start_response.lifecycle), "Pending");
    wait_for_lifecycle(
        &root,
        fx.tenant_id,
        fx.project_id,
        inst.id,
        "Running",
        SETTLE,
    )
    .await;

    // Restart: handler returns Stopping; agent drives the full
    // restart cycle Stopping → Pending → Provisioning → Running.
    let restart_response = root
        .restart_project_instance()
        .tenant_id(fx.tenant_id)
        .project_id(fx.project_id)
        .instance_id(inst.id)
        .send()
        .await
        .unwrap()
        .into_inner();
    assert_eq!(lifecycle_state(&restart_response.lifecycle), "Stopping");
    wait_for_lifecycle(
        &root,
        fx.tenant_id,
        fx.project_id,
        inst.id,
        "Running",
        SETTLE,
    )
    .await;

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn delete_running_instance_returns_409() {
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
    wait_for_lifecycle(
        &root,
        fx.tenant_id,
        fx.project_id,
        inst.id,
        "Running",
        SETTLE,
    )
    .await;

    let err = root
        .delete_project_instance()
        .tenant_id(fx.tenant_id)
        .project_id(fx.project_id)
        .instance_id(inst.id)
        .send()
        .await
        .expect_err("delete while running must 409");
    assert_status(err, 409);

    // Stop, wait for it to settle, then delete works.
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

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn zero_cpu_or_memory_returns_400() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let fx = build_fixture(&root).await;

    let mut req = instance_req(&fx, "zero-cpu");
    req.cpu = 0;
    let err = root
        .create_project_instance()
        .tenant_id(fx.tenant_id)
        .project_id(fx.project_id)
        .body(req)
        .send()
        .await
        .expect_err("zero cpu must 400");
    assert_status(err, 400);

    let mut req = instance_req(&fx, "zero-mem");
    req.memory_bytes = 0;
    let err = root
        .create_project_instance()
        .tenant_id(fx.tenant_id)
        .project_id(fx.project_id)
        .body(req)
        .send()
        .await
        .expect_err("zero memory must 400");
    assert_status(err, 400);

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unknown_image_returns_404() {
    // Slice F: image visibility is no longer enforced by silo
    // membership for the root principal — root sees every image.
    // The cross-tenant invariant is exercised against non-root
    // principals in `image_scope.rs`. This test keeps coverage
    // for the "image_id doesn't exist at all" branch.
    let test = TestServer::start().await;
    let root = test.root_client();
    let fx = build_fixture(&root).await;
    let mut req = instance_req(&fx, "x");
    req.image_id = Uuid::new_v4();
    let err = root
        .create_project_instance()
        .tenant_id(fx.tenant_id)
        .project_id(fx.project_id)
        .body(req)
        .send()
        .await
        .expect_err("unknown image must 404");
    assert_status(err, 404);

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cross_silo_get_returns_404() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let fx_a = build_fixture(&root).await;
    let fx_b = build_fixture(&root).await;
    let inst = root
        .create_project_instance()
        .tenant_id(fx_a.tenant_id)
        .project_id(fx_a.project_id)
        .body(instance_req(&fx_a, "web"))
        .send()
        .await
        .unwrap()
        .into_inner();

    // Same instance id, but path silo+project belong to fx_b →
    // defence-in-depth 404.
    let err = root
        .get_project_instance()
        .tenant_id(fx_b.tenant_id)
        .project_id(fx_b.project_id)
        .instance_id(inst.id)
        .send()
        .await
        .expect_err("cross-silo get must 404");
    assert_status(err, 404);

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn duplicate_instance_name_within_project_returns_409() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let fx = build_fixture(&root).await;
    root.create_project_instance()
        .tenant_id(fx.tenant_id)
        .project_id(fx.project_id)
        .body(instance_req(&fx, "web"))
        .send()
        .await
        .unwrap();
    let err = root
        .create_project_instance()
        .tenant_id(fx.tenant_id)
        .project_id(fx.project_id)
        .body(instance_req(&fx, "web"))
        .send()
        .await
        .expect_err("duplicate name must 409");
    assert_status(err, 409);

    test.close().await;
}

/// SG-4 end-to-end: creating an instance produces an observable
/// `instance-create` operation on `/v2/operations`. This is the
/// shippable proof that the saga engine + the operator-visibility
/// surface are wired correctly to each other.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn instance_create_appears_on_operations_surface() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let fx = build_fixture(&root).await;

    let inst = root
        .create_project_instance()
        .tenant_id(fx.tenant_id)
        .project_id(fx.project_id)
        .body(instance_req(&fx, "obs"))
        .send()
        .await
        .unwrap()
        .into_inner();
    assert_eq!(lifecycle_state(&inst.lifecycle), "Pending");

    // The saga finishes synchronously inside the handler (SG-2 is
    // a 4-action chain with no await_terminal); the operation
    // should land on the listing immediately.
    let ops = root.list_operations().send().await.unwrap().into_inner();
    let our_op = ops
        .iter()
        .find(|o| o.kind == "instance-create")
        .expect("instance-create operation must be visible on /v2/operations");
    assert_eq!(our_op.version, 1);
    // The saga has run to terminal (Done with Ok); the
    // operations surface maps that to "done".
    let state_str = serde_json::to_value(&our_op.state)
        .unwrap()
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(state_str, "done", "operation should land Done");
    assert!(
        our_op.stuck_reason.is_none(),
        "happy-path saga must not be Stuck"
    );

    // The detail surface returns the persisted DAG (4 actions for
    // SG-2). We don't assert exact DAG bytes — they're internal to
    // Steno — but the call should succeed and return our id.
    let detail = root
        .get_operation()
        .operation_id(our_op.id)
        .send()
        .await
        .unwrap()
        .into_inner();
    assert_eq!(detail.id, our_op.id);
    assert_eq!(detail.kind, "instance-create");

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn anonymous_cannot_reach_instance_endpoints() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let fx = build_fixture(&root).await;
    let anon = test.anonymous_client();
    let err = anon
        .list_project_instances()
        .tenant_id(fx.tenant_id)
        .project_id(fx.project_id)
        .send()
        .await
        .expect_err("anonymous list must be denied");
    assert_status(err, 404);

    test.close().await;
}
