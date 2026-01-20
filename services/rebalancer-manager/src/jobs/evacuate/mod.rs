// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2020 Joyent, Inc.
// Copyright 2026 Edgecast Cloud LLC.

//! Evacuate job implementation
//!
//! This module implements the evacuation of objects from a storage node being
//! decommissioned. The process involves:
//!
//! 1. Discovering objects on the source storage node (via sharkspotter or local DB)
//! 2. Creating assignments that group objects by destination shark
//! 3. Posting assignments to rebalancer agents on destination sharks
//! 4. Monitoring assignment progress and handling completions
//! 5. Updating metadata in Moray after successful object transfers
//!
//! The job runs as a set of coordinated async tasks communicating via channels.

mod agent;
mod assignment;
mod db;
mod types;

pub use types::{EvacuateObject, EvacuateObjectError, EvacuateObjectStatus};

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, watch, RwLock};
use tracing::{debug, error, info, warn};

use rebalancer_types::StorageNode;

use super::JobError;
use crate::storinfo::StorinfoClient;

use assignment::{Assignment, AssignmentCacheEntry, AssignmentState};
use db::EvacuateDb;

/// Configuration for an evacuate job
#[derive(Clone)]
pub struct EvacuateConfig {
    /// Maximum number of tasks per assignment
    pub max_tasks_per_assignment: usize,
    /// Maximum age of an assignment before forcing flush (seconds)
    pub max_assignment_age_secs: u64,
    /// Minimum available MB for a shark to be a destination
    pub min_avail_mb: u64,
    /// Maximum objects to process (for testing, None = unlimited)
    pub max_objects: Option<u32>,
    /// Agent HTTP request timeout
    pub agent_timeout_secs: u64,
}

impl Default for EvacuateConfig {
    fn default() -> Self {
        Self {
            max_tasks_per_assignment: 200,
            max_assignment_age_secs: 300, // 5 minutes
            min_avail_mb: 1000,           // 1GB minimum
            max_objects: None,
            agent_timeout_secs: 30,
        }
    }
}

/// Assignment ID type alias
pub type AssignmentId = String;

/// Hash of assignments in progress
pub type AssignmentCache = HashMap<AssignmentId, AssignmentCacheEntry>;

/// Messages for assignment workers
#[derive(Debug)]
enum AssignmentMsg {
    Object(EvacuateObject),
    Flush,
    Stop,
}

/// Messages for metadata update
#[derive(Debug)]
enum MetadataUpdateMsg {
    Assignment(AssignmentCacheEntry),
    Stop,
}

/// Evacuate job state machine
pub struct EvacuateJob {
    /// Job UUID (same as database name)
    job_id: String,

    /// Source storage node being evacuated
    from_shark: StorageNode,

    /// Configuration
    config: EvacuateConfig,

    /// Database connection for evacuate objects
    db: Arc<EvacuateDb>,

    /// Cache of in-progress assignments
    assignments: Arc<RwLock<AssignmentCache>>,

    /// Destination shark availability info
    dest_sharks: Arc<RwLock<HashMap<String, DestSharkInfo>>>,

    /// HTTP client for agent communication
    http_client: reqwest::Client,

    /// Storinfo client for shark discovery
    storinfo: Arc<StorinfoClient>,

    /// Shutdown signal
    shutdown_tx: watch::Sender<bool>,
}

/// Information about a destination shark
#[derive(Debug, Clone)]
struct DestSharkInfo {
    node: StorageNode,
    available_mb: u64,
    assigned_mb: u64,
}

impl EvacuateJob {
    /// Create a new evacuate job
    pub async fn new(
        job_id: String,
        from_shark: StorageNode,
        storinfo: Arc<StorinfoClient>,
        config: EvacuateConfig,
        database_url: &str,
    ) -> Result<Self, JobError> {
        let db = EvacuateDb::new(&job_id, database_url).await?;

        let http_client = reqwest::Client::builder()
            .timeout(Duration::from_secs(config.agent_timeout_secs))
            .build()?;

        let (shutdown_tx, _) = watch::channel(false);

        Ok(Self {
            job_id,
            from_shark,
            config,
            db: Arc::new(db),
            assignments: Arc::new(RwLock::new(HashMap::new())),
            dest_sharks: Arc::new(RwLock::new(HashMap::new())),
            http_client,
            storinfo,
            shutdown_tx,
        })
    }

