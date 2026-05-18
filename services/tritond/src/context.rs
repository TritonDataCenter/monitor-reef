// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Shared API handler state ([`ApiContext`]) and its builders.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use slog::Drain;
use tritond_audit::{Actor, Chain, Decision, MemChain, Outcome, PendingEvent};
use tritond_auth::JwtKey;
use tritond_saga::{
    ActionRegistry, MemSecStore, SagaAuditEmitter, SagaExecutor, SagaId, SecEpoch, SecId,
};
use tritond_store::{MemStore, Store};

use crate::audit::AuditService;
use crate::auth::AuthService;
use crate::rate_limit::{IpRateLimiter, LoginRateLimiter};

/// Bridge from `tritond_saga::SagaAuditEmitter` to the existing
/// `tritond_audit::Chain` (RFD 00004 D-Sg-11).
///
/// Saga lifecycle events (`saga.started` / `saga.finished`) land in
/// the same chain as every other audit event, with a `Saga::"<uuid>"`
/// resource and `actor=system` (sagas are control-plane-initiated;
/// the *triggering* operator's identity is on the per-silo
/// side-effect events the catalog actions write through the
/// existing `record_mutation` path). This gives operators
/// breadcrumbs to correlate the saga's lifecycle with the per-silo
/// resource writes via the shared `saga_id` payload field.
///
/// Strict per-silo / fleet chain separation (the RFD's `audit/saga/...`
/// keyspace) is deferred to a follow-up that introduces a second
/// chain instance.
struct ChainAuditEmitter {
    chain: Arc<dyn Chain>,
}

#[async_trait]
impl SagaAuditEmitter for ChainAuditEmitter {
    async fn operation_started(&self, saga_id: SagaId, kind: &str, version: u32) {
        let event = PendingEvent {
            ts: chrono::Utc::now(),
            actor: Actor::System,
            action: "saga.started".to_string(),
            resource: Some(format!("Saga::\"{}\"", saga_id.0)),
            request_id: None,
            decision: Decision::NotEvaluated,
            outcome: Outcome::Success {
                resource: Some(format!("Saga::\"{}\"", saga_id.0)),
            },
            payload: serde_json::json!({
                "saga_id": saga_id.0,
                "kind": kind,
                "version": version,
            }),
        };
        // Fail-open: saga shouldn't block on an audit-chain hiccup.
        // Drop the error and log via the saga executor's slog
        // logger instead.
        let _ = self.chain.append(event).await;
    }

    async fn operation_finished(&self, saga_id: SagaId, state: &str, error: Option<String>) {
        let outcome = if state == "succeeded" {
            Outcome::Success {
                resource: Some(format!("Saga::\"{}\"", saga_id.0)),
            }
        } else {
            Outcome::ServerError {
                message: error.clone().unwrap_or_else(|| state.to_string()),
            }
        };
        let event = PendingEvent {
            ts: chrono::Utc::now(),
            actor: Actor::System,
            action: "saga.finished".to_string(),
            resource: Some(format!("Saga::\"{}\"", saga_id.0)),
            request_id: None,
            decision: Decision::NotEvaluated,
            outcome,
            payload: serde_json::json!({
                "saga_id": saga_id.0,
                "state": state,
                "error": error,
            }),
        };
        let _ = self.chain.append(event).await;
    }
}

