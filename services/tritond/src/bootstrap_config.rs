// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Bootstrap configuration for `tritond`.
//!
//! This is the *minimum* the daemon needs before it can reach
//! FoundationDB and accept connections. Everything else lives in FDB
//! as cluster-wide [`Settings`](tritond_store::Settings), is read at
//! startup, and is managed with `tcadm config` (or the admin console).
//!
//! The file is optional: an absent file at the default path means
//! "use the built-in defaults". A file requested explicitly (via
//! `--config` or `TRITOND_CONFIG`) that does not exist is an error.
//!
//! Resolution per field, highest priority first:
//!   1. an environment variable (`TRITOND_BIND_ADDRESS`,
//!      `TRITOND_FDB_CLUSTER_FILE`, `RUST_LOG`),
//!   2. the config file,
//!   3. the built-in default.
//!
//! File shape (TOML; every key optional):
//!
//! ```toml
//! bind_address     = "127.0.0.1:8080"
//! fdb_cluster_file = "/etc/fdb.cluster"   # omit to use FDB's own resolution
//! log_filter       = "info"
//! peer_endpoints   = []                   # reserved for the HA controller; unused in v1
//! ```

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::DEFAULT_BIND_ADDRESS;

/// Default on-disk location of the bootstrap config file. Overridable
/// with `--config PATH` or the `TRITOND_CONFIG` environment variable.
pub const DEFAULT_CONFIG_PATH: &str = "/etc/tritond/config.toml";

/// Default `tracing` env-filter directive when neither `RUST_LOG` nor
/// the config file sets one.
pub const DEFAULT_LOG_FILTER: &str = "info";

/// Raw, as-parsed bootstrap config file. Every field is optional so a
/// partial (or absent) file is valid. Unknown keys are rejected so a
/// typo in this small, operator-edited file surfaces immediately.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigFile {
    bind_address: Option<String>,
    fdb_cluster_file: Option<String>,
    log_filter: Option<String>,
    #[serde(default)]
    peer_endpoints: Vec<String>,
}

/// The subset of the process environment that overrides bootstrap
/// config. Split out so [`BootstrapConfig::resolve`] can be unit-tested
/// without touching the real environment.
#[derive(Debug, Default)]
struct EnvOverrides {
    bind_address: Option<String>,
    fdb_cluster_file: Option<String>,
    log_filter: Option<String>,
}

impl EnvOverrides {
    fn from_process() -> Self {
        Self {
            bind_address: non_empty(std::env::var("TRITOND_BIND_ADDRESS").ok()),
            fdb_cluster_file: non_empty(std::env::var("TRITOND_FDB_CLUSTER_FILE").ok()),
            log_filter: non_empty(std::env::var("RUST_LOG").ok()),
        }
    }
}

/// Effective bootstrap configuration after layering env vars over the
/// file over the built-in defaults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootstrapConfig {
    /// HTTP listen address for the API server.
    pub bind_address: String,
    /// Path to the FoundationDB cluster file. `None` lets the FDB
    /// client do its own resolution (`FDB_CLUSTER_FILE`, then
    /// `/etc/foundationdb/fdb.cluster`).
    pub fdb_cluster_file: Option<String>,
    /// `tracing` env-filter directive used to initialise logging.
    pub log_filter: String,
    /// Peer controller endpoints. Reserved for the future HA
    /// controller; empty and unused in v1, kept so the file shape does
    /// not change when HA lands.
    pub peer_endpoints: Vec<String>,
}

impl Default for BootstrapConfig {
    fn default() -> Self {
        Self {
            bind_address: DEFAULT_BIND_ADDRESS.to_string(),
            fdb_cluster_file: None,
            log_filter: DEFAULT_LOG_FILTER.to_string(),
            peer_endpoints: Vec::new(),
        }
    }
}

