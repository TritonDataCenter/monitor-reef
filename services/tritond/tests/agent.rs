// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

#![allow(clippy::unwrap_used, clippy::expect_used)]

//! End-to-end tests for the agent transport seam (`/v2/agent/*`).
//!
//! Strategy: stand up a tritond with the in-process stub
//! provisioner *disabled* so the test owns the queue. Mint an
//! API key with `ApiKeyScope::Agent`. Drive claim → complete via
//! the generated client and verify (a) empty queue returns
//! `{"job": null}`, (b) a Pending job is claimed and transitions
//! to `InProgress`, (c) `complete` lands the job in `Completed`,
//! (d) a re-claim returns null, and (e) keys with other scopes
//! cannot reach `/v2/agent/*` at all.

use std::sync::Arc;

use chrono::Utc;
use tritond::audit::AuditService;
use tritond::auth::AuthService;
use tritond::{ApiContext, start_server_with_context};
use tritond_audit::MemChain;
use tritond_auth::{JwtKey, RedactedString, hash_password};
use tritond_client::types::{
    AddressFamily, ApiKeyScope, ApproveCnRequest, ClaimJobRequest, CnState, CompleteJobRequest,
    JobOutcome, JobStatus, LoginRequest, NatGateway, NetworkRealizationRequest, NetworkResourceId,
    NewApiKey, NewInstance as ClientNewInstance, NewNatGateway, NewProject, NewSilo, NewVpc,
    RealizationStatus, RealizerId, RegisterCnRequest,
};
use tritond_store::{
    Instance, JobKind, LifecycleState, LifecycleStateKind, MemStore, NewJob, Nic, Store, User,
};
use uuid::Uuid;

const ROOT_PASSWORD: &str = "correct horse battery staple";

struct TestServer {
    server: dropshot::HttpServer<ApiContext>,
    store: Arc<dyn Store>,
}

