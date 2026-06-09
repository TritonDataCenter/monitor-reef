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

/// Emits saga lifecycle events into the main audit chain as
/// `actor=system` events (the triggering operator is on the per-silo
/// side-effect events the catalog actions write). Operators correlate
/// via the shared `saga_id` payload field.
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
        // Fail-open: a saga must not block on an audit-chain hiccup.
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
    pub login_rate_limiter: Arc<LoginRateLimiter>,
    /// Independent bucket-set from `login_rate_limiter` so a
    /// brute-force on one surface can't drain the other.
    pub cn_approve_rate_limiter: Arc<IpRateLimiter>,
    /// Agent integration tests set `false` so a real `tritonagent`
    /// can claim jobs without racing the in-process stub.
    pub spawn_in_process_provisioner: bool,
    /// `None` by default so tests don't get unexpected background
    /// sweeps interfering with their job-state assertions.
    pub sweeper: Option<SweeperConfig>,
    /// `None` by default so tests don't get unexpected lease deletes
    /// interleaved with their IPAM assertions.
    pub dhcp_reconciler: Option<crate::dhcp_reconciler::ReconcilerConfig>,
    /// `None` by default; `main` enables it (RFD 00005 PL-6) only when
    /// a ClickHouse URL resolves. Tests leave it off so background
    /// roll-up writes don't race their placement assertions.
    pub load_materializer: Option<crate::load_materializer::LoadMaterializerConfig>,
    /// HMAC-SHA256 key for managed-zone identity (provision-time
    /// stamping into SmartOS `internal_metadata` + verification in
    /// CN status reports). `ApiContext::new` generates fresh per
    /// context so tests get isolated signatures; `main` installs
    /// the persisted bootstrap key via `with_identity_hmac_key`.
    pub identity_hmac_key: Arc<tritond_auth::IdentityHmacKey>,
    /// Separate from `store` so a metrics-tier hiccup never 5xx's
    /// the API surface.
    pub metrics: Arc<dyn tritond_metrics::MetricsStore>,
    pub logs: Arc<dyn tritond_logs::LogStore>,
    /// Tests that drive the agent protocol manually (e.g. `agent.rs`)
    /// set this `false`; otherwise `POST .../instances` blocks forever
    /// waiting for an agent the test hasn't started driving.
    pub saga_wait_for_agent: bool,
    /// Durable workflow executor for multi-resource ops and any
    /// operation that enqueues work for a `tritonagent`. Defaults to
    /// `MemSecStore`; production swaps to FDB via
    /// `with_fdb_saga_executor`.
    pub saga: Arc<SagaExecutor>,
    /// v2p invalidations: single global ring drained by every CN's
    /// `GET /v1/agent/peer-invalidations` long-poll. Per-CN filtering
    /// lands when the resolver-served-log does.
    pub peer_invalidations: Arc<crate::peer_invalidations::Ring>,
}

#[derive(Debug, Clone, Copy)]
pub struct SweeperConfig {
    pub interval: std::time::Duration,
    pub stale_after: std::time::Duration,
    /// Retention for terminal sagas. Stuck sagas are exempt.
    pub saga_retention: std::time::Duration,
}

/// Per-context executor with an isolated SEC id so tests don't
/// share saga state.
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
            load_materializer: None,
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

    /// Enable the placement load materializer (RFD 00005 PL-6). `main`
    /// calls this only when a ClickHouse URL resolves; tests leave it
    /// off. Defaults to `None`.
    #[must_use]
    pub fn with_load_materializer(
        mut self,
        cfg: crate::load_materializer::LoadMaterializerConfig,
    ) -> Self {
        self.load_materializer = Some(cfg);
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
