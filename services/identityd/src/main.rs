// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `identityd`: a minimal native OpenID Connect provider (RFD 00004).
//!
//! Boots with zero config and no FoundationDB: it seeds a fixed tenant
//! and system realm, a demo user, a role assignment, and the Workbench
//! OAuth client into an in-memory store, then serves the realm-scoped
//! discovery / JWKS / token / userinfo endpoints on `127.0.0.1:8090`.

mod bootstrap;
mod identifiers;
mod keys;
mod server;

use std::sync::Arc;

use anyhow::{Context, Result};
use dropshot::{
    ConfigDropshot, ConfigLogging, ConfigLoggingLevel, HttpServerStarter,
};
use identity_store::MemStore;
use tracing::info;

use crate::server::{Ctx, IdentitydImpl};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "identityd=info,dropshot=info".to_string()),
        ))
        .init();

    let signing = keys::load().context("load dev signing key")?;

    let store = MemStore::new();
    bootstrap::seed(&store, signing.public_jwk.clone())
        .await
        .context("seed identity store")?;
    let store: Arc<dyn identity_store::IdentityStore> = Arc::new(store);

    let ctx = Arc::new(Ctx { store, signing });

    let api = identityd_api::identityd_api_mod::api_description::<IdentitydImpl>()
        .map_err(|e| anyhow::anyhow!("build API description: {e}"))?;

    let bind_address = identifiers::BIND_ADDRESS
        .parse()
        .context("parse bind address")?;
    let config_dropshot = ConfigDropshot {
        bind_address,
        default_request_body_max_bytes: 1024 * 1024,
        default_handler_task_mode: dropshot::HandlerTaskMode::Detached,
        ..Default::default()
    };

    let log = ConfigLogging::StderrTerminal {
        level: ConfigLoggingLevel::Info,
    }
    .to_logger("identityd")
    .map_err(|e| anyhow::anyhow!("build logger: {e}"))?;

    let server = HttpServerStarter::new(&config_dropshot, api, ctx, &log)
        .map_err(|e| anyhow::anyhow!("start server: {e}"))?
        .start();

    info!("identityd listening on http://{bind_address}");
    server
        .await
        .map_err(|e| anyhow::anyhow!("server error: {e}"))
}
