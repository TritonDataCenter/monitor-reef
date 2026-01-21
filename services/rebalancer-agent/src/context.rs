// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2020 Joyent, Inc.
// Copyright 2026 Edgecast Cloud LLC.

//! API context for the rebalancer agent

use std::sync::Arc;

use anyhow::Result;
use tracing::{info, warn};

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
    ///
    /// This also resumes any incomplete assignments from a previous run.
    pub async fn new(config: AgentConfig) -> Result<Self> {
        // Ensure objects directory exists
        tokio::fs::create_dir_all(config.objects_dir()).await?;

        // Initialize storage
        let storage = Arc::new(AssignmentStorage::new(&config.db_path())?);

        // Initialize processor
        let processor = Arc::new(TaskProcessor::new(config, Arc::clone(&storage))?);

        let ctx = Self { storage, processor };

        // Resume any incomplete assignments from previous run
        ctx.resume_incomplete_assignments().await;

        Ok(ctx)
    }

    /// Resume processing of any incomplete assignments
    ///
    /// This is called on startup to handle assignments that were interrupted
    /// by a crash or restart.
    async fn resume_incomplete_assignments(&self) {
        match self.storage.get_incomplete_assignments().await {
            Ok(uuids) if uuids.is_empty() => {
                info!("No incomplete assignments to resume");
            }
            Ok(uuids) => {
                info!(
                    count = uuids.len(),
                    "Resuming incomplete assignments from previous run"
                );
                for uuid in uuids {
                    info!(assignment_id = %uuid, "Resuming assignment");
                    let processor = Arc::clone(&self.processor);
                    let uuid_clone = uuid.clone();
                    tokio::spawn(async move {
                        processor.process_assignment(&uuid_clone).await;
                    });
                }
            }
            Err(e) => {
                warn!(
                    error = %e,
                    "Failed to get incomplete assignments - some may not be resumed"
                );
            }
        }
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
