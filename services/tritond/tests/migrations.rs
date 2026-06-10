// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

#![allow(clippy::unwrap_used, clippy::expect_used)]

//! End-to-end tests for the `migrate-instance` saga v2 (LM-6d).
//!
//! Strategy: stand up a tritond with the in-process stub
//! provisioner disabled, register + approve a source and a target
//! CN over HTTP (so they get real migrate-ticket keys + SPKI
//! pins), pin a store-created instance to the source CN, and run
//! a scripted fake agent loop directly against the store's job
//! queue while the migrate POST drives the saga. The fake agent
//! completes every CN-routed job and attaches the result payloads
//! the saga reads (saved quota values, `bytes_streamed`).
//!
//! Covered here:
//! * cold migration of a RUNNING instance quiesces in order — the
//!   `Stop` job reaches terminal BEFORE the `@migration-final`
//!   send is enqueued (the v1 data-loss fix);
//! * an operator abort mid-sync unwinds: target cleanup + source
//!   quota restore jobs are enqueued, the record lands Aborted,
//!   and the active guard releases;
//! * the lifecycle guard: stop/delete return 409 while a
//!   migration is active and work again once it is terminal;
//! * the live lane (LM-7): pause before the final send, target
//!   listen strictly before the source vmm stream, and the stream
//!   failure policy: pre-Finish failures auto-resume the source
//!   and tear down the target, Finish-window failures leave both
//!   sides alone with the ambiguous marker on the record.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use tritond::audit::AuditService;
use tritond::auth::AuthService;
use tritond::{ApiContext, start_server_with_context};
use tritond_audit::MemChain;
use tritond_auth::{JwtKey, RedactedString, hash_password};
use tritond_client::types::{
    ApproveCnRequest, CnState, LoginRequest, MigrateInstanceBody,
    MigrationAction as ClientMigrationAction, RegisterCnRequest,
};
use tritond_store::{
    Instance, JobKind, JobOutcome, LifecycleState, LifecycleStateKind, MemStore, MigrationAction,
    MigrationJobRole, MigrationState, ProvisioningJob, QuotaDanceOp, Store, User,
};
use uuid::Uuid;

const ROOT_PASSWORD: &str = "correct horse battery staple";

/// Values the fake agent "saves" from the source dataset; the saga
/// must round-trip them into the target-side Restore job.
const SAVED_QUOTA_BYTES: u64 = 10 * 1024 * 1024 * 1024;
const SAVED_REFRES_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Ceiling for any await in these tests so a saga bug hangs the
/// test with a clear panic instead of wedging the suite.
const TEST_DEADLINE: Duration = Duration::from_secs(120);

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
            fleet_admin: false,
            capabilities: Default::default(),
            created_at: Utc::now(),
            tenant_id: None,
            federation: None,
        };
        store.create_user(user).await.unwrap();
        let auth = Arc::new(AuthService::new(JwtKey::generate()).unwrap());
        let audit = Arc::new(AuditService::new(Arc::new(MemChain::new())));
        let context = ApiContext::new(Arc::clone(&store), auth, audit)
            // The fake agent owns the queue — the unbound stub
            // must not race it for jobs.
            .without_in_process_provisioner()
            // The plain lifecycle sagas (used by the guard test's
            // post-release stop) must not block on an agent ack;
            // the migration saga always awaits its own jobs.
            .without_saga_wait_for_agent();
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

