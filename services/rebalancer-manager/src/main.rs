// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2020 Joyent, Inc.
// Copyright 2026 Edgecast Cloud LLC.

//! Rebalancer Manager Service
//!
//! The rebalancer manager orchestrates object evacuation jobs across
//! storage nodes. It:
//!
//! - Receives job creation requests (e.g., evacuate a storage node)
//! - Discovers objects on the source node via Sharkspotter/Moray
//! - Creates assignments and dispatches them to rebalancer agents
//! - Tracks job progress in PostgreSQL
//! - Provides status endpoints for monitoring

mod config;
mod context;
mod db;
mod jobs;
mod metrics;
mod moray;
mod storinfo;

use anyhow::{Context, Result};
use dropshot::{
    ConfigDropshot, ConfigLogging, ConfigLoggingLevel, HttpError, HttpResponseOk,
    HttpResponseUpdatedNoContent, HttpServerStarter, Path, RequestContext,
};
use rebalancer_manager_api::{JobPath, RebalancerManagerApi};
use rebalancer_types::{EvacuateJobUpdateMessage, JobDbEntry, JobPayload, JobStatus};
use tracing::info;

use crate::config::ManagerConfig;
use crate::context::ApiContext;

/// Default bind address for the HTTP server.
const DEFAULT_BIND_ADDRESS: &str = "0.0.0.0:8878";

/// Default maximum request body size (bytes).
const DEFAULT_BODY_MAX_BYTES: usize = 10 * 1024 * 1024; // 10MB

/// Rebalancer Manager API implementation
enum RebalancerManagerImpl {}

impl RebalancerManagerApi for RebalancerManagerImpl {
    type Context = ApiContext;

    async fn create_job(
        rqctx: RequestContext<Self::Context>,
        body: dropshot::TypedBody<JobPayload>,
    ) -> Result<HttpResponseOk<String>, HttpError> {
        let ctx = rqctx.context();
        let payload = body.into_inner();

        tracing::info!(payload = ?payload, "Received job creation request");

        let job_id = ctx
            .create_job(payload)
            .await
            .map_err(|e| HttpError::for_internal_error(format!("Failed to create job: {}", e)))?;

        Ok(HttpResponseOk(job_id))
    }

