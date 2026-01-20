// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2020 Joyent, Inc.
// Copyright 2026 Edgecast Cloud LLC.

//! Shared types for the rebalancer agent and manager services.
//!
//! This crate contains the common data structures used by both the rebalancer
//! agent (which downloads objects to storage nodes) and the rebalancer manager
//! (which coordinates evacuation jobs across agents).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use strum::{Display, EnumCount, EnumIter, EnumString, VariantNames};

// ============================================================================
// Type Aliases
// ============================================================================

/// HTTP status code type
pub type HttpStatusCode = u16;

/// Object identifier (UUID string)
pub type ObjectId = String;

/// Storage node identifier (hostname)
pub type StorageId = String;

/// Assignment identifier (UUID string)
pub type AssignmentId = String;

// ============================================================================
// Agent Types
// ============================================================================

/// A reference to a storage node (shark) in Manta.
///
/// This identifies where an object is physically stored. Note: This mirrors
/// the MantaObjectShark from libmanta but adds JsonSchema support for API use.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct StorageNode {
    /// The datacenter name where this storage node is located
    pub datacenter: String,
    /// The storage node identifier (e.g., "1.stor.domain.com")
    pub manta_storage_id: String,
}

/// Payload for creating a new assignment on an agent.
///
/// An assignment is a batch of object download tasks sent to an agent.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AssignmentPayload {
    /// Unique identifier for this assignment
    pub id: String,
    /// List of tasks (objects to download) in this assignment
    pub tasks: Vec<Task>,
}

impl From<AssignmentPayload> for (String, Vec<Task>) {
    fn from(p: AssignmentPayload) -> (String, Vec<Task>) {
        let AssignmentPayload { id, tasks } = p;
        (id, tasks)
    }
}

/// A single task within an assignment.
///
/// Each task represents one object that needs to be downloaded from a source
/// storage node to the local storage node.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct Task {
    /// The object's unique identifier (UUID)
    pub object_id: String,
    /// The owner account identifier (UUID)
    pub owner: String,
    /// MD5 checksum of the object (base64 encoded)
    pub md5sum: String,
    /// Source storage node to download from
    pub source: StorageNode,
    /// Current status of this task
    #[serde(default)]
    pub status: TaskStatus,
}

impl Task {
    /// Update the status of this task
    pub fn set_status(&mut self, status: TaskStatus) {
        self.status = status;
    }
}

/// Status of a task within an assignment.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, EnumCount)]
#[serde(tag = "state", content = "reason")]
pub enum TaskStatus {
    /// Task is waiting to be processed
    #[default]
    Pending,
    /// Task completed successfully
    Complete,
    /// Task failed with the given reason
    Failed(ObjectSkippedReason),
}

/// Reasons why an object may be skipped or fail during processing.
#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    JsonSchema,
    Display,
    EnumString,
    VariantNames,
    EnumIter,
)]
#[strum(serialize_all = "snake_case")]
#[serde(tag = "type", content = "status_code")]
pub enum ObjectSkippedReason {
    /// Agent encountered a local filesystem error
    AgentFSError,
    /// The specified agent does not have that assignment
    AgentAssignmentNoEnt,
    /// The agent is busy and can't accept assignments at this time
    AgentBusy,
    /// Internal assignment error
    AssignmentError,
    /// A mismatch of assignment data between the agent and the zone
    AssignmentMismatch,
    /// The assignment was rejected by the agent
    AssignmentRejected,
    /// Not enough space on destination storage node
    DestinationInsufficientSpace,
    /// Destination agent was not reachable
    DestinationUnreachable,
    /// MD5 mismatch between the file on disk and the metadata
    MD5Mismatch,
    /// Catchall for unspecified network errors
    NetworkError,
    /// The object is already on the proposed destination shark
    ObjectAlreadyOnDestShark,
    /// The object is already in the proposed destination datacenter
    ObjectAlreadyInDatacenter,
    /// Encountered some other HTTP error while contacting the source
    SourceOtherError,
    /// The only source available is the shark being evacuated
    SourceIsEvacShark,
    /// HTTP status code error
    HTTPStatusCode(HttpStatusCode),
}

impl ObjectSkippedReason {
    /// Convert to a string representation, handling the special case of
    /// HTTPStatusCode which includes a value.
    pub fn into_string(self) -> String {
        match self {
            ObjectSkippedReason::HTTPStatusCode(sc) => {
                format!("{{{}:{}}}", self, sc)
            }
            _ => self.to_string(),
        }
    }
}

/// Current state of an assignment being processed by an agent.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub enum AgentAssignmentState {
    /// Assignment received but not yet started
    Scheduled,
    /// Assignment is currently being processed
    Running,
    /// Assignment completed, optionally includes failed tasks
    Complete(Option<Vec<Task>>),
}

/// Statistics for an assignment's progress.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct AgentAssignmentStats {
    /// Current state of the assignment
    pub state: AgentAssignmentState,
    /// Number of tasks that failed
    pub failed: usize,
    /// Number of tasks completed (including failed)
    pub complete: usize,
    /// Total number of tasks in the assignment
    pub total: usize,
}

