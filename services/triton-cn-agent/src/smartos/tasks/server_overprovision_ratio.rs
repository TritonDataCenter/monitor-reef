// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `server_overprovision_ratio` — persist DAPI's chosen overprovision
//! ratio to the on-disk SDC config.
//!
//! Decision rule matches the legacy task:
//! * Headnodes (sysinfo `Boot Parameters.headnode == "true"`) write to
//!   `/usbkey/config`.
//! * Compute nodes write to `/opt/smartdc/config/node.config`.
//!
//! The file is a simple `key='value'` list. We rewrite the target key in
//! place, adding it if missing, matching `modifyConfig()` in the legacy
//! smartos/common.js module.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use cn_agent_api::{TaskError, TaskResult};
use serde::Deserialize;

use crate::registry::TaskHandler;
use crate::smartos::sysinfo::Sysinfo;

pub const DEFAULT_HEADNODE_CONFIG: &str = "/usbkey/config";
pub const DEFAULT_CN_CONFIG: &str = "/opt/smartdc/config/node.config";
pub const OVERPROVISION_KEY: &str = "overprovision_ratio";

#[derive(Debug, Deserialize)]
struct Params {
    value: String,
}

/// Trait abstracting "which sysinfo are we running on right now?". The
/// production implementation runs /usr/bin/sysinfo; tests supply a fixed
/// value.
#[async_trait]
pub trait SysinfoSource: Send + Sync + 'static {
    async fn load(&self) -> Result<Sysinfo, String>;
}

struct LiveSysinfoSource;

#[async_trait]
impl SysinfoSource for LiveSysinfoSource {
    async fn load(&self) -> Result<Sysinfo, String> {
        Sysinfo::collect().await.map_err(|e| e.to_string())
    }
}

pub struct ServerOverprovisionRatioTask {
    sysinfo: Arc<dyn SysinfoSource>,
    headnode_config: PathBuf,
    cn_config: PathBuf,
}

impl ServerOverprovisionRatioTask {
    pub fn new() -> Self {
        Self {
            sysinfo: Arc::new(LiveSysinfoSource),
            headnode_config: PathBuf::from(DEFAULT_HEADNODE_CONFIG),
            cn_config: PathBuf::from(DEFAULT_CN_CONFIG),
        }
    }

    pub fn with_sysinfo(mut self, source: Arc<dyn SysinfoSource>) -> Self {
        self.sysinfo = source;
        self
    }

    pub fn with_paths(mut self, headnode: impl Into<PathBuf>, cn: impl Into<PathBuf>) -> Self {
        self.headnode_config = headnode.into();
        self.cn_config = cn.into();
        self
    }
}

impl Default for ServerOverprovisionRatioTask {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl TaskHandler for ServerOverprovisionRatioTask {
    async fn run(&self, params: serde_json::Value) -> Result<TaskResult, TaskError> {
        let p: Params = serde_json::from_value(params)
            .map_err(|e| TaskError::new(format!("invalid params: {e}")))?;
        if p.value.trim().is_empty() {
            return Err(TaskError::new("no value given".to_string()));
        }

        let sysinfo = self
            .sysinfo
            .load()
            .await
            .map_err(|e| TaskError::new(format!("failed to read sysinfo: {e}")))?;

        let config_path = if is_headnode(&sysinfo) {
            &self.headnode_config
        } else {
            &self.cn_config
        };

        modify_config(config_path, OVERPROVISION_KEY, &p.value)
            .await
            .map_err(|e| TaskError::new(e.to_string()))?;

        Ok(serde_json::json!({}))
    }
}

fn is_headnode(sysinfo: &Sysinfo) -> bool {
    sysinfo
        .raw
        .pointer("/Boot Parameters/headnode")
        .and_then(|v| v.as_str())
        .map(|s| s == "true")
        .unwrap_or(false)
}

/// Rewrite or append a `key='value'` line, preserving the rest of the
/// file byte-for-byte. Mirrors `modifyConfig()` in the legacy
/// smartos/common.js, including its quote style.
pub(crate) async fn modify_config(path: &Path, key: &str, value: &str) -> std::io::Result<()> {
    let existing = match tokio::fs::read_to_string(path).await {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };

    // Walk line-by-line using `lines()` which strips terminators. This
    // keeps the "missing file" case (existing == "") from producing a
    // spurious leading newline that `split('\n')` would introduce.
    let mut out = String::with_capacity(existing.len() + key.len() + value.len() + 4);
    let mut found = false;
    for line in existing.lines() {
        let line_key = line.split_once('=').map(|(k, _)| k).unwrap_or("");
        if line_key == key {
            out.push_str(&format!("{key}='{value}'"));
            found = true;
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    if !found {
        out.push_str(&format!("{key}='{value}'\n"));
    }

    tokio::fs::write(path, out).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn modify_config_replaces_existing_key() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let path = tmp.path().join("node.config");
        tokio::fs::write(&path, "foo='bar'\noverprovision_ratio='1.0'\nbaz='qux'\n")
            .await
            .expect("write initial");
        modify_config(&path, "overprovision_ratio", "1.5")
            .await
            .expect("modify");
        let got = tokio::fs::read_to_string(&path).await.expect("read back");
        assert_eq!(got, "foo='bar'\noverprovision_ratio='1.5'\nbaz='qux'\n");
    }

    #[tokio::test]
    async fn modify_config_appends_missing_key() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let path = tmp.path().join("node.config");
        tokio::fs::write(&path, "foo='bar'\n").await.expect("init");
        modify_config(&path, "overprovision_ratio", "2.0")
            .await
            .expect("modify");
        let got = tokio::fs::read_to_string(&path).await.expect("read");
        assert_eq!(got, "foo='bar'\noverprovision_ratio='2.0'\n");
    }

    #[tokio::test]
    async fn modify_config_creates_missing_file() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let path = tmp.path().join("fresh.config");
        modify_config(&path, "overprovision_ratio", "1.0")
            .await
            .expect("modify");
        let got = tokio::fs::read_to_string(&path).await.expect("read");
        assert_eq!(got, "overprovision_ratio='1.0'\n");
    }

    #[test]
    fn is_headnode_reads_boot_parameters() {
        let sysinfo = Sysinfo {
            raw: serde_json::json!({"Boot Parameters": {"headnode": "true"}}),
        };
        assert!(is_headnode(&sysinfo));
        let sysinfo = Sysinfo {
            raw: serde_json::json!({"Boot Parameters": {"headnode": "false"}}),
        };
        assert!(!is_headnode(&sysinfo));
        let sysinfo = Sysinfo {
            raw: serde_json::json!({}),
        };
        assert!(!is_headnode(&sysinfo));
    }
}
