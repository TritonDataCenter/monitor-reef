// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2020 Joyent, Inc.
// Copyright 2026 Edgecast Cloud LLC.

//! API context for the rebalancer manager

use std::sync::Arc;

use anyhow::Result;

use rebalancer_types::{
    EvacuateJobPayload, EvacuateJobUpdateMessage, JobConfigEvacuate, JobDbEntry, JobPayload,
    JobState, JobStatus, JobStatusConfig, JobStatusResults, StorageNode,
};

use crate::config::ManagerConfig;
use crate::db::{Database, DbError};
use crate::storinfo::StorinfoClient;

/// API context shared across all request handlers
pub struct ApiContext {
    db: Arc<Database>,
    storinfo: Arc<StorinfoClient>,
    #[allow(dead_code)]
    config: ManagerConfig,
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

        // Do initial refresh of storinfo cache
        if let Err(e) = storinfo.refresh().await {
            tracing::warn!(error = %e, "Failed initial storinfo refresh (will retry later)");
        }

        Ok(Self {
            db,
            storinfo,
            config,
        })
    }

    /// Create a new job
    pub async fn create_job(&self, payload: JobPayload) -> Result<String, DbError> {
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

        // TODO: Start job processing in background
        // For now, jobs are created but not automatically started
        // This will be implemented when we add the job processor/state machine

        tracing::info!(
            job_id = %job_id,
            from_shark = %params.from_shark,
            "Created evacuate job"
        );

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

        // TODO: Actually apply the update to the running job
        // This requires integration with the job processor
        tracing::info!(
            job_id = %uuid,
            update = ?msg,
            "Job update requested (not yet implemented)"
        );

        Ok(())
    }

    /// Retry a failed job
    pub async fn retry_job(&self, uuid: &str) -> Result<String, DbError> {
        let id = uuid::Uuid::parse_str(uuid)
            .map_err(|e| DbError::Query(format!("Invalid UUID: {}", e)))?;

        let row = self.db.get_job(&id).await?;

        // Only allow retry from failed state
        if row.state != "failed" {
            return Err(DbError::CannotRetry(format!(
                "Job is in '{}' state, not 'failed'",
                row.state
            )));
        }

        // Create a new job with the same parameters
        let new_id = uuid::Uuid::new_v4();
        let job_id = self
            .db
            .create_evacuate_job(
                new_id,
                &row.from_shark.unwrap_or_default(),
                &row.from_shark_datacenter.unwrap_or_default(),
                row.max_objects.map(|n| n as u32),
            )
            .await?;

        // TODO: Link the new job to the old one for tracking
        // TODO: Start job processing

        tracing::info!(
            original_job_id = %uuid,
            new_job_id = %job_id,
            "Created retry job"
        );

        Ok(job_id)
    }
}
