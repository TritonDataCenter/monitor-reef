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
    ///
    /// # Errors
    ///
    /// Returns an error if the HTTP client cannot be created with the
    /// configured timeout settings.
    pub fn new(
        config: AgentConfig,
        storage: Arc<AssignmentStorage>,
    ) -> Result<Self, reqwest::Error> {
        let client = Client::builder()
            .timeout(Duration::from_secs(config.download_timeout_secs))
            .build()
            .inspect_err(|e| {
                tracing::error!(
                    timeout_secs = config.download_timeout_secs,
                    error = %e,
                    "Failed to create HTTP client with configured timeout"
                );
            })?;

        let semaphore = Arc::new(Semaphore::new(config.concurrent_downloads));

        Ok(Self {
            client,
            config,
            storage,
            semaphore,
        })
    }

    /// Process all tasks in an assignment
    pub async fn process_assignment(&self, assignment_uuid: &str) {
        // Mark assignment as running - abort if we can't update state
        let Ok(()) = self
            .storage
            .set_state(assignment_uuid, "running")
            .await
            .inspect_err(|e| {
                tracing::error!(
                    assignment_id = %assignment_uuid,
                    error = %e,
                    "Failed to set assignment state to running, aborting"
                );
            })
        else {
            return; // Cannot proceed without proper state tracking
        };

        // Get pending tasks - abort if we can't retrieve them
        let Ok(tasks) = self
            .storage
            .get_pending_tasks(assignment_uuid)
            .await
            .inspect_err(|e| {
                tracing::error!(
                    assignment_id = %assignment_uuid,
                    error = %e,
                    "Failed to get pending tasks, aborting"
                );
            })
        else {
            return; // Cannot proceed without task list
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

        // Wait for all tasks to complete - continue even if some panic
        for handle in handles {
            // Task panics are logged but don't stop processing of other tasks
            let _ = handle.await.inspect_err(|e| {
                tracing::error!(error = %e, "Task panicked, continuing with remaining tasks");
            });
        }

        // Mark assignment as complete - best effort, log if it fails
        let _ = self
            .storage
            .set_state(assignment_uuid, "complete")
            .await
            .inspect_err(|e| {
                tracing::error!(
                    assignment_id = %assignment_uuid,
                    error = %e,
                    "Failed to set assignment state to complete"
                );
            });

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
        // This should only fail if the semaphore is closed, which indicates shutdown
        let _permit = match self.semaphore.acquire().await {
            Ok(permit) => permit,
            Err(_) => {
                tracing::debug!(
                    assignment_id = %assignment_uuid,
                    object_id = %task.object_id,
                    "Semaphore closed, skipping task (likely shutdown)"
                );
                return;
            }
        };

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
                // Best effort to record completion - task is already done regardless
                let _ = self
                    .storage
                    .mark_task_complete(assignment_uuid, &task.object_id)
                    .await
                    .inspect_err(|e| {
                        tracing::error!(
                            assignment_id = %assignment_uuid,
                            object_id = %task.object_id,
                            error = %e,
                            "Failed to mark task complete in DB"
                        );
                    });
            }
            Err(reason) => {
                tracing::warn!(
                    assignment_id = %assignment_uuid,
                    object_id = %task.object_id,
                    reason = ?reason,
                    "Task failed"
                );
                // Best effort to record failure - task outcome is already determined
                let _ = self
                    .storage
                    .mark_task_failed(assignment_uuid, &task.object_id, &reason)
                    .await
                    .inspect_err(|e| {
                        tracing::error!(
                            assignment_id = %assignment_uuid,
                            object_id = %task.object_id,
                            error = %e,
                            "Failed to mark task failed in DB"
                        );
                    });
            }
        }
    }

    /// Download an object and verify its checksum
    ///
    /// This method implements several optimizations from the legacy agent:
    /// 1. Skip download if file already exists with correct MD5 (CRIT-9)
    /// 2. Download to temporary file first (CRIT-11)
    /// 3. Atomically rename to final path only after MD5 verification (CRIT-11)
    async fn download_and_verify(&self, task: &Task) -> Result<(), ObjectSkippedReason> {
        // Determine paths using the Manta path structure: /manta/{owner}/{object_id}
        let dest_path = self.config.manta_file_path(&task.owner, &task.object_id);
        let tmp_path = self.config.manta_tmp_path(&task.owner, &task.object_id);
        let dest_dir = dest_path.parent().unwrap();

        // CRIT-9: Check if file already exists with correct checksum
        // This avoids unnecessary re-downloads after agent restart or for retried assignments
        if dest_path.exists() {
            match self.compute_file_md5(&dest_path).await {
                Ok(existing_md5) if existing_md5 == task.md5sum => {
                    tracing::info!(
                        object_id = %task.object_id,
                        owner = %task.owner,
                        "File already exists with correct checksum, skipping download"
                    );
                    return Ok(());
                }
                Ok(existing_md5) => {
                    tracing::debug!(
                        object_id = %task.object_id,
                        expected = %task.md5sum,
                        existing = %existing_md5,
                        "Existing file has wrong checksum, will re-download"
                    );
                }
                Err(e) => {
                    tracing::debug!(
                        object_id = %task.object_id,
                        error = %e,
                        "Could not read existing file, will re-download"
                    );
                }
            }
        }

        // Ensure destination directory exists
        if let Err(e) = fs::create_dir_all(dest_dir).await {
            tracing::error!(
                path = %dest_dir.display(),
                error = %e,
                "Failed to create destination directory"
            );
            return Err(ObjectSkippedReason::AgentFSError);
        }

        // Build the URL for the object
        // Format: http://{storage_id}/{owner}/{object_id}
        let url = format!(
            "http://{}/{}/{}",
            task.source.manta_storage_id, task.owner, task.object_id
        );

        // Download the object
        let response = self.client.get(&url).send().await.map_err(|e| {
            tracing::debug!(url = %url, error = %e, "HTTP request failed");
            ObjectSkippedReason::NetworkError
        })?;

        // Check HTTP status
        let status = response.status();
        if !status.is_success() {
            tracing::debug!(url = %url, status = %status, "HTTP error response");
            return Err(ObjectSkippedReason::HTTPStatusCode(status.as_u16()));
        }

        // CRIT-11: Stream the response body to a TEMPORARY file while computing MD5
        // This ensures we never have a partial file at the final destination
        let computed_md5 = self
            .stream_to_file_with_md5(response, &tmp_path)
            .await
            .map_err(|e| {
                tracing::error!(
                    path = %tmp_path.display(),
                    error = %e,
                    "Failed to write object to temp file"
                );
                // Clean up the temp file on error
                let tmp_path_clone = tmp_path.clone();
                tokio::spawn(async move {
                    let _ = fs::remove_file(&tmp_path_clone).await;
                });
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
            // Remove the corrupted temp file
            if let Err(e) = fs::remove_file(&tmp_path).await {
                tracing::error!(
                    object_id = %task.object_id,
                    path = %tmp_path.display(),
                    error = %e,
                    "Failed to remove corrupted temp file after MD5 mismatch"
                );
            }
            return Err(ObjectSkippedReason::MD5Mismatch);
        }

        // CRIT-11: Atomically rename temp file to final destination
        // This ensures the file at dest_path is always complete and verified
        if let Err(e) = fs::rename(&tmp_path, &dest_path).await {
            tracing::error!(
                object_id = %task.object_id,
                tmp_path = %tmp_path.display(),
                dest_path = %dest_path.display(),
                error = %e,
                "Failed to rename temp file to final destination"
            );
            // Clean up the temp file
            let _ = fs::remove_file(&tmp_path).await;
            return Err(ObjectSkippedReason::AgentFSError);
        }

        tracing::debug!(
            object_id = %task.object_id,
            path = %dest_path.display(),
            "Object downloaded and verified successfully"
        );

        Ok(())
    }

    /// Compute the MD5 checksum of an existing file
    async fn compute_file_md5(&self, path: &PathBuf) -> std::io::Result<String> {
        use tokio::io::AsyncReadExt;

        let mut file = File::open(path).await?;
        let mut hasher = Md5::new();
        let mut buffer = vec![0u8; 64 * 1024]; // 64KB buffer

        loop {
            let bytes_read = file.read(&mut buffer).await?;
            if bytes_read == 0 {
                break;
            }
            hasher.update(&buffer[..bytes_read]);
        }

        let hash = hasher.finalize();
        Ok(base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            hash,
        ))
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
            let chunk = chunk_result.map_err(|e| std::io::Error::other(e.to_string()))?;
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
