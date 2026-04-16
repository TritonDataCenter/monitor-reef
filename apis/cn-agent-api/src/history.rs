// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Task history endpoint types.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::tasks::TaskName;

/// A single entry in the task history ring buffer.
///
/// The legacy agent keeps the 16 most-recent tasks in memory. We preserve that
/// behavior so operators relying on `curl /history` keep the same shape.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TaskHistoryEntry {
    /// ISO-8601 timestamp of when the task started.
    pub started_at: String,
    /// ISO-8601 timestamp of when the task finished (absent if still running).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    /// The task that ran.
    pub task: TaskName,
    /// Raw params the task was invoked with (may be redacted for `docker_build`).
    pub params: serde_json::Value,
    /// Current status of the task (active, finished, failed).
    pub status: TaskStatus,
    /// Number of error events emitted by the task (>0 implies failure).
    #[serde(default)]
    pub error_count: u32,
}

/// Lifecycle state of a task.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    JsonSchema,
    strum::Display,
    strum::EnumString,
)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum TaskStatus {
    /// Task is currently running.
    Active,
    /// Task finished successfully.
    Finished,
    /// Task finished with an error.
    Failed,
}

/// Response from `GET /history`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TaskHistoryResponse {
    pub entries: Vec<TaskHistoryEntry>,
}
