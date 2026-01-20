// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2020 Joyent, Inc.
// Copyright 2026 Edgecast Cloud LLC.

//! Task processing logic
//!
//! Downloads objects from source storage nodes and verifies checksums.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use md5::{Digest, Md5};
use reqwest::Client;
use tokio::fs::{self, File};
use tokio::io::AsyncWriteExt;
use tokio::sync::Semaphore;

use rebalancer_types::{ObjectSkippedReason, Task};

use crate::config::AgentConfig;
use crate::storage::AssignmentStorage;

/// Task processor that downloads objects and verifies checksums
pub struct TaskProcessor {
    client: Client,
    config: AgentConfig,
    storage: Arc<AssignmentStorage>,
    semaphore: Arc<Semaphore>,
}

impl TaskProcessor {
    /// Create a new task processor
    pub fn new(config: AgentConfig, storage: Arc<AssignmentStorage>) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(config.download_timeout_secs))
            .build()
            .unwrap_or_else(|_| Client::new());

        let semaphore = Arc::new(Semaphore::new(config.concurrent_downloads));

        Self {
            client,
            config,
            storage,
            semaphore,
        }
    }

    /// Process all tasks in an assignment
    pub async fn process_assignment(&self, assignment_uuid: &str) {
        // Mark assignment as running
        if let Err(e) = self.storage.set_state(assignment_uuid, "running").await {
            tracing::error!(
                assignment_id = %assignment_uuid,
                error = %e,
                "Failed to set assignment state to running"
            );
            return;
        }

        // Get pending tasks
        let tasks = match self.storage.get_pending_tasks(assignment_uuid).await {
            Ok(tasks) => tasks,
            Err(e) => {
                tracing::error!(
                    assignment_id = %assignment_uuid,
                    error = %e,
                    "Failed to get pending tasks"
                );
                return;
            }
        };

        tracing::info!(
            assignment_id = %assignment_uuid,
            task_count = tasks.len(),
            "Starting to process assignment"
        );

        // Process tasks concurrently with semaphore limiting
        let mut handles = Vec::with_capacity(tasks.len());

        for task in tasks {
            let processor = self.clone_for_task();
            let uuid = assignment_uuid.to_string();

            let handle = tokio::spawn(async move {
                processor.process_task(&uuid, task).await;
            });

            handles.push(handle);
        }

        // Wait for all tasks to complete
        for handle in handles {
            if let Err(e) = handle.await {
                tracing::error!(error = %e, "Task join error");
            }
        }

        // Mark assignment as complete
        if let Err(e) = self.storage.set_state(assignment_uuid, "complete").await {
            tracing::error!(
                assignment_id = %assignment_uuid,
                error = %e,
                "Failed to set assignment state to complete"
            );
        }

        tracing::info!(assignment_id = %assignment_uuid, "Assignment processing complete");
    }

    /// Clone the processor for use in a spawned task
    fn clone_for_task(&self) -> TaskProcessorHandle {
        TaskProcessorHandle {
            client: self.client.clone(),
            config: self.config.clone(),
            storage: Arc::clone(&self.storage),
            semaphore: Arc::clone(&self.semaphore),
        }
    }
}

/// Handle for processing a single task (cloneable for spawned tasks)
struct TaskProcessorHandle {
    client: Client,
    config: AgentConfig,
    storage: Arc<AssignmentStorage>,
    semaphore: Arc<Semaphore>,
}

impl TaskProcessorHandle {
    /// Process a single task
    async fn process_task(&self, assignment_uuid: &str, task: Task) {
        // Acquire semaphore permit to limit concurrency
        let _permit = self.semaphore.acquire().await;

        tracing::debug!(
            assignment_id = %assignment_uuid,
            object_id = %task.object_id,
            source = %task.source.manta_storage_id,
            "Processing task"
        );

        let result = self.download_and_verify(&task).await;

        match result {
            Ok(()) => {
                tracing::debug!(
                    assignment_id = %assignment_uuid,
                    object_id = %task.object_id,
                    "Task completed successfully"
                );
                if let Err(e) = self
                    .storage
                    .mark_task_complete(assignment_uuid, &task.object_id)
                    .await
                {
                    tracing::error!(
                        assignment_id = %assignment_uuid,
                        object_id = %task.object_id,
                        error = %e,
                        "Failed to mark task complete"
                    );
                }
            }
            Err(reason) => {
                tracing::warn!(
                    assignment_id = %assignment_uuid,
                    object_id = %task.object_id,
                    reason = ?reason,
                    "Task failed"
                );
                if let Err(e) = self
                    .storage
                    .mark_task_failed(assignment_uuid, &task.object_id, &reason)
                    .await
                {
                    tracing::error!(
                        assignment_id = %assignment_uuid,
                        object_id = %task.object_id,
                        error = %e,
                        "Failed to mark task failed"
                    );
                }
            }
        }
    }

    /// Download an object and verify its checksum
    async fn download_and_verify(&self, task: &Task) -> Result<(), ObjectSkippedReason> {
        // Build the URL for the object
        // Format: http://{storage_id}/{owner}/{object_id}
        let url = format!(
            "http://{}/{}/{}",
            task.source.manta_storage_id, task.owner, task.object_id
        );

        // Determine destination path
        let dest_dir = self.config.objects_dir().join(&task.owner);
        let dest_path = dest_dir.join(&task.object_id);

        // Ensure destination directory exists
        if let Err(e) = fs::create_dir_all(&dest_dir).await {
            tracing::error!(
                path = %dest_dir.display(),
                error = %e,
                "Failed to create destination directory"
            );
            return Err(ObjectSkippedReason::AgentFSError);
        }

        // Download the object
        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| {
                tracing::debug!(url = %url, error = %e, "HTTP request failed");
                ObjectSkippedReason::NetworkError
            })?;

        // Check HTTP status
        let status = response.status();
        if !status.is_success() {
            tracing::debug!(url = %url, status = %status, "HTTP error response");
            return Err(ObjectSkippedReason::HTTPStatusCode(status.as_u16()));
        }

        // Stream the response body to disk while computing MD5
        let computed_md5 = self
            .stream_to_file_with_md5(response, &dest_path)
            .await
            .map_err(|e| {
                tracing::error!(
                    path = %dest_path.display(),
                    error = %e,
                    "Failed to write object to disk"
                );
                ObjectSkippedReason::AgentFSError
            })?;

        // Verify MD5 checksum
        let expected_md5 = &task.md5sum;
        if computed_md5 != *expected_md5 {
            tracing::warn!(
                object_id = %task.object_id,
                expected = %expected_md5,
                computed = %computed_md5,
                "MD5 checksum mismatch"
            );
            // Remove the corrupted file
            let _ = fs::remove_file(&dest_path).await;
            return Err(ObjectSkippedReason::MD5Mismatch);
        }

        Ok(())
    }

    /// Stream HTTP response body to a file while computing MD5
    async fn stream_to_file_with_md5(
        &self,
        response: reqwest::Response,
        dest_path: &PathBuf,
    ) -> std::io::Result<String> {
        use futures_util::StreamExt;

        let mut file = File::create(dest_path).await?;
        let mut hasher = Md5::new();

        let mut stream = response.bytes_stream();

        while let Some(chunk_result) = stream.next().await {
            let chunk = chunk_result.map_err(|e| {
                std::io::Error::new(std::io::ErrorKind::Other, e.to_string())
            })?;
            hasher.update(&chunk);
            file.write_all(&chunk).await?;
        }

        file.flush().await?;

        let hash = hasher.finalize();
        Ok(base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            hash,
        ))
    }
}