    /// Run the evacuate job
    ///
    /// This is the main entry point that orchestrates all the workers.
    pub async fn run(self: Arc<Self>) -> Result<(), JobError> {
        info!(
            job_id = %self.job_id,
            from_shark = %self.from_shark.manta_storage_id,
            "Starting evacuate job"
        );

        // Create channels for worker communication
        let (object_tx, object_rx) = mpsc::channel::<EvacuateObject>(100);
        let (assignment_tx, assignment_rx) = mpsc::channel::<Assignment>(10);
        let (md_update_tx, md_update_rx) = mpsc::channel::<AssignmentCacheEntry>(10);

        // Subscribe to shutdown signal
        let _shutdown_rx = self.shutdown_tx.subscribe();

        // Refresh storinfo to get available sharks
        self.storinfo.refresh().await.map_err(|e| {
            JobError::Internal(format!("Failed to refresh storinfo: {}", e))
        })?;

        // Start workers
        let job = Arc::clone(&self);
        let assignment_poster = tokio::spawn({
            let job = Arc::clone(&job);
            async move { job.assignment_poster(assignment_rx).await }
        });

        let job = Arc::clone(&self);
        let assignment_checker = tokio::spawn({
            let job = Arc::clone(&job);
            let md_tx = md_update_tx.clone();
            async move { job.assignment_checker(md_tx).await }
        });

        let job = Arc::clone(&self);
        let metadata_updater = tokio::spawn({
            let job = Arc::clone(&job);
            async move { job.metadata_update_broker(md_update_rx).await }
        });

        let job = Arc::clone(&self);
        let assignment_manager = tokio::spawn({
            let job = Arc::clone(&job);
            async move { job.assignment_manager(object_rx, assignment_tx).await }
        });

        // For now, we don't have sharkspotter integrated, so we'll read from
        // the local database for retry jobs or simulate for testing.
        // In a full implementation, this would spawn a sharkspotter task.
        drop(object_tx); // Close the object channel to signal end of input

        // Wait for assignment manager to complete
        match assignment_manager.await {
            Ok(Ok(())) => debug!("Assignment manager completed"),
            Ok(Err(e)) => {
                error!("Assignment manager error: {}", e);
                self.shutdown_tx.send(true).ok();
            }
            Err(e) => {
                error!("Assignment manager panicked: {}", e);
                self.shutdown_tx.send(true).ok();
            }
        }

        // Signal shutdown and wait for other workers
        self.shutdown_tx.send(true).ok();
        drop(md_update_tx); // Close metadata channel

        // Wait for workers to complete
        let _ = assignment_poster.await;
        let _ = assignment_checker.await;
        let _ = metadata_updater.await;

        info!(job_id = %self.job_id, "Evacuate job completed");
        Ok(())
    }

    /// Assignment manager worker
    ///
    /// Receives objects and assigns them to destination sharks, creating
    /// assignments that are sent to the poster.
    async fn assignment_manager(
        &self,
        mut object_rx: mpsc::Receiver<EvacuateObject>,
        assignment_tx: mpsc::Sender<Assignment>,
    ) -> Result<(), JobError> {
        info!("Assignment manager started");

        // Track assignments per destination shark
        let mut shark_assignments: HashMap<String, Assignment> = HashMap::new();

        while let Some(mut eobj) = object_rx.recv().await {
            // Find a destination shark for this object
            let dest_shark = match self.select_destination(&eobj).await {
                Ok(Some(shark)) => shark,
                Ok(None) => {
                    // No suitable destination, skip the object
                    self.db
                        .mark_object_skipped(
                            &eobj.id,
                            rebalancer_types::ObjectSkippedReason::DestinationUnreachable,
                        )
                        .await?;
                    continue;
                }
                Err(e) => {
                    warn!("Error selecting destination for {}: {}", eobj.id, e);
                    continue;
                }
            };

            let dest_id = dest_shark.manta_storage_id.clone();
            eobj.dest_shark = dest_id.clone();

            // Get or create assignment for this shark
            let assignment = shark_assignments
                .entry(dest_id.clone())
                .or_insert_with(|| Assignment::new(dest_shark.clone()));

            // Add object to assignment
            assignment.add_task(&eobj);
            eobj.assignment_id = assignment.id.clone();
            eobj.status = EvacuateObjectStatus::Assigned;

            // Store in local DB
            self.db.insert_object(&eobj).await?;

            // Check if assignment is full
            if assignment.tasks.len() >= self.config.max_tasks_per_assignment {
                let full_assignment = shark_assignments.remove(&dest_id).ok_or_else(|| {
                    JobError::Internal("Assignment disappeared".to_string())
                })?;

                // Cache the assignment
                {
                    let mut cache = self.assignments.write().await;
                    cache.insert(full_assignment.id.clone(), full_assignment.clone().into());
                }

                // Send to poster
                if assignment_tx.send(full_assignment).await.is_err() {
                    warn!("Assignment poster channel closed");
                    break;
                }
            }
        }

        // Flush remaining assignments
        for (_, assignment) in shark_assignments {
            if !assignment.tasks.is_empty() {
                {
                    let mut cache = self.assignments.write().await;
                    cache.insert(assignment.id.clone(), assignment.clone().into());
                }
                if assignment_tx.send(assignment).await.is_err() {
                    warn!("Assignment poster channel closed during flush");
                }
            }
        }

        info!("Assignment manager completed");
        Ok(())
    }

