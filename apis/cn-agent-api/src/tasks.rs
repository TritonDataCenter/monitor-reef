// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Task dispatch types.
//!
//! The original Node.js cn-agent exposed a single `POST /tasks` endpoint that
//! dispatched on a task name in the request body. CNAPI is the primary caller,
//! and it sends payloads that look like:
//!
//! ```json
//! {
//!   "task": "machine_load",
//!   "params": { "uuid": "..." }
//! }
//! ```
//!
//! To preserve wire-compat with CNAPI, we keep that same shape. Each task has
//! its own strongly-typed request struct (defined alongside its implementation
//! in the service crate); here we only carry the untyped `params` value so the
//! dispatcher can deserialize it based on the selected task.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::types::Uuid;

/// Canonical set of tasks the SmartOS backend of cn-agent can dispatch.
///
/// Enum variants are serialized as the exact string names used in the legacy
/// Node.js agent (see `lib/backends/smartos/index.js:queueDefns`). A catch-all
/// `Unknown` variant lets newer CNAPIs send tasks an older agent does not yet
/// understand without blowing up deserialization.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    JsonSchema,
    strum::Display,
    strum::EnumString,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum TaskName {
    // Machine lifecycle
    MachineCreate,
    MachineReprovision,
    MachineDestroy,
    MachineBoot,
    MachineShutdown,
    MachineReboot,
    MachineKill,
    MachineUpdate,
    MachineUpdateNics,
    MachineScreenshot,
    MachineCreateSnapshot,
    MachineDeleteSnapshot,
    MachineRollbackSnapshot,
    MachineCreateImage,
    MachineMigrate,
    MachineMigrateReceive,

    // Machine query
    MachineLoad,
    MachineInfo,
    MachineProc,

    // Images
    ImageEnsurePresent,
    ImageGet,

    // Server
    ServerSysinfo,
    ServerReboot,
    ServerUpdateNics,
    ServerOverprovisionRatio,
    CommandExecute,
    RecoveryConfig,

    // Agents
    AgentInstall,
    AgentsUninstall,
    RefreshAgents,
    ShutdownCnAgentUpdate,

    // Docker
    DockerBuild,
    DockerCopy,
    DockerExec,
    DockerStats,

    // ZFS mutations
    ZfsCreateDataset,
    ZfsDestroyDataset,
    ZfsRenameDataset,
    ZfsSnapshotDataset,
    ZfsRollbackDataset,
    ZfsCloneDataset,
    ZfsSetProperties,

    // ZFS queries
    ZfsGetProperties,
    ZfsListDatasets,
    ZfsListSnapshots,
    ZfsListPools,

    // Test / diagnostic
    Nop,
    Sleep,
    TestSubtask,

    /// Catch-all for tasks this agent version does not understand.
    /// Dispatch will return an error explaining the task is unsupported.
    #[serde(other)]
    Unknown,
}

/// Request body for `POST /tasks`.
///
/// Matches the wire shape CNAPI sends today. `params` is deliberately untyped
/// here; the service deserializes it into a task-specific struct after
/// selecting a handler based on `task`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TaskRequest {
    /// The task to execute.
    pub task: TaskName,
    /// Arbitrary task-specific parameters.
    #[serde(default)]
    pub params: serde_json::Value,
}

/// Sleep task parameters (used for testing and integration diagnostics).
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct SleepParams {
    /// Seconds to sleep before finishing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sleep: Option<u64>,
    /// If set, fail the task with this error message instead of succeeding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Params for any task that identifies a machine by UUID.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MachineUuidParams {
    pub uuid: Uuid,
    /// Include VMs marked `do_not_inventory` (machine_load-specific flag,
    /// tolerated on other endpoints for wire compat).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_dni: Option<bool>,
}

/// Response from `POST /tasks`. Mirrors the legacy semantics:
///
/// * Task succeeded → HTTP 200 with the raw JSON the task emitted via its
///   `finish` event.
/// * Task failed → HTTP 500 with a `TaskError` body.
///
/// The Dropshot trait declares the success type as [`TaskResult`]; the error
/// path goes through `HttpError` so handlers can attach status codes.
pub type TaskResult = serde_json::Value;

/// Structured error payload for a failed task.
///
/// Legacy cn-agent returns task failures as arbitrary JSON objects (often with
/// `error` or `message` fields). We preserve that by allowing arbitrary extra
/// fields, while pinning the `error` key so callers can rely on it.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TaskError {
    /// Human-readable error message.
    pub error: String,
    /// Optional restify-style error code, preserved from the legacy agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rest_code: Option<String>,
}

impl TaskError {
    pub fn new(msg: impl Into<String>) -> Self {
        Self {
            error: msg.into(),
            rest_code: None,
        }
    }
}