/// Register + approve a CN over HTTP so it carries everything the
/// migration saga's `resolve_migrate_peer` needs: an admin IP, a
/// console-listener SPKI pin, and (minted at approval) a
/// migrate-ticket key.
async fn register_and_approve_migration_cn(test: &TestServer, cn_uuid: Uuid, admin_ip: &str) {
    let anon = test.anonymous_client();
    let registered = anon
        .agent_register()
        .body(RegisterCnRequest {
            server_uuid: cn_uuid,
            hostname: format!("cn-{cn_uuid}"),
            admin_ip: Some(admin_ip.parse().unwrap()),
            sysinfo: serde_json::json!({ "hostname": format!("cn-{cn_uuid}") }),
            console_listen_port: Some(9101),
            console_tls_spki_sha256_hex: Some("ab".repeat(32)),
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
}

/// Publish a generous `cn-capacity` row so placement treats the CN
/// as eligible. Real agents post this via `/v1/agent/capacity`;
/// tests write it straight to the store.
async fn publish_capacity(test: &TestServer, cn_uuid: Uuid) {
    publish_capacity_with_probe(test, cn_uuid, false).await;
}

/// `probe = true` fills the LM-0b capability-probe fields the live
/// lane's designate gate + compat filters require (matching values
/// on both CNs so the filters accept).
async fn publish_capacity_with_probe(test: &TestServer, cn_uuid: Uuid, probe: bool) {
    use tritond_store::{CnCapacity, StorageTier, UnderlayCapability, ZpoolCapacity};
    test.store
        .put_cn_capacity(CnCapacity {
            server_uuid: cn_uuid,
            cpu_cores_physical: 16,
            cpu_threads_logical: 32,
            numa_nodes: Vec::new(),
            ram_total_mb: 65_536,
            ram_available_mb: 60_000,
            cpu_utilization_pct: 0.1,
            zpools: vec![ZpoolCapacity {
                name: "zones".to_string(),
                total_bytes: 1_000_000_000_000,
                free_bytes: 800_000_000_000,
                tier: StorageTier::Ssd,
            }],
            nic_tags: Vec::new(),
            underlay: UnderlayCapability {
                ipv4: true,
                ipv6: false,
            },
            devices: Vec::new(),
            platform_version: "20260101T000000Z".to_string(),
            hvm_supported: true,
            reported_at: chrono::Utc::now(),
            vmm_protocol_version: probe.then(|| "vmm-migrate-ron/0".to_string()),
            cpu_features: Vec::new(),
            tsc_offset_ns: probe.then_some(0),
            zpool_props: Default::default(),
        })
        .await
        .expect("put_cn_capacity");
}

/// Store-created instance pinned to `source_cn` in lifecycle
/// Running. Returns `(tenant_id, project_id, instance)`.
async fn running_instance_on(
    test: &TestServer,
    silo_name: &str,
    source_cn: Uuid,
) -> (Uuid, Uuid, Instance) {
    running_instance_on_with_brand(test, silo_name, source_cn, false).await
}

/// `bhyve = true` gives the image a bhyve compatibility block so
/// the instance's brand is Bhyve; the migrate handler forces
/// `cold` for every other brand, and the live tests need the live
/// lane to actually run.
async fn running_instance_on_with_brand(
    test: &TestServer,
    silo_name: &str,
    source_cn: Uuid,
    bhyve: bool,
) -> (Uuid, Uuid, Instance) {
    let compatibility = bhyve.then(|| tritond_store::ImageCompatibility {
        brand: "bhyve".to_string(),
        arch: "x86_64".to_string(),
        min_smartos_platform: None,
    });
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
                compatibility,
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
                name: "migratee".to_string(),
                description: None,
                image_id: image.id,
                primary_subnet_id: subnet.id,
                ssh_key_ids: Vec::new(),
                cpu: 1,
                memory_bytes: 256 * 1024 * 1024,
                disk_bytes: None,
                extra_nics: Vec::new(),
                mac: None,
            },
        )
        .await
        .unwrap();
    let instance = test
        .store
        .set_instance_host_cn(created.instance.id, Some(source_cn))
        .await
        .unwrap();
    let instance = test
        .store
        .transition_instance_lifecycle(
            instance.id,
            &[LifecycleStateKind::Pending],
            LifecycleState::Running,
        )
        .await
        .unwrap();
    (silo.default_tenant_id, project.id, instance)
}

/// How the fake agent plays the migration jobs.
#[derive(Clone, Copy, PartialEq, Eq)]
enum FakeAgentScript {
    /// Complete everything with the canned results.
    Normal,
    /// Flip `action_requested` to Abort before completing the first
    /// sync round's source job, so the saga's abort poll unwinds.
    AbortOnSync1,
    /// Fail the source-role `MigrateVmmStream`, reporting the given
    /// `last_phase` in the job result; drives the live failure
    /// policy.
    FailVmmStream { last_phase: &'static str },
}

/// Scripted fake agent: claims every CN-routed job for `cns` and
/// completes it, attaching the result payloads the saga reads.
/// Sync rounds converge on round 2 (round 1 streams 100 MiB, above
/// the 50 MiB default threshold; round 2 streams 1 MiB).
fn spawn_fake_agent(
    store: Arc<dyn Store>,
    cns: Vec<Uuid>,
    script: FakeAgentScript,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let mut idle = true;
            for cn in &cns {
                while let Ok(job) = store.claim_next_job("fake-agent", Some(*cn)).await {
                    idle = false;
                    complete_scripted(&store, &job, script).await;
                }
            }
            if idle {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
    })
}