impl TestServer {
    async fn start() -> Self {
        let store: Arc<dyn Store> = Arc::new(MemStore::new());
        let user = User {
            id: Uuid::new_v4(),
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
        let auth = Arc::new(AuthService::new(JwtKey::generate()).unwrap());
        let audit = Arc::new(AuditService::new(Arc::new(MemChain::new())));
        let context = ApiContext::new(Arc::clone(&store), auth, audit)
            // The agent test owns the queue — disable the stub so
            // the real-agent path is the only consumer.
            .without_in_process_provisioner();
        let server = start_server_with_context("127.0.0.1:0", context)
            .await
            .unwrap();
        Self { server, store }
    }

    fn bind(&self) -> std::net::SocketAddr {
        self.server.local_addr()
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

/// Authenticate as root and mint an API key at the requested scope.
/// Returns the wire-form `tcadm_…` plaintext.
async fn mint_key(test: &TestServer, scope: ApiKeyScope) -> String {
    let session = root_client(test).await;
    let created = session
        .create_api_key()
        .body(NewApiKey {
            description: format!("agent-test-{scope:?}"),
            scope,
        })
        .send()
        .await
        .unwrap()
        .into_inner();
    created.secret
}

async fn root_client(test: &TestServer) -> tritond_client::Client {
    let anon = test.anonymous_client();
    let token = anon
        .login()
        .body(LoginRequest {
            username: "root".to_string(),
            password: ROOT_PASSWORD.to_string(),
        })
        .send()
        .await
        .unwrap()
        .into_inner();
    test.bearer_client(&token.access_token)
}

async fn register_and_approve(test: &TestServer, cn_uuid: Uuid, hostname: &str) -> String {
    let anon = test.anonymous_client();
    let registered = anon
        .agent_register()
        .body(RegisterCnRequest {
            server_uuid: cn_uuid,
            hostname: hostname.to_string(),
            admin_ip: None,
            sysinfo: serde_json::json!({ "hostname": hostname }),
        })
        .send()
        .await
        .unwrap()
        .into_inner();
    assert!(matches!(registered.state, CnState::Pending));

    let root = root_client(test).await;
    root.approve_cn()
        .body(ApproveCnRequest {
            code: registered.claim_code.unwrap(),
        })
        .send()
        .await
        .unwrap();

    let status = anon
        .agent_register_status()
        .poll_token(&registered.poll_token)
        .send()
        .await
        .unwrap()
        .into_inner();
    assert!(matches!(status.state, CnState::Approved));
    status.api_key.unwrap()
}

async fn create_nat_gateway(test: &TestServer, silo_name: &str) -> NatGateway {
    let root = root_client(test).await;
    let silo = root
        .create_silo()
        .body(NewSilo {
            name: silo_name.to_string(),
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
            name: "sandbox".to_string(),
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
            name: "prod".to_string(),
            description: None,
            ipv4_block: Some("10.0.0.0/16".to_string()),
            ipv6_block: None,
        })
        .send()
        .await
        .unwrap()
        .into_inner();
    root.create_vpc_nat_gateway()
        .tenant_id(silo.default_tenant_id)
        .project_id(project.id)
        .vpc_id(vpc.id)
        .body(NewNatGateway {
            name: "egress".to_string(),
            description: None,
            family: AddressFamily::V4,
        })
        .send()
        .await
        .unwrap()
        .into_inner()
}

fn realization_report(
    nat_gateway_id: Uuid,
    realizer: RealizerId,
    generation: u64,
    status: RealizationStatus,
) -> NetworkRealizationRequest {
    NetworkRealizationRequest {
        resource: NetworkResourceId::NatGateway(nat_gateway_id),
        realizer,
        generation,
        status,
        message: Some("test report".to_string()),
    }
}

fn realizer_cn_id(realizer: &RealizerId) -> Option<Uuid> {
    match realizer {
        RealizerId::Cn(id) => Some(*id),
        _ => None,
    }
}

fn assert_status(err: progenitor_client::Error<tritond_client::types::Error>, want: u16) {
    let progenitor_client::Error::ErrorResponse(response) = err else {
        panic!("expected ErrorResponse, got {err:?}");
    };
    assert_eq!(response.status().as_u16(), want);
}

fn parse_test_mac_bytes(value: &str) -> [u8; 6] {
    let mut mac = [0u8; 6];
    let mut parts = value.split(':');
    for byte in &mut mac {
        let part = parts.next().expect("stored test MAC has six octets");
        *byte = u8::from_str_radix(part, 16).expect("stored test MAC uses hex octets");
    }
    assert!(parts.next().is_none(), "stored test MAC has six octets");
    mac
}

async fn create_instance_with_primary_nic(test: &TestServer, silo_name: &str) -> (Instance, Nic) {
    let silo = test
        .store
        .create_silo(tritond_store::NewSilo {
            name: silo_name.to_string(),
            description: None,
        })
        .await
        .unwrap();
    let project = test
        .store
        .create_project(
            silo.default_tenant_id,
            tritond_store::NewProject {
                name: "p1".to_string(),
                description: None,
            },
        )
        .await
        .unwrap();
    let image = test
        .store
        .create_image_silo(
            silo.id,
            tritond_store::NewImage {
                name: "test-image".to_string(),
                description: None,
                os: "smartos".to_string(),
                version: "test".to_string(),
                size_bytes: 1_000_000,
                sha256: "0".repeat(64),
                source_url: None,
                id: None,
                compatibility: None,
            },
        )
        .await
        .unwrap();
    let vpc = test
        .store
        .create_vpc(
            silo.default_tenant_id,
            project.id,
            tritond_store::NewVpc {
                name: "v1".to_string(),
                description: None,
                ipv4_block: Some("10.0.0.0/24".parse().unwrap()),
                ipv6_block: None,
            },
        )
        .await
        .unwrap();
    let subnet = test
        .store
        .create_subnet(
            silo.default_tenant_id,
            project.id,
            vpc.id,
            tritond_store::NewSubnet {
                name: "s1".to_string(),
                description: None,
                ipv4_block: Some("10.0.0.0/29".parse().unwrap()),
                ipv6_block: None,
            },
        )
        .await
        .unwrap();
    let created = test
        .store
        .create_instance(
            silo.default_tenant_id,
            project.id,
            tritond_store::NewInstance {
                name: "port-blueprint-test".to_string(),
                description: None,
                image_id: image.id,
                primary_subnet_id: subnet.id,
                ssh_key_ids: Vec::new(),
                cpu: 1,
                memory_bytes: 256 * 1024 * 1024,
                extra_nics: Vec::new(),
            },
        )
        .await
        .unwrap();
    let instance = created.instance;
    let primary_nic = test
        .store
        .list_nics_for_instance(instance.id)
        .await
        .unwrap()
        .into_iter()
        .next()
        .expect("instance create should create a primary NIC");

    (instance, primary_nic)
}

async fn http_instance_fixture(test: &TestServer, silo_name: &str) -> (Uuid, Uuid, Uuid, Uuid) {
    let silo = test
        .store
        .create_silo(tritond_store::NewSilo {
            name: silo_name.to_string(),
            description: None,
        })
        .await
        .unwrap();
    let project = test
        .store
        .create_project(
            silo.default_tenant_id,
            tritond_store::NewProject {
                name: "p1".to_string(),
                description: None,
            },
        )
        .await
        .unwrap();
    let image = test
        .store
        .create_image_silo(
            silo.id,
            tritond_store::NewImage {
                name: "test-image".to_string(),
                description: None,
                os: "smartos".to_string(),
                version: "test".to_string(),
                size_bytes: 1_000_000,
                sha256: "0".repeat(64),
                source_url: None,
                id: None,
                compatibility: None,
            },
        )
        .await
        .unwrap();
    let vpc = test
        .store
        .create_vpc(
            silo.default_tenant_id,
            project.id,
            tritond_store::NewVpc {
                name: "v1".to_string(),
                description: None,
                ipv4_block: Some("10.0.0.0/24".parse().unwrap()),
                ipv6_block: None,
            },
        )
        .await
        .unwrap();
    let subnet = test
        .store
        .create_subnet(
            silo.default_tenant_id,
            project.id,
            vpc.id,
            tritond_store::NewSubnet {
                name: "s1".to_string(),
                description: None,
                ipv4_block: Some("10.0.0.0/29".parse().unwrap()),
                ipv6_block: None,
            },
        )
        .await
        .unwrap();

    (silo.default_tenant_id, project.id, image.id, subnet.id)
}

fn client_instance_req(name: &str, image_id: Uuid, subnet_id: Uuid) -> ClientNewInstance {
    ClientNewInstance {
        name: name.to_string(),
        description: None,
        image_id,
        primary_subnet_id: subnet_id,
        ssh_key_ids: Vec::new(),
        cpu: 1,
        memory_bytes: 256 * 1024 * 1024,
        extra_nics: Vec::new(),
    }
}

#[tokio::test]
async fn agent_claim_returns_null_on_empty_queue() {
    let test = TestServer::start().await;
    let secret = mint_key(&test, ApiKeyScope::Agent).await;
    let client = test.bearer_client(&secret);

    let resp = client
        .agent_claim_job()
        .body(ClaimJobRequest {
            claimed_by: "agent-A".to_string(),
        })
        .send()
        .await
        .expect("Agent claim must succeed even on empty queue");
    assert!(
        resp.into_inner().job.is_none(),
        "empty queue should yield job=None",
    );

    test.close().await;
}

#[tokio::test]
async fn instance_create_routes_jobs_to_least_assigned_tenant_cn() {
    let test = TestServer::start().await;
    let cn_a = Uuid::from_u128(1);
    let cn_b = Uuid::from_u128(2);
    let key_a = register_and_approve(&test, cn_a, "cn-a").await;
    let key_b = register_and_approve(&test, cn_b, "cn-b").await;
    let (tenant_id, project_id, image_id, subnet_id) =
        http_instance_fixture(&test, "agent-placement").await;
    let root = root_client(&test).await;

    let first = root
        .create_project_instance()
        .tenant_id(tenant_id)
        .project_id(project_id)
        .body(client_instance_req("web-a", image_id, subnet_id))
        .send()
        .await
        .unwrap()
        .into_inner();
    let second = root
        .create_project_instance()
        .tenant_id(tenant_id)
        .project_id(project_id)
        .body(client_instance_req("web-b", image_id, subnet_id))
        .send()
        .await
        .unwrap()
        .into_inner();

    assert_eq!(first.host_cn_uuid, Some(cn_a));
    assert_eq!(second.host_cn_uuid, Some(cn_b));

    let agent_b = test.bearer_client(&key_b);
    let claimed_b = agent_b
        .agent_claim_job()
        .body(ClaimJobRequest {
            claimed_by: cn_b.to_string(),
        })
        .send()
        .await
        .unwrap()
        .into_inner()
        .job
        .expect("cn-b should claim the second routed job");
    assert_eq!(claimed_b.target_cn_uuid, Some(cn_b));
    match claimed_b.kind {
        tritond_client::types::JobKind::Provision { instance_id } => {
            assert_eq!(instance_id, second.id);
        }
        other => panic!("expected provision job, got {other:?}"),
    }

    let agent_a = test.bearer_client(&key_a);
    let claimed_a = agent_a
        .agent_claim_job()
        .body(ClaimJobRequest {
            claimed_by: cn_a.to_string(),
        })
        .send()
        .await
        .unwrap()
        .into_inner()
        .job
        .expect("cn-a should claim the first routed job");
    assert_eq!(claimed_a.target_cn_uuid, Some(cn_a));
    match claimed_a.kind {
        tritond_client::types::JobKind::Provision { instance_id } => {
            assert_eq!(instance_id, first.id);
        }
        other => panic!("expected provision job, got {other:?}"),
    }

    test.close().await;
}

#[tokio::test]
async fn agent_claim_then_complete_drains_queue() {
    let test = TestServer::start().await;
    let secret = mint_key(&test, ApiKeyScope::Agent).await;
    let client = test.bearer_client(&secret);

    // Direct-store enqueue: skips the instance-create flow so the
    // test focuses on the agent surface alone.
    let instance_id = Uuid::new_v4();
    let queued = test
        .store
        .enqueue_job(NewJob {
            kind: JobKind::Provision { instance_id },
            target_cn_uuid: None,
        })
        .await
        .unwrap();

    let claimed = client
        .agent_claim_job()
        .body(ClaimJobRequest {
            claimed_by: "agent-B".to_string(),
        })
        .send()
        .await
        .expect("claim should succeed")
        .into_inner()
        .job
        .expect("queue had a job; claim must return Some");
    assert_eq!(claimed.id, queued.id);
    assert!(matches!(claimed.status, JobStatus::InProgress));
    assert_eq!(claimed.claimed_by.as_deref(), Some("agent-B"));

    let completed = client
        .agent_complete_job()
        .job_id(claimed.id)
        .body(CompleteJobRequest {
            outcome: JobOutcome::Completed,
        })
        .send()
        .await
        .expect("complete should succeed")
        .into_inner();
    assert!(matches!(completed.status, JobStatus::Completed));

    // Queue is now drained.
    let next = client
        .agent_claim_job()
        .body(ClaimJobRequest {
            claimed_by: "agent-B".to_string(),
        })
        .send()
        .await
        .unwrap()
        .into_inner();
    assert!(next.job.is_none(), "queue should be empty after complete");

    test.close().await;
}

#[tokio::test]
async fn agent_complete_with_failure_records_reason() {
    let test = TestServer::start().await;
    let secret = mint_key(&test, ApiKeyScope::Agent).await;
    let client = test.bearer_client(&secret);

    let instance_id = Uuid::new_v4();
    test.store
        .enqueue_job(NewJob {
            kind: JobKind::Provision { instance_id },
            target_cn_uuid: None,
        })
        .await
        .unwrap();

    let claimed = client
        .agent_claim_job()
        .body(ClaimJobRequest {
            claimed_by: "agent-fail".to_string(),
        })
        .send()
        .await
        .unwrap()
        .into_inner()
        .job
        .unwrap();

    let completed = client
        .agent_complete_job()
        .job_id(claimed.id)
        .body(CompleteJobRequest {
            outcome: JobOutcome::Failed("image not on host".to_string()),
        })
        .send()
        .await
        .expect("complete with failure should succeed")
        .into_inner();
    match completed.status {
        JobStatus::Failed(reason) => assert_eq!(reason, "image not on host"),
        other => panic!("expected Failed, got {other:?}"),
    }

    test.close().await;
}

#[tokio::test]
async fn read_only_scope_cannot_reach_agent_surface() {
    // ReadOnly scope is the highest read-allowed scope; if it can't
    // touch /v2/agent/* then neither can anything narrower.
    let test = TestServer::start().await;
    let secret = mint_key(&test, ApiKeyScope::ReadOnly).await;
    let client = test.bearer_client(&secret);

    let err = client
        .agent_claim_job()
        .body(ClaimJobRequest {
            claimed_by: "should-not-reach".to_string(),
        })
        .send()
        .await
        .expect_err("ReadOnly scope must not authorise agent_claim");
    let progenitor_client::Error::ErrorResponse(resp) = err else {
        panic!("expected ErrorResponse, got {err:?}");
    };
    assert_eq!(resp.status().as_u16(), 403);

    test.close().await;
}

#[tokio::test]
async fn blueprint_returns_kind_and_instance_when_present() {
    let test = TestServer::start().await;
    let secret = mint_key(&test, ApiKeyScope::Agent).await;
    let client = test.bearer_client(&secret);

    // Enqueue a Provision job pointing at a synthetic instance id.
    // The instance itself doesn't exist in this test (we're not
    // exercising the full instance create path); the blueprint
    // must still resolve cleanly with `instance: None`.
    let phantom_instance = Uuid::new_v4();
    let job = test
        .store
        .enqueue_job(NewJob {
            kind: JobKind::Provision {
                instance_id: phantom_instance,
            },
            target_cn_uuid: None,
        })
        .await
        .unwrap();

    let bp = client
        .agent_job_blueprint()
        .job_id(job.id)
        .send()
        .await
        .expect("Agent scope must be able to fetch its blueprint")
        .into_inner();
    assert_eq!(bp.job_id, job.id);
    assert!(bp.instance.is_none(), "phantom instance → instance: None");
    assert!(bp.image.is_none());
    assert!(bp.nics.is_empty());
    assert!(bp.disks.is_empty());
    assert!(bp.ssh_public_keys.is_empty());

    test.close().await;
}

#[tokio::test]
async fn blueprint_returns_empty_instance_payload_for_edge_jobs() {
    let test = TestServer::start().await;
    let secret = mint_key(&test, ApiKeyScope::Agent).await;
    let client = test.bearer_client(&secret);

    let edge_instance_id = Uuid::new_v4();
    let manifest_bytes = br#"{"dataplane":{"backend":"nftables"}}"#.to_vec();
    let job = test
        .store
        .enqueue_job(NewJob {
            kind: JobKind::EdgeApply {
                edge_instance_id,
                manifest_bytes,
            },
            target_cn_uuid: None,
        })
        .await
        .unwrap();

    let bp = client
        .agent_job_blueprint()
        .job_id(job.id)
        .send()
        .await
        .expect("edge job blueprint should not require a VM instance")
        .into_inner();
    assert_eq!(bp.job_id, job.id);
    assert!(bp.instance.is_none());
    assert!(bp.image.is_none());
    assert!(bp.nics.is_empty());
    assert!(bp.disks.is_empty());
    assert!(bp.ssh_public_keys.is_empty());

    test.close().await;
}

#[tokio::test]
async fn blueprint_denied_to_read_only_scope() {
    let test = TestServer::start().await;
    let phantom_instance = Uuid::new_v4();
    let job = test
        .store
        .enqueue_job(NewJob {
            kind: JobKind::Provision {
                instance_id: phantom_instance,
            },
            target_cn_uuid: None,
        })
        .await
        .unwrap();
    let secret = mint_key(&test, ApiKeyScope::ReadOnly).await;
    let client = test.bearer_client(&secret);
    let err = client
        .agent_job_blueprint()
        .job_id(job.id)
        .send()
        .await
        .expect_err("ReadOnly scope must not authorise blueprint reads");
    let progenitor_client::Error::ErrorResponse(resp) = err else {
        panic!("expected ErrorResponse, got {err:?}");
    };
    assert_eq!(resp.status().as_u16(), 403);
    test.close().await;
}

#[tokio::test]
async fn bound_agent_can_fetch_port_blueprint_for_claimed_instance() {
    let test = TestServer::start().await;
    let (instance, nic) = create_instance_with_primary_nic(&test, "agent-port-blueprint").await;
    let queued = test
        .store
        .enqueue_job(NewJob {
            kind: JobKind::Provision {
                instance_id: instance.id,
            },
            target_cn_uuid: None,
        })
        .await
        .unwrap();

    let cn_uuid = Uuid::new_v4();
    let api_key = register_and_approve(&test, cn_uuid, "cn-port-blueprint").await;
    let agent = test.bearer_client(&api_key);
    let claimed = agent
        .agent_claim_job()
        .body(ClaimJobRequest {
            claimed_by: cn_uuid.to_string(),
        })
        .send()
        .await
        .unwrap()
        .into_inner()
        .job
        .expect("queue should contain the Provision job");
    assert_eq!(claimed.id, queued.id);

    let response = agent
        .agent_port_blueprint()
        .port_id(nic.id)
        .send()
        .await
        .expect("owning CN can fetch port blueprint")
        .into_inner();
    assert_eq!(response.port_id, nic.id);
    assert_eq!(response.generation, 1);

    let bytes = base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        &response.blueprint_postcard_base64,
    )
    .expect("response should be base64");
    let port: proteus_api::blueprint::PortBlueprint =
        postcard::from_bytes(&bytes).expect("response should be a Proteus PortBlueprint");
    assert_eq!(port.port_id, proteus_api::ids::PortId(nic.id));
    assert_eq!(port.network_id, proteus_api::ids::NetworkId::TRITON_VPC);
    assert_eq!(
        port.schema_version,
        proteus_api::blueprint::PORT_BLUEPRINT_SCHEMA_V0
    );
    assert_eq!(port.generation, proteus_api::ids::Generation::new(1));
    assert_eq!(port.link.mtu, 1500);
    assert_eq!(port.link.mac_address, Some(parse_test_mac_bytes(&nic.mac)));
    assert_eq!(port.link.vlan_id, None);
    assert_eq!(
        port.plugin_config.network,
        proteus_api::ids::NetworkId::TRITON_VPC
    );
    assert_eq!(
        port.plugin_config.plugin_schema,
        triton_vpc::TRITON_VPC_BLUEPRINT_SCHEMA_V1
    );
    assert!(!port.plugin_config.bytes.is_empty());

    test.close().await;
}

#[tokio::test]
async fn port_blueprint_requires_bound_agent_with_inprogress_claim() {
    let test = TestServer::start().await;
    let (_instance, nic) =
        create_instance_with_primary_nic(&test, "agent-port-blueprint-auth").await;

    let unbound_secret = mint_key(&test, ApiKeyScope::Agent).await;
    let err = test
        .bearer_client(&unbound_secret)
        .agent_port_blueprint()
        .port_id(nic.id)
        .send()
        .await
        .expect_err("unbound Agent keys cannot fetch port blueprints");
    assert_status(err, 403);

    let cn_uuid = Uuid::new_v4();
    let api_key = register_and_approve(&test, cn_uuid, "cn-port-blueprint-denied").await;
    let err = test
        .bearer_client(&api_key)
        .agent_port_blueprint()
        .port_id(nic.id)
        .send()
        .await
        .expect_err("bound Agent needs an in-progress claim for the instance");
    assert_status(err, 403);

    test.close().await;
}

/// End-to-end lifecycle drive: an instance in `Pending` is
/// observed in `Provisioning` after the agent claims its
/// Provision job, and in `Running` after the agent reports
/// `Completed`. Skipping vmadm — we're testing the control
/// plane's lifecycle invariant, not the agent's vmadm wrapper.
#[tokio::test]
async fn provision_job_drives_lifecycle_pending_to_running() {
    let test = TestServer::start().await;

    // Set up the resource graph the instance create needs:
    // silo + project + image + vpc + subnet. Creating these via
    // direct store calls is faster than HTTP and the test isn't
    // about the create-flow contracts.
    let silo = test
        .store
        .create_silo(tritond_store::NewSilo {
            name: "agent-lifecycle".to_string(),
            description: None,
        })
        .await
        .unwrap();
    let project = test
        .store
        .create_project(
            silo.default_tenant_id,
            tritond_store::NewProject {
                name: "p1".to_string(),
                description: None,
            },
        )
        .await
        .unwrap();
    let image = test
        .store
        .create_image_silo(
            silo.id,
            tritond_store::NewImage {
                name: "test-image".to_string(),
                description: None,
                os: "smartos".to_string(),
                version: "test".to_string(),
                size_bytes: 1_000_000,
                sha256: "0".repeat(64),
                source_url: None,
                id: None,
                compatibility: None,
            },
        )
        .await
        .unwrap();
    let vpc = test
        .store
        .create_vpc(
            silo.default_tenant_id,
            project.id,
            tritond_store::NewVpc {
                name: "v1".to_string(),
                description: None,
                ipv4_block: Some("10.0.0.0/24".parse().unwrap()),
                ipv6_block: None,
            },
        )
        .await
        .unwrap();
    let subnet = test
        .store
        .create_subnet(
            silo.default_tenant_id,
            project.id,
            vpc.id,
            tritond_store::NewSubnet {
                name: "s1".to_string(),
                description: None,
                ipv4_block: Some("10.0.0.0/29".parse().unwrap()),
                ipv6_block: None,
            },
        )
        .await
        .unwrap();
    let created = test
        .store
        .create_instance(
            silo.default_tenant_id,
            project.id,
            tritond_store::NewInstance {
                name: "lifecycle-test".to_string(),
                description: None,
                image_id: image.id,
                primary_subnet_id: subnet.id,
                ssh_key_ids: Vec::new(),
                cpu: 1,
                memory_bytes: 256 * 1024 * 1024,
                extra_nics: Vec::new(),
            },
        )
        .await
        .unwrap();
    let instance: Instance = created.instance;
    assert!(matches!(instance.lifecycle, LifecycleState::Pending));
    // The HTTP `instance_create` handler enqueues the Provision
    // job; calling the store directly skips that, so we enqueue
    // it explicitly.
    test.store
        .enqueue_job(NewJob {
            kind: JobKind::Provision {
                instance_id: instance.id,
            },
            target_cn_uuid: None,
        })
        .await
        .unwrap();

    // Mint an Agent key and use it to claim the job tritond
    // enqueued during create_instance.
    let secret = mint_key(&test, ApiKeyScope::Agent).await;
    let client = test.bearer_client(&secret);
    let claimed = client
        .agent_claim_job()
        .body(ClaimJobRequest {
            claimed_by: "lifecycle-agent".to_string(),
        })
        .send()
        .await
        .unwrap()
        .into_inner()
        .job
        .expect("queue had the Provision job we just enqueued");
    assert!(matches!(
        claimed.kind,
        tritond_client::types::JobKind::Provision { .. },
    ));

    // The claim handler should have advanced Pending → Provisioning.
    let after_claim = test.store.get_instance(instance.id).await.unwrap();
    assert!(
        matches!(after_claim.lifecycle, LifecycleState::Provisioning),
        "expected Provisioning after claim, got {:?}",
        after_claim.lifecycle,
    );

    // Report Completed: lifecycle should land at Running.
    let _ = client
        .agent_complete_job()
        .job_id(claimed.id)
        .body(CompleteJobRequest {
            outcome: JobOutcome::Completed,
        })
        .send()
        .await
        .unwrap();
    let after_complete = test.store.get_instance(instance.id).await.unwrap();
    assert!(
        matches!(after_complete.lifecycle, LifecycleState::Running),
        "expected Running after complete, got {:?}",
        after_complete.lifecycle,
    );

    test.close().await;
}

/// The sweeper reaps an InProgress job whose claim is older
/// than the configured threshold: the job moves to
/// `Failed { reason: "agent claimed but never completed; reaped
/// by sweeper" }` and the instance lifecycle is driven to
/// `Failed`. Uses very short interval + threshold so the test
/// completes in seconds.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sweeper_reaps_stale_inprogress_job() {
    use std::time::Duration;
    use tritond::SweeperConfig;

    let store: Arc<dyn Store> = Arc::new(MemStore::new());
    let user = User {
        id: Uuid::new_v4(),
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
    let auth = Arc::new(AuthService::new(JwtKey::generate()).unwrap());
    let audit = Arc::new(AuditService::new(Arc::new(MemChain::new())));
    let context = ApiContext::new(Arc::clone(&store), auth, audit)
        .without_in_process_provisioner()
        // Sweep aggressively: every 200ms, anything older than
        // 500ms is stale. The test asserts the sweeper acted
        // within ~3s.
        .with_sweeper(SweeperConfig {
            interval: Duration::from_millis(200),
            stale_after: Duration::from_millis(500),
        });
    let server = start_server_with_context("127.0.0.1:0", context)
        .await
        .unwrap();
    let test = TestServer { server, store };

    // Enqueue + claim a Provision job so it's InProgress with a
    // claimed_at timestamp.
    let phantom_instance = Uuid::new_v4();
    let queued = test
        .store
        .enqueue_job(NewJob {
            kind: JobKind::Provision {
                instance_id: phantom_instance,
            },
            target_cn_uuid: None,
        })
        .await
        .unwrap();
    let claimed = test
        .store
        .claim_next_job("crashed-agent", None)
        .await
        .unwrap();
    assert_eq!(claimed.id, queued.id);
    assert!(matches!(
        claimed.status,
        tritond_store::JobStatus::InProgress
    ));

    // Wait for the sweeper to do its thing. Threshold + interval
    // = 700ms; give it a generous 3s for tokio scheduling jitter.
    let mut final_status = claimed.status.clone();
    for _ in 0..30 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let j = test.store.get_job(queued.id).await.unwrap();
        if matches!(
            j.status.kind(),
            tritond_store::JobStatusKind::Failed | tritond_store::JobStatusKind::Completed
        ) {
            final_status = j.status;
            break;
        }
    }
    match final_status {
        tritond_store::JobStatus::Failed { reason } => {
            assert!(
                reason.contains("sweeper"),
                "Failed reason should mention sweeper, got {reason}",
            );
        }
        other => panic!("expected Failed after sweep, got {other:?}"),
    }

    test.close().await;
}

/// Instance delete enqueues a `JobKind::Delete` job *in
/// addition to* clearing the tritond record. The agent then
/// gets to claim it and drive vmadm-delete on its own host.
#[tokio::test]
async fn instance_delete_enqueues_delete_job_for_agent() {
    let test = TestServer::start().await;
    let silo = test
        .store
        .create_silo(tritond_store::NewSilo {
            name: "agent-delete".to_string(),
            description: None,
        })
        .await
        .unwrap();
    let project = test
        .store
        .create_project(
            silo.default_tenant_id,
            tritond_store::NewProject {
                name: "p1".to_string(),
                description: None,
            },
        )
        .await
        .unwrap();
    let image = test
        .store
        .create_image_silo(
            silo.id,
            tritond_store::NewImage {
                name: "img".to_string(),
                description: None,
                os: "smartos".to_string(),
                version: "test".to_string(),
                size_bytes: 1_000_000,
                sha256: "0".repeat(64),
                source_url: None,
                id: None,
                compatibility: None,
            },
        )
        .await
        .unwrap();
    let vpc = test
        .store
        .create_vpc(
            silo.default_tenant_id,
            project.id,
            tritond_store::NewVpc {
                name: "v1".to_string(),
                description: None,
                ipv4_block: Some("10.0.0.0/24".parse().unwrap()),
                ipv6_block: None,
            },
        )
        .await
        .unwrap();
    let subnet = test
        .store
        .create_subnet(
            silo.default_tenant_id,
            project.id,
            vpc.id,
            tritond_store::NewSubnet {
                name: "s1".to_string(),
                description: None,
                ipv4_block: Some("10.0.0.0/29".parse().unwrap()),
                ipv6_block: None,
            },
        )
        .await
        .unwrap();
    let created = test
        .store
        .create_instance(
            silo.default_tenant_id,
            project.id,
            tritond_store::NewInstance {
                name: "to-delete".to_string(),
                description: None,
                image_id: image.id,
                primary_subnet_id: subnet.id,
                ssh_key_ids: Vec::new(),
                cpu: 1,
                memory_bytes: 256 * 1024 * 1024,
                extra_nics: Vec::new(),
            },
        )
        .await
        .unwrap();
    let instance_id = created.instance.id;
    // The store-side delete only accepts terminal states. Drive
    // the test instance to Stopped directly so we can exercise
    // the delete + Delete-job path without going through the
    // full Provision → Running → Stop dance.
    test.store
        .transition_instance_lifecycle(
            instance_id,
            &[LifecycleStateKind::Pending],
            LifecycleState::Stopped,
        )
        .await
        .unwrap();

    // Authenticated DELETE through the normal operator surface.
    let anon = test.anonymous_client();
    let token = anon
        .login()
        .body(LoginRequest {
            username: "root".to_string(),
            password: ROOT_PASSWORD.to_string(),
        })
        .send()
        .await
        .unwrap()
        .into_inner();
    let session = test.bearer_client(&token.access_token);
    session
        .delete_project_instance()
        .tenant_id(silo.default_tenant_id)
        .project_id(project.id)
        .instance_id(instance_id)
        .send()
        .await
        .expect("delete should succeed");

    // tritond's instance record is gone …
    assert!(matches!(
        test.store.get_instance(instance_id).await,
        Err(tritond_store::StoreError::NotFound),
    ));

    // … and a Delete job is now waiting for an agent to claim.
    let secret = mint_key(&test, ApiKeyScope::Agent).await;
    let agent = test.bearer_client(&secret);
    let claimed = agent
        .agent_claim_job()
        .body(ClaimJobRequest {
            claimed_by: "delete-agent".to_string(),
        })
        .send()
        .await
        .unwrap()
        .into_inner()
        .job
        .expect("queue should hold the Delete job we just enqueued");
    match claimed.kind {
        tritond_client::types::JobKind::Delete {
            instance_id: target,
        } => assert_eq!(target, instance_id),
        other => panic!("expected Delete, got {other:?}"),
    }

    // The blueprint for a Delete job carries kind + None for
    // everything else (the tritond record is gone).
    let bp = agent
        .agent_job_blueprint()
        .job_id(claimed.id)
        .send()
        .await
        .unwrap()
        .into_inner();
    assert!(bp.instance.is_none());
    assert!(bp.image.is_none());
    assert!(bp.nics.is_empty());

    // Reporting Completed for a Delete is a clean exit (no
    // lifecycle to advance — the instance is gone).
    let _ = agent
        .agent_complete_job()
        .job_id(claimed.id)
        .body(CompleteJobRequest {
            outcome: JobOutcome::Completed,
        })
        .send()
        .await
        .unwrap();

    test.close().await;
}

/// On `JobOutcome::Failed`, the lifecycle must land in
/// `Failed { reason }` carrying the agent's reason verbatim.
/// Tests the wildcard `expected_from` arm of the complete-side
/// CAS: we're going from `Provisioning` (claim-time advance)
/// straight to Failed.
#[tokio::test]
async fn provision_job_failed_outcome_lands_in_failed_state() {
    let test = TestServer::start().await;
    let silo = test
        .store
        .create_silo(tritond_store::NewSilo {
            name: "agent-failed".to_string(),
            description: None,
        })
        .await
        .unwrap();
    let project = test
        .store
        .create_project(
            silo.default_tenant_id,
            tritond_store::NewProject {
                name: "p1".to_string(),
                description: None,
            },
        )
        .await
        .unwrap();
    let image = test
        .store
        .create_image_silo(
            silo.id,
            tritond_store::NewImage {
                name: "test-image".to_string(),
                description: None,
                os: "smartos".to_string(),
                version: "test".to_string(),
                size_bytes: 1_000_000,
                sha256: "0".repeat(64),
                source_url: None,
                id: None,
                compatibility: None,
            },
        )
        .await
        .unwrap();
    let vpc = test
        .store
        .create_vpc(
            silo.default_tenant_id,
            project.id,
            tritond_store::NewVpc {
                name: "v1".to_string(),
                description: None,
                ipv4_block: Some("10.0.0.0/24".parse().unwrap()),
                ipv6_block: None,
            },
        )
        .await
        .unwrap();
    let subnet = test
        .store
        .create_subnet(
            silo.default_tenant_id,
            project.id,
            vpc.id,
            tritond_store::NewSubnet {
                name: "s1".to_string(),
                description: None,
                ipv4_block: Some("10.0.0.0/29".parse().unwrap()),
                ipv6_block: None,
            },
        )
        .await
        .unwrap();
    let created = test
        .store
        .create_instance(
            silo.default_tenant_id,
            project.id,
            tritond_store::NewInstance {
                name: "fails".to_string(),
                description: None,
                image_id: image.id,
                primary_subnet_id: subnet.id,
                ssh_key_ids: Vec::new(),
                cpu: 1,
                memory_bytes: 256 * 1024 * 1024,
                extra_nics: Vec::new(),
            },
        )
        .await
        .unwrap();
    test.store
        .enqueue_job(NewJob {
            kind: JobKind::Provision {
                instance_id: created.instance.id,
            },
            target_cn_uuid: None,
        })
        .await
        .unwrap();

    let secret = mint_key(&test, ApiKeyScope::Agent).await;
    let client = test.bearer_client(&secret);
    let claimed = client
        .agent_claim_job()
        .body(ClaimJobRequest {
            claimed_by: "fail-agent".to_string(),
        })
        .send()
        .await
        .unwrap()
        .into_inner()
        .job
        .unwrap();
    let _ = client
        .agent_complete_job()
        .job_id(claimed.id)
        .body(CompleteJobRequest {
            outcome: JobOutcome::Failed("image not on host".to_string()),
        })
        .send()
        .await
        .unwrap();
    let after = test.store.get_instance(created.instance.id).await.unwrap();
    match after.lifecycle {
        LifecycleState::Failed { reason } => {
            assert_eq!(reason, "image not on host");
        }
        other => panic!("expected Failed, got {other:?}"),
    }
    // Sanity: LifecycleStateKind machinery still includes Failed
    // among its discriminants (compile-time guard against
    // someone removing it).
    let _: LifecycleStateKind = LifecycleStateKind::Failed;

    test.close().await;
}

#[tokio::test]
async fn bound_agent_can_report_nat_gateway_realization() {
    let test = TestServer::start().await;
    let nat = create_nat_gateway(&test, "realized-alpha").await;
    let cn_uuid = Uuid::new_v4();
    let api_key = register_and_approve(&test, cn_uuid, "cn-realized").await;
    let agent = test.bearer_client(&api_key);

    agent
        .agent_report_network_realization()
        .body(realization_report(
            nat.id,
            RealizerId::Cn(cn_uuid),
            1,
            RealizationStatus::Applied,
        ))
        .send()
        .await
        .expect("bound agent can report realization");

    let root = root_client(&test).await;
    let refreshed = root
        .get_vpc_nat_gateway()
        .tenant_id(nat.tenant_id)
        .project_id(nat.project_id)
        .vpc_id(nat.vpc_id)
        .nat_gateway_id(nat.id)
        .send()
        .await
        .unwrap()
        .into_inner();
    assert_eq!(refreshed.realized.desired_generation, 1);
    assert_eq!(refreshed.realized.applied_generation, Some(1));
    assert_eq!(refreshed.realized.realizations.len(), 1);
    let row = &refreshed.realized.realizations[0];
    assert_eq!(realizer_cn_id(&row.realizer), Some(cn_uuid));
    assert_eq!(row.generation, 1);
    assert!(matches!(row.status, RealizationStatus::Applied));

    test.close().await;
}

#[tokio::test]
async fn network_realization_rejects_backward_generation() {
    let test = TestServer::start().await;
    let nat = create_nat_gateway(&test, "realized-beta").await;
    let cn_uuid = Uuid::new_v4();
    let api_key = register_and_approve(&test, cn_uuid, "cn-stale").await;
    let agent = test.bearer_client(&api_key);

    agent
        .agent_report_network_realization()
        .body(realization_report(
            nat.id,
            RealizerId::Cn(cn_uuid),
            2,
            RealizationStatus::Applied,
        ))
        .send()
        .await
        .unwrap();
    let err = agent
        .agent_report_network_realization()
        .body(realization_report(
            nat.id,
            RealizerId::Cn(cn_uuid),
            1,
            RealizationStatus::Accepted,
        ))
        .send()
        .await
        .expect_err("backward generation report must conflict");
    assert_status(err, 409);

    test.close().await;
}

#[tokio::test]
async fn network_realization_requires_bound_agent_and_matching_cn_realizer() {
    let test = TestServer::start().await;
    let unbound_secret = mint_key(&test, ApiKeyScope::Agent).await;
    let unbound = test.bearer_client(&unbound_secret);
    let err = unbound
        .agent_report_network_realization()
        .body(realization_report(
            Uuid::new_v4(),
            RealizerId::Cn(Uuid::new_v4()),
            1,
            RealizationStatus::Accepted,
        ))
        .send()
        .await
        .expect_err("unbound Agent key must not report realization");
    assert_status(err, 403);

    let bound_cn = Uuid::new_v4();
    let api_key = register_and_approve(&test, bound_cn, "cn-bound-only").await;
    let bound = test.bearer_client(&api_key);
    let err = bound
        .agent_report_network_realization()
        .body(realization_report(
            Uuid::new_v4(),
            RealizerId::Cn(Uuid::new_v4()),
            1,
            RealizationStatus::Accepted,
        ))
        .send()
        .await
        .expect_err("bound Agent key must match Cn realizer id");
    assert_status(err, 403);

    test.close().await;
}

#[tokio::test]
async fn network_realization_tracks_multiple_cn_realizers() {
    let test = TestServer::start().await;
    let nat = create_nat_gateway(&test, "realized-gamma").await;
    let cn_a = Uuid::new_v4();
    let cn_b = Uuid::new_v4();
    let key_a = register_and_approve(&test, cn_a, "cn-a").await;
    let key_b = register_and_approve(&test, cn_b, "cn-b").await;

    test.bearer_client(&key_a)
        .agent_report_network_realization()
        .body(realization_report(
            nat.id,
            RealizerId::Cn(cn_a),
            1,
            RealizationStatus::Applied,
        ))
        .send()
        .await
        .unwrap();
    test.bearer_client(&key_b)
        .agent_report_network_realization()
        .body(realization_report(
            nat.id,
            RealizerId::Cn(cn_b),
            1,
            RealizationStatus::Accepted,
        ))
        .send()
        .await
        .unwrap();

    let root = root_client(&test).await;
    let refreshed = root
        .get_vpc_nat_gateway()
        .tenant_id(nat.tenant_id)
        .project_id(nat.project_id)
        .vpc_id(nat.vpc_id)
        .nat_gateway_id(nat.id)
        .send()
        .await
        .unwrap()
        .into_inner();
    let realizers: Vec<Uuid> = refreshed
        .realized
        .realizations
        .iter()
        .filter_map(|row| realizer_cn_id(&row.realizer))
        .collect();
    assert_eq!(refreshed.realized.realizations.len(), 2);
    assert!(realizers.contains(&cn_a));
    assert!(realizers.contains(&cn_b));
    assert_eq!(refreshed.realized.applied_generation, Some(1));

    test.close().await;
}

#[tokio::test]
async fn anonymous_cannot_reach_agent_surface() {
    let test = TestServer::start().await;
    let anon = test.anonymous_client();
    let err = anon
        .agent_claim_job()
        .body(ClaimJobRequest {
            claimed_by: "anon".to_string(),
        })
        .send()
        .await
        .expect_err("anonymous principal must not authorise agent_claim");
    let progenitor_client::Error::ErrorResponse(resp) = err else {
        panic!("expected ErrorResponse, got {err:?}");
    };
    // Cedar denies anonymous on agent_* via default-deny — fleet
    // scope returns 403 (matching `forbidden_for`).
    assert_eq!(resp.status().as_u16(), 403);

    let err = anon
        .agent_report_network_realization()
        .body(realization_report(
            Uuid::new_v4(),
            RealizerId::Cn(Uuid::new_v4()),
            1,
            RealizationStatus::Accepted,
        ))
        .send()
        .await
        .expect_err("anonymous principal must not authorise realization report");
    assert_status(err, 403);

    test.close().await;
}
