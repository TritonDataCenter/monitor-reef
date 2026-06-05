// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Shared command-line plumbing for the two Triton Cloud CLIs:
//! `tritonadm` (operator plane) and `tritonctl` (tenant plane).
//!
//! Both binaries differ only in their [`App`] descriptor (config
//! subdirectory + environment-variable prefix) and which API surface
//! they target. Everything else — on-disk config, per-command session
//! and credential resolution, the SmartOS-safe HTTPS client, and the
//! output-format contract — lives here so the two cannot drift.

mod config;
mod http;
mod output;
mod session;

pub use config::{Config, Tokens};
pub use http::build_http_client;
pub use output::{OutputFormat, Table, emit};
pub use session::{Session, login};

/// Per-binary identity. Drives the config path
/// (`~/.config/triton/<name>/config.json`) and the environment-variable
/// prefix (`<ENV_PREFIX>_ENDPOINT`, `_API_KEY`, `_ACCESS_TOKEN`,
/// `_CONFIG_DIR`).
///
/// Keeping the two CLIs' on-disk and environment namespaces separate is
/// deliberate: their auth planes differ, and `TRITON_*` is already
/// owned by the legacy node-triton CLI.
#[derive(Debug, Clone, Copy)]
pub struct App {
    /// Binary name; also the config subdirectory under `triton/`.
    pub name: &'static str,
    /// Uppercase environment-variable prefix (no trailing underscore).
    pub env_prefix: &'static str,
}

impl App {
    pub const fn new(name: &'static str, env_prefix: &'static str) -> Self {
        Self { name, env_prefix }
    }

    /// `<PREFIX>_<SUFFIX>`, e.g. `TRITONCTL_ENDPOINT`. Useful for
    /// binaries that resolve their own env vars (e.g. in `configure`).
    pub fn env(&self, suffix: &str) -> String {
        format!("{}_{}", self.env_prefix, suffix)
    }
}