impl BootstrapConfig {
    /// Load the bootstrap config: parse the file (if any), then apply
    /// env-var overrides. `explicit_path` is the value of a `--config`
    /// flag, if the operator passed one.
    pub fn load(explicit_path: Option<&Path>) -> Result<Self> {
        let env_path = std::env::var_os("TRITOND_CONFIG").map(PathBuf::from);
        let (path, required) = match (explicit_path, env_path) {
            (Some(p), _) => (p.to_path_buf(), true),
            (None, Some(p)) => (p, true),
            (None, None) => (PathBuf::from(DEFAULT_CONFIG_PATH), false),
        };

        let file = match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str::<ConfigFile>(&text)
                .with_context(|| format!("parse bootstrap config {}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && !required => {
                ConfigFile::default()
            }
            Err(e) => {
                return Err(e).with_context(|| format!("read bootstrap config {}", path.display()));
            }
        };

        Ok(Self::resolve(file, EnvOverrides::from_process()))
    }

    fn resolve(file: ConfigFile, env: EnvOverrides) -> Self {
        let defaults = BootstrapConfig::default();
        BootstrapConfig {
            bind_address: env
                .bind_address
                .or(file.bind_address)
                .unwrap_or(defaults.bind_address),
            fdb_cluster_file: env.fdb_cluster_file.or(file.fdb_cluster_file),
            log_filter: env
                .log_filter
                .or(file.log_filter)
                .unwrap_or(defaults.log_filter),
            peer_endpoints: file.peer_endpoints,
        }
    }
}

fn non_empty(v: Option<String>) -> Option<String> {
    v.filter(|s| !s.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_file_no_env_yields_defaults() {
        let got = BootstrapConfig::resolve(ConfigFile::default(), EnvOverrides::default());
        assert_eq!(got, BootstrapConfig::default());
    }

    #[test]
    fn file_overrides_defaults() {
        let file = ConfigFile {
            bind_address: Some("0.0.0.0:9000".to_string()),
            fdb_cluster_file: Some("/etc/fdb.cluster".to_string()),
            log_filter: Some("debug".to_string()),
            peer_endpoints: vec!["http://b:8080".to_string()],
        };
        let got = BootstrapConfig::resolve(file, EnvOverrides::default());
        assert_eq!(got.bind_address, "0.0.0.0:9000");
        assert_eq!(got.fdb_cluster_file.as_deref(), Some("/etc/fdb.cluster"));
        assert_eq!(got.log_filter, "debug");
        assert_eq!(got.peer_endpoints, vec!["http://b:8080".to_string()]);
    }

    #[test]
    fn env_overrides_file() {
        let file = ConfigFile {
            bind_address: Some("0.0.0.0:9000".to_string()),
            fdb_cluster_file: Some("/etc/from-file.cluster".to_string()),
            log_filter: Some("debug".to_string()),
            peer_endpoints: Vec::new(),
        };
        let env = EnvOverrides {
            bind_address: Some("127.0.0.1:7777".to_string()),
            fdb_cluster_file: Some("/etc/from-env.cluster".to_string()),
            log_filter: Some("trace".to_string()),
        };
        let got = BootstrapConfig::resolve(file, env);
        assert_eq!(got.bind_address, "127.0.0.1:7777");
        assert_eq!(
            got.fdb_cluster_file.as_deref(),
            Some("/etc/from-env.cluster")
        );
        assert_eq!(got.log_filter, "trace");
    }

    #[test]
    fn partial_file_parses() {
        let file: ConfigFile = toml::from_str(r#"bind_address = "0.0.0.0:8080""#).unwrap();
        assert_eq!(file.bind_address.as_deref(), Some("0.0.0.0:8080"));
        assert!(file.fdb_cluster_file.is_none());
        assert!(file.peer_endpoints.is_empty());
    }

    #[test]
    fn unknown_key_is_rejected() {
        let err = toml::from_str::<ConfigFile>(r#"bindaddress = "oops""#).expect_err("typo");
        assert!(err.to_string().contains("bindaddress") || err.to_string().contains("unknown"));
    }

    #[test]
    fn missing_explicit_path_errors() {
        let err = BootstrapConfig::load(Some(Path::new("/no/such/tritond/config.toml")))
            .expect_err("explicit missing file must error");
        assert!(err.to_string().contains("read bootstrap config"));
    }
}
