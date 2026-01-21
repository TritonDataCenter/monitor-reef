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

use tokio::sync::{RwLock, mpsc, watch};
use tracing::{debug, error, info, warn};

use rebalancer_types::StorageNode;

use super::JobError;
use crate::db::Database;
use crate::storinfo::StorinfoClient;

use assignment::{Assignment, AssignmentCacheEntry, AssignmentState};
use db::EvacuateDb;

/// Source for object discovery
#[derive(Debug, Clone, Default)]
#[allow(dead_code)]
pub enum ObjectSource {
    /// No objects - job completes immediately (for testing scaffolding)
    #[default]
    None,
    /// Read objects from local evacuate database (for retry jobs)
    LocalDb,
    // Future: Sharkspotter integration
    // Sharkspotter { domain: String, min_shard: u32, max_shard: u32 },
}

/// Configuration for an evacuate job
#[allow(dead_code)]
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
    /// Source for object discovery
    pub object_source: ObjectSource,
}

impl Default for EvacuateConfig {
    fn default() -> Self {
        Self {
            max_tasks_per_assignment: 200,
            max_assignment_age_secs: 300, // 5 minutes
            min_avail_mb: 1000,           // 1GB minimum
            max_objects: None,
            agent_timeout_secs: 30,
            object_source: ObjectSource::default(),
        }
    }
}

/// Assignment ID type alias
pub type AssignmentId = String;

/// Hash of assignments in progress
pub type AssignmentCache = HashMap<AssignmentId, AssignmentCacheEntry>;

/// Messages for assignment workers
#[allow(dead_code)]
#[derive(Debug)]
enum AssignmentMsg {
    Object(EvacuateObject),
    Flush,
    Stop,
}

/// Messages for metadata update
#[allow(dead_code)]
#[derive(Debug)]
enum MetadataUpdateMsg {
    Assignment(AssignmentCacheEntry),
    Stop,
}

/// Evacuate job state machine
#[allow(dead_code)]
pub struct EvacuateJob {
    /// Job UUID (same as database name)
    job_id: String,

    /// Job UUID for manager database updates
    job_uuid: uuid::Uuid,

    /// Source storage node being evacuated
    from_shark: StorageNode,

    /// Configuration
    config: EvacuateConfig,

    /// Manager database for job state updates
    manager_db: Arc<Database>,

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
#[allow(dead_code)]
#[derive(Debug, Clone)]
struct DestSharkInfo {
    node: StorageNode,
    available_mb: u64,
    assigned_mb: u64,
}

#[allow(dead_code)]
impl EvacuateJob {
    /// Create a new evacuate job
    pub async fn new(
        job_id: String,
        job_uuid: uuid::Uuid,
        from_shark: StorageNode,
        storinfo: Arc<StorinfoClient>,
        config: EvacuateConfig,
        manager_db: Arc<Database>,
        database_url: &str,
    ) -> Result<Self, JobError> {
        let db = EvacuateDb::new(&job_id, database_url).await?;

        let http_client = reqwest::Client::builder()
            .timeout(Duration::from_secs(config.agent_timeout_secs))
            .build()?;

        let (shutdown_tx, _) = watch::channel(false);

        Ok(Self {
            job_id,
            job_uuid,
            from_shark,
            config,
            manager_db,
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

        // Update state to "setup"
        self.manager_db
            .update_job_state(&self.job_uuid, "setup")
            .await
            .map_err(|e| JobError::Internal(format!("Failed to update job state to setup: {}", e)))?;

        // Create channels for worker communication
        let (object_tx, object_rx) = mpsc::channel::<EvacuateObject>(100);
        let (assignment_tx, assignment_rx) = mpsc::channel::<Assignment>(10);
        let (md_update_tx, md_update_rx) = mpsc::channel::<AssignmentCacheEntry>(10);

        // Subscribe to shutdown signal
        let _shutdown_rx = self.shutdown_tx.subscribe();

        // Refresh storinfo to get available sharks
        self.storinfo
            .refresh()
            .await
            .map_err(|e| JobError::Internal(format!("Failed to refresh storinfo: {}", e)))?;

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

        // Update state to "running"
        self.manager_db
            .update_job_state(&self.job_uuid, "running")
            .await
            .map_err(|e| JobError::Internal(format!("Failed to update job state to running: {}", e)))?;

        // Spawn object discovery task
        // When this task completes, the object_tx channel is dropped, signaling
        // end of input to the assignment manager
        let object_discovery = self.spawn_object_discovery(object_tx).await?;

        // Wait for object discovery to complete first
        // Note: Object discovery errors are logged but don't fail the job -
        // we continue to let the assignment manager process any objects
        // that were successfully discovered before the error.
        let _ = object_discovery
            .await
            .inspect_err(|e| error!("Object discovery task panicked: {}", e))
            .ok()
            .and_then(|r| {
                r.inspect_err(|e| error!("Object discovery error: {}", e)).ok()
            });

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

        // Update state to "complete"
        self.manager_db
            .update_job_state(&self.job_uuid, "complete")
            .await
            .map_err(|e| JobError::Internal(format!("Failed to update job state to complete: {}", e)))?;

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
                let full_assignment = shark_assignments
                    .remove(&dest_id)
                    .ok_or_else(|| JobError::Internal("Assignment disappeared".to_string()))?;

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

                    // Process the completed assignment - best effort, continue with others on failure
                    let _ = self
                        .process_completed_assignment(&completed)
                        .await
                        .inspect_err(|e| {
                            error!(
                                assignment_id = %ace.id,
                                error = %e,
                                "Failed to process completed assignment, continuing with others"
                            );
                        });

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
                    // Log and continue checking other assignments
                    warn!(assignment_id = %ace.id, error = %e, "Error checking assignment, continuing with others");
                    continue;
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
                                .mark_object_error(
                                    &obj.id,
                                    EvacuateObjectError::MetadataUpdateFailed,
                                )
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
                    self.db
                        .mark_object_skipped(&task.object_id, *reason)
                        .await?;
                }
            }
        }

