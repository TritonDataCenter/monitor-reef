// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Explicit "not supported by this backend" handler.
//!
//! A few legacy tasks are deliberately not ported to the Rust agent:
//!
//! * **Docker tasks** (`docker_exec`, `docker_copy`, `docker_stats`,
//!   `docker_build`) — rely on the sdc-docker-stdio runtime helper,
//!   which is ~1500 lines of PTY/stdio/websocket handling we haven't
//!   ported. Docker on Triton is a legacy feature; CNs that need it
//!   should continue to run the Node.js cn-agent.
//!
//! * **Migration tasks** (`machine_migrate`, `machine_migrate_receive`)
//!   — roughly 800 lines of WebSocket state-machine and ZFS send/recv
//!   coordination; porting is a dedicated project. The legacy cn-agent
//!   handles these today.
//!
//! Rather than leaving these TaskName variants unregistered (which
//! would surface as a generic 404 "no handler for task X"), we
//! register this explicit handler that returns a structured error
//! message operators can recognize. The `rest_code` is stable so CNAPI
//! can match on it without string comparison.

use async_trait::async_trait;
use cn_agent_api::{TaskError, TaskResult};

use crate::registry::TaskHandler;

/// Stable error code CNAPI can match on to detect this specific state.
pub const UNSUPPORTED_REST_CODE: &str = "TaskNotSupportedByRustAgent";

/// Handler that explains why a given task is not supported.
#[derive(Debug, Clone)]
pub struct UnsupportedTask {
    /// Canonical task name being rejected. Included in the error
    /// message so CNAPI log lines are self-explanatory.
    pub task_name: &'static str,
    /// Short human-readable reason.
    pub reason: &'static str,
}

impl UnsupportedTask {
    pub const fn new(task_name: &'static str, reason: &'static str) -> Self {
        Self { task_name, reason }
    }
}

#[async_trait]
impl TaskHandler for UnsupportedTask {
    async fn run(&self, _params: serde_json::Value) -> Result<TaskResult, TaskError> {
        let msg = format!(
            "task '{}' is not supported by this Rust cn-agent build: {}",
            self.task_name, self.reason
        );
        tracing::warn!(
            task = self.task_name,
            reason = self.reason,
            "rejecting unsupported task"
        );
        let mut err = TaskError::new(msg);
        err.rest_code = Some(UNSUPPORTED_REST_CODE.to_string());
        Err(err)
    }
}
