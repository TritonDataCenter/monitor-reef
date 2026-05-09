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
//! * `TRITOND_DHCP_RECONCILE_INTERVAL_SECS` — cadence for the
//!   γ.3 DHCP lease reconciler. Default 300 (5 min).
//! * `TRITOND_DHCP_LEASE_GC_THRESHOLD_SECS` — minimum
//!   `now - last_activity` before a lease is GC-eligible.
//!   Default 604_800 (7 days).
//!
//! Startup runs [`tritond::bootstrap::ensure`] which mints the JWT
//! signing key and the root operator on first run, then loads them on
//! every subsequent run. The audit chain ships with the same backend
//! choice as the store: in-memory by default, FDB when configured.

use std::sync::Arc;

use anyhow::{Context, Result, bail};
use std::time::Duration;
use tracing::info;

use tritond::audit::AuditService;
use tritond::auth::AuthService;
use tritond::{
    ApiContext, DEFAULT_BIND_ADDRESS, SweeperConfig, VERSION, bootstrap, dhcp_reconciler,
    start_server_with_context,
};
use tritond_audit::{Chain, MemChain};
use tritond_store::{MemStore, Store};

enum Command {
    Serve,
    ResetRootPassword { fdb_cluster_file: Option<String> },
    Help,
}

#[tokio::main]
async fn main() -> Result<()> {
    let command = parse_command(std::env::args().skip(1))?;
    if matches!(command, Command::Help) {
        print_usage();
        return Ok(());
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    // rustls 0.23 requires a process-default `CryptoProvider`
    // before the first `ClientConfig::builder()` call. The
    // bundle ingest path (`POST /v2/silos/.../image-bundles`)
    // uses reqwest which arms TLS even for plaintext URLs;
    // without this line tritond panics on the first ingest on
    // a cold SmartOS GZ. `install_default` returns Err if a
    // provider is already installed, which is harmless.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    match command {
        Command::Serve => serve().await,
        Command::ResetRootPassword { fdb_cluster_file } => {
            reset_root_password(fdb_cluster_file.as_deref()).await
        }
        Command::Help => Ok(()),
    }
}

async fn serve() -> Result<()> {
    let bind_address =
        std::env::var("TRITOND_BIND_ADDRESS").unwrap_or_else(|_| DEFAULT_BIND_ADDRESS.to_string());

    let (store, audit_chain) = build_store_and_audit(None)?;
    let (jwt_key, identity_hmac_key) = bootstrap::ensure(store.as_ref())
        .await
        .context("first-run bootstrap")?;
    let auth = Arc::new(AuthService::new(jwt_key).context("build auth service")?);
    let audit = Arc::new(AuditService::new(audit_chain));
    let mut context =
        ApiContext::new(store, auth, audit).with_identity_hmac_key(Arc::new(identity_hmac_key));
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

    let dhcp_reconcile_interval = env_secs("TRITOND_DHCP_RECONCILE_INTERVAL_SECS")
        .unwrap_or(dhcp_reconciler::DEFAULT_RECONCILE_INTERVAL);
    let dhcp_gc_threshold = env_secs("TRITOND_DHCP_LEASE_GC_THRESHOLD_SECS")
        .unwrap_or(dhcp_reconciler::DEFAULT_LEASE_GC_THRESHOLD);
    info!(
        dhcp_reconcile_interval_secs = dhcp_reconcile_interval.as_secs(),
        dhcp_gc_threshold_secs = dhcp_gc_threshold.as_secs(),
        "enabling dhcp lease reconciler",
    );
    context = context.with_dhcp_reconciler(dhcp_reconciler::ReconcilerConfig {
        interval: dhcp_reconcile_interval,
        gc_threshold: dhcp_gc_threshold,
    });

    info!(version = VERSION, %bind_address, "tritond starting");

    let server = start_server_with_context(&bind_address, context).await?;
    server
        .await
        .map_err(|e| anyhow::anyhow!("HTTP server error: {e}"))?;

    Ok(())
}

async fn reset_root_password(fdb_cluster_file: Option<&str>) -> Result<()> {
    let store = build_store(fdb_cluster_file)?;
    let password = bootstrap::reset_root_password(store.as_ref()).await?;

    eprintln!();
    eprintln!("============================================================");
    eprintln!("  tritond recovery: reset root operator password");
    eprintln!();
    eprintln!("  username: {}", bootstrap::ROOT_USERNAME);
    eprintln!("  password: {}", password.expose());
    eprintln!();
    eprintln!("  Save this password now. It will not be shown again.");
    eprintln!("  Use `tcadm configure` to authenticate, then create");
    eprintln!("  long-lived API keys with `tcadm api-key create`.");
    eprintln!("============================================================");
    eprintln!();

    Ok(())
}

