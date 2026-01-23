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

/// Maximum number of metadata update threads that can be set dynamically.
///
/// This is a safety limit to prevent the rebalancer from hammering the metadata
/// tier due to a fat finger. It is still possible to set this number higher
/// but only at the start of a job. See MANTA-5284 for context.
pub const MAX_TUNABLE_MD_UPDATE_THREADS: u32 = 250;

/// Update message for dynamically configuring a running evacuate job.
///
/// The JSON format matches the legacy API:
/// ```json
/// {"action": "set_metadata_threads", "params": 30}
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "action", content = "params", rename_all = "snake_case")]
pub enum EvacuateJobUpdateMessage {
    /// Set the number of metadata update threads (1-250)
    SetMetadataThreads(u32),
}

impl EvacuateJobUpdateMessage {
    /// Validate the update message parameters.
    ///
    /// For `SetMetadataThreads`, ensures the thread count is between 1 and
    /// `MAX_TUNABLE_MD_UPDATE_THREADS` (250).
    pub fn validate(&self) -> Result<(), String> {
        match self {
            EvacuateJobUpdateMessage::SetMetadataThreads(n) => {
                if *n < 1 {
                    return Err("Cannot set metadata update threads below 1".to_string());
                }
                if *n > MAX_TUNABLE_MD_UPDATE_THREADS {
                    return Err(format!(
                        "Cannot set metadata update threads above {}",
                        MAX_TUNABLE_MD_UPDATE_THREADS
                    ));
                }
                Ok(())
            }
        }
    }
}

// ============================================================================
// Arbitrary Implementations for Property-Based Testing
// ============================================================================

#[cfg(test)]
mod arbitrary_impls {
    use super::*;
    use quickcheck::{Arbitrary, Gen};
    use quickcheck_helpers::random::string as random_string;

    impl Arbitrary for StorageNode {
        fn arbitrary(g: &mut Gen) -> Self {
            let dc_num = u8::arbitrary(g) % 5 + 1;
            let shark_num = u8::arbitrary(g) % 10 + 1;
            StorageNode {
                datacenter: format!("dc{}", dc_num),
                manta_storage_id: format!("{}.stor.test.domain", shark_num),
            }
        }
    }

    impl Arbitrary for ObjectSkippedReason {
        fn arbitrary(g: &mut Gen) -> Self {
            let variant = u8::arbitrary(g) % 15;
            match variant {
                0 => ObjectSkippedReason::AgentFSError,
                1 => ObjectSkippedReason::AgentAssignmentNoEnt,
                2 => ObjectSkippedReason::AgentBusy,
                3 => ObjectSkippedReason::AssignmentError,
                4 => ObjectSkippedReason::AssignmentMismatch,
                5 => ObjectSkippedReason::AssignmentRejected,
                6 => ObjectSkippedReason::DestinationInsufficientSpace,
                7 => ObjectSkippedReason::DestinationUnreachable,
                8 => ObjectSkippedReason::MD5Mismatch,
                9 => ObjectSkippedReason::NetworkError,
                10 => ObjectSkippedReason::ObjectAlreadyOnDestShark,
                11 => ObjectSkippedReason::ObjectAlreadyInDatacenter,
                12 => ObjectSkippedReason::SourceOtherError,
                13 => ObjectSkippedReason::SourceIsEvacShark,
                _ => {
                    // Generate an HTTP status code (common codes: 400, 403, 404, 500, 502, 503)
                    let codes = [400u16, 403, 404, 500, 502, 503, 504];
                    let idx = usize::arbitrary(g) % codes.len();
                    ObjectSkippedReason::HTTPStatusCode(codes[idx])
                }
            }
        }
    }

    impl Arbitrary for TaskStatus {
        fn arbitrary(g: &mut Gen) -> Self {
            let variant = u8::arbitrary(g) % 3;
            match variant {
                0 => TaskStatus::Pending,
                1 => TaskStatus::Complete,
                _ => TaskStatus::Failed(ObjectSkippedReason::arbitrary(g)),
            }
        }
    }

    impl Arbitrary for Task {
        fn arbitrary(g: &mut Gen) -> Self {
            Task {
                object_id: format!("{}", uuid::Uuid::new_v4()),
                owner: format!("{}", uuid::Uuid::new_v4()),
                md5sum: random_string(g, 24), // Base64 encoded MD5 is 24 chars
                source: StorageNode::arbitrary(g),
                status: TaskStatus::arbitrary(g),
            }
        }
    }

    impl Arbitrary for AssignmentPayload {
        fn arbitrary(g: &mut Gen) -> Self {
            let num_tasks = usize::arbitrary(g) % 100 + 1; // 1-100 tasks
            let tasks: Vec<Task> = (0..num_tasks).map(|_| Task::arbitrary(g)).collect();

            AssignmentPayload {
                id: format!("{}", uuid::Uuid::new_v4()),
                tasks,
            }
        }
    }