async fn complete_scripted(store: &Arc<dyn Store>, job: &ProvisioningJob, script: FakeAgentScript) {
    let result = match &job.kind {
        JobKind::MigrateQuotaDance {
            op: QuotaDanceOp::SaveAndClear,
            ..
        } => Some(serde_json::json!({
            "quota_bytes": SAVED_QUOTA_BYTES,
            "refreservation_bytes": SAVED_REFRES_BYTES,
        })),
        JobKind::MigrateZfsSend {
            role: MigrationJobRole::Source,
            to_snap,
            migration_id,
            ..
        } => {
            if script == FakeAgentScript::AbortOnSync1 && to_snap.ends_with("@migration-sync-1") {
                // Operator abort lands while the round is still
                // in flight; the saga's abort poll must convert
                // it into an unwind.
                if let Ok(mut record) = store.get_migration(*migration_id).await {
                    record.action_requested = MigrationAction::Abort;
                    let _ = store.put_migration(record).await;
                }
            }
            let bytes_streamed: u64 = if to_snap.ends_with("@migration-sync-1") {
                100 * 1024 * 1024
            } else if to_snap.contains("@migration-sync-") {
                1024 * 1024
            } else {
                512 * 1024 * 1024
            };
            Some(serde_json::json!({ "bytes_streamed": bytes_streamed }))
        }
        JobKind::MigratePauseSource { .. } => {
            Some(serde_json::json!({ "pause_complete_ts": 1_234_u64 }))
        }
        JobKind::MigrateTargetListen { .. } => Some(serde_json::json!({ "listen_ready": true })),
        JobKind::MigrateVmmStream {
            role: MigrationJobRole::Source,
            ..
        } => {
            if let FakeAgentScript::FailVmmStream { last_phase } = script {
                // The real agent ships the phase report alongside
                // the Failed outcome (StreamFailed carrier).
                let _ = store
                    .complete_job(
                        job.id,
                        JobOutcome::Failed {
                            reason: "scripted vmm stream failure".to_string(),
                        },
                        Some(serde_json::json!({ "last_phase": last_phase })),
                    )
                    .await;
                return;
            }
            Some(serde_json::json!({ "last_phase": "complete" }))
        }
        _ => None,
    };
    let _ = store
        .complete_job(job.id, JobOutcome::Completed, result)
        .await;
}

fn find_job<'a>(
    jobs: &'a [ProvisioningJob],
    pred: impl Fn(&ProvisioningJob) -> bool,
    what: &str,
) -> &'a ProvisioningJob {
    jobs.iter()
        .find(|j| pred(j))
        .unwrap_or_else(|| panic!("expected job not found: {what}"))
}

fn is_restore_on(job: &ProvisioningJob, cn: Uuid) -> bool {
    matches!(
        job.kind,
        JobKind::MigrateQuotaDance {
            op: QuotaDanceOp::Restore { .. },
            ..
        }
    ) && job.target_cn_uuid == Some(cn)
}

fn is_source_send_to(job: &ProvisioningJob, snap_suffix: &str) -> bool {
    matches!(
        &job.kind,
        JobKind::MigrateZfsSend {
            role: MigrationJobRole::Source,
            to_snap,
            ..
        } if to_snap.ends_with(snap_suffix)
    )
}

fn assert_status(err: progenitor_client::Error<tritond_client::types::Error>, want: u16) {
    let progenitor_client::Error::ErrorResponse(response) = err else {
        panic!("expected ErrorResponse, got {err:?}");
    };
    assert_eq!(response.status().as_u16(), want);
}

