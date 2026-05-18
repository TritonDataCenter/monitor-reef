// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Triton Cloud control plane daemon (binary entry point).
//!
//! # Configuration
//!
//! tritond needs almost nothing to start: a place to listen and a way
//! to reach FoundationDB. That minimum lives in a small TOML bootstrap
//! file (see [`tritond::bootstrap_config`]); everything else is
//! cluster-wide [`Settings`](tritond_store::Settings) read from FDB at
//! startup and managed with `tcadm config` (or the admin console).
//!
//! Bootstrap file (`--config PATH`, else `$TRITOND_CONFIG`, else
//! `/etc/tritond/config.toml`; absent at the default path = built-in
//! defaults):
//!
//! ```toml
//! bind_address     = "127.0.0.1:8080"
//! fdb_cluster_file = "/etc/fdb.cluster"
//! log_filter       = "info"
//! ```
//!
//! `RUST_LOG`, `TRITOND_BIND_ADDRESS` and `TRITOND_FDB_CLUSTER_FILE`
//! still work and take precedence over the file. The legacy
//! `TRITOND_*` knobs that used to configure the sweeper, the DHCP
//! reconciler, the in-process provisioner and the metrics backend are
//! now FDB settings; the env vars remain as boot-time overrides
//! (env > FDB > default) and `tritond` logs a warning for any that are
//! shadowing a stored value.
//!
//! Startup runs [`tritond::bootstrap::ensure`] which mints the JWT
//! signing key, the per-deployment identity HMAC key and the root
//! operator on first run, then loads them on every subsequent run. The
//! audit chain uses the same backend as the store: in-memory by
//! default, FDB when a cluster file is configured.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tracing::info;

use tritond::audit::AuditService;
use tritond::auth::AuthService;
use tritond::bootstrap_config::BootstrapConfig;
use tritond::{
    ApiContext, SweeperConfig, VERSION, bootstrap, dhcp_reconciler, settings,
    start_server_with_context,
};
use tritond_audit::{Chain, MemChain};
use tritond_store::{MemStore, MetricsBackend, Store};

enum Command {
    Serve {
        config: Option<PathBuf>,
    },
    ResetRootPassword {
        config: Option<PathBuf>,
        fdb_cluster_file: Option<String>,
    },
    Help,
}

#[tokio::main]
async fn main() -> Result<()> {
    match parse_command(std::env::args().skip(1))? {
        Command::Help => {
            print_usage();
            Ok(())
        }
        Command::Serve { config } => {
            let boot = BootstrapConfig::load(config.as_deref()).context("load bootstrap config")?;
            init_process(&boot.log_filter);
            serve(boot).await
        }
        Command::ResetRootPassword {
            config,
            fdb_cluster_file,
        } => {
            let boot = BootstrapConfig::load(config.as_deref()).context("load bootstrap config")?;
            init_process(&boot.log_filter);
            let cluster_file = fdb_cluster_file.or(boot.fdb_cluster_file);
            reset_root_password(cluster_file.as_deref()).await
        }
    }
}

/// One-time process setup shared by every runnable command: install
/// the tracing subscriber from the resolved log filter, and arm the
/// rustls process-default crypto provider.
fn init_process(log_filter: &str) {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(log_filter))
        .init();

    // rustls 0.23 requires a process-default `CryptoProvider` before
    // the first `ClientConfig::builder()` call. The bundle ingest path
    // (`POST /v2/silos/.../image-bundles`) uses reqwest which arms TLS
    // even for plaintext URLs; without this `tritond` panics on the
    // first ingest on a cold SmartOS GZ. `install_default` returns Err
    // if a provider is already installed, which is harmless.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
}

