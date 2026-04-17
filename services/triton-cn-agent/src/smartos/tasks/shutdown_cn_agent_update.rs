// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `shutdown_cn_agent_update` — run `svcadm disable cn-agent-update` so
//! the helper SMF service that cn-agent uses to self-update stops. Called
//! by the agent self-update flow once the main agent is ready to take
//! over again.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use cn_agent_api::{TaskError, TaskResult};

use crate::registry::TaskHandler;

pub const DEFAULT_SVCADM_BIN: &str = "/usr/sbin/svcadm";
pub const CN_AGENT_UPDATE_FMRI: &str = "cn-agent-update";

pub struct ShutdownCnAgentUpdateTask {
    svcadm_bin: PathBuf,
    fmri: String,
}

impl ShutdownCnAgentUpdateTask {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_svcadm(bin: impl Into<PathBuf>) -> Self {
        Self {
            svcadm_bin: bin.into(),
            fmri: CN_AGENT_UPDATE_FMRI.to_string(),
        }
    }

    pub fn with_fmri(mut self, fmri: impl Into<String>) -> Self {
        self.fmri = fmri.into();
        self
    }
}

impl Default for ShutdownCnAgentUpdateTask {
    fn default() -> Self {
        Self {
            svcadm_bin: PathBuf::from(DEFAULT_SVCADM_BIN),
            fmri: CN_AGENT_UPDATE_FMRI.to_string(),
        }
    }
}

#[async_trait]
impl TaskHandler for ShutdownCnAgentUpdateTask {
    async fn run(&self, _params: serde_json::Value) -> Result<TaskResult, TaskError> {
        let output = spawn_svcadm(&self.svcadm_bin, &self.fmri)
            .await
            .map_err(|e| TaskError::new(format!("Disable cn-agent-update error: {e}")))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            return Err(TaskError::new(format!(
                "Disable cn-agent-update error: svcadm exited with {}: {}",
                output.status, stderr
            )));
        }
        Ok(serde_json::json!({}))
    }
}

async fn spawn_svcadm(bin: &Path, fmri: &str) -> std::io::Result<std::process::Output> {
    tokio::process::Command::new(bin)
        .args(["disable", fmri])
        .output()
        .await
}