/// Cold migration of a RUNNING instance: the saga must stop the
/// source guest and see the Stop job terminal BEFORE it enqueues
/// the `@migration-final` send (the v1 data-loss fix), converge
/// the sync loop on the scripted round-2 delta, flip ownership,
/// start the guest on the target, and restore the saved quota
/// there.
#[tokio::test]
async fn cold_migration_of_running_instance_stops_source_before_final_send() {
    let test = TestServer::start().await;
    let source_cn = Uuid::new_v4();
    let target_cn = Uuid::new_v4();
    register_and_approve_migration_cn(&test, source_cn, "10.99.99.1").await;
    register_and_approve_migration_cn(&test, target_cn, "10.99.99.2").await;
    publish_capacity(&test, source_cn).await;
    publish_capacity(&test, target_cn).await;
    let (tenant_id, project_id, instance) =
        running_instance_on(&test, "migrate-cold", source_cn).await;

    let agent = spawn_fake_agent(
        Arc::clone(&test.store),
        vec![source_cn, target_cn],
        FakeAgentScript::Normal,
    );

    let root = root_client(&test).await;
    let response = tokio::time::timeout(
        TEST_DEADLINE,
        root.migrate_project_instance()
            .tenant_id(tenant_id)
            .project_id(project_id)
            .instance_id(instance.id)
            .body(MigrateInstanceBody {
                action: ClientMigrationAction::Begin,
                affinity: None,
                cold: true,
                target_server_uuid: Some(target_cn),
            })
            .send(),
    )
    .await
    .expect("migration saga must finish before the deadline")
    .unwrap()
    .into_inner();

    let record = test
        .store
        .get_migration(response.migration_id)
        .await
        .unwrap();
    assert_eq!(
        record.state,
        MigrationState::Successful,
        "{:?}",
        record.error
    );
    assert_eq!(record.target_cn, Some(target_cn));
    let details = record.source_filesystem_details.expect("details recorded");
    assert_eq!(details.original_quota_bytes, Some(SAVED_QUOTA_BYTES));
    assert_eq!(
        details.original_refreservation_bytes,
        Some(SAVED_REFRES_BYTES)
    );

    // Ownership flipped.
    let migrated = test.store.get_instance(instance.id).await.unwrap();
    assert_eq!(migrated.host_cn_uuid, Some(target_cn));

    let jobs = test.store.list_recent_jobs(200).await.unwrap();

    // THE data-loss fix: Stop reached terminal before the final
    // send existed. seq proves enqueue order; the timestamps prove
    // the saga observed the terminal state first.
    let stop = find_job(
        &jobs,
        |j| matches!(j.kind, JobKind::Stop { instance_id } if instance_id == instance.id),
        "Stop on source",
    );
    assert_eq!(stop.target_cn_uuid, Some(source_cn));
    let final_send = find_job(
        &jobs,
        |j| is_source_send_to(j, "@migration-final"),
        "final source send",
    );
    assert!(
        stop.seq < final_send.seq,
        "Stop (seq {}) must be enqueued before the final send (seq {})",
        stop.seq,
        final_send.seq,
    );
    assert!(
        stop.completed_at.expect("stop terminal") <= final_send.created_at,
        "Stop must reach terminal before the final send is enqueued",
    );

    // Convergence script: round 1 over threshold, round 2 under,
    // so exactly two sync rounds ran.
    assert!(
        jobs.iter()
            .any(|j| is_source_send_to(j, "@migration-sync-1"))
    );
    assert!(
        jobs.iter()
            .any(|j| is_source_send_to(j, "@migration-sync-2"))
    );
    assert!(
        !jobs
            .iter()
            .any(|j| is_source_send_to(j, "@migration-sync-3"))
    );

    // Target-side provision used the migration-specific kind, not
    // a guest-booting Provision.
    let provision = find_job(
        &jobs,
        |j| matches!(j.kind, JobKind::MigrationProvisionTarget { .. }),
        "MigrationProvisionTarget",
    );
    assert_eq!(provision.target_cn_uuid, Some(target_cn));
    assert!(
        !jobs
            .iter()
            .any(|j| matches!(j.kind, JobKind::Provision { .. }))
    );

    // activate_target: Start on the target (instance was running)
    // + quota Restore with the values the fake agent reported.
    let start = find_job(
        &jobs,
        |j| matches!(j.kind, JobKind::Start { instance_id } if instance_id == instance.id),
        "Start on target",
    );
    assert_eq!(start.target_cn_uuid, Some(target_cn));
    let restore = find_job(
        &jobs,
        |j| is_restore_on(j, target_cn),
        "quota Restore on target",
    );
    if let JobKind::MigrateQuotaDance {
        op:
            QuotaDanceOp::Restore {
                quota_bytes,
                refreservation_bytes,
            },
        ..
    } = &restore.kind
    {
        assert_eq!(*quota_bytes, Some(SAVED_QUOTA_BYTES));
        assert_eq!(*refreservation_bytes, Some(SAVED_REFRES_BYTES));
    }

    // Post-switch source teardown enqueued.
    assert!(
        jobs.iter()
            .any(|j| matches!(j.kind, JobKind::MigrationCleanupSource { .. }))
    );

    // Terminal record released the guard.
    assert!(
        test.store
            .get_active_migration(instance.id)
            .await
            .unwrap()
            .is_none()
    );

    agent.abort();
    test.close().await;
}