async fn serve(boot: BootstrapConfig) -> Result<()> {
    let (store, audit_chain, fdb_db) = build_store_and_audit(boot.fdb_cluster_file.as_deref())?;
    let (jwt_key, identity_hmac_key) = bootstrap::ensure(store.as_ref())
        .await
        .context("first-run bootstrap")?;
    let auth = Arc::new(AuthService::new(jwt_key).context("build auth service")?);
    let audit = Arc::new(AuditService::new(audit_chain));

    for key in settings::active_env_overrides() {
        tracing::warn!(
            key = key.as_str(),
            env = key.env_var(),
            "config key overridden by environment variable (env > FDB)",
        );
    }
    let resolved = settings::resolve_settings(
        store
            .get_settings()
            .await
            .context("load cluster settings")?,
    );

    let identity_hmac_key = Arc::new(identity_hmac_key);
    let mut context = ApiContext::new(Arc::clone(&store), auth, audit)
        .with_identity_hmac_key(Arc::clone(&identity_hmac_key));

    // RFD 00004 SG-1b: when running with FDB, swap the default
    // MemSecStore-backed executor for one backed by the same FDB
    // Database the store and audit chain use. Sagas in FDB land in
    // the region's single cluster (Locked Decision #4) under the
    // `saga/...` prefix and survive `tritond` restarts.
    #[cfg(feature = "foundationdb")]
    if let Some(db) = fdb_db.clone() {
        let saga = tritond::context::fdb_saga_executor(
            db,
            &store,
            &identity_hmac_key,
            context.audit.chain(),
        );
        context = context.with_saga_executor(saga);
        info!("saga engine: FDB-backed SecStore enabled");
    }
    #[cfg(not(feature = "foundationdb"))]
    let _ = fdb_db; // unused without the feature

    if resolved.provisioner_inprocess_disabled {
        info!("in-process stub provisioner disabled; expecting external tritonagent");
        context = context.without_in_process_provisioner();
    }

    let sweeper_interval = Duration::from_secs(resolved.sweeper_interval_secs);
    let stale_after = Duration::from_secs(resolved.stale_claim_threshold_secs);
    let saga_retention = Duration::from_secs(resolved.saga_retention_secs);
    info!(
        sweeper_interval_secs = sweeper_interval.as_secs(),
        stale_after_secs = stale_after.as_secs(),
        saga_retention_secs = saga_retention.as_secs(),
        "enabling stale-claim sweeper",
    );
    context = context.with_sweeper(SweeperConfig {
        interval: sweeper_interval,
        stale_after,
        saga_retention,
    });

    let dhcp_reconcile_interval = Duration::from_secs(resolved.dhcp_reconcile_interval_secs);
    let dhcp_gc_threshold = Duration::from_secs(resolved.dhcp_lease_gc_threshold_secs);
    info!(
        dhcp_reconcile_interval_secs = dhcp_reconcile_interval.as_secs(),
        dhcp_gc_threshold_secs = dhcp_gc_threshold.as_secs(),
        "enabling dhcp lease reconciler",
    );
    context = context.with_dhcp_reconciler(dhcp_reconciler::ReconcilerConfig {
        interval: dhcp_reconcile_interval,
        gc_threshold: dhcp_gc_threshold,
    });

    // Metrics backend. Default is the in-memory ring buffer (set up by
    // ApiContext::new). When `metrics.backend` is `clickhouse` and
    // `metrics.clickhouse_url` is set, swap in the ClickHouse store and
    // self-bootstrap its schema. A connectivity failure here logs a
    // warning and falls back to the ring buffer rather than refusing to
    // start -- metrics are best-effort.
    if matches!(resolved.metrics_backend, MetricsBackend::Clickhouse) {
        match resolved.metrics_clickhouse_url.as_deref() {
            Some(url) => match tritond_metrics::store::ClickHouseStore::new(url.to_string()) {
                Ok(ch) => match ch.ensure_schema().await {
                    Ok(()) => {
                        info!(clickhouse_url = %url, "metrics store: ClickHouse");
                        context = context.with_metrics(Arc::new(ch));
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, clickhouse_url = %url, "ClickHouse schema bootstrap failed; falling back to in-memory metrics");
                    }
                },
                Err(e) => {
                    tracing::warn!(error = %e, "ClickHouse store init failed; falling back to in-memory metrics");
                }
            },
            None => {
                tracing::warn!(
                    "metrics.backend=clickhouse but metrics.clickhouse_url is unset; using in-memory metrics"
                );
            }
        }
    }

    info!(version = VERSION, bind_address = %boot.bind_address, "tritond starting");

    let server = start_server_with_context(&boot.bind_address, context).await?;
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
        return Ok(Command::Serve { config: None });
    };

    match first.as_str() {
        "serve" => parse_serve(args),
        "reset-root-password" => parse_reset_root_password(args),
        "-h" | "--help" | "help" => Ok(Command::Help),
        other => bail!("unknown command: {other}"),
    }
}

fn parse_serve<I>(mut args: I) -> Result<Command>
where
    I: Iterator<Item = String>,
{
    let mut config = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" => {
                let Some(value) = args.next() else {
                    bail!("--config requires a path");
                };
                config = Some(PathBuf::from(value));
            }
            "-h" | "--help" => return Ok(Command::Help),
            other => bail!("unexpected argument for serve: {other}"),
        }
    }
    Ok(Command::Serve { config })
}

fn parse_reset_root_password<I>(mut args: I) -> Result<Command>
where
    I: Iterator<Item = String>,
{
    let mut config = None;
    let mut fdb_cluster_file = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" => {
                let Some(value) = args.next() else {
                    bail!("--config requires a path");
                };
                config = Some(PathBuf::from(value));
            }
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
    Ok(Command::ResetRootPassword {
        config,
        fdb_cluster_file,
    })
}

fn print_usage() {
    println!(
        "\
usage:
  tritond [serve] [--config PATH]
  tritond reset-root-password [--config PATH] [--fdb-cluster-file PATH]

bootstrap config file (TOML; --config PATH, else $TRITOND_CONFIG, else
/etc/tritond/config.toml; absent at the default path = built-in defaults):
  bind_address     = \"127.0.0.1:8080\"
  fdb_cluster_file = \"/etc/fdb.cluster\"
  log_filter       = \"info\"

environment overrides (take precedence over the file):
  TRITOND_BIND_ADDRESS       listen address
  TRITOND_FDB_CLUSTER_FILE   FoundationDB cluster file
  RUST_LOG                   tracing filter

all other tunables (sweeper, dhcp reconciler, in-process provisioner,
metrics backend) live in FoundationDB; manage them with `tcadm config`.
"
    );
}