    impl Arbitrary for JobState {
        fn arbitrary(g: &mut Gen) -> Self {
            let variant = u8::arbitrary(g) % 6;
            match variant {
                0 => JobState::Init,
                1 => JobState::Setup,
                2 => JobState::Running,
                3 => JobState::Stopped,
                4 => JobState::Complete,
                _ => JobState::Failed,
            }
        }
    }

    impl Arbitrary for JobAction {
        fn arbitrary(g: &mut Gen) -> Self {
            if bool::arbitrary(g) {
                JobAction::Evacuate
            } else {
                JobAction::None
            }
        }
    }

    impl Arbitrary for JobDbEntry {
        fn arbitrary(g: &mut Gen) -> Self {
            JobDbEntry {
                id: format!("{}", uuid::Uuid::new_v4()),
                action: JobAction::arbitrary(g),
                state: JobState::arbitrary(g),
            }
        }
    }

    impl Arbitrary for EvacuateJobPayload {
        fn arbitrary(g: &mut Gen) -> Self {
            let shark_num = u8::arbitrary(g) % 10 + 1;
            EvacuateJobPayload {
                from_shark: format!("{}.stor.test.domain", shark_num),
                max_objects: if bool::arbitrary(g) {
                    Some(u32::arbitrary(g) % 10000 + 1)
                } else {
                    None
                },
            }
        }
    }

    impl Arbitrary for EvacuateJobUpdateMessage {
        fn arbitrary(g: &mut Gen) -> Self {
            // Ensure we generate valid values (> 0)
            let n = u32::arbitrary(g) % 100 + 1;
            EvacuateJobUpdateMessage::SetMetadataThreads(n)
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

    // -------------------------------------------------------------------------
    // EvacuateJobUpdateMessage Tests (MIN-7: Dynamic Thread Tuning)
    // -------------------------------------------------------------------------

    #[test]
    fn test_evacuate_job_update_message_serialization() {
        // Test that the JSON format matches legacy API: {"action": "set_metadata_threads", "params": N}
        let msg = EvacuateJobUpdateMessage::SetMetadataThreads(30);
        let json = serde_json::to_string(&msg).expect("serialize message");
        assert_eq!(json, r#"{"action":"set_metadata_threads","params":30}"#);

        // Test deserialization
        let parsed: EvacuateJobUpdateMessage =
            serde_json::from_str(r#"{"action":"set_metadata_threads","params":50}"#)
                .expect("deserialize message");
        assert!(matches!(
            parsed,
            EvacuateJobUpdateMessage::SetMetadataThreads(50)
        ));
    }

    #[test]
    fn test_evacuate_job_update_message_validation_valid() {
        // Minimum valid value
        assert!(
            EvacuateJobUpdateMessage::SetMetadataThreads(1)
                .validate()
                .is_ok()
        );

        // Typical value
        assert!(
            EvacuateJobUpdateMessage::SetMetadataThreads(30)
                .validate()
                .is_ok()
        );

        // Maximum valid value (250)
        assert!(
            EvacuateJobUpdateMessage::SetMetadataThreads(MAX_TUNABLE_MD_UPDATE_THREADS)
                .validate()
                .is_ok()
        );
    }

    #[test]
    fn test_evacuate_job_update_message_validation_zero() {
        // Zero is invalid (must be >= 1)
        let result = EvacuateJobUpdateMessage::SetMetadataThreads(0).validate();
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err(),
            "Cannot set metadata update threads below 1"
        );
    }

    #[test]
    fn test_evacuate_job_update_message_validation_too_high() {
        // Above MAX_TUNABLE_MD_UPDATE_THREADS (250) is invalid
        let result = EvacuateJobUpdateMessage::SetMetadataThreads(251).validate();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("above 250"));

        // Way too high
        let result = EvacuateJobUpdateMessage::SetMetadataThreads(1000).validate();
        assert!(result.is_err());
    }

    #[test]
    fn test_max_tunable_md_update_threads_constant() {
        // Verify the constant matches legacy value
        assert_eq!(MAX_TUNABLE_MD_UPDATE_THREADS, 250);
    }
}

// ============================================================================
// Property-Based Tests (QuickCheck)
// ============================================================================

#[cfg(test)]
mod quickcheck_tests {
    use super::*;
    use quickcheck::quickcheck;

    // -------------------------------------------------------------------------
    // Serialization Round-Trip Tests
    // -------------------------------------------------------------------------

