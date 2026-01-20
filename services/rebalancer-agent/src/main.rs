// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2020 Joyent, Inc.
// Copyright 2026 Edgecast Cloud LLC.

//! Rebalancer Agent Service
//!
//! The rebalancer agent runs on storage nodes and processes object download
//! assignments sent by the rebalancer manager. It:
//!
//! - Receives assignments (batches of objects to download)
//! - Persists assignments to SQLite for crash recovery
//! - Downloads objects from source storage nodes using HTTP
//! - Verifies MD5 checksums
//! - Reports assignment status back to the manager

use anyhow::{Context, Result};
use dropshot::{ConfigDropshot, ConfigLogging, ConfigLoggingLevel, HttpServerStarter};
use tracing::info;

use rebalancer_agent::RebalancerAgentImpl;
use rebalancer_agent::config::AgentConfig;
use rebalancer_agent::context::ApiContext;

/// Default bind address for the HTTP server.
const DEFAULT_BIND_ADDRESS: &str = "0.0.0.0:7878";

/// Default maximum request body size (bytes).
const DEFAULT_BODY_MAX_BYTES: usize = 100 * 1024 * 1024; // 100MB for large assignments

fn print_version() {
    let version = env!("CARGO_PKG_VERSION");
    let name = env!("CARGO_PKG_NAME");
    let buildstamp = option_env!("STAMP").unwrap_or("no-STAMP");
    println!("{} {} ({})", name, version, buildstamp);
}

#[tokio::main]
async fn main() -> Result<()> {
    // Handle --version and --help
    let args: Vec<String> = std::env::args().collect();
    #[allow(clippy::never_loop)] // Intentional: early return on first recognized arg
    for arg in &args[1..] {
        match arg.as_str() {
            "-V" | "--version" => {
                print_version();
                return Ok(());
            }
            "-h" | "--help" => {
                print_version();
                println!("Usage: {} [OPTIONS]", args[0]);
                println!();
                println!("Options:");
                println!("  -h, --help       Display this information");
                println!("  -V, --version    Display the program's version number");
                println!();
                println!("Environment variables:");
                println!(
                    "  BIND_ADDRESS     Server bind address (default: {})",
                    DEFAULT_BIND_ADDRESS
                );
                println!(
                    "  DATA_DIR         Data directory for SQLite and objects (default: /var/tmp/rebalancer)"
                );
                println!(
                    "  RUST_LOG         Log filter (default: rebalancer_agent=info,dropshot=info)"
                );
                return Ok(());
            }
            _ => {
                eprintln!("Unknown option: {}", arg);
                std::process::exit(1);
            }
        }
    }

    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(
            std::env::var("RUST_LOG")
                .unwrap_or_else(|_| "rebalancer_agent=info,dropshot=info".to_string()),
        ))
        .init();

    print_version();

    // Load configuration
    let config = AgentConfig::from_env();
    info!("Data directory: {}", config.data_dir.display());

    // Ensure data directory exists
    tokio::fs::create_dir_all(&config.data_dir)
        .await
        .with_context(|| {
            format!(
                "Failed to create data directory: {}",
                config.data_dir.display()
            )
        })?;

    // Create API context
    let api_context = ApiContext::new(config.clone())
        .await
        .context("Failed to create API context")?;

    // Get API description from the trait implementation
    let api =
        rebalancer_agent_api::rebalancer_agent_api_mod::api_description::<RebalancerAgentImpl>()
            .map_err(|e| anyhow::anyhow!("Failed to create API description: {}", e))?;

    // Configure the server
    let bind_address = std::env::var("BIND_ADDRESS")
        .unwrap_or_else(|_| DEFAULT_BIND_ADDRESS.to_string())
        .parse()
        .context("Invalid BIND_ADDRESS")?;

    let config_dropshot = ConfigDropshot {
        bind_address,
        default_request_body_max_bytes: DEFAULT_BODY_MAX_BYTES,
        default_handler_task_mode: dropshot::HandlerTaskMode::Detached,
        ..Default::default()
    };

    let config_logging = ConfigLogging::StderrTerminal {
        level: ConfigLoggingLevel::Info,
    };

    let log = config_logging
        .to_logger("rebalancer-agent")
        .map_err(|error| anyhow::anyhow!("failed to create logger: {}", error))?;

    // Start the server
    let server = HttpServerStarter::new(&config_dropshot, api, api_context, &log)
        .map_err(|error| anyhow::anyhow!("failed to create server: {}", error))?
        .start();

    info!("Rebalancer agent running on http://{}", bind_address);

    server
        .await
        .map_err(|error| anyhow::anyhow!("server failed: {}", error))
}
