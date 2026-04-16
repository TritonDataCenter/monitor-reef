// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Triton Compute Node Agent binary entrypoint.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use cn_agent_api::cn_agent_api_mod;
use dropshot::{ConfigDropshot, ConfigLogging, ConfigLoggingLevel, HttpServerStarter};
use triton_cn_agent::{
    AgentContext, AgentMetadata, DEFAULT_AGENT_PORT, api_impl::CnAgentApiImpl, tasks,
};

/// Command-line arguments.
#[derive(Parser, Debug)]
#[command(name = "triton-cn-agent", version)]
struct Args {
    /// Address to bind the HTTP server to.
    ///
    /// Production installs pass the compute node's admin IP here. For
    /// development, leave unset to bind to the loopback address.
    #[arg(long, env = "CN_AGENT_BIND_ADDR", default_value_t = default_bind_addr())]
    bind_addr: SocketAddr,

    /// Backend identifier reported via `/ping`.
    ///
    /// The real agent picks this based on `os.platform()`; the Rust port
    /// accepts it as an arg for dev/test flexibility. Valid values:
    /// `dummy` (platform-neutral tasks only) or `smartos` (adds sysinfo +
    /// whatever else has been ported). `smartos` is still being built out,
    /// so most tasks will 404 until they're implemented.
    #[arg(long, env = "CN_AGENT_BACKEND", default_value = "dummy")]
    backend: String,

    /// Server UUID this agent is running on. Must be a valid UUID.
    #[arg(long, env = "CN_AGENT_SERVER_UUID")]
    server_uuid: Option<uuid::Uuid>,
}

fn default_bind_addr() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], DEFAULT_AGENT_PORT))
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

    let registry = match args.backend.as_str() {
        "dummy" => tasks::common_registry(),
        "smartos" => tasks::smartos_registry(),
        other => {
            anyhow::bail!("backend '{other}' not supported. Valid values: dummy, smartos.");
        }
    };

    let server_uuid = args.server_uuid.unwrap_or_else(uuid::Uuid::nil);

    let metadata = AgentMetadata {
        name: "cn-agent".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        server_uuid,
        backend: args.backend.clone(),
    };

    let context = Arc::new(AgentContext::new(metadata, registry));

    let api = cn_agent_api_mod::api_description::<CnAgentApiImpl>()
        .map_err(|e| anyhow::anyhow!("build api description: {e}"))?;

    let config = ConfigDropshot {
        bind_address: args.bind_addr,
        default_request_body_max_bytes: 4 * 1024 * 1024, // 4 MiB; docker_build payloads are large
        default_handler_task_mode: dropshot::HandlerTaskMode::Detached,
        ..Default::default()
    };

    let log = ConfigLogging::StderrTerminal {
        level: ConfigLoggingLevel::Info,
    }
    .to_logger("triton-cn-agent")
    .map_err(|e| anyhow::anyhow!("build logger: {e}"))?;

    let server = HttpServerStarter::new(&config, api, context, &log)
        .map_err(|e| anyhow::anyhow!("start http server: {e}"))?
        .start();

    tracing::info!(bind = %args.bind_addr, backend = %args.backend, "cn-agent listening");

    server
        .await
        .map_err(|e| anyhow::anyhow!("server exited with error: {e}"))
        .context("cn-agent server loop")
}
