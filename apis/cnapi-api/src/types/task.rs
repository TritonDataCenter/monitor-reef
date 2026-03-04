// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Path parameter for task endpoints
#[derive(Debug, Deserialize, JsonSchema)]
pub struct TaskPath {
    pub taskid: String,
}

/// Query parameters for GET /tasks/:taskid/wait
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct TaskWaitParams {
    /// Timeout in seconds
    #[serde(default)]
    pub timeout: Option<u64>,
}

/// Task status
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum TaskStatus {
    Active,
    Complete,
    Failure,
    /// Forward-compatible catch-all
    #[serde(other)]
    Unknown,
}

/// Task object as stored in Moray
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Task {
    pub id: String,
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub status: Option<TaskStatus>,
    #[serde(default)]
    pub progress: Option<serde_json::Value>,
    #[serde(default)]
    pub history: Option<Vec<TaskHistory>>,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub finished_at: Option<String>,
    #[serde(default)]
    pub server_uuid: Option<String>,
    #[serde(flatten)]
    pub extra: Option<serde_json::Value>,
}

/// Task history entry
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TaskHistory {
    #[serde(default)]
    pub event: Option<String>,
    #[serde(default)]
    pub timestamp: Option<String>,
}