    quickcheck! {
        /// Any Task can be serialized to JSON and deserialized back
        fn prop_task_json_roundtrip(task: Task) -> bool {
            let json = serde_json::to_string(&task).unwrap();
            let decoded: Task = serde_json::from_str(&json).unwrap();
            decoded.object_id == task.object_id
                && decoded.owner == task.owner
                && decoded.md5sum == task.md5sum
        }

        /// Any StorageNode can be serialized and deserialized
        fn prop_storage_node_roundtrip(node: StorageNode) -> bool {
            let json = serde_json::to_string(&node).unwrap();
            let decoded: StorageNode = serde_json::from_str(&json).unwrap();
            decoded == node
        }

        /// Any TaskStatus can be serialized and deserialized
        fn prop_task_status_roundtrip(status: TaskStatus) -> bool {
            let json = serde_json::to_string(&status).unwrap();
            let decoded: TaskStatus = serde_json::from_str(&json).unwrap();
            decoded == status
        }

        /// Any JobState can be serialized and deserialized
        fn prop_job_state_roundtrip(state: JobState) -> bool {
            let json = serde_json::to_string(&state).unwrap();
            let decoded: JobState = serde_json::from_str(&json).unwrap();
            decoded == state
        }

        /// Any JobDbEntry can be serialized and deserialized
        fn prop_job_db_entry_roundtrip(entry: JobDbEntry) -> bool {
            let json = serde_json::to_string(&entry).unwrap();
            let decoded: JobDbEntry = serde_json::from_str(&json).unwrap();
            decoded == entry
        }

        /// Any AssignmentPayload can be serialized and deserialized
        fn prop_assignment_payload_roundtrip(payload: AssignmentPayload) -> bool {
            let json = serde_json::to_string(&payload).unwrap();
            let decoded: AssignmentPayload = serde_json::from_str(&json).unwrap();
            decoded.id == payload.id && decoded.tasks.len() == payload.tasks.len()
        }
    }

    // -------------------------------------------------------------------------
    // Invariant Tests
    // -------------------------------------------------------------------------

    quickcheck! {
        /// Assignment payload always has at least one task (from our Arbitrary impl)
        fn prop_assignment_has_tasks(payload: AssignmentPayload) -> bool {
            !payload.tasks.is_empty()
        }

        /// EvacuateJobUpdateMessage validation: SetMetadataThreads(0) is invalid
        fn prop_update_message_validation(msg: EvacuateJobUpdateMessage) -> bool {
            // Our Arbitrary impl generates valid values (1-100), so validation should pass
            msg.validate().is_ok()
        }

        /// HTTPStatusCode variant can round-trip its status code
        fn prop_http_status_code_preserved(code: u16) -> bool {
            let reason = ObjectSkippedReason::HTTPStatusCode(code);
            match reason {
                ObjectSkippedReason::HTTPStatusCode(c) => c == code,
                _ => false,
            }
        }

        /// ObjectSkippedReason into_string never panics
        fn prop_skipped_reason_into_string_safe(reason: ObjectSkippedReason) -> bool {
            let s = reason.into_string();
            !s.is_empty()
        }
    }

    // -------------------------------------------------------------------------
    // Large-Scale Tests
    // -------------------------------------------------------------------------

    /// Test processing many tasks in a single assignment
    #[test]
    fn test_large_assignment() {
        use quickcheck::{Arbitrary, Gen};

        let mut g = Gen::new(100);
        let mut tasks = Vec::new();

        // Generate 1000 tasks
        for _ in 0..1000 {
            tasks.push(Task::arbitrary(&mut g));
        }

        let payload = AssignmentPayload {
            id: uuid::Uuid::new_v4().to_string(),
            tasks,
        };

        // Verify serialization works with large payload
        let json = serde_json::to_string(&payload).expect("serialize large payload");
        let decoded: AssignmentPayload =
            serde_json::from_str(&json).expect("deserialize large payload");

        assert_eq!(decoded.tasks.len(), 1000);
    }

    /// Test all ObjectSkippedReason variants can be serialized
    #[test]
    fn test_all_skipped_reasons_serialize() {
        use strum::IntoEnumIterator;

        for reason in ObjectSkippedReason::iter() {
            let status = TaskStatus::Failed(reason);
            let json = serde_json::to_string(&status).expect("serialize status");
            let _decoded: TaskStatus = serde_json::from_str(&json).expect("deserialize status");
        }
    }

    /// Test all JobState variants can be serialized
    #[test]
    fn test_all_job_states_serialize() {
        let states = [
            JobState::Init,
            JobState::Setup,
            JobState::Running,
            JobState::Stopped,
            JobState::Complete,
            JobState::Failed,
        ];

        for state in states {
            let json = serde_json::to_string(&state).expect("serialize state");
            let decoded: JobState = serde_json::from_str(&json).expect("deserialize state");
            assert_eq!(decoded, state);
        }
    }
}
