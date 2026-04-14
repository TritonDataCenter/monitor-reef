// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

use anyhow::Result;
use dropshot::{
    ConfigDropshot, ConfigLogging, ConfigLoggingLevel, HttpError, HttpResponseOk,
    HttpServerStarter, RequestContext,
};
use serde::Deserialize;
use tracing::{error, info};
use triton_api::{PingResponse, TritonApi};

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
        }
    }
}

/// Load config from TRITON__CONFIG_FILE env var, falling back to defaults.
fn load_config() -> ApiServerConfig {
    let path = std::env::var("TRITON__CONFIG_FILE").ok();
    if let Some(ref path) = path {
        match std::fs::read_to_string(path) {
            Ok(contents) => match serde_json::from_str(&contents) {
                Ok(config) => {
                    info!("loaded config from {}", path);
                    return config;
                }
                Err(e) => {
                    error!("failed to parse config from {}: {}", path, e);
                }
            },
            Err(e) => {
                error!("failed to read config from {}: {}", path, e);
            }
        }
    }
    info!("using default config");
    ApiServerConfig::default()
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

    let config = load_config();

    let api = triton_api::triton_api_mod::api_description::<TritonApiImpl>()
        .map_err(|e| anyhow::anyhow!("Failed to create API description: {}", e))?;

    let config_dropshot = ConfigDropshot {
        bind_address: config.bind_address.parse()?,
        default_request_body_max_bytes: 1024 * 1024,
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

    server
        .await
        .map_err(|error| anyhow::anyhow!("server failed: {}", error))
}
