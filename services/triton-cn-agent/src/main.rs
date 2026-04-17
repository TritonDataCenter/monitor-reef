// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Triton Compute Node Agent binary entrypoint.
//!
//! The binary has two operating modes:
//!
//! * `--backend smartos` (production): boots through the full
//!   [`startup::SmartosStartup`] pipeline — reads agent config + sysinfo +
//!   SDC config, binds the admin IP, registers with CNAPI, and runs the
//!   heartbeater plus zoneevent/zones watchers until SIGTERM.
//!
//! * `--backend dummy` (dev/test): stands up only the HTTP server with
//!   the platform-neutral task registry. Useful for exercising the task
//!   dispatcher on non-illumos hosts.

use std::net::SocketAddr;

use anyhow::{Context, Result};
use clap::Parser;
use triton_cn_agent::{
    DEFAULT_AGENT_PORT,
    startup::{SmartosStartup, start_dummy},
};

/// Command-line arguments.
#[derive(Parser, Debug)]
#[command(name = "triton-cn-agent", version)]
struct Args {
    /// Backend to run. Valid values:
    ///
    /// * `smartos` — the production backend; boots the full startup
    ///   pipeline. Requires the standard SmartOS install layout.
    /// * `dummy` — minimal HTTP server for dev/test, no CNAPI or
    ///   watchers.
    #[arg(long, env = "CN_AGENT_BACKEND", default_value = "smartos")]
    backend: String,

    /// Optional bind-address override. In production this is derived
    /// from the admin NIC in sysinfo; setting it explicitly is useful
    /// during development or when running multiple agents on one host.
    #[arg(long, env = "CN_AGENT_BIND_ADDR")]
    bind_addr: Option<SocketAddr>,

    /// Override the CNAPI URL. Only meaningful for `--backend smartos`.
    /// When unset, cn-agent picks the URL from `cn-agent.config.json`
    /// (`cnapi.url`) or falls back to `cnapi.<dc>.<domain>` constructed
    /// from `/lib/sdc/config.sh`.
    #[arg(long, env = "CN_AGENT_CNAPI_URL")]
    cnapi_url: Option<String>,

    /// Server UUID used by the `dummy` backend. Ignored in production —
    /// the SmartOS startup reads the UUID from sysinfo.
    #[arg(long, env = "CN_AGENT_SERVER_UUID")]
    dummy_server_uuid: Option<uuid::Uuid>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                tracing_subscriber::EnvFilter::new("triton_cn_agent=info,dropshot=info")
            }),
        )
        .init();

    let args = Args::parse();

    match args.backend.as_str() {
        "smartos" => run_smartos(&args).await,
        "dummy" => run_dummy(&args).await,
        other => anyhow::bail!("backend '{other}' not supported. Valid values: smartos, dummy."),
    }
}

async fn run_smartos(args: &Args) -> Result<()> {
    let mut startup = SmartosStartup::production();
    if let Some(url) = &args.cnapi_url {
        startup = startup.with_cnapi_url(url.clone());
    }
    if let Some(addr) = args.bind_addr {
        startup = startup.with_bind_address(addr);
    }
    let agent = startup.start().await.context("start SmartOS agent")?;
    agent.run_until_shutdown().await
}

async fn run_dummy(args: &Args) -> Result<()> {
    let bind_addr = args
        .bind_addr
        .unwrap_or_else(|| SocketAddr::from(([127, 0, 0, 1], DEFAULT_AGENT_PORT)));
    let server_uuid = args.dummy_server_uuid.unwrap_or_else(uuid::Uuid::nil);
    tracing::info!(
        bind = %bind_addr,
        server_uuid = %server_uuid,
        "starting dummy cn-agent (no CNAPI, no watchers)"
    );
    let server = start_dummy(bind_addr, server_uuid)?;

    // Block on the server until it exits; dummy backend has no heartbeater
    // to shut down, so this is the whole story.
    server
        .await
        .map_err(|e| anyhow::anyhow!("server exited: {e}"))
        .context("dummy backend server loop")
}
