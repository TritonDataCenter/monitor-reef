// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! morayd entry point.
//!
//! Two backends, picked at compile time:
//!  * `--features fdb` → FDB cluster at `$MORAYD_CLUSTER_FILE`
//!    (default `/etc/fdb/fdb.cluster`).
//!  * default → in-memory store (laptop dev / tests).
//!
//! Listen address comes from `$MORAYD_LISTEN` (default `0.0.0.0:2020` — the
//! historical moray port).

use std::sync::Arc;

use morayd::server;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[cfg(feature = "fdb")]
use morayd::store::fdb::FdbStore;

#[cfg(not(feature = "fdb"))]
use morayd::store::mem::MemStore;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("morayd=info,warn")))
        .with_target(false)
        .init();

    let listen = std::env::var("MORAYD_LISTEN").unwrap_or_else(|_| "0.0.0.0:2020".into());

    #[cfg(feature = "fdb")]
    {
        // FDB network thread bootstrap. The triton-fdb wrapper enforces the
        // "exactly once per process" rule and leaks the guard so shutdown
        // does not stall on C-client teardown.
        triton_fdb::boot_and_forget()?;
        let cluster = std::env::var("MORAYD_CLUSTER_FILE")
            .unwrap_or_else(|_| "/etc/fdb/fdb.cluster".into());
        info!(cluster = %cluster, "opening FDB store");
        let store = Arc::new(FdbStore::open(&cluster)?);
        info!(backend = "fdb", "morayd starting");
        server::run(store, listen.as_str()).await?;
    }

    #[cfg(not(feature = "fdb"))]
    {
        info!(backend = "mem", "morayd starting (dev)");
        let store = Arc::new(MemStore::new());
        server::run(store, listen.as_str()).await?;
    }

    Ok(())
}