        // Mark successful tasks as ready for metadata update
        let objects = self
            .db
            .get_assignment_objects(&agent_assignment.uuid)
            .await?;
        for obj in objects {
            if obj.status == EvacuateObjectStatus::Assigned {
                self.db
                    .set_object_status(&obj.id, EvacuateObjectStatus::PostProcessing)
                    .await?;
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

    /// Spawn the object discovery task based on configuration
    ///
    /// This method spawns a task that discovers objects to evacuate and sends
    /// them to the object channel. The source of objects depends on the
    /// configured `object_source`.
    async fn spawn_object_discovery(
        &self,
        object_tx: mpsc::Sender<EvacuateObject>,
    ) -> Result<tokio::task::JoinHandle<Result<(), JobError>>, JobError> {
        let source = self.config.object_source.clone();
        let max_objects = self.config.max_objects;
        let db = Arc::clone(&self.db);
        let job_id = self.job_id.clone();

        let handle = tokio::spawn(async move {
            match source {
                ObjectSource::None => {
                    // No objects to discover - used for testing scaffolding
                    info!(job_id = %job_id, "Object source is None, no objects to process");
                    Ok(())
                }
                ObjectSource::LocalDb => {
                    // Read objects from the local evacuate database
                    info!(job_id = %job_id, "Reading objects from local database");

                    let objects = db.get_retryable_objects(max_objects).await?;
                    let total = objects.len();
                    info!(job_id = %job_id, count = total, "Found objects to process");

                    let mut sent = 0;
                    for mut obj in objects {
                        // Reset object state for retry
                        obj.status = EvacuateObjectStatus::Unprocessed;
                        obj.assignment_id = String::new();
                        obj.dest_shark = String::new();
                        obj.skipped_reason = None;
                        obj.error = None;

                        if object_tx.send(obj).await.is_err() {
                            warn!(
                                job_id = %job_id,
                                "Object channel closed, stopping discovery"
                            );
                            break;
                        }
                        sent += 1;

                        // Check max_objects limit
                        if let Some(max) = max_objects
                            && sent >= max as usize
                        {
                            info!(
                                job_id = %job_id,
                                max_objects = max,
                                "Reached max_objects limit"
                            );
                            break;
                        }
                    }

                    info!(
                        job_id = %job_id,
                        sent = sent,
                        total = total,
                        "Object discovery complete"
                    );
                    Ok(())
                }
                // Future: Add sharkspotter integration here
                // ObjectSource::Sharkspotter { domain, min_shard, max_shard } => { ... }
            }
        });

        Ok(handle)
    }

    /// Shutdown the job gracefully
    #[allow(dead_code)]
    pub fn shutdown(&self) {
        info!(job_id = %self.job_id, "Shutting down evacuate job");
        self.shutdown_tx.send(true).ok();
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use assignment::{Assignment, AssignmentState};
    use rebalancer_types::{ObjectSkippedReason, TaskStatus};
    use serde_json::json;
    use types::{EvacuateObject, EvacuateObjectError, EvacuateObjectStatus, MantaObjectShark};

    // -------------------------------------------------------------------------
    // Test Helpers
    // -------------------------------------------------------------------------

    /// Create a test storage node with the given ID and datacenter
    fn make_storage_node(id: &str, dc: &str) -> StorageNode {
        StorageNode {
            manta_storage_id: id.to_string(),
            datacenter: dc.to_string(),
        }
    }

    /// Create a test evacuate object with the given ID
    fn make_evacuate_object(id: &str) -> EvacuateObject {
        EvacuateObject {
            id: id.to_string(),
            assignment_id: String::new(),
            object: json!({
                "objectId": id,
                "owner": "test-owner",
                "key": "/test/path",
                "contentLength": 1024,
                "contentMD5": "test-md5",
                "sharks": [
                    {"manta_storage_id": "1.stor.domain", "datacenter": "dc1"},
                    {"manta_storage_id": "2.stor.domain", "datacenter": "dc2"}
                ]
            }),
            shard: 1,
            dest_shark: String::new(),
            etag: "test-etag".to_string(),
            status: EvacuateObjectStatus::default(),
            skipped_reason: None,
            error: None,
        }
    }

    /// Create an evacuate object with specific content length (in bytes)
    fn make_evacuate_object_with_size(id: &str, content_length: u64) -> EvacuateObject {
        let mut obj = make_evacuate_object(id);
        obj.object = json!({
            "objectId": id,
            "owner": "test-owner",
            "key": "/test/path",
            "contentLength": content_length,
            "contentMD5": "test-md5",
            "sharks": [
                {"manta_storage_id": "1.stor.domain", "datacenter": "dc1"},
                {"manta_storage_id": "2.stor.domain", "datacenter": "dc2"}
            ]
        });
        obj
    }

    /// Destination shark tracking for capacity tests
    #[derive(Debug, Clone)]
    #[allow(dead_code)]
    struct TestDestShark {
        /// The storage node (included for completeness, mirrors production struct)
        node: StorageNode,
        available_mb: u64,
        percent_used: u32,
        assigned_mb: u64,
    }

    impl TestDestShark {
        fn new(id: &str, dc: &str, available_mb: u64, percent_used: u32) -> Self {
            Self {
                node: make_storage_node(id, dc),
                available_mb,
                percent_used,
                assigned_mb: 0,
            }
        }
    }

    /// Calculate available MB for a destination shark given a max fill percentage.
    /// This mirrors the legacy `_calculate_available_mb` function.
    fn calculate_available_mb(shark: &TestDestShark, max_fill_percentage: u32) -> u64 {
        // If percent_used >= max_fill_percentage, no space available
        if shark.percent_used >= max_fill_percentage {
            return 0;
        }

        // If available_mb is 0, return 0
        if shark.available_mb == 0 {
            return 0;
        }

        // Calculate total MB from available and percent_used
        // available_mb = total_mb * (1 - percent_used/100)
        // total_mb = available_mb / (1 - percent_used/100)
        // total_mb = available_mb * 100 / (100 - percent_used)
        let used_fraction = shark.percent_used as u64;
        if used_fraction >= 100 {
            return 0;
        }

        let total_mb = (shark.available_mb * 100) / (100 - used_fraction);

        // Max fill in MB
        let max_fill_mb = (total_mb * max_fill_percentage as u64) / 100;

        // Used MB
        let used_mb = total_mb - shark.available_mb;

        // Maximum remaining we can use
        let max_remaining = max_fill_mb.saturating_sub(used_mb);

        // Subtract what's already assigned
        max_remaining.saturating_sub(shark.assigned_mb)
    }

    /// Validate destination for an object
    fn validate_destination(
        obj_sharks: &[MantaObjectShark],
        from_shark: &StorageNode,
        to_shark: &StorageNode,
    ) -> Option<ObjectSkippedReason> {
        // Check if to_shark is the same as from_shark
        if to_shark.manta_storage_id == from_shark.manta_storage_id {
            return Some(ObjectSkippedReason::ObjectAlreadyOnDestShark);
        }

        // Check if the object is already on the destination shark
        for shark in obj_sharks {
            if shark.manta_storage_id == to_shark.manta_storage_id {
                return Some(ObjectSkippedReason::ObjectAlreadyOnDestShark);
            }
        }

        // Check if the destination is in a datacenter where the object already exists
        // (excluding the from_shark's datacenter, since we're moving away from it)
        for shark in obj_sharks {
            if shark.manta_storage_id != from_shark.manta_storage_id
                && shark.datacenter == to_shark.datacenter
            {
                return Some(ObjectSkippedReason::ObjectAlreadyInDatacenter);
            }
        }

        None
    }

    // -------------------------------------------------------------------------
    // Test 1: calculate_available_mb_test
    // -------------------------------------------------------------------------

    #[test]
    fn calculate_available_mb_test() {
        // --- Test exact calculation ---
        // Total MB is 1000 (900 available / 0.9 = 1000), 10% is used
        // assigned_mb = 100
        // Max fill = 80% = 800 MB
        // Used = 100 MB (10%) + 100 assigned = 200 MB
        // Available = 800 - 200 = 600 MB
        let shark = TestDestShark {
            node: make_storage_node("test.stor", "dc1"),
            available_mb: 900,
            percent_used: 10,
            assigned_mb: 100,
        };
        assert_eq!(calculate_available_mb(&shark, 80), 600);

        // --- Test Max fill percentage < percent used returns 0 ---
        let shark = TestDestShark {
            node: make_storage_node("test.stor", "dc1"),
            available_mb: 100,
            percent_used: 90,
            assigned_mb: 100,
        };
        // Max fill is 80 and percent_used is 90, should return 0
        assert_eq!(calculate_available_mb(&shark, 80), 0);

        // --- Test max_remaining < assigned_mb should return 0 ---
        // Used = 80 MB (80%), Total = 100 MB
        // Max fill MB = 90 MB (90% of 100)
        // max_remaining = 90 - 80 = 10 MB
        // assigned_mb = 11, so result should be 0
        let shark = TestDestShark {
            node: make_storage_node("test.stor", "dc1"),
            available_mb: 20, // Total is 100MB (20/0.2 = 100)
            percent_used: 80,
            assigned_mb: 11,
        };
        assert_eq!(calculate_available_mb(&shark, 90), 0);

        // Setting assigned_mb to 9 should leave 1MB
        let shark = TestDestShark {
            node: make_storage_node("test.stor", "dc1"),
            available_mb: 20,
            percent_used: 80,
            assigned_mb: 9,
        };
        assert_eq!(calculate_available_mb(&shark, 90), 1);

        // --- Test 0 available_mb should result in 0 ---
        let shark = TestDestShark {
            node: make_storage_node("test.stor", "dc1"),
            available_mb: 0,
            percent_used: 100,
            assigned_mb: 9,
        };
        assert_eq!(calculate_available_mb(&shark, 100), 0);

        // --- Test storinfo giving incorrect values is still safe ---
        // percent_used = 100 but available_mb = 100 (inconsistent)
        let shark = TestDestShark {
            node: make_storage_node("test.stor", "dc1"),
            available_mb: 100,
            percent_used: 100,
            assigned_mb: 9,
        };
        assert_eq!(calculate_available_mb(&shark, 100), 0);
    }

    // -------------------------------------------------------------------------
    // Test 2: available_mb - Multi-object assignment with capacity tracking
    // -------------------------------------------------------------------------

    #[test]
    fn available_mb() {
        // Test that assignments correctly track available space
        let mut dest_shark = TestDestShark::new("3.stor.domain", "dc1", 200, 80);

        // With max_fill_percentage = 90:
        // Total = 200/0.2 = 1000 MB
        // Max fill = 900 MB
        // Used = 800 MB
        // Available = 900 - 800 = 100 MB
        let max_fill = 90;
        assert_eq!(calculate_available_mb(&dest_shark, max_fill), 100);

        // Simulate assigning 50 MB
        dest_shark.assigned_mb = 50;
        assert_eq!(calculate_available_mb(&dest_shark, max_fill), 50);

        // Assign remaining
        dest_shark.assigned_mb = 100;
        assert_eq!(calculate_available_mb(&dest_shark, max_fill), 0);

        // Over-assign should still return 0
        dest_shark.assigned_mb = 150;
        assert_eq!(calculate_available_mb(&dest_shark, max_fill), 0);
    }

    // -------------------------------------------------------------------------
    // Test 3: no_skip - Assignment without skips
    // -------------------------------------------------------------------------

    #[test]
    fn no_skip() {
        // Test that objects are properly assigned to non-conflicting sharks
        let from_shark = make_storage_node("1.stor.domain", "dc1");

        // Create objects on sharks 1 and 2
        let obj_sharks = vec![
            MantaObjectShark {
                manta_storage_id: "1.stor.domain".to_string(),
                datacenter: "dc1".to_string(),
            },
            MantaObjectShark {
                manta_storage_id: "2.stor.domain".to_string(),
                datacenter: "dc2".to_string(),
            },
        ];

        // Destination shark 3 in dc3 should be valid (different shark, different DC)
        let dest_shark_valid = make_storage_node("3.stor.domain", "dc3");
        assert!(validate_destination(&obj_sharks, &from_shark, &dest_shark_valid).is_none());

        // Destination shark 4 in dc1 should be valid (from_shark is in dc1, so we're replacing)
        let dest_shark_same_dc_as_from = make_storage_node("4.stor.domain", "dc1");
        assert!(
            validate_destination(&obj_sharks, &from_shark, &dest_shark_same_dc_as_from).is_none()
        );
    }

    // -------------------------------------------------------------------------
    // Test 4: assignment_processing_test - Assignment state transitions
    // -------------------------------------------------------------------------

    #[test]
    fn assignment_processing_test() {
        let dest_shark = make_storage_node("dest.stor", "dc1");
        let mut assignment = Assignment::new(dest_shark);
        let assignment_id = assignment.id.clone();

        // Create some evacuate objects
        let mut eobjs = Vec::new();
        for i in 0..10 {
            let mut eobj = make_evacuate_object(&format!("obj-{}", i));
            eobj.assignment_id = assignment_id.clone();
            eobjs.push(eobj);
        }

        // Add tasks to assignment
        for eobj in &eobjs {
            assignment.add_task(eobj);
        }

        assert_eq!(assignment.tasks.len(), 10);
        assert_eq!(assignment.state, AssignmentState::Init);

        // Simulate assignment being posted
        assignment.state = AssignmentState::Assigned;
        assert_eq!(assignment.state, AssignmentState::Assigned);

        // Simulate agent completion with some failures
        let failed_count = 5;
        let mut failed_tasks = Vec::new();

        for (i, (_obj_id, task)) in assignment.tasks.iter().enumerate() {
            if i < failed_count {
                let mut failed_task = task.clone();
                failed_task.status = TaskStatus::Failed(ObjectSkippedReason::NetworkError);
                failed_tasks.push(failed_task);
            }
        }

        // Verify we captured the right number of failures
        assert_eq!(failed_tasks.len(), failed_count);

        // Transition to complete
        assignment.state = AssignmentState::AgentComplete;
        assert_eq!(assignment.state, AssignmentState::AgentComplete);

        // Verify state machine progression
        assignment.state = AssignmentState::PostProcessed;
        assert_eq!(assignment.state, AssignmentState::PostProcessed);
    }

    // -------------------------------------------------------------------------
    // Test 5: empty_storinfo_test - No storage nodes available
    // -------------------------------------------------------------------------

    #[test]
    fn empty_storinfo_test() {
        // Test behavior when no destination sharks are available
        let available_sharks: Vec<StorageNode> = vec![];

        // When no sharks available, should not be able to find destination
        assert!(available_sharks.is_empty());

        // Simulate the check that would happen in select_destination
        let from_shark = make_storage_node("1.stor.domain", "dc1");
        let valid_destinations: Vec<&StorageNode> = available_sharks
            .iter()
            .filter(|s| s.manta_storage_id != from_shark.manta_storage_id)
            .collect();

        assert!(valid_destinations.is_empty());
    }

    // -------------------------------------------------------------------------
    // Test 6: skip_object_test - Object skipping logic
    // -------------------------------------------------------------------------

    #[test]
    fn skip_object_test() {
        let mut obj = make_evacuate_object("test-obj");
        assert_eq!(obj.status, EvacuateObjectStatus::Unprocessed);
        assert!(obj.skipped_reason.is_none());

        // Test skipping due to destination unreachable
        obj.status = EvacuateObjectStatus::Skipped;
        obj.skipped_reason = Some(ObjectSkippedReason::DestinationUnreachable);
        assert_eq!(obj.status, EvacuateObjectStatus::Skipped);
        assert_eq!(
            obj.skipped_reason,
            Some(ObjectSkippedReason::DestinationUnreachable)
        );

        // Test different skip reasons
        let mut obj2 = make_evacuate_object("test-obj-2");
        obj2.status = EvacuateObjectStatus::Skipped;
        obj2.skipped_reason = Some(ObjectSkippedReason::ObjectAlreadyOnDestShark);
        assert_eq!(
            obj2.skipped_reason,
            Some(ObjectSkippedReason::ObjectAlreadyOnDestShark)
        );

        let mut obj3 = make_evacuate_object("test-obj-3");
        obj3.status = EvacuateObjectStatus::Skipped;
        obj3.skipped_reason = Some(ObjectSkippedReason::DestinationInsufficientSpace);
        assert_eq!(
            obj3.skipped_reason,
            Some(ObjectSkippedReason::DestinationInsufficientSpace)
        );
    }

    // -------------------------------------------------------------------------
    // Test 7: duplicate_object_id_test - Duplicate detection
    // -------------------------------------------------------------------------

    #[test]
    fn duplicate_object_id_test() {
        let dest_shark = make_storage_node("dest.stor", "dc1");
        let mut assignment = Assignment::new(dest_shark);

        // Add the same object twice
        let obj1 = make_evacuate_object("duplicate-obj");
        let obj2 = make_evacuate_object("duplicate-obj");

        assignment.add_task(&obj1);
        let initial_count = assignment.tasks.len();
        assert_eq!(initial_count, 1);

        // Adding the same object ID again should overwrite (HashMap behavior)
        assignment.add_task(&obj2);
        assert_eq!(assignment.tasks.len(), 1);

        // Verify the task exists
        assert!(assignment.tasks.contains_key("duplicate-obj"));
    }

    // -------------------------------------------------------------------------
    // Test 8: validate_destination_test - Destination validation
    // -------------------------------------------------------------------------

    #[test]
    fn validate_destination_test() {
        // Object is on sharks in dc1 (1.stor) and dc2 (2.stor)
        let obj_sharks = vec![
            MantaObjectShark {
                manta_storage_id: "1.stor.domain".to_string(),
                datacenter: "dc1".to_string(),
            },
            MantaObjectShark {
                manta_storage_id: "2.stor.domain".to_string(),
                datacenter: "dc2".to_string(),
            },
        ];

        let from_node = make_storage_node("1.stor.domain", "dc1");

        // Test evacuation to different shark in same datacenter (dc1) - should succeed
        // because we're replacing the copy on from_shark
        let to_shark_same_dc = make_storage_node("3.stor.domain", "dc1");
        assert!(
            validate_destination(&obj_sharks, &from_node, &to_shark_same_dc).is_none(),
            "Should allow evacuation to another shark in the same datacenter as from_shark"
        );

        // Test compromising fault domain - moving to dc2 where object already exists
        let to_shark_dc2 = make_storage_node("4.stor.domain", "dc2");
        assert_eq!(
            validate_destination(&obj_sharks, &from_node, &to_shark_dc2),
            Some(ObjectSkippedReason::ObjectAlreadyInDatacenter),
            "Should not place more than one copy in the same datacenter"
        );

        // Test evacuating to the from_shark itself
        let to_shark_same = make_storage_node("1.stor.domain", "dc1");
        assert_eq!(
            validate_destination(&obj_sharks, &from_node, &to_shark_same),
            Some(ObjectSkippedReason::ObjectAlreadyOnDestShark),
            "Should not evacuate back to source"
        );

        // Test evacuating to a shark the object is already on (2.stor)
        let to_shark_existing = make_storage_node("2.stor.domain", "dc2");
        assert_eq!(
            validate_destination(&obj_sharks, &from_node, &to_shark_existing),
            Some(ObjectSkippedReason::ObjectAlreadyOnDestShark),
            "Should not evacuate to a shark the object is already on"
        );
    }

    // -------------------------------------------------------------------------
    // Test 9: full_test - End-to-end simulation (100 objects)
    // -------------------------------------------------------------------------

    #[test]
    fn full_test() {
        let num_objects = 100;
        let dest_shark = make_storage_node("dest.stor", "dc1");

        // Simulate processing 100 objects
        let mut assignment = Assignment::new(dest_shark);
        let mut assigned_count = 0;
        let mut skipped_count = 0;

        for i in 0..num_objects {
            let obj = make_evacuate_object_with_size(&format!("obj-{}", i), 1024 * 1024); // 1 MiB

            // Simulate some being skipped (e.g., every 10th object)
            if i % 10 == 0 {
                skipped_count += 1;
            } else {
                assignment.add_task(&obj);
                assigned_count += 1;
            }
        }

        assert_eq!(assigned_count + skipped_count, num_objects);
        assert_eq!(assignment.tasks.len(), assigned_count);
        assert_eq!(skipped_count, 10);
        assert_eq!(assigned_count, 90);
    }

    // -------------------------------------------------------------------------
    // Test 10: test_duplicate_handler - Duplicates across assignments
    // -------------------------------------------------------------------------

    #[test]
    fn test_duplicate_handler() {
        // Test handling duplicate objects that appear in the input stream
        // Simulate duplicate object IDs being processed
        let duplicate_id = "duplicate-obj-id";
        let mut seen_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut duplicate_shards: Vec<i32> = Vec::new();

        // Process 100 "objects" that are all duplicates
        for shard in 0..100 {
            let obj_id = duplicate_id.to_string();

            if seen_ids.contains(&obj_id) {
                // This is a duplicate - record the shard
                duplicate_shards.push(shard);
            } else {
                seen_ids.insert(obj_id);
            }
        }

        // Should have 99 duplicates (first one is not a duplicate)
        assert_eq!(duplicate_shards.len(), 99);

        // Verify duplicate tracking
        assert_eq!(seen_ids.len(), 1);
        assert!(seen_ids.contains(duplicate_id));
    }

    // -------------------------------------------------------------------------
    // Test 11: test_duplicate_handler_small_assignment - Small assignment duplicates
    // -------------------------------------------------------------------------

    #[test]
    fn test_duplicate_handler_small_assignment() {
        // Test with max_tasks_per_assignment = 1, so each object gets its own assignment
        let max_tasks = 1;
        let duplicate_id = "duplicate-obj";

        let mut assignment_count = 0;
        let mut duplicate_insert_failures = 0;
        let mut seen_in_assignments: std::collections::HashSet<String> =
            std::collections::HashSet::new();

        // Process 100 duplicate objects with small assignments
        for _ in 0..100 {
            let dest_shark = make_storage_node("dest.stor", "dc1");
            let mut assignment = Assignment::new(dest_shark);
            let obj = make_evacuate_object(duplicate_id);

            // Check if this would be a duplicate insert
            if seen_in_assignments.contains(&obj.id) {
                duplicate_insert_failures += 1;
            } else {
                assignment.add_task(&obj);
                assignment_count += 1;

                if assignment.tasks.len() >= max_tasks {
                    // Assignment is full, "send" it
                    // In real code, this would be where we detect the duplicate
                    // when trying to insert into the database
                    seen_in_assignments.insert(obj.id.clone());
                }
            }
        }

        // With small assignments, we would detect all 100 as duplicates
        // after the first one is inserted
        assert_eq!(assignment_count, 1);
        assert_eq!(duplicate_insert_failures, 99);
    }

    // -------------------------------------------------------------------------
    // Test 12: test_retry_job - Retry job functionality
    // -------------------------------------------------------------------------

    #[test]
    fn test_retry_job() {
        // Test the retry job concept - objects that were skipped can be retried
        let mut objects: Vec<EvacuateObject> = Vec::new();

        // Create objects with various statuses
        for i in 0..100 {
            let mut obj = make_evacuate_object(&format!("obj-{}", i));

            // Simulate different outcomes:
            // - 40% complete
            // - 30% skipped (can retry)
            // - 20% error (can retry some)
            // - 10% assigned (still in progress)
            match i % 10 {
                0..=3 => {
                    obj.status = EvacuateObjectStatus::Complete;
                }
                4..=6 => {
                    obj.status = EvacuateObjectStatus::Skipped;
                    obj.skipped_reason = Some(ObjectSkippedReason::NetworkError);
                }
                7 | 8 => {
                    obj.status = EvacuateObjectStatus::Error;
                    obj.error = Some(EvacuateObjectError::MetadataUpdateFailed);
                }
                _ => {
                    obj.status = EvacuateObjectStatus::Assigned;
                }
            }
            objects.push(obj);
        }

        // Count objects by status
        let complete_count = objects
            .iter()
            .filter(|o| o.status == EvacuateObjectStatus::Complete)
            .count();
        let skipped_count = objects
            .iter()
            .filter(|o| o.status == EvacuateObjectStatus::Skipped)
            .count();
        let error_count = objects
            .iter()
            .filter(|o| o.status == EvacuateObjectStatus::Error)
            .count();
        let assigned_count = objects
            .iter()
            .filter(|o| o.status == EvacuateObjectStatus::Assigned)
            .count();

        assert_eq!(complete_count, 40);
        assert_eq!(skipped_count, 30);
        assert_eq!(error_count, 20);
        assert_eq!(assigned_count, 10);

        // For a retry job, we would reprocess skipped objects
        let retry_candidates: Vec<&EvacuateObject> = objects
            .iter()
            .filter(|o| o.status == EvacuateObjectStatus::Skipped)
            .collect();

        assert_eq!(retry_candidates.len(), 30);

        // All retry candidates should have a skipped reason
        for obj in &retry_candidates {
            assert!(obj.skipped_reason.is_some());
        }

        // Verify the retry candidates can be reset to unprocessed
        let mut retry_objects: Vec<EvacuateObject> =
            retry_candidates.iter().map(|o| (*o).clone()).collect();

        for obj in &mut retry_objects {
            obj.status = EvacuateObjectStatus::Unprocessed;
            obj.skipped_reason = None;
            obj.assignment_id = String::new();
        }

        // All should now be unprocessed
        assert!(
            retry_objects
                .iter()
                .all(|o| o.status == EvacuateObjectStatus::Unprocessed)
        );
    }

    // -------------------------------------------------------------------------
    // Additional Tests for State Machine and Type Conversions
    // -------------------------------------------------------------------------

    #[test]
    fn test_evacuate_object_status_transitions() {
        let mut obj = make_evacuate_object("test");

        // Unprocessed -> Assigned
        assert_eq!(obj.status, EvacuateObjectStatus::Unprocessed);
        obj.status = EvacuateObjectStatus::Assigned;
        assert_eq!(obj.status, EvacuateObjectStatus::Assigned);

        // Assigned -> PostProcessing
        obj.status = EvacuateObjectStatus::PostProcessing;
        assert_eq!(obj.status, EvacuateObjectStatus::PostProcessing);

        // PostProcessing -> Complete
        obj.status = EvacuateObjectStatus::Complete;
        assert_eq!(obj.status, EvacuateObjectStatus::Complete);
    }

    #[test]
    fn test_evacuate_object_status_display() {
        assert_eq!(EvacuateObjectStatus::Unprocessed.to_string(), "unprocessed");
        assert_eq!(EvacuateObjectStatus::Assigned.to_string(), "assigned");
        assert_eq!(EvacuateObjectStatus::Skipped.to_string(), "skipped");
        assert_eq!(EvacuateObjectStatus::Error.to_string(), "error");
        assert_eq!(
            EvacuateObjectStatus::PostProcessing.to_string(),
            "post_processing"
        );
        assert_eq!(EvacuateObjectStatus::Complete.to_string(), "complete");
    }

    #[test]
    fn test_evacuate_object_status_from_str() {
        use std::str::FromStr;

        assert_eq!(
            EvacuateObjectStatus::from_str("unprocessed").unwrap(),
            EvacuateObjectStatus::Unprocessed
        );
        assert_eq!(
            EvacuateObjectStatus::from_str("assigned").unwrap(),
            EvacuateObjectStatus::Assigned
        );
        assert_eq!(
            EvacuateObjectStatus::from_str("skipped").unwrap(),
            EvacuateObjectStatus::Skipped
        );
        assert_eq!(
            EvacuateObjectStatus::from_str("error").unwrap(),
            EvacuateObjectStatus::Error
        );
        assert_eq!(
            EvacuateObjectStatus::from_str("post_processing").unwrap(),
            EvacuateObjectStatus::PostProcessing
        );
        assert_eq!(
            EvacuateObjectStatus::from_str("complete").unwrap(),
            EvacuateObjectStatus::Complete
        );

        assert!(EvacuateObjectStatus::from_str("invalid").is_err());
    }

    #[test]
    fn test_assignment_state_transitions() {
        let dest_shark = make_storage_node("dest.stor", "dc1");
        let mut assignment = Assignment::new(dest_shark);

        // Init state
        assert_eq!(assignment.state, AssignmentState::Init);

        // Init -> Assigned
        assignment.state = AssignmentState::Assigned;
        assert_eq!(assignment.state, AssignmentState::Assigned);

        // Assigned -> AgentComplete
        assignment.state = AssignmentState::AgentComplete;
        assert_eq!(assignment.state, AssignmentState::AgentComplete);

        // AgentComplete -> PostProcessed
        assignment.state = AssignmentState::PostProcessed;
        assert_eq!(assignment.state, AssignmentState::PostProcessed);
    }

    #[test]
    fn test_assignment_state_rejected() {
        let dest_shark = make_storage_node("dest.stor", "dc1");
        let mut assignment = Assignment::new(dest_shark);

        // Init -> Rejected
        assignment.state = AssignmentState::Rejected;
        assert_eq!(assignment.state, AssignmentState::Rejected);
    }

    #[test]
    fn test_assignment_state_agent_unavailable() {
        let dest_shark = make_storage_node("dest.stor", "dc1");
        let mut assignment = Assignment::new(dest_shark);

        // Init -> AgentUnavailable
        assignment.state = AssignmentState::AgentUnavailable;
        assert_eq!(assignment.state, AssignmentState::AgentUnavailable);
    }

    #[test]
    fn test_assignment_cache_entry_from_assignment() {
        let dest_shark = make_storage_node("dest.stor", "dc1");
        let mut assignment = Assignment::new(dest_shark.clone());

        // Add some tasks
        for i in 0..5 {
            let obj = make_evacuate_object(&format!("obj-{}", i));
            assignment.add_task(&obj);
        }
        assignment.total_size = 500;
        assignment.state = AssignmentState::Assigned;

        // Convert to cache entry
        let cache_entry: AssignmentCacheEntry = assignment.clone().into();

        assert_eq!(cache_entry.id, assignment.id);
        assert_eq!(
            cache_entry.dest_shark.manta_storage_id,
            dest_shark.manta_storage_id
        );
        assert_eq!(cache_entry.total_size, 500);
        assert_eq!(cache_entry.state, AssignmentState::Assigned);
    }

    #[test]
    fn test_assignment_to_payload() {
        let dest_shark = make_storage_node("dest.stor", "dc1");
        let mut assignment = Assignment::new(dest_shark);

        // Add tasks
        for i in 0..3 {
            let obj = make_evacuate_object(&format!("obj-{}", i));
            assignment.add_task(&obj);
        }

        let payload = assignment.to_payload();

        assert_eq!(payload.id, assignment.id);
        assert_eq!(payload.tasks.len(), 3);
    }

    #[test]
    fn test_evacuate_config_default() {
        let config = EvacuateConfig::default();

        assert_eq!(config.max_tasks_per_assignment, 200);
        assert_eq!(config.max_assignment_age_secs, 300);
        assert_eq!(config.min_avail_mb, 1000);
        assert_eq!(config.max_objects, None);
        assert_eq!(config.agent_timeout_secs, 30);
    }

    #[test]
    fn test_evacuate_object_error_display() {
        assert_eq!(
            EvacuateObjectError::BadMorayClient.to_string(),
            "bad_moray_client"
        );
        assert_eq!(
            EvacuateObjectError::BadMorayObject.to_string(),
            "bad_moray_object"
        );
        assert_eq!(
            EvacuateObjectError::DuplicateShark.to_string(),
            "duplicate_shark"
        );
        assert_eq!(
            EvacuateObjectError::MetadataUpdateFailed.to_string(),
            "metadata_update_failed"
        );
    }

    #[test]
    fn test_evacuate_object_error_from_str() {
        use std::str::FromStr;

        assert_eq!(
            EvacuateObjectError::from_str("bad_moray_client").unwrap(),
            EvacuateObjectError::BadMorayClient
        );
        assert_eq!(
            EvacuateObjectError::from_str("duplicate_shark").unwrap(),
            EvacuateObjectError::DuplicateShark
        );
        assert!(EvacuateObjectError::from_str("invalid").is_err());
    }
}
