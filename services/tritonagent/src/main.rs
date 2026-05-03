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
use tritond_cn_platform::smartos::Sysinfo;
use tritonagent::{AgentConfig, credentials, registration};

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

    /// When set, skip `vmadm` entirely and mark every claimed
    /// job `Completed`. Useful for transport-only smoke testing
    /// on hosts without SmartOS. Off by default — the production
    /// path is the obvious one.
    #[arg(long, env = "TRITONAGENT_DRY_RUN", default_value_t = false)]
    dry_run: bool,
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

    let api_key = registration::register_or_resume(
        &cli.endpoint,
        &sysinfo,
        server_uuid,
        &cli.credential_path,
        REGISTER_TIMEOUT,
    )
    .await
    .context("register or resume against tritond")?;

    let cfg = AgentConfig {
        endpoint: cli.endpoint,
        api_key,
        // claimed_by must be the server_uuid string — tritond's
        // bound-key check pins each per-CN key to a specific CN
        // identity, and that identity is the SmartOS server_uuid.
        agent_id: server_uuid.to_string(),
        poll_interval: Duration::from_secs(cli.poll_interval_secs),
        dry_run: cli.dry_run,
    };
    tritonagent::run(cfg).await
}
