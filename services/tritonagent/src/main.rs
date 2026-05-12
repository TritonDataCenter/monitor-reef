// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Tritonagent binary entry point. See [`tritonagent`] for the
//! agent loop and design notes.
//!
//! Configuration is via `clap`-parsed args + env vars:
//!
//! * `--endpoint` / `TRITONAGENT_ENDPOINT` — tritond URL
//! * `--credential-path` / `TRITONAGENT_CREDENTIAL_PATH` —
//!   on-disk file holding the wire-form `tcadm_…` per-CN API key.
//!   Default `/var/lib/tritonagent/credentials`.
//! * `--sysinfo-bin` / `TRITONAGENT_SYSINFO_BIN` — path to the
//!   SmartOS `sysinfo` binary. Default `/usr/bin/sysinfo`. Tests
//!   stub this.
//! * `--poll-interval-secs` / `TRITONAGENT_POLL_INTERVAL_SECS` —
//!   default 5
//! * `--proteus-dev` / `TRITONAGENT_PROTEUS_DEV` — Proteus device
//!   node. Default `/dev/proteus`.
//! * `--edge-root` / `TRITONAGENT_EDGE_ROOT` — directory where edge
//!   manifests and control sockets live. The legacy host-process edge
//!   shim also stores pid files and logs there.
//! * `--fhrun-bin` / `TRITONAGENT_FHRUN_BIN` — fhrun launcher path.
//! * `--console-listen-port` / `TRITONAGENT_CONSOLE_PORT` — TCP port the
//!   on-CN serial / VNC console listener binds on the admin IP. Default
//!   `9101`.
//! * `--dry-run` / `TRITONAGENT_DRY_RUN`
//!
//! There is no longer an `--api-key` flag: on first boot the agent
//! self-registers with tritond, prints a claim code on the console
//! for an operator to approve, and persists the resulting per-CN
//! API key to the credential file. Subsequent boots resume from
//! disk without re-registering.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use tracing_subscriber::EnvFilter;
use tritonagent::{AgentConfig, console_creds, credentials, registration};
use tritond_cn_platform::smartos::Sysinfo;

/// Maximum time the agent waits for an operator to approve the
/// registration before giving up and exiting. Hard-coded at 1h
/// because it is not user-tunable today and there is no value in
/// making it env-configurable until ops have an actual ask.
const REGISTER_TIMEOUT: Duration = Duration::from_secs(3600);

#[derive(Debug, Parser)]
#[command(
    version,
    about = "Triton Cloud per-CN provisioning agent (Phase 0 stub)"
)]
struct Cli {
    /// Tritond URL, e.g. `http://10.199.199.10:8080`.
    #[arg(long, env = "TRITONAGENT_ENDPOINT")]
    endpoint: String,

