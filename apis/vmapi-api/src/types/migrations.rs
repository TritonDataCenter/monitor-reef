// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Migration-related types for VMAPI
//!
//! Note: VMAPI uses snake_case for JSON field names (internal Triton API convention).

use super::common::{Timestamp, Uuid};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// ============================================================================
// Path Parameters
// ============================================================================

/// Path parameter for migration operations (uses VM UUID)
#[derive(Debug, Deserialize, JsonSchema)]
pub struct MigrationPath {
    /// VM UUID (migrations are identified by the VM they apply to)
    pub uuid: Uuid,
}

// ============================================================================
// Query Parameters
// ============================================================================

/// Query parameters for listing migrations
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListMigrationsQuery {
    /// Filter by state
    #[serde(default)]
    pub state: Option<MigrationState>,
    /// Filter by source server UUID
    #[serde(default)]
    pub source_server_uuid: Option<Uuid>,
    /// Filter by target server UUID
    #[serde(default)]
    pub target_server_uuid: Option<Uuid>,
    /// Pagination offset
    #[serde(default)]
    pub offset: Option<u64>,
    /// Pagination limit
    #[serde(default)]
    pub limit: Option<u64>,
}

// ============================================================================
// Migration Entity Types
// ============================================================================

/// Migration state
#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, clap::ValueEnum,
)]
#[serde(rename_all = "lowercase")]
pub enum MigrationState {
    /// Migration is starting
    Begin,
    /// Estimating migration parameters
    Estimate,
    /// Syncing data
    Sync,
    /// Migration paused
    Paused,
    /// Switching to target
    Switch,
    /// Migration aborted
    Aborted,
    /// Migration rolled back
    #[serde(rename = "rollback")]
    RolledBack,
    /// Migration successful
    Successful,
    /// Migration failed
    Failed,
    /// Running (in progress)
    Running,
    /// Unknown state (forward compatibility)
    #[serde(other)]
    Unknown,
}

/// Migration phase
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MigrationPhase {
    Begin,
    Sync,
    Switch,
}

/// Migration progress entry
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct MigrationProgress {
    /// Progress type (e.g., "progress", "end")
    #[serde(rename = "type")]
    pub progress_type: String,
    /// Progress phase
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<MigrationPhase>,
    /// Current state
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<MigrationState>,
    /// Progress percentage (0-100)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub percentage: Option<f64>,
    /// Bytes transferred
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transferred_bytes: Option<u64>,
    /// Total bytes to transfer
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_bytes: Option<u64>,
    /// ETA in seconds
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eta_ms: Option<u64>,
    /// Message
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Error (if failed)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Duration in milliseconds
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Timestamp
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<Timestamp>,
}

/// Migration record
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct Migration {
    /// VM UUID being migrated
    pub vm_uuid: Uuid,
    /// Current migration state
    pub state: MigrationState,
    /// Current phase
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<MigrationPhase>,
    /// Source server UUID
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_server_uuid: Option<Uuid>,
    /// Target server UUID
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_server_uuid: Option<Uuid>,
    /// Creation timestamp
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_timestamp: Option<Timestamp>,
    /// Started timestamp
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_timestamp: Option<Timestamp>,
    /// Finished timestamp
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_timestamp: Option<Timestamp>,
    /// Progress history
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress_history: Option<Vec<MigrationProgress>>,
    /// Total duration in milliseconds
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Error message if migration failed
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Automatic migration flag
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub automatic: Option<bool>,
}

// ============================================================================
// Internal API Request Types
// ============================================================================

/// Request body for POST /migrations/:uuid/store (internal)
#[derive(Debug, Deserialize, JsonSchema)]
pub struct StoreMigrationRecordRequest {
    /// Migration record to store
    #[serde(flatten)]
    pub migration: serde_json::Value,
}

/// Request body for POST /migrations/:uuid/progress (internal)
#[derive(Debug, Deserialize, JsonSchema)]
pub struct MigrationProgressRequest {
    /// Progress entry
    #[serde(flatten)]
    pub progress: MigrationProgress,
}

/// Request body for POST /migrations/:uuid/updateVmServerUuid (internal)
#[derive(Debug, Deserialize, JsonSchema)]
pub struct UpdateVmServerUuidRequest {
    /// New server UUID
    pub server_uuid: Uuid,
}