/// Shared state for API handlers.
pub struct ApiContext {
    pub store: Arc<dyn Store>,
    pub auth: Arc<AuthService>,
    pub audit: Arc<AuditService>,
    /// Per-source-IP throttle on `POST /v2/auth/login`. See
    /// [`crate::rate_limit`] for the shape of the limiter and why it
    /// only fronts login.
    pub login_rate_limiter: Arc<LoginRateLimiter>,
    /// Per-source-IP throttle on `POST /v2/cns/approve`. Independent
    /// bucket-set from the login limiter so a brute-force on one
    /// surface doesn't drain the other's budget.
    pub cn_approve_rate_limiter: Arc<IpRateLimiter>,
    /// When `false`, [`crate::start_server_with_context`] does *not*
    /// spawn the in-process stub provisioner. The agent integration
    /// test sets this so a real `tritonagent` (or its test stand-in)
    /// can claim jobs without racing the stub. Defaults to `true`.
    pub spawn_in_process_provisioner: bool,
    /// Stale-claim sweeper config. When `Some(...)`,
    /// [`crate::start_server_with_context`] spawns the sweeper task
    /// from [`crate::sweeper::spawn`] with the given interval +
    /// staleness threshold. Defaults to `None` so test contexts
    /// don't get an unexpected background task that would
    /// interfere with explicit job-state assertions.
    pub sweeper: Option<SweeperConfig>,
    /// DHCP-lease reconciler config (γ.3). When `Some(...)`,
    /// [`crate::start_server_with_context`] spawns the reconciler task
    /// from [`crate::dhcp_reconciler::spawn`] with the given
    /// interval + GC threshold. Defaults to `None` so test
    /// contexts don't get unexpected lease deletes interleaved
    /// with explicit IPAM assertions.
    pub dhcp_reconciler: Option<crate::dhcp_reconciler::ReconcilerConfig>,
    /// Per-deployment HMAC-SHA256 key used to stamp managed-zone
    /// identity (`instance_id`/`tenant_id`/`project_id`) into
    /// SmartOS `internal_metadata` at provision time, and to verify
    /// that identity in CN status reports. `ApiContext::new` defaults
    /// to a freshly-generated key so tests get isolated per-context
    /// signatures; `main` overrides via `with_identity_hmac_key` to
    /// install the bootstrap-loaded, persisted key.
    pub identity_hmac_key: Arc<tritond_auth::IdentityHmacKey>,
    /// Timeseries metrics sink. Defaults to an in-memory ring
    /// buffer; production deploys swap in a ClickHouse-backed
    /// implementation via [`ApiContext::with_metrics`]. The store
    /// is consumed by the agent metrics-ingest endpoint and the
    /// per-instance range query, and is intentionally separate
    /// from `store` (control-plane state) so the metrics path
    /// can fail-open without taking the API surface offline.
    pub metrics: Arc<dyn tritond_metrics::MetricsStore>,
    /// Per-VM log line sink. Defaults to an in-memory ring buffer
    /// (last ~10k lines per `(instance, source)`); production deploys
    /// swap in a ClickHouse-backed store via
    /// [`ApiContext::with_logs`]. Same fail-open behaviour as
    /// `metrics` -- a storage hiccup never 5xx's the agent.
    pub logs: Arc<dyn tritond_logs::LogStore>,
    /// When `true` (the default), the `instance-create` saga's
    /// `await_provision_terminal` action waits for the agent to ack
    /// the Provision job before returning. This is what completes
    /// SG-2's unwind story (RFD 00004): a Provision-failed agent
    /// outcome triggers the saga's unwind tail, which enqueues a
    /// Delete job and tears the instance record back down.
    ///
    /// Set to `false` in tests that drive the agent protocol manually
    /// (e.g. `agent.rs` opts out of the in-process provisioner and
    /// then issues `claim_next_job`/`agent_complete_job` after the
    /// `POST .../instances`). Without that opt-out the POST would
    /// block forever waiting for an agent that the test hasn't yet
    /// started driving.
    pub saga_wait_for_agent: bool,
    /// Durable workflow executor (RFD 00004). Every multi-resource
    /// operation that touches more than one FDB resource, or that
    /// enqueues work for any `tritonagent`, runs as a registered
    /// saga with explicit per-action undo. Catalog modules in
    /// `crate::sagas` (SG-2 onwards) register their actions on this
    /// executor.
    ///
    /// SG-1 builds a default executor over [`MemSecStore`] regardless
    /// of the underlying [`Store`] backend; SG-1b will select
    /// `FdbSecStore` when `Store` is `FdbStore` so sagas survive
    /// process restarts. With an empty catalog the executor is a
    /// no-op for everyone except the heartbeat/recovery plumbing it
    /// keeps alive for SG-2.
    pub saga: Arc<SagaExecutor>,
    /// v2p invalidation ring (PROTEUS_PLAN §11.7.1 item 8). Pushed
    /// onto by NIC teardown / migration paths; drained by
    /// `GET /v2/agent/peer-invalidations` per-CN polls. Phase A
    /// shape: a single global ring broadcast to every CN that polls
    /// (acceptable for low-churn cluster sizes). Phase B narrows
    /// to per-CN filtering once the resolver-served-log lands.
    pub peer_invalidations: Arc<crate::peer_invalidations::Ring>,
}

