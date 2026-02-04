// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Job-related types for VMAPI
//!
//! Note: VMAPI uses snake_case for JSON field names (internal Triton API convention).

use super::common::{Timestamp, Uuid};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ============================================================================
// Path Parameters
// ============================================================================

/// Path parameter for job operations
#[derive(Debug, Deserialize, JsonSchema)]
pub struct JobPath {
    /// Job UUID
    pub job_uuid: Uuid,
}

/// Path parameter for VM jobs
#[derive(Debug, Deserialize, JsonSchema)]
pub struct VmJobsPath {
    /// VM UUID
    pub uuid: Uuid,
}

// ============================================================================
// Query Parameters
// ============================================================================

/// Query parameters for listing jobs
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListJobsQuery {
    /// Filter by VM UUID
    #[serde(default)]
    pub vm_uuid: Option<Uuid>,
    /// Filter by execution state (e.g., "succeeded", "failed", "running")
    #[serde(default)]
    pub execution: Option<String>,
    /// Filter by task (job type)
    #[serde(default)]
    pub task: Option<String>,
    /// Pagination offset
    #[serde(default)]
    pub offset: Option<u64>,
    /// Pagination limit
    #[serde(default)]
    pub limit: Option<u64>,
}

// ============================================================================
// Job Entity Types
// ============================================================================

/// Job execution state
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum JobExecution {
    Queued,
    Running,
    Succeeded,
    Failed,
    Canceled,
}

/// Task chain entry in a job
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct TaskChainEntry {
    /// Task name
    pub name: String,
    /// Task body (function name)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// Timeout in seconds
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u64>,
    /// Retry count
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<u64>,
}

/// Result of a task in the job
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct TaskResult {
    /// Result value
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    /// Error message if task failed
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Task name
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Start time
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<Timestamp>,
    /// Finish time
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<Timestamp>,
}

/// Job object returned by VMAPI
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct Job {
    /// Job UUID
    pub uuid: Uuid,
    /// Job name/task type (e.g., "provision", "start", "reboot")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Job execution state
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<JobExecution>,
    /// VM UUID this job operates on
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vm_uuid: Option<Uuid>,
    /// Parameters passed to the job
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<HashMap<String, serde_json::Value>>,
    /// Task chain (ordered list of tasks)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chain: Option<Vec<TaskChainEntry>>,
    /// Task results
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chain_results: Option<Vec<TaskResult>>,
    /// Onerror chain (tasks to run on failure)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub onerror: Option<Vec<TaskChainEntry>>,
    /// Onerror results
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub onerror_results: Option<Vec<TaskResult>>,
    /// Creation timestamp
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<Timestamp>,
    /// Start timestamp (when execution began)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started: Option<Timestamp>,
    /// Completion timestamp
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elapsed: Option<f64>,
    /// Timeout in seconds
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u64>,
    /// Number of tasks completed
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub num_tasks_done: Option<u32>,
}

/// Request body for POST /job_results (workflow callback)
#[derive(Debug, Deserialize, JsonSchema)]
pub struct PostJobResultsRequest {
    /// Job UUID
    pub job_uuid: Uuid,
    /// Job execution state
    #[serde(default)]
    pub execution: Option<JobExecution>,
    /// Error info if failed
    #[serde(default)]
    pub info: Option<serde_json::Value>,
}