/// Operator abort mid-sync: the abort poll unwinds the saga, which
/// must enqueue the target cleanup + the source quota Restore,
/// land the record in Aborted, and release the active guard.
#[tokio::test]
async fn abort_mid_sync_unwinds_cleanup_and_quota_restore() {
    let test = TestServer::start().await;
    let source_cn = Uuid::new_v4();
    let target_cn = Uuid::new_v4();
    register_and_approve_migration_cn(&test, source_cn, "10.99.98.1").await;
    register_and_approve_migration_cn(&test, target_cn, "10.99.98.2").await;
    publish_capacity(&test, source_cn).await;
    publish_capacity(&test, target_cn).await;
    let (tenant_id, project_id, instance) =
        running_instance_on(&test, "migrate-abort", source_cn).await;

    let agent = spawn_fake_agent(
        Arc::clone(&test.store),
        vec![source_cn, target_cn],
        FakeAgentScript::AbortOnSync1,
    );

    let root = root_client(&test).await;
    let response = tokio::time::timeout(
        TEST_DEADLINE,
        root.migrate_project_instance()
            .tenant_id(tenant_id)
            .project_id(project_id)
            .instance_id(instance.id)
            .body(MigrateInstanceBody {
                action: ClientMigrationAction::Begin,
                affinity: None,
                cold: true,
                target_server_uuid: Some(target_cn),
            })
            .send(),
    )
    .await
    .expect("aborted migration saga must finish before the deadline")
    .unwrap()
    .into_inner();

    let record = test
        .store
        .get_migration(response.migration_id)
        .await
        .unwrap();
    assert_eq!(record.state, MigrationState::Aborted);
    assert!(record.finished_at.is_some());

    // The guest never quiesced and ownership never flipped.
    let untouched = test.store.get_instance(instance.id).await.unwrap();
    assert_eq!(untouched.host_cn_uuid, Some(source_cn));
    let jobs = test.store.list_recent_jobs(200).await.unwrap();
    assert!(
        !jobs
            .iter()
            .any(|j| matches!(j.kind, JobKind::Stop { .. } | JobKind::Start { .. })),
        "abort before quiesce must not touch guest power state",
    );
    assert!(
        !jobs
            .iter()
            .any(|j| is_source_send_to(j, "@migration-final"))
    );

    // Unwind artifacts: target teardown + source quota restore.
    let cleanup = find_job(
        &jobs,
        |j| matches!(j.kind, JobKind::MigrationCleanupTarget { .. }),
        "MigrationCleanupTarget",
    );
    assert_eq!(cleanup.target_cn_uuid, Some(target_cn));
    let restore = find_job(
        &jobs,
        |j| is_restore_on(j, source_cn),
        "quota Restore on source",
    );
    if let JobKind::MigrateQuotaDance {
        op:
            QuotaDanceOp::Restore {
                quota_bytes,
                refreservation_bytes,
            },
        ..
    } = &restore.kind
    {
        assert_eq!(*quota_bytes, Some(SAVED_QUOTA_BYTES));
        assert_eq!(*refreservation_bytes, Some(SAVED_REFRES_BYTES));
    }

    // Terminal record released the guard — a fresh migration of
    // the same instance is accepted again.
    assert!(
        test.store
            .get_active_migration(instance.id)
            .await
            .unwrap()
            .is_none()
    );

    agent.abort();
    test.close().await;
}

/// Shared live-lane setup: two probe-publishing CNs, a running
/// bhyve instance on the source, a scripted fake agent, and the
/// live (`cold: false`) migrate POST driven to saga-terminal.
/// Returns everything the assertions need.
struct LiveRun {
    test: TestServer,
    source_cn: Uuid,
    target_cn: Uuid,
    instance: Instance,
    migration_id: Uuid,
    agent: tokio::task::JoinHandle<()>,
}