/// Cadence and staleness threshold for the
/// [`crate::sweeper`] background task. See module docs.
#[derive(Debug, Clone, Copy)]
pub struct SweeperConfig {
    pub interval: std::time::Duration,
    pub stale_after: std::time::Duration,
    /// How long terminal sagas are kept before the retention pass
    /// deletes them. Default 30 days (RFD 00004 SG-4). Stuck sagas
    /// are exempt and stay until human cleanup.
    pub saga_retention: std::time::Duration,
}

/// Build a default `SagaExecutor` over an in-memory SecStore, with
/// every catalog action in [`crate::sagas`] pre-registered. Used by
/// both [`ApiContext::new`] and [`ApiContext::in_memory`] so every
/// test fixture gets an isolated SEC id and a fully wired catalog.
///
/// Production deploys with FoundationDB call
/// [`ApiContext::with_fdb_saga_executor`] from `main` to swap this
/// out for an FDB-backed executor.
fn default_saga_executor(
    state_store: &Arc<dyn Store>,
    identity_hmac_key: &Arc<tritond_auth::IdentityHmacKey>,
    audit_chain: &Arc<dyn Chain>,
) -> Arc<SagaExecutor> {
    let drain = slog::Discard;
    let log = slog::Logger::root(drain.fuse(), slog::o!("component" => "tritond-saga"));
    let sec_store = MemSecStore::new();
    let mut registry = ActionRegistry::new();
    crate::sagas::register_all_actions(&mut registry);
    let audit_emitter: Arc<dyn SagaAuditEmitter> = Arc::new(ChainAuditEmitter {
        chain: Arc::clone(audit_chain),
    });
    let mut exec = SagaExecutor::new_with_mem_store(
        SecId::random(),
        SecEpoch::new(1),
        sec_store,
        registry,
        log,
    )
    .with_store(Arc::clone(state_store))
    .with_identity_hmac_key(Arc::clone(identity_hmac_key))
    .with_audit(audit_emitter);
    for (name, version) in crate::sagas::registered_versions() {
        exec.register_saga_version(name, version);
    }
    Arc::new(exec)
}

/// Build a production `SagaExecutor` backed by FoundationDB. The
/// supplied `db` handle is the same one `tritond_store::FdbStore`
/// uses, so the saga state lives in the region's single FDB
/// cluster (Locked Decision #4) under the `saga/...` prefix.
#[cfg(feature = "foundationdb")]
pub fn fdb_saga_executor(
    db: Arc<tritond_saga::FdbDatabase>,
    state_store: &Arc<dyn Store>,
    identity_hmac_key: &Arc<tritond_auth::IdentityHmacKey>,
    audit_chain: &Arc<dyn Chain>,
) -> Arc<SagaExecutor> {
    let drain = slog::Discard;
    let log = slog::Logger::root(
        drain.fuse(),
        slog::o!("component" => "tritond-saga", "backend" => "fdb"),
    );
    let mut registry = ActionRegistry::new();
    crate::sagas::register_all_actions(&mut registry);
    let audit_emitter: Arc<dyn SagaAuditEmitter> = Arc::new(ChainAuditEmitter {
        chain: Arc::clone(audit_chain),
    });
    let mut exec =
        SagaExecutor::new_with_fdb_store(SecId::random(), SecEpoch::new(1), db, registry, log)
            .with_store(Arc::clone(state_store))
            .with_identity_hmac_key(Arc::clone(identity_hmac_key))
            .with_audit(audit_emitter);
    for (name, version) in crate::sagas::registered_versions() {
        exec.register_saga_version(name, version);
    }
    Arc::new(exec)
}

impl ApiContext {
    pub fn new(store: Arc<dyn Store>, auth: Arc<AuthService>, audit: Arc<AuditService>) -> Self {
        let identity_hmac_key = Arc::new(tritond_auth::IdentityHmacKey::generate());
        let saga = default_saga_executor(&store, &identity_hmac_key, audit.chain());
        Self {
            store,
            auth,
            audit,
            login_rate_limiter: Arc::new(LoginRateLimiter::new()),
            cn_approve_rate_limiter: Arc::new(IpRateLimiter::for_cn_approve()),
            spawn_in_process_provisioner: true,
            sweeper: None,
            dhcp_reconciler: None,
            identity_hmac_key,
            metrics: Arc::new(tritond_metrics::store::RingBufferStore::new()),
            logs: Arc::new(tritond_logs::RingBufferLogStore::new()),
            saga,
            saga_wait_for_agent: true,
            peer_invalidations: Arc::new(crate::peer_invalidations::Ring::new()),
        }
    }

