// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2020 Joyent, Inc.
// Copyright 2026 Edgecast Cloud LLC.

//! API context for the rebalancer manager

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::{RwLock, watch};

use rebalancer_types::{
    EvacuateJobPayload, EvacuateJobUpdateMessage, JobConfigEvacuate, JobDbEntry, JobPayload,
    JobState, JobStatus, JobStatusConfig, JobStatusResults, StorageNode,
};

use crate::config::ManagerConfig;
use crate::db::{Database, DbError};
use crate::jobs::evacuate::{EvacuateConfig, EvacuateJob, ObjectSource};
use crate::storinfo::StorinfoClient;

/// Registry of running jobs and their update channels
type JobUpdateRegistry =
    Arc<RwLock<HashMap<uuid::Uuid, watch::Sender<Option<EvacuateJobUpdateMessage>>>>>;

/// API context shared across all request handlers
pub struct ApiContext {
    db: Arc<Database>,
    storinfo: Arc<StorinfoClient>,
    config: ManagerConfig,
    /// Registry of running jobs with their update channels
    job_updates: JobUpdateRegistry,
}

impl ApiContext {
    /// Create a new API context
    pub async fn new(config: ManagerConfig) -> Result<Self> {
        // Initialize database connection pool
        let db = Arc::new(Database::new(&config.database_url).await?);

        // Initialize Storinfo client
        let storinfo = Arc::new(StorinfoClient::new(
            config.storinfo_url.clone(),
            config.http_timeout_secs,
        )?);

        // Do initial refresh of storinfo cache - non-fatal, background task will retry
        let _ = storinfo.refresh().await.inspect_err(|e| {
            tracing::warn!(error = %e, "Failed initial storinfo refresh, background task will retry");
        });

        Ok(Self {
            db,
            storinfo,
            config,
            job_updates: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    /// Create a new job
    ///
    /// Returns an error if `snaplink_cleanup_required` is set in the configuration,
    /// indicating that snaplink cleanup must be completed before evacuate jobs can run.
    pub async fn create_job(&self, payload: JobPayload) -> Result<String, DbError> {
        // IMP-16: Check if snaplink cleanup is required before allowing job creation
        if self.config.snaplink_cleanup_required {
            return Err(DbError::CannotCreate(
                "Snaplink cleanup required - evacuate jobs cannot be created until snaplink \
                 cleanup is complete. Set SNAPLINK_CLEANUP_REQUIRED=false after cleanup."
                    .to_string(),
            ));
        }

        match payload {
            JobPayload::Evacuate(params) => self.create_evacuate_job(params).await,
        }
    }

    /// Create a new evacuate job
    async fn create_evacuate_job(&self, params: EvacuateJobPayload) -> Result<String, DbError> {
        let id = uuid::Uuid::new_v4();

        // Try to look up the datacenter from storinfo
        let datacenter = match self.storinfo.get_node(&params.from_shark).await {
            Ok(node) => node.datacenter,
            Err(_) => {
                tracing::warn!(
                    from_shark = %params.from_shark,
                    "Could not find shark in storinfo, using 'unknown' datacenter"
                );
                "unknown".to_string()
            }
        };

        let job_id = self
            .db
            .create_evacuate_job(id, &params.from_shark, &datacenter, params.max_objects)
            .await?;

        tracing::info!(
            job_id = %job_id,
            from_shark = %params.from_shark,
            "Created evacuate job"
        );

        // Build the from_shark StorageNode
        let from_shark = StorageNode {
            manta_storage_id: params.from_shark.clone(),
            datacenter,
        };

        // Create evacuate config
        let evacuate_config = EvacuateConfig {
            max_objects: params.max_objects,
            blacklist_datacenters: self.config.blacklist_datacenters.clone(),
            ..Default::default()
        };

        // Create update channel for runtime configuration changes
        let (update_tx, update_rx) = watch::channel(None);

        // Register in job updates registry
        {
            let mut registry = self.job_updates.write().await;
            registry.insert(id, update_tx);
        }

        // Spawn job in background
        let storinfo = Arc::clone(&self.storinfo);
        let manager_db = Arc::clone(&self.db);
        let job_updates = Arc::clone(&self.job_updates);
        let database_url = self.config.database_url.clone();
        let job_id_clone = job_id.clone();
        let job_uuid = id;
        tokio::spawn(async move {
            match EvacuateJob::new(
                job_id_clone.clone(),
                job_uuid,
                from_shark,
                storinfo,
                evacuate_config,
                Arc::clone(&manager_db),
                &database_url,
                update_rx,
            )
            .await
            {
                Ok(job) => {
                    let job = Arc::new(job);
                    if let Err(e) = job.run().await {
                        tracing::error!(job_id = %job_id_clone, error = %e, "Evacuate job failed");
                        let _ = manager_db.update_job_state(&job_uuid, "failed").await.inspect_err(|db_err| {
                            tracing::error!(job_id = %job_id_clone, error = %db_err, "Failed to update job state to failed");
                        });
                    }
                }
                Err(e) => {
                    tracing::error!(job_id = %job_id_clone, error = %e, "Failed to initialize evacuate job");
                    let _ = manager_db.update_job_state(&job_uuid, "failed").await.inspect_err(|db_err| {
                        tracing::error!(job_id = %job_id_clone, error = %db_err, "Failed to update job state to failed");
                    });
                }
            }

            // Clean up from registry when job completes
            let mut registry = job_updates.write().await;
            registry.remove(&job_uuid);
            tracing::debug!(job_id = %job_id_clone, "Removed job from update registry");
        });

        Ok(job_id)
    }

    /// List jobs
    pub async fn list_jobs(&self) -> Result<Vec<JobDbEntry>, DbError> {
        self.db.list_jobs().await
    }

    /// Get job status
    pub async fn get_job_status(&self, uuid: &str) -> Result<JobStatus, DbError> {
        let id = uuid::Uuid::parse_str(uuid)
            .map_err(|e| DbError::Query(format!("Invalid UUID: {}", e)))?;

        let row = self.db.get_job(&id).await?;

        // Check if job is still initializing
        if row.state == "init" {
            return Err(DbError::Query("Job is still initializing".to_string()));
        }

        let results = self.db.get_job_results(&id).await?;

        // Build config based on job action
        let config = match row.action.as_str() {
            "evacuate" => {
                let from_shark = StorageNode {
                    manta_storage_id: row.from_shark.unwrap_or_default(),
                    datacenter: row.from_shark_datacenter.unwrap_or_default(),
                };
                JobStatusConfig::Evacuate(JobConfigEvacuate { from_shark })
            }
            _ => {
                return Err(DbError::Query(format!("Unknown action: {}", row.action)));
            }
        };

        let state = match row.state.as_str() {
            "init" => JobState::Init,
            "setup" => JobState::Setup,
            "running" => JobState::Running,
            "stopped" => JobState::Stopped,
            "complete" => JobState::Complete,
            "failed" => JobState::Failed,
            _ => JobState::Init,
        };

        Ok(JobStatus {
            config,
            results: JobStatusResults::Evacuate(results),
            state,
        })
    }

    /// Update a running job
    ///
    /// Sends a configuration update message to the running job. Currently supports:
    /// - `SetMetadataThreads`: Adjusts the number of metadata update threads
    pub async fn update_job(
        &self,
        uuid: &str,
        msg: EvacuateJobUpdateMessage,
    ) -> Result<(), DbError> {
        let id = uuid::Uuid::parse_str(uuid)
            .map_err(|e| DbError::Query(format!("Invalid UUID: {}", e)))?;

        let row = self.db.get_job(&id).await?;

        // Only allow updates when job is running
        if row.state != "running" {
            return Err(DbError::CannotUpdate(format!(
                "Job is in '{}' state, not 'running'",
                row.state
            )));
        }

        // Validate the message
        msg.validate()
            .map_err(|e| DbError::CannotUpdate(format!("Invalid update: {}", e)))?;

        // Send update through the channel
        let registry = self.job_updates.read().await;
        if let Some(tx) = registry.get(&id) {
            // Send the update - watch::Sender::send() never fails unless there are no receivers,
            // which shouldn't happen for a running job
            let _ = tx.send(Some(msg.clone()));
            tracing::info!(
                job_id = %uuid,
                update = ?msg,
                "Job update sent"
            );
        } else {
            // Job exists in DB but not in registry - it may have just completed
            return Err(DbError::CannotUpdate(
                "Job is not currently running (may have just completed)".to_string(),
            ));
        }

        Ok(())
    }

    /// Retry a failed job
    ///
    /// Creates a new job that reads retryable objects from the original job's
    /// database and reprocesses them.
    pub async fn retry_job(&self, uuid: &str) -> Result<String, DbError> {
        let original_id = uuid::Uuid::parse_str(uuid)
            .map_err(|e| DbError::Query(format!("Invalid UUID: {}", e)))?;

        let row = self.db.get_job(&original_id).await?;

        // Only allow retry from failed state
        if row.state != "failed" {
            return Err(DbError::CannotRetry(format!(
                "Job is in '{}' state, not 'failed'",
                row.state
            )));
        }

        // Create a new job with the same parameters
        let new_id = uuid::Uuid::new_v4();
        let from_shark = row.from_shark.clone().unwrap_or_default();
        let datacenter = row.from_shark_datacenter.clone().unwrap_or_default();
        let max_objects = row.max_objects.map(|n| n as u32);

        let job_id = self
            .db
            .create_evacuate_job(new_id, &from_shark, &datacenter, max_objects)
            .await?;

        tracing::info!(
            original_job_id = %uuid,
            new_job_id = %job_id,
            "Created retry job, spawning execution"
        );

        // Build the from_shark StorageNode
        let from_shark_node = StorageNode {
            manta_storage_id: from_shark,
            datacenter,
        };

        // Create evacuate config for retry job
        // Use ObjectSource::LocalDb with source_job_id to read from original job's database
        let evacuate_config = EvacuateConfig {
            max_objects,
            object_source: ObjectSource::LocalDb,
            source_job_id: Some(uuid.to_string()),
            blacklist_datacenters: self.config.blacklist_datacenters.clone(),
            ..Default::default()
        };

        // Create update channel for runtime configuration changes
        let (update_tx, update_rx) = watch::channel(None);

        // Register in job updates registry
        {
            let mut registry = self.job_updates.write().await;
            registry.insert(new_id, update_tx);
        }

        // Spawn job in background
        let storinfo = Arc::clone(&self.storinfo);
        let manager_db = Arc::clone(&self.db);
        let job_updates = Arc::clone(&self.job_updates);
        let database_url = self.config.database_url.clone();
        let job_id_clone = job_id.clone();
        let job_uuid = new_id;
        tokio::spawn(async move {
            match EvacuateJob::new(
                job_id_clone.clone(),
                job_uuid,
                from_shark_node,
                storinfo,
                evacuate_config,
                Arc::clone(&manager_db),
                &database_url,
                update_rx,
            )
            .await
            {
                Ok(job) => {
                    let job = Arc::new(job);
                    if let Err(e) = job.run().await {
                        tracing::error!(job_id = %job_id_clone, error = %e, "Retry job failed");
                        let _ = manager_db
                            .update_job_state(&job_uuid, "failed")
                            .await
                            .inspect_err(|db_err| {
                                tracing::error!(
                                    job_id = %job_id_clone,
                                    error = %db_err,
                                    "Failed to update job state to failed"
                                );
                            });
                    }
                }
                Err(e) => {
                    tracing::error!(job_id = %job_id_clone, error = %e, "Failed to initialize retry job");
                    let _ = manager_db
                        .update_job_state(&job_uuid, "failed")
                        .await
                        .inspect_err(|db_err| {
                            tracing::error!(
                                job_id = %job_id_clone,
                                error = %db_err,
                                "Failed to update job state to failed"
                            );
                        });
                }
            }

            // Clean up from registry when job completes
            let mut registry = job_updates.write().await;
            registry.remove(&job_uuid);
            tracing::debug!(job_id = %job_id_clone, "Removed retry job from update registry");
        });

        Ok(job_id)
    }
}
