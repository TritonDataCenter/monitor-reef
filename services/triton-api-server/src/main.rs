// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

use anyhow::{Context, Result};
use dropshot::{
    ConfigDropshot, ConfigLogging, ConfigLoggingLevel, HttpError, HttpResponseOk,
    HttpServerStarter, RequestContext,
};
use serde::Deserialize;
use tracing::{error, info};
use triton_api::{PingResponse, TritonApi};

/// Default request body size limit: 10 MiB.
///
/// 1 MiB was too small for CloudAPI-compat use cases (user-scripts, machine
/// metadata, image manifests). This is a reasonable starting point; it can be
/// overridden per-deployment via the `max_body_bytes` field in the SAPI config.
const DEFAULT_MAX_BODY_BYTES: u64 = 10 * 1024 * 1024;

#[derive(Deserialize)]
#[allow(dead_code)]
struct ApiServerConfig {
    #[serde(default)]
    datacenter_name: Option<String>,
    #[serde(default)]
    instance_uuid: Option<String>,
    #[serde(default)]
    server_uuid: Option<String>,
    #[serde(default)]
    admin_ip: Option<String>,
    #[serde(default = "default_bind_address")]
    bind_address: String,
    #[serde(default)]
    max_body_bytes: Option<u64>,
}

fn default_bind_address() -> String {
    "127.0.0.1:8080".to_string()
}

impl Default for ApiServerConfig {
    fn default() -> Self {
        Self {
            datacenter_name: None,
            instance_uuid: None,
            server_uuid: None,
            admin_ip: None,
            bind_address: default_bind_address(),
            max_body_bytes: None,
        }
    }
}

/// Load config from TRITON__CONFIG_FILE env var.
///
/// If the env var is unset, returns defaults (useful for dev).
/// If the env var is set but the file cannot be read or parsed, returns
/// an error so the process exits non-zero -- SMF will mark the service in
/// maintenance and an operator will notice.
fn load_config() -> Result<ApiServerConfig> {
    let Some(path) = std::env::var("TRITON__CONFIG_FILE").ok() else {
        info!("TRITON__CONFIG_FILE not set; using default config");
        return Ok(ApiServerConfig::default());
    };

    let contents = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read config from {}", path))?;
    let config: ApiServerConfig = serde_json::from_str(&contents)
        .with_context(|| format!("failed to parse config from {}", path))?;
    info!("loaded config from {}", path);
    Ok(config)
}

struct ApiContext {}

enum TritonApiImpl {}

impl TritonApi for TritonApiImpl {
    type Context = ApiContext;

    async fn ping(
        _rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseOk<PingResponse>, HttpError> {
        Ok(HttpResponseOk(PingResponse {
            status: "OK".to_string(),
            healthy: Some(true),
        }))
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(
            "triton_api_server=info,dropshot=info",
        ))
        .init();

    let config = load_config()?;

    let api = triton_api::triton_api_mod::api_description::<TritonApiImpl>()
        .map_err(|e| anyhow::anyhow!("Failed to create API description: {}", e))?;

    let max_body_bytes_u64 = config.max_body_bytes.unwrap_or(DEFAULT_MAX_BODY_BYTES);
    let max_body_bytes: usize = usize::try_from(max_body_bytes_u64).with_context(|| {
        format!(
            "max_body_bytes {} does not fit in usize on this platform",
            max_body_bytes_u64
        )
    })?;
    info!("request body size limit: {} bytes", max_body_bytes);

    let config_dropshot = ConfigDropshot {
        bind_address: config.bind_address.parse()?,
        default_request_body_max_bytes: max_body_bytes,
        default_handler_task_mode: dropshot::HandlerTaskMode::Detached,
        ..Default::default()
    };

    let config_logging = ConfigLogging::StderrTerminal {
        level: ConfigLoggingLevel::Info,
    };

    let log = config_logging
        .to_logger("triton-api-server")
        .map_err(|error| anyhow::anyhow!("failed to create logger: {}", error))?;

    let server = HttpServerStarter::new(&config_dropshot, api, ApiContext {}, &log)
        .map_err(|error| anyhow::anyhow!("failed to create server: {}", error))?
        .start();

    info!(
        "triton-api-server listening on http://{}",
        config.bind_address
    );

    // Graceful shutdown: race `wait_for_shutdown()` (which does NOT trigger
    // shutdown, only observes it) against SIGTERM/SIGINT. On signal, call
    // `HttpServer::close()` which triggers Dropshot's built-in graceful
    // shutdown (stops accepting new connections and waits for in-flight
    // handlers to complete, since handlers run in `Detached` mode). SMF's
    // `timeout_seconds` on the stop method is the hard backstop if draining
    // takes too long.
    tokio::select! {
        result = server.wait_for_shutdown() => {
            // Server exited on its own (unusual -- normally runs until close()
            // is called).
            return result.map_err(|error| anyhow::anyhow!("server failed: {}", error));
        }
        () = shutdown_signal() => {
            // Fall through to the close path below.
        }
    }

    server
        .close()
        .await
        .map_err(|error| anyhow::anyhow!("graceful shutdown failed: {}", error))
}

/// Await either SIGTERM or SIGINT (Ctrl-C), whichever arrives first.
///
/// SMF's `stop` method sends SIGTERM; Ctrl-C in a dev shell sends SIGINT.
/// When the signal arrives we log and return, letting the caller trigger
/// graceful shutdown. SMF's `timeout_seconds` on the stop method is the
/// hard backstop if draining takes too long.
async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    let mut sigterm = match signal(SignalKind::terminate()) {
        Ok(s) => s,
        Err(e) => {
            error!("failed to install SIGTERM handler: {}", e);
            return;
        }
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {},
        _ = sigterm.recv() => {},
    }
    info!("shutdown signal received, draining in-flight requests");
}