    async fn list_jobs(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseOk<Vec<JobDbEntry>>, HttpError> {
        let ctx = rqctx.context();

        let jobs = ctx
            .list_jobs()
            .await
            .map_err(|e| HttpError::for_internal_error(format!("Failed to list jobs: {}", e)))?;

        Ok(HttpResponseOk(jobs))
    }

    async fn get_job(
        rqctx: RequestContext<Self::Context>,
        path: Path<JobPath>,
    ) -> Result<HttpResponseOk<JobStatus>, HttpError> {
        let ctx = rqctx.context();
        let uuid = path.into_inner().uuid;

        // Validate UUID format
        uuid::Uuid::parse_str(&uuid).map_err(|_| {
            HttpError::for_bad_request(None, format!("Invalid UUID format: {}", uuid))
        })?;

        let status = ctx.get_job_status(&uuid).await.map_err(|e| {
            let msg = e.to_string();
            if msg.contains("not found") {
                HttpError::for_bad_request(None, format!("Job {} not found", uuid))
            } else if msg.contains("initializing") {
                HttpError::for_internal_error(format!("Job {} is still initializing", uuid))
            } else {
                HttpError::for_internal_error(format!("Failed to get job: {}", e))
            }
        })?;

        Ok(HttpResponseOk(status))
    }

    async fn update_job(
        rqctx: RequestContext<Self::Context>,
        path: Path<JobPath>,
        body: dropshot::TypedBody<EvacuateJobUpdateMessage>,
    ) -> Result<HttpResponseUpdatedNoContent, HttpError> {
        let ctx = rqctx.context();
        let uuid = path.into_inner().uuid;
        let msg = body.into_inner();

        // Validate UUID format
        uuid::Uuid::parse_str(&uuid).map_err(|_| {
            HttpError::for_bad_request(None, format!("Invalid UUID format: {}", uuid))
        })?;

        ctx.update_job(&uuid, msg).await.map_err(|e| {
            let msg = e.to_string();
            if msg.contains("not found") {
                HttpError::for_bad_request(None, format!("Job {} not found", uuid))
            } else if msg.contains("Cannot update") {
                HttpError::for_bad_request(None, msg)
            } else {
                HttpError::for_internal_error(format!("Failed to update job: {}", e))
            }
        })?;

        Ok(HttpResponseUpdatedNoContent())
    }

    async fn retry_job(
        rqctx: RequestContext<Self::Context>,
        path: Path<JobPath>,
    ) -> Result<HttpResponseOk<String>, HttpError> {
        let ctx = rqctx.context();
        let uuid = path.into_inner().uuid;

        // Validate UUID format
        uuid::Uuid::parse_str(&uuid).map_err(|_| {
            HttpError::for_bad_request(None, format!("Invalid UUID format: {}", uuid))
        })?;

        let new_job_id = ctx
            .retry_job(&uuid)
            .await
            .map_err(|e| HttpError::for_internal_error(format!("Failed to retry job: {}", e)))?;

        tracing::info!(
            original_job_id = %uuid,
            new_job_id = %new_job_id,
            "Job retry initiated"
        );

        Ok(HttpResponseOk(new_job_id))
    }
}

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
                println!("  DATABASE_URL     PostgreSQL connection URL (required)");
                println!("  STORINFO_URL     Storinfo service URL (required)");
                println!(
                    "  CONFIG_FILE      Path to JSON config file for SIGUSR1 reloading (optional)"
                );
                println!(
                    "  RUST_LOG         Log filter (default: rebalancer_manager=info,dropshot=info)"
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
                .unwrap_or_else(|_| "rebalancer_manager=info,dropshot=info".to_string()),
        ))
        .init();

    print_version();

    // Load configuration
    let config = ManagerConfig::from_env().context("Failed to load configuration")?;
    info!("Database URL: {}", config.database_url_display());
    info!("Storinfo URL: {}", config.storinfo_url);

    // Start config file watcher if CONFIG_FILE is set (Unix only)
    #[cfg(unix)]
    if let Ok(config_file) = std::env::var("CONFIG_FILE") {
        use std::path::PathBuf;
        use tokio::sync::watch;

        let config_path = PathBuf::from(&config_file);
        if tokio::fs::try_exists(&config_path).await.unwrap_or(false) {
            let (config_tx, _config_rx) = watch::channel(config.clone());
            tokio::spawn(ManagerConfig::start_config_watcher(
                config_path,
                config.clone(),
                config_tx,
            ));
            info!(
                config_file = %config_file,
                "Config watcher started - send SIGUSR1 to reload"
            );
        } else {
            tracing::warn!(
                config_file = %config_file,
                "CONFIG_FILE specified but file does not exist, config reloading disabled"
            );
        }
    }

    // Create API context
    let api_context = ApiContext::new(config.clone())
        .await
        .context("Failed to create API context")?;

    // Get API description from the trait implementation
    let api = rebalancer_manager_api::rebalancer_manager_api_mod::api_description::<
        RebalancerManagerImpl,
    >()
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
        .to_logger("rebalancer-manager")
        .map_err(|error| anyhow::anyhow!("failed to create logger: {}", error))?;

    // Start the server
    let server = HttpServerStarter::new(&config_dropshot, api, api_context, &log)
        .map_err(|error| anyhow::anyhow!("failed to create server: {}", error))?
        .start();

    info!("Rebalancer manager running on http://{}", bind_address);

    server
        .await
        .map_err(|error| anyhow::anyhow!("server failed: {}", error))
}
