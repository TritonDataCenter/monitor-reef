// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `server_reboot` — reboot the compute node.
//!
//! The legacy agent detaches a helper script under `ctrun` so the reboot
//! survives cn-agent itself being killed during shutdown. We reproduce
//! that by spawning `/usr/bin/ctrun` with a small inline shell script
//! that runs `shutdown -y -g 0 -i 6`, falling back to a hard `reboot`
//! after 5 minutes if `shutdown` hasn't taken effect. The task returns
//! as soon as the helper is successfully spawned.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use async_trait::async_trait;
use cn_agent_api::{TaskError, TaskResult};

use crate::registry::TaskHandler;

pub const DEFAULT_CTRUN_BIN: &str = "/usr/bin/ctrun";
pub const DEFAULT_SHELL_BIN: &str = "/bin/sh";

/// Commands the detached reboot script runs. Split out so tests can
/// override via `ServerRebootTask::with_shell_command`.
pub const DEFAULT_REBOOT_COMMAND: &str =
    "/usr/sbin/shutdown -y -g 0 -i 6; sleep 300; /usr/sbin/reboot";

pub struct ServerRebootTask {
    ctrun_bin: PathBuf,
    shell_bin: PathBuf,
    shell_command: String,
}

impl ServerRebootTask {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_ctrun(bin: impl Into<PathBuf>) -> Self {
        Self {
            ctrun_bin: bin.into(),
            ..Self::default()
        }
    }

    pub fn with_shell_command(mut self, cmd: impl Into<String>) -> Self {
        self.shell_command = cmd.into();
        self
    }

    pub fn with_shell_bin(mut self, bin: impl Into<PathBuf>) -> Self {
        self.shell_bin = bin.into();
        self
    }
}

impl Default for ServerRebootTask {
    fn default() -> Self {
        Self {
            ctrun_bin: PathBuf::from(DEFAULT_CTRUN_BIN),
            shell_bin: PathBuf::from(DEFAULT_SHELL_BIN),
            shell_command: DEFAULT_REBOOT_COMMAND.to_string(),
        }
    }
}

#[async_trait]
impl TaskHandler for ServerRebootTask {
    async fn run(&self, _params: serde_json::Value) -> Result<TaskResult, TaskError> {
        let pid = spawn_rebooter(&self.ctrun_bin, &self.shell_bin, &self.shell_command)
            .await
            .map_err(|e| TaskError::new(format!("failed to spawn rebooter child: {e}")))?;
        tracing::info!(pid, "reboot helper spawned");
        Ok(serde_json::json!({ "pid": pid }))
    }
}

/// Spawn the reboot helper detached and return its pid. Using `ctrun`
/// puts the child in its own process contract so it survives cn-agent
/// being killed by the shutdown it's about to trigger.
async fn spawn_rebooter(ctrun: &Path, shell: &Path, shell_command: &str) -> std::io::Result<u32> {
    let child = tokio::process::Command::new(ctrun)
        .arg(shell)
        .arg("-c")
        .arg(shell_command)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let pid = child.id().unwrap_or(0);
    // Deliberately drop the Child without awaiting — we want the rebooter
    // to outlive cn-agent. We log the pid for operator diagnosis.
    std::mem::forget(child);
    Ok(pid)
}