    /// Assignment poster worker
    ///
    /// Posts assignments to rebalancer agents on destination sharks.
    async fn assignment_poster(
        &self,
        mut assignment_rx: mpsc::Receiver<Assignment>,
    ) -> Result<(), JobError> {
        info!("Assignment poster started");

        while let Some(assignment) = assignment_rx.recv().await {
            match self.post_assignment(&assignment).await {
                Ok(()) => {
                    debug!(
                        assignment_id = %assignment.id,
                        dest = %assignment.dest_shark.manta_storage_id,
                        tasks = assignment.tasks.len(),
                        "Assignment posted successfully"
                    );

                    // Update assignment state
                    let mut cache = self.assignments.write().await;
                    if let Some(entry) = cache.get_mut(&assignment.id) {
                        entry.state = AssignmentState::Assigned;
                    }
                }
                Err(e) => {
                    error!(
                        assignment_id = %assignment.id,
                        error = %e,
                        "Failed to post assignment"
                    );

                    // Mark assignment as rejected
                    let mut cache = self.assignments.write().await;
                    if let Some(entry) = cache.get_mut(&assignment.id) {
                        entry.state = AssignmentState::Rejected;
                    }

                    // Mark objects as skipped
                    for task in assignment.tasks.values() {
                        self.db
                            .mark_object_skipped(
                                &task.object_id,
                                rebalancer_types::ObjectSkippedReason::AssignmentRejected,
                            )
                            .await?;
                    }
                }
            }
        }

        info!("Assignment poster completed");
        Ok(())
    }

    /// Assignment checker worker
    ///
    /// Periodically checks on assigned assignments and processes completions.
    async fn assignment_checker(
        &self,
        md_update_tx: mpsc::Sender<AssignmentCacheEntry>,
    ) -> Result<(), JobError> {
        info!("Assignment checker started");

        let mut shutdown_rx = self.shutdown_tx.subscribe();
        let check_interval = Duration::from_secs(5);

        loop {
            tokio::select! {
                _ = tokio::time::sleep(check_interval) => {}
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        // Check one more time for any remaining assignments
                        self.check_assignments_once(&md_update_tx).await;
                        break;
                    }
                }
            }