async fn run_live_migration(silo_name: &str, script: FakeAgentScript) -> LiveRun {
    let test = TestServer::start().await;
    let source_cn = Uuid::new_v4();
    let target_cn = Uuid::new_v4();
    register_and_approve_migration_cn(&test, source_cn, "10.99.97.1").await;
    register_and_approve_migration_cn(&test, target_cn, "10.99.97.2").await;
    publish_capacity_with_probe(&test, source_cn, true).await;
    publish_capacity_with_probe(&test, target_cn, true).await;
    let (tenant_id, project_id, instance) =
        running_instance_on_with_brand(&test, silo_name, source_cn, true).await;

    let agent = spawn_fake_agent(Arc::clone(&test.store), vec![source_cn, target_cn], script);

    let root = root_client(&test).await;
    let response = tokio::time::timeout(
        TEST_DEADLINE,
        root.migrate_project_instance()
            .tenant_id(tenant_id)
            .project_id(project_id)
            .instance_id(instance.id)
            .body(MigrateInstanceBody {
                action: ClientMigrationAction::Begin,
                affinity: None,
                cold: false,
                target_server_uuid: Some(target_cn),
            })
            .send(),
    )
    .await
    .expect("live migration saga must finish before the deadline")
    .unwrap()
    .into_inner();

    LiveRun {
        test,
        source_cn,
        target_cn,
        instance,
        migration_id: response.migration_id,
        agent,
    }
}

/// Happy-path live migration: the guest is paused (not stopped)
/// before the final send, the target listens before the source
/// streams, ownership flips, and no power-cycle jobs ever run.
#[tokio::test]
async fn live_migration_pauses_listens_then_streams() {
    let run = run_live_migration("migrate-live", FakeAgentScript::Normal).await;

    let record = run
        .test
        .store
        .get_migration(run.migration_id)
        .await
        .unwrap();
    assert_eq!(
        record.state,
        MigrationState::Successful,
        "{:?}",
        record.error
    );
    let migrated = run.test.store.get_instance(run.instance.id).await.unwrap();
    assert_eq!(migrated.host_cn_uuid, Some(run.target_cn));

    let jobs = run.test.store.list_recent_jobs(200).await.unwrap();

    // Live quiesce is a pause, never a power cycle.
    assert!(
        !jobs
            .iter()
            .any(|j| matches!(j.kind, JobKind::Stop { .. } | JobKind::Start { .. })),
        "live migration must not stop/start the guest",
    );
    let pause = find_job(
        &jobs,
        |j| matches!(j.kind, JobKind::MigratePauseSource { .. }),
        "MigratePauseSource",
    );
    assert_eq!(pause.target_cn_uuid, Some(run.source_cn));
    let final_send = find_job(
        &jobs,
        |j| is_source_send_to(j, "@migration-final"),
        "final source send",
    );
    assert!(
        pause.completed_at.expect("pause terminal") <= final_send.created_at,
        "pause must reach terminal before the final send is enqueued",
    );

    // Target listens strictly before the source streams.
    let listen = find_job(
        &jobs,
        |j| matches!(j.kind, JobKind::MigrateTargetListen { .. }),
        "MigrateTargetListen",
    );
    assert_eq!(listen.target_cn_uuid, Some(run.target_cn));
    assert!(
        final_send.seq < listen.seq,
        "target listen belongs to stream_vmm, after the final send",
    );
    let stream = find_job(
        &jobs,
        |j| {
            matches!(
                j.kind,
                JobKind::MigrateVmmStream {
                    role: MigrationJobRole::Source,
                    ..
                }
            )
        },
        "source MigrateVmmStream",
    );
    assert_eq!(stream.target_cn_uuid, Some(run.source_cn));
    assert!(
        listen.seq < stream.seq,
        "listen (seq {}) must be enqueued before the stream (seq {})",
        listen.seq,
        stream.seq,
    );
    assert!(
        listen.completed_at.expect("listen terminal") <= stream.created_at,
        "listen must reach terminal before the stream is enqueued",
    );
    // The source job carries the full dial bundle.
    if let JobKind::MigrateVmmStream {
        peer_endpoint,
        peer_spki_sha256_hex,
        ticket,
        ..
    } = &stream.kind
    {
        assert!(
            peer_endpoint
                .as_deref()
                .is_some_and(|e| e.starts_with("wss://"))
        );
        assert!(peer_spki_sha256_hex.is_some());
        assert!(ticket.is_some());
    }

    run.agent.abort();
    run.test.close().await;
}