    /// Disable the `await_provision_terminal` action on the
    /// instance-create saga. Used by test fixtures that drive the
    /// agent protocol manually after issuing creates (see
    /// [`Self::saga_wait_for_agent`] doc for the rationale).
    #[must_use]
    pub fn without_saga_wait_for_agent(mut self) -> Self {
        self.saga_wait_for_agent = false;
        self
    }

    /// Install a real metrics store (e.g. ClickHouse). Tests and dev
    /// runs can leave the default ring buffer in place; production
    /// startup overrides via this builder once the ClickHouse client
    /// is healthy.
    #[must_use]
    pub fn with_metrics(mut self, metrics: Arc<dyn tritond_metrics::MetricsStore>) -> Self {
        self.metrics = metrics;
        self
    }

    /// Install a real log store. Parallels `with_metrics`.
    #[must_use]
    pub fn with_logs(mut self, logs: Arc<dyn tritond_logs::LogStore>) -> Self {
        self.logs = logs;
        self
    }

    /// Install a specific identity HMAC key (typically the
    /// bootstrap-loaded persisted one). Tests that need to verify
    /// identity tags across a context boundary share a key via
    /// this builder.
    #[must_use]
    pub fn with_identity_hmac_key(mut self, key: Arc<tritond_auth::IdentityHmacKey>) -> Self {
        self.identity_hmac_key = key;
        self
    }

    /// Replace the default CN-approve rate limiter — integration
    /// tests use this to install a tighter quota than production
    /// without slowing the login bucket.
    #[must_use]
    pub fn with_cn_approve_rate_limiter(mut self, limiter: Arc<IpRateLimiter>) -> Self {
        self.cn_approve_rate_limiter = limiter;
        self
    }

    /// Enable the stale-claim sweeper at the given cadence.
    /// Used by `main` (env-driven) and by integration tests
    /// that want to exercise sweeper behavior with tight
    /// thresholds. Defaults to `None`.
    #[must_use]
    pub fn with_sweeper(mut self, cfg: SweeperConfig) -> Self {
        self.sweeper = Some(cfg);
        self
    }

    /// Enable the DHCP-lease reconciler (γ.3) at the given
    /// cadence + GC threshold. Used by `main` (env-driven) and by
    /// integration tests that want to exercise reconciler
    /// behaviour with tight thresholds. Defaults to `None`.
    #[must_use]
    pub fn with_dhcp_reconciler(mut self, cfg: crate::dhcp_reconciler::ReconcilerConfig) -> Self {
        self.dhcp_reconciler = Some(cfg);
        self
    }

    /// Replace the default `SagaExecutor` with a caller-built one.
    /// SG-2 onwards uses this to install an executor whose registry
    /// contains the catalog actions; SG-1b will use it from `main`
    /// to install an FDB-backed executor in production deploys.
    #[must_use]
    pub fn with_saga_executor(mut self, saga: Arc<SagaExecutor>) -> Self {
        self.saga = saga;
        self
    }

    /// Disable the in-process stub provisioner — the agent
    /// integration test uses this so a test-side claim doesn't
    /// race the stub. Production deploys with a real `tritonagent`
    /// will eventually call this too.
    #[must_use]
    pub fn without_in_process_provisioner(mut self) -> Self {
        self.spawn_in_process_provisioner = false;
        self
    }

    /// Build a context backed by a fresh in-memory store, a fresh
    /// random JWT key, and an in-memory audit chain. Convenient for
    /// integration tests.
    pub fn in_memory() -> Result<Self> {
        let store: Arc<dyn Store> = Arc::new(MemStore::new());
        let auth = Arc::new(AuthService::new(JwtKey::generate())?);
        let audit = Arc::new(AuditService::new(Arc::new(MemChain::new())));
        Ok(Self::new(store, auth, audit))
    }

    /// Replace the default login rate limiter — used by integration
    /// tests that need a tighter quota than production. Returns
    /// `self` so test setup can chain off `ApiContext::in_memory()`.
    #[must_use]
    pub fn with_login_rate_limiter(mut self, limiter: Arc<LoginRateLimiter>) -> Self {
        self.login_rate_limiter = limiter;
        self
    }
}