impl AgentAssignmentStats {
    /// Create new stats for an assignment with the given total task count.
    pub fn new(total: usize) -> AgentAssignmentStats {
        AgentAssignmentStats {
            state: AgentAssignmentState::Scheduled,
            failed: 0,
            complete: 0,
            total,
        }
    }
}

/// Full assignment information returned when querying status.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Assignment {
    /// Unique identifier for this assignment
    pub uuid: String,
    /// Statistics about the assignment's progress
    pub stats: AgentAssignmentStats,
}

// ============================================================================
// Manager Types
// ============================================================================

/// Payload for creating a new job.
///
/// Jobs are created by sending a JSON object with an "action" field specifying
/// the job type and a "params" field containing action-specific parameters.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "action", content = "params")]
#[serde(rename_all = "lowercase")]
pub enum JobPayload {
    /// Evacuate all objects from a storage node
    Evacuate(EvacuateJobPayload),
}

/// Parameters for creating an evacuate job.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct EvacuateJobPayload {
    /// The storage node to evacuate (e.g., "1.stor.domain.com")
    pub from_shark: String,
    /// Optional limit on number of objects to process (for testing)
    pub max_objects: Option<u32>,
}

/// State of a job in the manager.
#[derive(
    Debug,
    Clone,
    Default,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    JsonSchema,
    Display,
    EnumString,
    VariantNames,
)]
#[strum(serialize_all = "snake_case")]
pub enum JobState {
    /// Job is being initialized
    #[default]
    Init,
    /// Job is setting up
    Setup,
    /// Job is actively running
    Running,
    /// Job has been stopped
    Stopped,
    /// Job completed successfully
    Complete,
    /// Job failed
    Failed,
}

/// Type of job action.
#[derive(
    Debug,
    Clone,
    Default,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    JsonSchema,
    Display,
    EnumString,
    VariantNames,
)]
#[strum(serialize_all = "snake_case")]
pub enum JobAction {
    /// Evacuate objects from a storage node
    Evacuate,
    /// No action (placeholder)
    #[default]
    None,
}

/// Database entry representation of a job (for listing).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct JobDbEntry {
    /// Unique identifier for this job (UUID)
    pub id: String,
    /// Type of job action
    pub action: JobAction,
    /// Current state of the job
    pub state: JobState,
}

/// Configuration information for an evacuate job.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct JobConfigEvacuate {
    /// The storage node being evacuated
    pub from_shark: StorageNode,
}

/// Job status configuration, tagged by action type.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "action")]
pub enum JobStatusConfig {
    /// Configuration for an evacuate job
    Evacuate(JobConfigEvacuate),
}

/// Job status results (status counts by category).
pub type JobStatusResultsEvacuate = std::collections::HashMap<String, i64>;

/// Job status results, tagged by action type.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum JobStatusResults {
    /// Results for an evacuate job
    Evacuate(JobStatusResultsEvacuate),
}

/// Full job status including configuration, results, and state.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct JobStatus {
    /// Job configuration
    pub config: JobStatusConfig,
    /// Job results (status counts)
    pub results: JobStatusResults,
    /// Current state of the job
    pub state: JobState,
}

/// Update message for dynamically configuring a running evacuate job.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", content = "value")]
pub enum EvacuateJobUpdateMessage {
    /// Set the number of metadata update threads
    SetMetadataThreads(u32),
}

impl EvacuateJobUpdateMessage {
    /// Validate the update message parameters.
    pub fn validate(&self) -> Result<(), String> {
        match self {
            EvacuateJobUpdateMessage::SetMetadataThreads(n) => {
                if *n == 0 {
                    return Err("Metadata threads must be > 0".to_string());
                }
                Ok(())
            }
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_task_status_serialization() {
        let pending = TaskStatus::Pending;
        let json = serde_json::to_string(&pending).expect("serialize pending");
        assert_eq!(json, r#"{"state":"Pending"}"#);

        let complete = TaskStatus::Complete;
        let json = serde_json::to_string(&complete).expect("serialize complete");
        assert_eq!(json, r#"{"state":"Complete"}"#);

        let failed = TaskStatus::Failed(ObjectSkippedReason::MD5Mismatch);
        let json = serde_json::to_string(&failed).expect("serialize failed");
        assert!(json.contains("Failed"));
        assert!(json.contains("MD5Mismatch"));
    }

    #[test]
    fn test_job_payload_serialization() {
        let payload = JobPayload::Evacuate(EvacuateJobPayload {
            from_shark: "1.stor.domain.com".to_string(),
            max_objects: Some(100),
        });
        let json = serde_json::to_string(&payload).expect("serialize payload");
        assert!(json.contains("evacuate"));
        assert!(json.contains("1.stor.domain.com"));
    }

    #[test]
    fn test_job_state_display() {
        assert_eq!(JobState::Init.to_string(), "init");
        assert_eq!(JobState::Running.to_string(), "running");
        assert_eq!(JobState::Complete.to_string(), "complete");
    }

    #[test]
    fn test_object_skipped_reason_into_string() {
        assert_eq!(
            ObjectSkippedReason::MD5Mismatch.into_string(),
            "md5_mismatch"
        );
        assert_eq!(
            ObjectSkippedReason::HTTPStatusCode(404).into_string(),
            "{http_status_code:404}"
        );
    }
}
