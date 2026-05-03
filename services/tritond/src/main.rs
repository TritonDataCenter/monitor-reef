// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Triton Cloud control plane daemon (binary entry point).
//!
//! Configuration:
//!
//! * `TRITOND_BIND_ADDRESS` — listen address. Defaults to
//!   `127.0.0.1:8080`.
//! * `TRITOND_FDB_CLUSTER_FILE` — path to a FoundationDB cluster file.
//!   Triggers the FDB-backed [`Store`] and audit chain when the
//!   binary is built with the `foundationdb` feature; an error if
//!   set with the feature disabled.
//! * `TRITOND_DISABLE_INPROCESS_PROVISIONER` — when set to `1` /
//!   `true`, skip spawning the in-process stub provisioner. Use
//!   this when a real `tritonagent` is running against this
//!   tritond, so the queue is drained by the agent and not by
//!   the stub.
//! * `TRITOND_SWEEPER_INTERVAL_SECS` — cadence for the
//!   stale-claim sweeper. Default 60.
//! * `TRITOND_STALE_CLAIM_THRESHOLD_SECS` — how old a claim
//!   must be before the sweeper reaps it. Default 600 (10 min).
//!
//! Startup runs [`tritond::bootstrap::ensure`] which mints the JWT
//! signing key and the root operator on first run, then loads them on
//! every subsequent run. The audit chain ships with the same backend
//! choice as the store: in-memory by default, FDB when configured.

use std::sync::Arc;

use anyhow::{Context, Result};
use tracing::info;
use std::time::Duration;

use tritond::audit::AuditService;
use tritond::auth::AuthService;
use tritond::{
    ApiContext, DEFAULT_BIND_ADDRESS, SweeperConfig, VERSION, bootstrap, start_server_with_context,
};
use tritond_audit::{Chain, MemChain};
use tritond_store::{MemStore, Store};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let bind_address =
        std::env::var("TRITOND_BIND_ADDRESS").unwrap_or_else(|_| DEFAULT_BIND_ADDRESS.to_string());

    let (store, audit_chain) = build_store_and_audit()?;
    let jwt_key = bootstrap::ensure(store.as_ref())
        .await
        .context("first-run bootstrap")?;
    let auth = Arc::new(AuthService::new(jwt_key).context("build auth service")?);
    let audit = Arc::new(AuditService::new(audit_chain));
    let mut context = ApiContext::new(store, auth, audit);
    if env_flag("TRITOND_DISABLE_INPROCESS_PROVISIONER") {
        info!("disabling in-process stub provisioner; expecting external tritonagent");
        context = context.without_in_process_provisioner();
    }
    let sweeper_interval =
        env_secs("TRITOND_SWEEPER_INTERVAL_SECS").unwrap_or(Duration::from_secs(60));
    let stale_after =
        env_secs("TRITOND_STALE_CLAIM_THRESHOLD_SECS").unwrap_or(Duration::from_secs(600));
    info!(
        sweeper_interval_secs = sweeper_interval.as_secs(),
        stale_after_secs = stale_after.as_secs(),
        "enabling stale-claim sweeper",
    );
    context = context.with_sweeper(SweeperConfig {
        interval: sweeper_interval,
        stale_after,
    });

    info!(version = VERSION, %bind_address, "tritond starting");

    let server = start_server_with_context(&bind_address, context).await?;
    server
        .await
        .map_err(|e| anyhow::anyhow!("HTTP server error: {e}"))?;

    Ok(())
}

/// True when `name` is set and equals `1` / `true` (case-insensitive).
fn env_flag(name: &str) -> bool {
    matches!(
        std::env::var(name).ok().as_deref(),
        Some("1") | Some("true") | Some("True") | Some("TRUE")
    )
}

/// Parse `name` as `Duration::from_secs`. Returns `None` when
/// the var is unset or unparseable; lets callers fall back to a
/// hardcoded default.
fn env_secs(name: &str) -> Option<Duration> {
    let raw = std::env::var(name).ok()?;
    raw.parse::<u64>().ok().map(Duration::from_secs)
}

#[cfg(feature = "foundationdb")]
fn build_store_and_audit() -> Result<(Arc<dyn Store>, Arc<dyn Chain>)> {
    if let Ok(cluster_file) = std::env::var("TRITOND_FDB_CLUSTER_FILE") {
        info!(%cluster_file, "using FoundationDB backend (store + audit)");
        let store = tritond_store::FdbStore::open(Some(&cluster_file))
            .map_err(|e| anyhow::anyhow!("open FDB store: {e}"))?;
        // Share the FDB Database handle with the audit chain so we
        // don't have two `boot()` callers. FdbStore holds it as
        // Arc<Database>; FdbChain takes its own Arc reference.
        let audit_chain: Arc<dyn Chain> = Arc::new(tritond_audit::FdbChain::new(store.database()));
        Ok((Arc::new(store), audit_chain))
    } else {
        info!("TRITOND_FDB_CLUSTER_FILE not set; using in-memory store + audit");
        let store: Arc<dyn Store> = Arc::new(MemStore::new());
        let audit: Arc<dyn Chain> = Arc::new(MemChain::new());
        Ok((store, audit))
    }
}

#[cfg(not(feature = "foundationdb"))]
fn build_store_and_audit() -> Result<(Arc<dyn Store>, Arc<dyn Chain>)> {
    if std::env::var("TRITOND_FDB_CLUSTER_FILE").is_ok() {
        anyhow::bail!(
            "TRITOND_FDB_CLUSTER_FILE is set but tritond was built without the `foundationdb` feature"
        );
    }
    info!("using in-memory store + audit (binary not built with `foundationdb` feature)");
    let store: Arc<dyn Store> = Arc::new(MemStore::new());
    let audit: Arc<dyn Chain> = Arc::new(MemChain::new());
    Ok((store, audit))
}
