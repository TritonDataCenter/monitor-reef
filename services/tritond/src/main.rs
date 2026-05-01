// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Triton Cloud control plane daemon (binary entry point).
//!
//! Configuration:
//!
//! * `TRITOND_BIND_ADDRESS` — listen address. Defaults to
//!   `127.0.0.1:8080`.
//! * `TRITOND_FDB_CLUSTER_FILE` — path to a FoundationDB cluster file.
//!   Triggers the FDB-backed [`Store`] when the binary is built with
//!   the `foundationdb` feature; an error if set with the feature
//!   disabled.
//!
//! Startup runs [`tritond::bootstrap::ensure`] which mints the JWT
//! signing key and the root operator on first run, then loads them on
//! every subsequent run.

use std::sync::Arc;

use anyhow::{Context, Result};
use tracing::info;
use tritond::auth::AuthService;
use tritond::{ApiContext, DEFAULT_BIND_ADDRESS, VERSION, bootstrap, start_server_with_context};
use tritond_store::{MemStore, Store};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let bind_address =
        std::env::var("TRITOND_BIND_ADDRESS").unwrap_or_else(|_| DEFAULT_BIND_ADDRESS.to_string());

    let store = build_store()?;
    let jwt_key = bootstrap::ensure(store.as_ref())
        .await
        .context("first-run bootstrap")?;
    let auth = Arc::new(AuthService::new(jwt_key).context("build auth service")?);
    let context = ApiContext::new(store, auth);

    info!(version = VERSION, %bind_address, "tritond starting");

    let server = start_server_with_context(&bind_address, context).await?;
    server
        .await
        .map_err(|e| anyhow::anyhow!("HTTP server error: {e}"))?;

    Ok(())
}

#[cfg(feature = "foundationdb")]
fn build_store() -> Result<Arc<dyn Store>> {
    if let Ok(cluster_file) = std::env::var("TRITOND_FDB_CLUSTER_FILE") {
        info!(%cluster_file, "using FoundationDB backend");
        let store = tritond_store::FdbStore::open(Some(&cluster_file))
            .map_err(|e| anyhow::anyhow!("open FDB store: {e}"))?;
        Ok(Arc::new(store))
    } else {
        info!("TRITOND_FDB_CLUSTER_FILE not set; using in-memory store");
        Ok(Arc::new(MemStore::new()))
    }
}

#[cfg(not(feature = "foundationdb"))]
fn build_store() -> Result<Arc<dyn Store>> {
    if std::env::var("TRITOND_FDB_CLUSTER_FILE").is_ok() {
        anyhow::bail!(
            "TRITOND_FDB_CLUSTER_FILE is set but tritond was built without the `foundationdb` feature"
        );
    }
    info!("using in-memory store (binary not built with `foundationdb` feature)");
    Ok(Arc::new(MemStore::new()))
}