/// Stream failure BEFORE the wire's Finish phase: the target cannot
/// have imported, so the saga unwinds: resume the paused source,
/// tear down the target zone.
#[tokio::test]
async fn live_stream_pre_finish_failure_resumes_source() {
    let run = run_live_migration(
        "migrate-live-fail",
        FakeAgentScript::FailVmmStream {
            last_phase: "ram_push",
        },
    )
    .await;

    let record = run
        .test
        .store
        .get_migration(run.migration_id)
        .await
        .unwrap();
    assert_eq!(record.state, MigrationState::Failed);

    // Ownership never flipped.
    let untouched = run.test.store.get_instance(run.instance.id).await.unwrap();
    assert_eq!(untouched.host_cn_uuid, Some(run.source_cn));

    let jobs = run.test.store.list_recent_jobs(200).await.unwrap();
    let resume = find_job(
        &jobs,
        |j| matches!(j.kind, JobKind::MigrateResumeSource { .. }),
        "MigrateResumeSource",
    );
    assert_eq!(resume.target_cn_uuid, Some(run.source_cn));
    let cleanup = find_job(
        &jobs,
        |j| matches!(j.kind, JobKind::MigrationCleanupTarget { .. }),
        "MigrationCleanupTarget",
    );
    assert_eq!(cleanup.target_cn_uuid, Some(run.target_cn));

    run.agent.abort();
    run.test.close().await;
}

/// Stream failure inside the Finish window: the target may already
/// be running the guest, so the unwind must NOT resume the source
/// and must NOT tear down the target; the record carries the
/// structured ambiguous-failure marker for the operator instead.
#[tokio::test]
async fn live_stream_finish_window_failure_leaves_both_sides() {
    let run = run_live_migration(
        "migrate-live-ambig",
        FakeAgentScript::FailVmmStream {
            last_phase: "finish",
        },
    )
    .await;

    let record = run
        .test
        .store
        .get_migration(run.migration_id)
        .await
        .unwrap();
    assert_eq!(record.state, MigrationState::Failed);
    let error = record.error.expect("ambiguous marker stamped");
    assert!(
        error.starts_with("live migration ambiguous failure"),
        "unexpected error: {error}",
    );
    assert!(error.contains(&run.source_cn.to_string()));
    assert!(error.contains(&run.target_cn.to_string()));

    // Ownership never flipped; both sides left alone.
    let untouched = run.test.store.get_instance(run.instance.id).await.unwrap();
    assert_eq!(untouched.host_cn_uuid, Some(run.source_cn));
    let jobs = run.test.store.list_recent_jobs(200).await.unwrap();
    assert!(
        !jobs
            .iter()
            .any(|j| matches!(j.kind, JobKind::MigrateResumeSource { .. })),
        "ambiguous failure must not auto-resume the source",
    );
    assert!(
        !jobs
            .iter()
            .any(|j| matches!(j.kind, JobKind::MigrationCleanupTarget { .. })),
        "ambiguous failure must not tear down the target zone",
    );

    run.agent.abort();
    run.test.close().await;
}

/// While the `migration/active/<instance>` guard is held, the
/// stop/delete entry points 409; once the record is terminal they
/// work again.
#[tokio::test]
async fn lifecycle_mutations_conflict_while_migration_active() {
    let test = TestServer::start().await;
    let source_cn = Uuid::new_v4();
    let (tenant_id, project_id, instance) =
        running_instance_on(&test, "migrate-guard", source_cn).await;

    // Take the guard the way the migrate handler does, without
    // running the saga.
    let mut record = test
        .store
        .create_migration(tritond_store::NewMigration {
            instance_id: instance.id,
            tenant_id,
            project_id,
            source_cn,
            action_requested: MigrationAction::Begin,
            automatic: false,
        })
        .await
        .unwrap();

    let root = root_client(&test).await;
    let err = root
        .stop_instance_v1()
        .instance_id(instance.id)
        .send()
        .await
        .expect_err("stop must conflict while migration is active");
    assert_status(err, 409);
    let err = root
        .delete_instance_v1()
        .instance_id(instance.id)
        .send()
        .await
        .expect_err("delete must conflict while migration is active");
    assert_status(err, 409);

    // Terminal record releases the guard; stop works again.
    record.state = MigrationState::Failed;
    test.store.put_migration(record).await.unwrap();
    root.stop_instance_v1()
        .instance_id(instance.id)
        .send()
        .await
        .expect("stop must succeed once the migration is terminal");

    test.close().await;
}