#[cfg(feature = "foundationdb")]
fn build_store(fdb_cluster_file: Option<&str>) -> Result<Arc<dyn Store>> {
    if let Some(cluster_file) = fdb_cluster_file {
        info!(%cluster_file, "using FoundationDB backend (store)");
        let store = tritond_store::FdbStore::open(Some(cluster_file))
            .map_err(|e| anyhow::anyhow!("open FDB store: {e}"))?;
        Ok(Arc::new(store))
    } else {
        info!("no FoundationDB cluster file configured; using in-memory store");
        Ok(Arc::new(MemStore::new()))
    }
}

#[cfg(feature = "foundationdb")]
fn build_store_and_audit(
    fdb_cluster_file: Option<&str>,
) -> Result<(
    Arc<dyn Store>,
    Arc<dyn Chain>,
    Option<Arc<tritond_saga::FdbDatabase>>,
)> {
    if let Some(cluster_file) = fdb_cluster_file {
        info!(%cluster_file, "using FoundationDB backend (store + audit + saga)");
        let store = tritond_store::FdbStore::open(Some(cluster_file))
            .map_err(|e| anyhow::anyhow!("open FDB store: {e}"))?;
        // Share the FDB Database handle with the audit chain + saga
        // SecStore so we don't have multiple `boot()` callers.
        // FdbStore holds it as Arc<Database>; FdbChain and
        // FdbSecStore take their own Arc references.
        let db = store.database();
        let audit_chain: Arc<dyn Chain> = Arc::new(tritond_audit::FdbChain::new(Arc::clone(&db)));
        Ok((Arc::new(store), audit_chain, Some(db)))
    } else {
        info!("no FoundationDB cluster file configured; using in-memory store + audit + saga");
        let store: Arc<dyn Store> = Arc::new(MemStore::new());
        let audit: Arc<dyn Chain> = Arc::new(MemChain::new());
        Ok((store, audit, None))
    }
}

#[cfg(not(feature = "foundationdb"))]
fn build_store(fdb_cluster_file: Option<&str>) -> Result<Arc<dyn Store>> {
    if fdb_cluster_file.is_some() {
        anyhow::bail!(
            "a FoundationDB cluster file is configured but tritond was built without the `foundationdb` feature"
        );
    }
    info!("using in-memory store (binary not built with `foundationdb` feature)");
    Ok(Arc::new(MemStore::new()))
}

#[cfg(not(feature = "foundationdb"))]
fn build_store_and_audit(
    fdb_cluster_file: Option<&str>,
) -> Result<(Arc<dyn Store>, Arc<dyn Chain>, Option<()>)> {
    if fdb_cluster_file.is_some() {
        anyhow::bail!(
            "a FoundationDB cluster file is configured but tritond was built without the `foundationdb` feature"
        );
    }
    info!("using in-memory store + audit (binary not built with `foundationdb` feature)");
    let store: Arc<dyn Store> = Arc::new(MemStore::new());
    let audit: Arc<dyn Chain> = Arc::new(MemChain::new());
    Ok((store, audit, None))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn parse_default_command_serves() {
        match parse_command(std::iter::empty()).unwrap() {
            Command::Serve { config } => assert!(config.is_none()),
            _ => panic!("expected serve"),
        }
    }

    #[test]
    fn parse_serve_with_config_flag() {
        let parsed = parse_command(
            [
                "serve".to_string(),
                "--config".to_string(),
                "/etc/t.toml".to_string(),
            ]
            .into_iter(),
        )
        .unwrap();
        match parsed {
            Command::Serve { config } => {
                assert_eq!(config.as_deref(), Some(Path::new("/etc/t.toml")));
            }
            _ => panic!("expected serve"),
        }
    }

    #[test]
    fn parse_reset_root_password_command() {
        let parsed = parse_command(
            [
                "reset-root-password".to_string(),
                "--config".to_string(),
                "/etc/t.toml".to_string(),
                "--fdb-cluster-file".to_string(),
                "/etc/fdb.cluster".to_string(),
            ]
            .into_iter(),
        )
        .unwrap();
        match parsed {
            Command::ResetRootPassword {
                config,
                fdb_cluster_file,
            } => {
                assert_eq!(config.as_deref(), Some(Path::new("/etc/t.toml")));
                assert_eq!(fdb_cluster_file.as_deref(), Some("/etc/fdb.cluster"));
            }
            _ => panic!("expected reset command"),
        }
    }

    #[test]
    fn unknown_command_errors() {
        assert!(parse_command(["bogus".to_string()].into_iter()).is_err());
    }
}