            self.check_assignments_once(&md_update_tx).await;
        }

        info!("Assignment checker completed");
        Ok(())
    }

    /// Check all assigned assignments once
    async fn check_assignments_once(&self, md_update_tx: &mpsc::Sender<AssignmentCacheEntry>) {
        // Get a snapshot of assignments to check
        let assignments: Vec<_> = {
            let cache = self.assignments.read().await;
            cache
                .values()
                .filter(|a| a.state == AssignmentState::Assigned)
                .cloned()
                .collect()
        };

        for ace in assignments {
            match self.check_assignment(&ace).await {
                Ok(Some(completed)) => {
                    // Assignment is complete, update state and send to metadata updater
                    {
                        let mut cache = self.assignments.write().await;
                        if let Some(entry) = cache.get_mut(&ace.id) {
                            entry.state = AssignmentState::AgentComplete;
                        }
                    }

                    // Process the completed assignment
                    if let Err(e) = self.process_completed_assignment(&completed).await {
                        error!(
                            assignment_id = %ace.id,
                            error = %e,
                            "Failed to process completed assignment"
                        );
                    }

                    // Send to metadata updater
                    if md_update_tx.send(ace.clone()).await.is_err() {
                        warn!("Metadata update channel closed");
                    }
                }
                Ok(None) => {
                    // Assignment still in progress
                    debug!(assignment_id = %ace.id, "Assignment still in progress");
                }
                Err(e) => {
                    warn!(assignment_id = %ace.id, error = %e, "Error checking assignment");
                }
            }
        }
    }

    /// Metadata update broker worker
    ///
    /// Updates object metadata in Moray after successful transfers.
    async fn metadata_update_broker(
        &self,
        mut md_rx: mpsc::Receiver<AssignmentCacheEntry>,
    ) -> Result<(), JobError> {
        info!("Metadata update broker started");

        while let Some(ace) = md_rx.recv().await {
            // Get objects from this assignment that need metadata updates
            let objects = self.db.get_assignment_objects(&ace.id).await?;

            for obj in objects {
                if obj.status == EvacuateObjectStatus::PostProcessing {
                    match self.update_object_metadata(&obj).await {
                        Ok(()) => {
                            self.db.mark_object_complete(&obj.id).await?;
                        }
                        Err(e) => {
                            error!(
                                object_id = %obj.id,
                                error = %e,
                                "Failed to update metadata"
                            );
                            self.db
                                .mark_object_error(&obj.id, EvacuateObjectError::MetadataUpdateFailed)
                                .await?;
                        }
                    }
                }
            }

            // Mark assignment as fully processed
            {
                let mut cache = self.assignments.write().await;
                if let Some(entry) = cache.get_mut(&ace.id) {
                    entry.state = AssignmentState::PostProcessed;
                }
            }
        }

        info!("Metadata update broker completed");
        Ok(())
    }

    /// Select a destination shark for an object
    async fn select_destination(
        &self,
        _eobj: &EvacuateObject,
    ) -> Result<Option<StorageNode>, JobError> {
        // For now, return None - full implementation would:
        // 1. Get available sharks from storinfo
        // 2. Filter out the source shark
        // 3. Filter out sharks in the same datacenter (for cross-DC replication)
        // 4. Select shark with most available space
        //
        // This is a placeholder for the full shark selection logic
        Ok(None)
    }

    /// Post an assignment to an agent
    async fn post_assignment(&self, assignment: &Assignment) -> Result<(), JobError> {
        let agent_url = format!(
            "http://{}/assignments",
            assignment.dest_shark.manta_storage_id
        );

        let payload = assignment.to_payload();

        let response = self
            .http_client
            .post(&agent_url)
            .json(&payload)
            .send()
            .await?;

        if response.status().is_success() {
            Ok(())
        } else {
            Err(JobError::AgentUnavailable(format!(
                "Agent returned status {}",
                response.status()
            )))
        }
    }

    /// Check assignment status from agent
    async fn check_assignment(
        &self,
        ace: &AssignmentCacheEntry,
    ) -> Result<Option<rebalancer_types::Assignment>, JobError> {
        let agent_url = format!(
            "http://{}/assignments/{}",
            ace.dest_shark.manta_storage_id, ace.id
        );

        let response = self.http_client.get(&agent_url).send().await?;

        if !response.status().is_success() {
            return Err(JobError::AgentUnavailable(format!(
                "Agent returned status {}",
                response.status()
            )));
        }

        let agent_assignment: rebalancer_types::Assignment = response.json().await?;

        // Check if complete
        match &agent_assignment.stats.state {
            rebalancer_types::AgentAssignmentState::Complete(_) => Ok(Some(agent_assignment)),
            _ => Ok(None),
        }
    }

    /// Process a completed assignment from the agent
    async fn process_completed_assignment(
        &self,
        agent_assignment: &rebalancer_types::Assignment,
    ) -> Result<(), JobError> {
        // Update local DB based on agent's task results
        if let rebalancer_types::AgentAssignmentState::Complete(Some(failed_tasks)) =
            &agent_assignment.stats.state
        {
            // Mark failed tasks
            for task in failed_tasks {
                if let rebalancer_types::TaskStatus::Failed(reason) = &task.status {
                    self.db.mark_object_skipped(&task.object_id, reason.clone()).await?;
                }
            }
        }

        // Mark successful tasks as ready for metadata update
        let objects = self.db.get_assignment_objects(&agent_assignment.uuid).await?;
        for obj in objects {
            if obj.status == EvacuateObjectStatus::Assigned {
                self.db.set_object_status(&obj.id, EvacuateObjectStatus::PostProcessing).await?;
            }
        }

        Ok(())
    }

    /// Update object metadata in Moray
    async fn update_object_metadata(&self, _obj: &EvacuateObject) -> Result<(), JobError> {
        // This would:
        // 1. Connect to the appropriate Moray shard
        // 2. Read the current object metadata
        // 3. Update the sharks array to replace the old shark with the new one
        // 4. Write the updated metadata back
        //
        // For now, this is a placeholder
        Ok(())
    }

    /// Shutdown the job gracefully
    #[allow(dead_code)]
    pub fn shutdown(&self) {
        info!(job_id = %self.job_id, "Shutting down evacuate job");
        self.shutdown_tx.send(true).ok();
    }
}
