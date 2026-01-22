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
    /// This also:
    /// 1. Cleans up any stale .tmp files from interrupted downloads (MIN-5)
    /// 2. Resumes any incomplete assignments from a previous run
    pub async fn new(config: AgentConfig) -> Result<Self> {
        // Ensure data directory exists (for database)
        tokio::fs::create_dir_all(&config.data_dir).await?;

        // MIN-5: Clean up any stale .tmp files from interrupted downloads
        // This must happen before resuming assignments to avoid confusion
        Self::cleanup_temp_files(&config).await;

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

    /// Clean up any stale .tmp files from interrupted downloads (MIN-5)
    ///
    /// This is called on startup to remove partial downloads that were
    /// interrupted by a crash or restart. These files have a `.tmp` extension
    /// and are located in the manta_root directory tree.
    async fn cleanup_temp_files(config: &AgentConfig) {
        let manta_root = config.temp_dir();

        // If the manta root doesn't exist yet, nothing to clean up
        if !tokio::fs::try_exists(manta_root).await.unwrap_or(false) {
            info!(path = %manta_root.display(), "Manta root does not exist, skipping temp file cleanup");
            return;
        }

        info!(path = %manta_root.display(), "Scanning for stale .tmp files from interrupted downloads");

        let mut cleaned = 0u64;
        let mut errors = 0u64;

        // Walk the directory tree looking for .tmp files
        match Self::find_and_remove_tmp_files(manta_root, &mut cleaned, &mut errors).await {
            Ok(()) => {
                if cleaned > 0 {
                    info!(
                        cleaned = cleaned,
                        errors = errors,
                        "Cleaned up stale .tmp files from previous run"
                    );
                } else {
                    info!("No stale .tmp files found");
                }
            }
            Err(e) => {
                warn!(
                    error = %e,
                    cleaned = cleaned,
                    "Error during temp file cleanup - some files may remain"
                );
            }
        }
    }

    /// Recursively find and remove .tmp files
    async fn find_and_remove_tmp_files(
        dir: &std::path::Path,
        cleaned: &mut u64,
        errors: &mut u64,
    ) -> std::io::Result<()> {
        let mut entries = tokio::fs::read_dir(dir).await?;

        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            let file_type = entry.file_type().await?;

            // arch-lint: allow(no-sync-io) reason="is_dir() on FileType is in-memory, not I/O"
            if file_type.is_dir() {
                // Recursively process subdirectories
                // Ignore errors in subdirectories - continue with others
                if let Err(e) =
                    Box::pin(Self::find_and_remove_tmp_files(&path, cleaned, errors)).await
                {
                    warn!(path = %path.display(), error = %e, "Error scanning subdirectory");
                    *errors += 1;
                }
            // arch-lint: allow(no-sync-io) reason="is_file() on FileType is in-memory, not I/O"
            } else if file_type.is_file() {
                // Check if this is a .tmp file
                if let Some(ext) = path.extension()
                    && ext == "tmp"
                {
                    info!(path = %path.display(), "Removing stale temp file");
                    if let Err(e) = tokio::fs::remove_file(&path).await {
                        warn!(path = %path.display(), error = %e, "Failed to remove temp file");
                        *errors += 1;
                    } else {
                        *cleaned += 1;
                    }
                }
            }
        }

        Ok(())
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