    /// Path to the on-disk credential file.
    ///
    /// On first boot, the agent self-registers with tritond and writes
    /// the per-CN API key here. On subsequent boots, the agent reads
    /// this file directly and skips registration.
    #[arg(
        long,
        env = "TRITONAGENT_CREDENTIAL_PATH",
        default_value_os_t = credentials::path_default(),
    )]
    credential_path: PathBuf,

    /// Path to the SmartOS `sysinfo` binary. Tests stub this with a
    /// shell script that prints a fixture.
    #[arg(
        long,
        env = "TRITONAGENT_SYSINFO_BIN",
        default_value_t = String::from(tritond_cn_platform::smartos::sysinfo::SYSINFO_BIN),
    )]
    sysinfo_bin: String,

    /// Sleep between empty-queue polls.
    #[arg(long, env = "TRITONAGENT_POLL_INTERVAL_SECS", default_value_t = 5)]
    poll_interval_secs: u64,

    /// Proteus kernel device path used for per-NIC port realization.
    #[arg(
        long,
        env = "TRITONAGENT_PROTEUS_DEV",
        default_value_t = String::from(tritonagent::DEFAULT_PROTEUS_DEVICE),
    )]
    proteus_dev: String,

    /// Root directory for per-edge-instance fhrun manifests, pid
    /// files, logs, and edge-control Unix sockets.
    #[arg(
        long,
        env = "TRITONAGENT_EDGE_ROOT",
        default_value_t = String::from(tritonagent::DEFAULT_EDGE_ROOT),
    )]
    edge_root: String,

    /// Path to the fhrun launcher used for edge microVM jobs.
    #[arg(
        long,
        env = "TRITONAGENT_FHRUN_BIN",
        default_value_t = String::from(tritonagent::DEFAULT_FHRUN_BIN),
    )]
    fhrun_bin: String,

    /// TCP port the on-CN serial / VNC console listener binds on the
    /// admin IP. tritond dials `wss://<admin-ip>:<this>/console/{uuid}`.
    #[arg(
        long,
        env = "TRITONAGENT_CONSOLE_PORT",
        default_value_t = tritonagent::DEFAULT_CONSOLE_LISTEN_PORT,
    )]
    console_listen_port: u16,

    /// When set, skip `vmadm` entirely and mark every claimed
    /// job `Completed`. Useful for transport-only smoke testing
    /// on hosts without SmartOS. Off by default — the production
    /// path is the obvious one.
    #[arg(long, env = "TRITONAGENT_DRY_RUN", default_value_t = false)]
    dry_run: bool,

    /// When set, do NOT spawn the background heartbeater /
    /// zoneevent watcher. The agent will only run the job-claim
    /// loop. Used by tritond integration tests that don't want
    /// the heartbeater chattering at the test server. Off by
    /// default — production CNs are expected to publish liveness
    /// + status.
    #[arg(
        long = "no-heartbeater",
        env = "TRITONAGENT_DISABLE_HEARTBEATER",
        default_value_t = false
    )]
    no_heartbeater: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    // rustls 0.23 requires a crypto provider be set as the
    // process default before the first `ClientConfig::builder()`
    // call. Reqwest doesn't do this for us in all cases (cold
    // SmartOS GZ panics without it). aws-lc-rs is the workspace
    // default; the only failure mode of `install_default` is
    // "already installed," which is harmless.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let cli = Cli::parse();

    let sysinfo = Sysinfo::collect_from_path(&cli.sysinfo_bin)
        .await
        .with_context(|| format!("collect sysinfo from {}", cli.sysinfo_bin))?;
    let server_uuid = sysinfo.uuid().ok_or_else(|| {
        anyhow::anyhow!(
            "sysinfo from {} did not include a UUID; agent identity is unknown",
            cli.sysinfo_bin,
        )
    })?;

    // Load (or generate, on first boot) the stable self-signed TLS
    // keypair for the console listener and compute its SPKI fingerprint.
    // This must happen before registration so the fingerprint can be
    // sent in the register payload (tritond pins it). The admin IP, when
    // known, is baked in as a cert SAN.
    let admin_ip = sysinfo.admin_ip();
    let console_tls = console_creds::load_or_init_tls(&cli.credential_path, admin_ip)
        .context("load or init console TLS material")?;

    let outcome = registration::register_or_resume(
        &cli.endpoint,
        &sysinfo,
        server_uuid,
        &cli.credential_path,
        cli.console_listen_port,
        console_tls.spki_sha256_hex.clone(),
        REGISTER_TIMEOUT,
    )
    .await
    .context("register or resume against tritond")?;

    let cfg = AgentConfig {
        endpoint: cli.endpoint,
        api_key: outcome.api_key,
        // claimed_by must be the server_uuid string — tritond's
        // bound-key check pins each per-CN key to a specific CN
        // identity, and that identity is the SmartOS server_uuid.
        agent_id: server_uuid.to_string(),
        poll_interval: Duration::from_secs(cli.poll_interval_secs),
        proteus_dev: PathBuf::from(cli.proteus_dev),
        edge_root: PathBuf::from(cli.edge_root),
        fhrun_bin: PathBuf::from(cli.fhrun_bin),
        dry_run: cli.dry_run,
        spawn_heartbeater: !cli.no_heartbeater,
        admin_ip,
        console_listen_port: cli.console_listen_port,
        console_ticket_key: outcome.console_ticket_key,
        console_tls: Some(console_tls),
    };
    tritonagent::run(cfg).await
}