fn parse_command<I>(mut args: I) -> Result<Command>
where
    I: Iterator<Item = String>,
{
    let Some(first) = args.next() else {
        return Ok(Command::Serve);
    };

    match first.as_str() {
        "serve" => {
            if let Some(extra) = args.next() {
                bail!("unexpected argument for serve: {extra}");
            }
            Ok(Command::Serve)
        }
        "reset-root-password" => parse_reset_root_password(args),
        "-h" | "--help" | "help" => Ok(Command::Help),
        other => bail!("unknown command: {other}"),
    }
}

fn parse_reset_root_password<I>(mut args: I) -> Result<Command>
where
    I: Iterator<Item = String>,
{
    let mut fdb_cluster_file = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--fdb-cluster-file" => {
                let Some(value) = args.next() else {
                    bail!("--fdb-cluster-file requires a path");
                };
                fdb_cluster_file = Some(value);
            }
            "-h" | "--help" => return Ok(Command::Help),
            other => bail!("unexpected argument for reset-root-password: {other}"),
        }
    }
    Ok(Command::ResetRootPassword { fdb_cluster_file })
}

fn print_usage() {
    println!(
        "\
usage:
  tritond [serve]
  tritond reset-root-password [--fdb-cluster-file PATH]

environment:
  TRITOND_BIND_ADDRESS                 listen address for serve
  TRITOND_FDB_CLUSTER_FILE             FoundationDB cluster file
  TRITOND_DISABLE_INPROCESS_PROVISIONER=1
"
    );
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
fn build_store(fdb_cluster_file: Option<&str>) -> Result<Arc<dyn Store>> {
    if let Some(cluster_file) = fdb_cluster_file
        .map(ToOwned::to_owned)
        .or_else(|| std::env::var("TRITOND_FDB_CLUSTER_FILE").ok())
    {
        info!(%cluster_file, "using FoundationDB backend (store)");
        let store = tritond_store::FdbStore::open(Some(&cluster_file))
            .map_err(|e| anyhow::anyhow!("open FDB store: {e}"))?;
        Ok(Arc::new(store))
    } else {
        info!("TRITOND_FDB_CLUSTER_FILE not set; using in-memory store");
        Ok(Arc::new(MemStore::new()))
    }
}

#[cfg(feature = "foundationdb")]
fn build_store_and_audit(
    fdb_cluster_file: Option<&str>,
) -> Result<(Arc<dyn Store>, Arc<dyn Chain>)> {
    if let Some(cluster_file) = fdb_cluster_file
        .map(ToOwned::to_owned)
        .or_else(|| std::env::var("TRITOND_FDB_CLUSTER_FILE").ok())
    {
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
fn build_store(fdb_cluster_file: Option<&str>) -> Result<Arc<dyn Store>> {
    if fdb_cluster_file.is_some() || std::env::var("TRITOND_FDB_CLUSTER_FILE").is_ok() {
        anyhow::bail!(
            "TRITOND_FDB_CLUSTER_FILE is set but tritond was built without the `foundationdb` feature"
        );
    }
    info!("using in-memory store (binary not built with `foundationdb` feature)");
    Ok(Arc::new(MemStore::new()))
}

#[cfg(not(feature = "foundationdb"))]
fn build_store_and_audit(
    fdb_cluster_file: Option<&str>,
) -> Result<(Arc<dyn Store>, Arc<dyn Chain>)> {
    if fdb_cluster_file.is_some() || std::env::var("TRITOND_FDB_CLUSTER_FILE").is_ok() {
        anyhow::bail!(
            "TRITOND_FDB_CLUSTER_FILE is set but tritond was built without the `foundationdb` feature"
        );
    }
    info!("using in-memory store + audit (binary not built with `foundationdb` feature)");
    let store: Arc<dyn Store> = Arc::new(MemStore::new());
    let audit: Arc<dyn Chain> = Arc::new(MemChain::new());
    Ok((store, audit))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_default_command_serves() {
        assert!(matches!(
            parse_command(std::iter::empty()).unwrap(),
            Command::Serve
        ));
    }

    #[test]
    fn parse_reset_root_password_command() {
        let parsed = parse_command(
            [
                "reset-root-password".to_string(),
                "--fdb-cluster-file".to_string(),
                "/etc/fdb.cluster".to_string(),
            ]
            .into_iter(),
        )
        .unwrap();
        match parsed {
            Command::ResetRootPassword { fdb_cluster_file } => {
                assert_eq!(fdb_cluster_file.as_deref(), Some("/etc/fdb.cluster"));
            }
            _ => panic!("expected reset command"),
        }
    }
}
