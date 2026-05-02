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
//! * `--api-key` / `TRITONAGENT_API_KEY` — Agent-scoped `tcadm_…` key
//! * `--agent-id` / `TRITONAGENT_AGENT_ID` — defaults to hostname
//! * `--poll-interval-secs` / `TRITONAGENT_POLL_INTERVAL_SECS` —
//!   default 5
//!
//! API keys are sensitive; prefer the env var so the secret does
//! not appear in `ps`.

use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use tracing_subscriber::EnvFilter;
use tritonagent::AgentConfig;

#[derive(Debug, Parser)]
#[command(
    version,
    about = "Triton Cloud per-CN provisioning agent (Phase 0 stub)"
)]
struct Cli {
    /// Tritond URL, e.g. `http://10.199.199.10:8080`.
    #[arg(long, env = "TRITONAGENT_ENDPOINT")]
    endpoint: String,

    /// Agent-scoped API key (`tcadm_…`). Prefer the env var
    /// so the secret is not visible in `ps`.
    #[arg(long, env = "TRITONAGENT_API_KEY", hide_env_values = true)]
    api_key: String,

    /// Self-reported agent identity. Defaults to the host's
    /// machine name.
    #[arg(long, env = "TRITONAGENT_AGENT_ID")]
    agent_id: Option<String>,

    /// Sleep between empty-queue polls.
    #[arg(long, env = "TRITONAGENT_POLL_INTERVAL_SECS", default_value_t = 5)]
    poll_interval_secs: u64,

    /// Stub-mode work simulation. Tests set this to 0; the
    /// default 50ms keeps the agent's lifecycle visible.
    #[arg(long, env = "TRITONAGENT_STUB_WORK_MS", default_value_t = 50)]
    stub_work_ms: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    let agent_id = match cli.agent_id {
        Some(id) => id,
        None => hostname::get()
            .context("read hostname")?
            .to_string_lossy()
            .into_owned(),
    };
    let cfg = AgentConfig {
        endpoint: cli.endpoint,
        api_key: cli.api_key,
        agent_id,
        poll_interval: Duration::from_secs(cli.poll_interval_secs),
        stub_work_duration: Duration::from_millis(cli.stub_work_ms),
    };
    tritonagent::run(cfg).await
}
