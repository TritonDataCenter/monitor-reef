// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2020 Joyent, Inc.
// Copyright 2026 Edgecast Cloud LLC.

//! Assignment management for evacuate jobs

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use rebalancer_types::{AssignmentPayload, StorageNode, Task, TaskStatus};

use super::AssignmentId;
use super::types::EvacuateObject;

/// State of an assignment in the evacuation process
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AssignmentState {
    /// Assignment is in the process of being created
    Init,
    /// Assignment has been submitted to the Agent
    Assigned,
    /// Agent has rejected the Assignment
    Rejected,
    /// Could not connect to agent
    AgentUnavailable,
    /// Agent has completed its work
    AgentComplete,
    /// The Assignment has completed all necessary work
    PostProcessed,
}

impl Default for AssignmentState {
    fn default() -> Self {
        Self::Init
    }
}

/// An assignment of tasks to a destination shark
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Assignment {
    /// Unique identifier for this assignment
    pub id: AssignmentId,

    /// Destination storage node
    pub dest_shark: StorageNode,

    /// Tasks in this assignment (object_id -> Task)
    pub tasks: HashMap<String, Task>,

    /// Maximum size in MB for this assignment
    pub max_size: u64,

    /// Total size in MB of all tasks
    pub total_size: u64,

    /// Current state
    pub state: AssignmentState,
}

#[allow(dead_code)]
impl Assignment {
    /// Create a new assignment for a destination shark
    pub fn new(dest_shark: StorageNode) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            dest_shark,
            tasks: HashMap::new(),
            max_size: 0,
            total_size: 0,
            state: AssignmentState::Init,
        }
    }

    /// Add a task for an evacuate object
    pub fn add_task(&mut self, eobj: &EvacuateObject) {
        // Extract essential info from the Manta object
        // In a full implementation, we'd parse the object JSON to get
        // owner, md5sum, source shark, etc.
        let task = Task {
            object_id: eobj.id.clone(),
            owner: String::new(),  // Would come from MantaObject
            md5sum: String::new(), // Would come from MantaObject
            source: StorageNode {
                // Would be extracted from MantaObject.sharks
                manta_storage_id: String::new(),
                datacenter: String::new(),
            },
            status: TaskStatus::Pending,
        };

        self.tasks.insert(eobj.id.clone(), task);
    }

    /// Convert to assignment payload for posting to agent
    pub fn to_payload(&self) -> AssignmentPayload {
        AssignmentPayload {
            id: self.id.clone(),
            tasks: self.tasks.values().cloned().collect(),
        }
    }
}

/// Cache entry for tracking assignment state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssignmentCacheEntry {
    /// Assignment ID
    pub id: AssignmentId,

    /// Destination storage node
    pub dest_shark: StorageNode,

    /// Total size in MB
    pub total_size: u64,

    /// Current state
    pub state: AssignmentState,
}

impl From<Assignment> for AssignmentCacheEntry {
    fn from(assignment: Assignment) -> Self {
        AssignmentCacheEntry {
            id: assignment.id,
            dest_shark: assignment.dest_shark,
            total_size: assignment.total_size,
            state: assignment.state,
        }
    }
}
