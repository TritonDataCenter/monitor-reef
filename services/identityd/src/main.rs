// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `identityd`: a minimal native OpenID Connect provider (RFD 00004).
//!
//! Boots with zero config: it seeds a fixed tenant and system realm, a
//! demo user, a role assignment, and the Workbench OAuth client into the
//! store, then serves the realm-scoped discovery / JWKS / token / userinfo
//! endpoints on `127.0.0.1:8090`.
//!
//! # Store backend
//!
//! By default the store is an in-process [`MemStore`] (zero-config dev,
//! state lost on exit). When the binary is built with the `foundationdb`
//! feature *and* a cluster file is configured (the `IDENTITYD_FDB_CLUSTER_FILE`
//! env var, or the standard `/etc/foundationdb/fdb.cluster` resolved by
//! passing `None`), the store is the durable FoundationDB-backed
//! `FdbStore`. The seed is idempotent (see [`bootstrap::seed`]), so a
//! restart against an already-seeded FDB is a no-op.

mod admin;
mod bootstrap;
mod identifiers;
mod keys;
mod server;

use std::sync::Arc;

use anyhow::{Context, Result};
use dropshot::{ConfigDropshot, ConfigLogging, ConfigLoggingLevel, HttpServerStarter};
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

    // Mirror tritond's seam: a configured cluster file + the `foundationdb`
    // feature selects the durable backend; otherwise MemStore.
    let fdb_cluster_file = std::env::var("IDENTITYD_FDB_CLUSTER_FILE")
        .ok()
        .filter(|s| !s.is_empty());
    let store = build_store(fdb_cluster_file.as_deref())?;

    // Seed only when the store is empty (bootstrap::seed probes for the
    // System realm first), so a durable FDB realm set is not re-seeded.
    bootstrap::seed(store.as_ref(), signing.public_jwk.clone())
        .await
        .context("seed identity store")?;

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

/// Select the store backend. With the `foundationdb` feature, a configured
/// cluster file opens the durable `FdbStore`; otherwise (or when no cluster
/// file is configured) the in-memory store. Mirrors tritond's seam.
#[cfg(feature = "foundationdb")]
fn build_store(fdb_cluster_file: Option<&str>) -> Result<Arc<dyn identity_store::IdentityStore>> {
    match fdb_cluster_file {
        Some(cluster_file) => {
            info!(%cluster_file, "using FoundationDB backend (identity store)");
            let store = identity_store::FdbStore::open(Some(cluster_file))
                .map_err(|e| anyhow::anyhow!("open FoundationDB: {e}"))?;
            Ok(Arc::new(store))
        }
        None => {
            info!("no cluster file configured; using in-memory identity store");
            Ok(Arc::new(identity_store::MemStore::new()))
        }
    }
}

/// Without the `foundationdb` feature the binary is MemStore-only. A
/// configured cluster file is a misconfiguration we surface loudly rather
/// than silently dropping durable state on the floor.
#[cfg(not(feature = "foundationdb"))]
fn build_store(fdb_cluster_file: Option<&str>) -> Result<Arc<dyn identity_store::IdentityStore>> {
    if fdb_cluster_file.is_some() {
        anyhow::bail!(
            "IDENTITYD_FDB_CLUSTER_FILE is set but identityd was built without the `foundationdb` feature"
        );
    }
    info!("using in-memory identity store (binary not built with `foundationdb` feature)");
    Ok(Arc::new(identity_store::MemStore::new()))
}
