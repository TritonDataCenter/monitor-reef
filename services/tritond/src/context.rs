// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Shared API handler state ([`ApiContext`]) and its builders.

use std::sync::Arc;

use anyhow::Result;
use slog::Drain;
use tritond_audit::MemChain;
use tritond_auth::JwtKey;
use tritond_saga::{ActionRegistry, MemSecStore, SagaExecutor, SecEpoch, SecId};
use tritond_store::{MemStore, Store};

use crate::audit::AuditService;
use crate::auth::AuthService;
use crate::rate_limit::{IpRateLimiter, LoginRateLimiter};

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
}

/// Cadence and staleness threshold for the
/// [`crate::sweeper`] background task. See module docs.
#[derive(Debug, Clone, Copy)]
pub struct SweeperConfig {
    pub interval: std::time::Duration,
    pub stale_after: std::time::Duration,
}

/// Build a default `SagaExecutor` over a fresh in-memory SecStore.
/// Used by both [`ApiContext::new`] and [`ApiContext::in_memory`]
/// so every test fixture gets an isolated SEC id. SG-1b will add a
/// `with_saga_executor` builder that lets production override with
/// an FDB-backed SecStore once `FdbSecStore` is implemented.
fn default_saga_executor() -> Arc<SagaExecutor> {
    let drain = slog::Discard;
    let log = slog::Logger::root(drain.fuse(), slog::o!("component" => "tritond-saga"));
    let store = MemSecStore::new();
    // Catalog is empty at SG-1; SG-2 onwards register actions
    // (and saga versions) via dedicated builder calls before the
    // server starts.
    let exec = SagaExecutor::new_with_mem_store(
        SecId::random(),
        SecEpoch::new(1),
        store,
        ActionRegistry::new(),
        log,
    );
    Arc::new(exec)
}

impl ApiContext {
    pub fn new(store: Arc<dyn Store>, auth: Arc<AuthService>, audit: Arc<AuditService>) -> Self {
        Self {
            store,
            auth,
            audit,
            login_rate_limiter: Arc::new(LoginRateLimiter::new()),
            cn_approve_rate_limiter: Arc::new(IpRateLimiter::for_cn_approve()),
            spawn_in_process_provisioner: true,
            sweeper: None,
            dhcp_reconciler: None,
            identity_hmac_key: Arc::new(tritond_auth::IdentityHmacKey::generate()),
            metrics: Arc::new(tritond_metrics::store::RingBufferStore::new()),
            logs: Arc::new(tritond_logs::RingBufferLogStore::new()),
            saga: default_saga_executor(),
        }
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
