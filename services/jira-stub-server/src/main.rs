// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Copyright 2025 Edgecast Cloud LLC.

//! Standalone stub JIRA server for testing and development
//!
//! Run with:
//! ```bash
//! cargo run -p jira-stub-server
//! ```
//!
//! Then point bugview-service at it:
//! ```bash
//! JIRA_BASE_URL=http://localhost:9090 cargo run -p bugview-service
//! ```

use anyhow::Result;
use dropshot::{ConfigDropshot, ConfigLogging, ConfigLoggingLevel, HttpServerStarter};
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;

use jira_stub_server::{StubContext, api_description};

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    let log_config = ConfigLogging::StderrTerminal {
        level: ConfigLoggingLevel::Info,
    };
    let log = log_config.to_logger("jira-stub-server")?;

    // Load fixture data
    let fixtures_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    let context = Arc::new(StubContext::from_fixtures(&fixtures_dir)?);

    tracing::info!("Loaded {} issues from fixtures", context.issue_keys().len());

    // Configure the server
    let config = ConfigDropshot {
        bind_address: SocketAddr::from((Ipv4Addr::LOCALHOST, 9090)),
        default_request_body_max_bytes: 1024 * 1024,
        default_handler_task_mode: dropshot::HandlerTaskMode::Detached,
        ..Default::default()
    };

    // Create and start the server
    let api = api_description().map_err(|e| anyhow::anyhow!(e))?;
    let server = HttpServerStarter::new(&config, api, context, &log)
        .map_err(|e| anyhow::anyhow!("Failed to create server: {}", e))?
        .start();

    tracing::info!("Stub JIRA server listening on http://localhost:9090");
    tracing::info!("Available endpoints:");
    tracing::info!("  GET /rest/api/3/search/jql?jql=...");
    tracing::info!("  GET /rest/api/3/issue/{{issueIdOrKey}}");
    tracing::info!("  GET /rest/api/3/issue/{{issueIdOrKey}}/remotelink");

    server
        .await
        .map_err(|e| anyhow::anyhow!("Server error: {}", e))
}
