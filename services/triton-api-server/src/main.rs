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
use tracing::info;
use triton_api::{PingResponse, TritonApi};

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

    let api = triton_api::triton_api_mod::api_description::<TritonApiImpl>()
        .map_err(|e| anyhow::anyhow!("Failed to create API description: {}", e))?;

    // TODO: read bind address from SAPI-generated config file
    // (/opt/triton/triton-api/etc/config.json)
    let config_dropshot = ConfigDropshot {
        bind_address: "127.0.0.1:8080".parse()?,
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

    info!("triton-api-server listening on http://127.0.0.1:8080");

    server
        .await
        .map_err(|error| anyhow::anyhow!("server failed: {}", error))
}
