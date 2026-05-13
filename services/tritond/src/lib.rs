// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Triton Cloud control plane daemon.
//!
//! Phase 0e ships `/v2/health`, the silo CRUD primitives, and the
//! operator-auth surface (`/v2/auth/login`, `/v2/auth/refresh`,
//! `/v2/auth/api-keys`). The store is pluggable ([`MemStore`] for
//! tests, `FdbStore` in production); the auth service holds the
//! cluster-wide HS256 signing key and the embedded Cedar policy
//! bundle.
//!
//! The library exposes the building blocks (`TritondServiceImpl`,
//! `ApiContext`, `api_description`, `start_server*`,
//! `bootstrap::ensure`) so integration tests can spin up the service
//! in-process; the binary is a thin wrapper around them.

pub mod audit;
pub mod auth;
pub mod bootstrap;
pub mod bootstrap_config;
pub mod dhcp_reconciler;
pub mod edge;
pub mod legacy_classify;
pub mod provisioner;
pub mod rate_limit;
pub mod settings;
pub mod sigv4;
pub mod storage;
pub mod sweeper;

mod blueprint;
mod bundle;
mod cn_credential;
mod console;
mod context;
mod edge_cluster;
mod error;
mod handlers;
mod lifecycle;
mod principal;
mod realized_meta;
mod scope;
mod service_impl;
mod validate;

/// Service version, populated from Cargo at build time.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Default bind address for the Dropshot HTTP server.
pub const DEFAULT_BIND_ADDRESS: &str = "127.0.0.1:8080";

pub use context::{ApiContext, SweeperConfig};
pub use realized_meta::build_instance_realized_view;
pub use service_impl::{
    TritondServiceImpl, api_description, start_server, start_server_with_context,
};
