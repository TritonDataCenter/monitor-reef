// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `server_sysinfo` task.
//!
//! Runs `/usr/bin/sysinfo` and returns its JSON output. Mirrors
//! `lib/backends/smartos/tasks/server_sysinfo.js` in the legacy agent:
//! the response body is `{"sysinfo": <raw sysinfo JSON>}`.

use async_trait::async_trait;
use cn_agent_api::{TaskError, TaskResult};

use crate::registry::TaskHandler;
use crate::smartos::sysinfo::{Sysinfo, SysinfoError};

/// Handler for `TaskName::ServerSysinfo`.
///
/// Carries an optional override binary path so tests can point it at a mock
/// script. In production leave [`binary_path`](Self::binary_path) as
/// [`None`] and we call the stock `/usr/bin/sysinfo`.
pub struct ServerSysinfoTask {
    binary_path: Option<String>,
}

impl ServerSysinfoTask {
    pub fn new() -> Self {
        Self { binary_path: None }
    }

    /// Construct a handler that runs a specific binary instead of the
    /// default `/usr/bin/sysinfo`. Used in tests and for running against a
    /// captured sysinfo script.
    pub fn with_binary(path: impl Into<String>) -> Self {
        Self {
            binary_path: Some(path.into()),
        }
    }
}

impl Default for ServerSysinfoTask {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl TaskHandler for ServerSysinfoTask {
    async fn run(&self, _params: serde_json::Value) -> Result<TaskResult, TaskError> {
        let sysinfo = match &self.binary_path {
            Some(path) => Sysinfo::collect_from_path(path).await,
            None => Sysinfo::collect().await,
        }
        .map_err(sysinfo_error_to_task)?;

        Ok(serde_json::json!({ "sysinfo": sysinfo.raw }))
    }
}

fn sysinfo_error_to_task(err: SysinfoError) -> TaskError {
    TaskError::new(format!("sysinfo failed: {err}"))
}
