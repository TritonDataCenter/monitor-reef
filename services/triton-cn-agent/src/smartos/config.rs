// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! SDC and agent config loaders.
//!
//! The legacy agent reads three sources on startup:
//! * `/lib/sdc/config.sh -json` → datacenter-level config (DNS, DC name, etc.)
//! * `/opt/smartdc/agents/etc/cn-agent.config.json` → agent-specific config
//! * `/usr/bin/sysinfo` → handled in [`super::sysinfo`]
//!
//! Each file is optional in tests; the loaders accept `&Path` overrides so
//! integration tests can feed in fixtures.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const DEFAULT_AGENT_CONFIG_PATH: &str = "/opt/smartdc/agents/etc/cn-agent.config.json";
pub const DEFAULT_SDC_CONFIG_SCRIPT: &str = "/lib/sdc/config.sh";

/// cn-agent-specific configuration (`cn-agent.config.json`).
///
/// Only the handful of fields cn-agent itself reads are typed; the rest of
/// the file survives in `extras` so operators can add feature flags without
/// us having to redeploy.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentConfig {
    /// Present + true → start the agent. Mirrors legacy behavior: historically
    /// cn-agent checked for this flag to avoid fighting an older rabbitmq-based
    /// service. All modern deployments set it to `true`.
    #[serde(default)]
    pub no_rabbit: bool,

    /// Optional CNAPI override. Historically most deployments let cn-agent
    /// discover CNAPI via DNS (`cnapi.<dc>.<dns_domain>`), but the config may
    /// pin a specific URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cnapi: Option<CnapiConfig>,

    /// Optional path cn-agent will write task logs to. Defaults to
    /// `/var/log/cn-agent/logs`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tasklogdir: Option<PathBuf>,

    /// Preserve any additional fields so future config options don't require
    /// a code change to read.
    #[serde(flatten)]
    pub extras: serde_json::Map<String, serde_json::Value>,
}

/// CNAPI connection config nested under `agentConfig.cnapi`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CnapiConfig {
    pub url: String,
}

/// Datacenter-level config produced by `/lib/sdc/config.sh -json`.
///
/// The real script emits dozens of fields; cn-agent only requires
/// `datacenter_name` and `dns_domain` to build the CNAPI address. Everything
/// else is preserved as JSON for future use.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SdcConfig {
    pub datacenter_name: String,
    pub dns_domain: String,
    #[serde(flatten)]
    pub extras: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to read {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("failed to spawn {path}: {source}")]
    Spawn {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{path} exited with status {status}: {stderr}")]
    NonZeroExit {
        path: PathBuf,
        status: String,
        stderr: String,
    },
}

impl AgentConfig {
    /// Load from the default path.
    pub async fn load() -> Result<Self, ConfigError> {
        Self::load_from(DEFAULT_AGENT_CONFIG_PATH).await
    }

    pub async fn load_from(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        let bytes = tokio::fs::read(path)
            .await
            .map_err(|source| ConfigError::Read {
                path: path.to_path_buf(),
                source,
            })?;
        serde_json::from_slice(&bytes).map_err(|source| ConfigError::Parse {
            path: path.to_path_buf(),
            source,
        })
    }
}

impl SdcConfig {
    /// Load from the default `/lib/sdc/config.sh -json` script.
    pub async fn load() -> Result<Self, ConfigError> {
        Self::load_from_script("/bin/bash", &["/lib/sdc/config.sh", "-json"]).await
    }

    /// Run the given command, parse stdout as JSON.
    ///
    /// `/lib/sdc/config.sh -json` is a shell script that reads
    /// `/usbkey/config` (on the headnode) or `/var/tmp/node.config/node.config`
    /// (on a compute node) and prints the union as JSON. The Rust service
    /// shells out the same way so we inherit whatever logic the script holds.
    pub async fn load_from_script(
        program: impl AsRef<Path>,
        args: &[&str],
    ) -> Result<Self, ConfigError> {
        let program = program.as_ref();
        let output = tokio::process::Command::new(program)
            .args(args)
            .output()
            .await
            .map_err(|source| ConfigError::Spawn {
                path: program.to_path_buf(),
                source,
            })?;

        if !output.status.success() {
            return Err(ConfigError::NonZeroExit {
                path: program.to_path_buf(),
                status: output.status.to_string(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }

        serde_json::from_slice(&output.stdout).map_err(|source| ConfigError::Parse {
            path: program.to_path_buf(),
            source,
        })
    }

    /// Parse pre-captured JSON (primarily for tests).
    pub fn from_json(bytes: &[u8]) -> Result<Self, ConfigError> {
        serde_json::from_slice(bytes).map_err(|source| ConfigError::Parse {
            path: PathBuf::from("<in-memory>"),
            source,
        })
    }

    /// Hostname cn-agent should send CNAPI requests to when no explicit URL
    /// is configured.
    pub fn cnapi_dns_name(&self) -> String {
        format!("cnapi.{}.{}", self.datacenter_name, self.dns_domain)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn agent_config_loads_and_preserves_extras() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let path = tmp.path().join("cn-agent.config.json");
        std::fs::write(
            &path,
            br#"{"no_rabbit": true, "cnapi": {"url": "http://cnapi.test"}, "fluentd_host": "logs"}"#,
        )
        .expect("write config");

        let cfg = AgentConfig::load_from(&path).await.expect("load");
        assert!(cfg.no_rabbit);
        assert_eq!(
            cfg.cnapi.as_ref().map(|c| c.url.as_str()),
            Some("http://cnapi.test")
        );
        assert_eq!(
            cfg.extras.get("fluentd_host").and_then(|v| v.as_str()),
            Some("logs")
        );
    }

    #[test]
    fn sdc_config_builds_cnapi_dns_name() {
        let cfg = SdcConfig::from_json(
            br#"{"datacenter_name":"us-east-1","dns_domain":"example.com","admin_ip":"10.0.0.1"}"#,
        )
        .expect("parse");
        assert_eq!(cfg.datacenter_name, "us-east-1");
        assert_eq!(cfg.cnapi_dns_name(), "cnapi.us-east-1.example.com");
        assert_eq!(
            cfg.extras.get("admin_ip").and_then(|v| v.as_str()),
            Some("10.0.0.1")
        );
    }
}
