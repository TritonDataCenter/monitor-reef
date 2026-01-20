// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2020 Joyent, Inc.
// Copyright 2026 Edgecast Cloud LLC.

//! API context for the rebalancer agent

use std::sync::Arc;

use anyhow::Result;

use rebalancer_types::{Assignment, AssignmentPayload};

use crate::config::AgentConfig;
use crate::processor::TaskProcessor;
use crate::storage::{AssignmentStorage, StorageError};

/// API context shared across all request handlers
pub struct ApiContext {
    storage: Arc<AssignmentStorage>,
    processor: Arc<TaskProcessor>,
}

impl ApiContext {
    /// Create a new API context
    pub async fn new(config: AgentConfig) -> Result<Self> {
        // Ensure objects directory exists
        tokio::fs::create_dir_all(config.objects_dir()).await?;

        // Initialize storage
        let storage = Arc::new(AssignmentStorage::new(&config.db_path())?);

        // Initialize processor
        let processor = Arc::new(TaskProcessor::new(config, Arc::clone(&storage)));

        Ok(Self { storage, processor })
    }

    /// Check if an assignment exists
    pub async fn assignment_exists(&self, uuid: &str) -> bool {
        self.storage.has_assignment(uuid).await.unwrap_or(false)
    }

    /// Create a new assignment and start processing it
    pub async fn create_assignment(&self, payload: AssignmentPayload) -> Result<()> {
        let (uuid, tasks) = payload.into();

        // Store the assignment
        self.storage.create(&uuid, &tasks).await?;

        // Start processing in the background
        let processor = Arc::clone(&self.processor);
        let uuid_clone = uuid.clone();
        tokio::spawn(async move {
            processor.process_assignment(&uuid_clone).await;
        });

        Ok(())
    }

    /// Get assignment status
    pub async fn get_assignment(&self, uuid: &str) -> Option<Assignment> {
        self.storage.get(uuid).await.ok()
    }

    /// Delete a completed assignment
    pub async fn delete_assignment(&self, uuid: &str) -> Result<(), StorageError> {
        self.storage.delete(uuid).await
    }
}
