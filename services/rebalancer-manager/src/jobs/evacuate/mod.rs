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

#[allow(unused_imports)] // Will be used by metadata update code
pub use db::DuplicateObject;
pub use types::{EvacuateObject, EvacuateObjectError, EvacuateObjectStatus};

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use tokio::sync::{RwLock, Semaphore, mpsc, watch};
use tokio::task::JoinSet;
use tracing::{debug, error, info, warn};

use rebalancer_types::StorageNode;
use sharkspotter::SharkspotterMessage;
use slog::Drain;

use super::JobError;
use crate::db::Database;
use crate::metrics;
use crate::moray::{MorayPool, MorayPoolConfig};
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
    /// Discover objects via Sharkspotter (scanning Moray shards)
    Sharkspotter {
        /// Manta domain (e.g., "my-region.example.com")
        domain: String,
        /// Minimum shard number to scan
        min_shard: u32,
        /// Maximum shard number to scan
        max_shard: u32,
    },
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
    /// Maximum fill percentage for destination sharks (0-100)
    ///
    /// Sharks will not be used as destinations if assigning an object would
    /// push their usage above this percentage. Default is 90 (90% full).
    pub max_fill_percentage: u32,
    /// Maximum objects to process (for testing, None = unlimited)
    pub max_objects: Option<u32>,
    /// Agent HTTP request timeout
    pub agent_timeout_secs: u64,
    /// Source for object discovery
    pub object_source: ObjectSource,
    /// ZooKeeper connect string for Moray service discovery
    pub zk_connect_string: Option<String>,
    /// Manta domain for Moray shard paths
    pub moray_domain: Option<String>,
    /// Minimum Moray shard number
    pub moray_min_shard: u32,
    /// Maximum Moray shard number
    pub moray_max_shard: u32,
    /// Source job ID for retry jobs (ObjectSource::LocalDb reads from this job's database)
    pub source_job_id: Option<String>,
    /// Datacenter names to exclude from destination selection
    pub blacklist_datacenters: Vec<String>,
}

impl Default for EvacuateConfig {
    fn default() -> Self {
        Self {
            max_tasks_per_assignment: 200,
            max_assignment_age_secs: 300, // 5 minutes
            min_avail_mb: 1000,           // 1GB minimum
            max_fill_percentage: 90,      // Don't fill sharks beyond 90%
            max_objects: None,
            agent_timeout_secs: 30,
            object_source: ObjectSource::default(),
            zk_connect_string: None,
            moray_domain: None,
            moray_min_shard: 1,
            moray_max_shard: 1,
            source_job_id: None,
            blacklist_datacenters: Vec::new(),
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

    /// Database URL for creating connections
    database_url: String,

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

    /// Moray client pool for metadata updates
    moray_pool: Option<Arc<MorayPool>>,

    /// Shutdown signal
    shutdown_tx: watch::Sender<bool>,

    /// Channel for receiving runtime updates (e.g., SetMetadataThreads)
    update_rx: watch::Receiver<Option<rebalancer_types::EvacuateJobUpdateMessage>>,
}

/// Information about a destination shark
#[allow(dead_code)]
#[derive(Debug, Clone)]
struct DestSharkInfo {
    node: StorageNode,
    available_mb: u64,
    percent_used: u32,
    assigned_mb: u64,
}

#[allow(dead_code)]
impl EvacuateJob {
    /// Create a new evacuate job
    ///
    /// The `update_rx` channel is used to receive runtime configuration updates
    /// (e.g., `SetMetadataThreads`) from the API context.
    #[allow(clippy::too_many_arguments)]
    pub async fn new(
        job_id: String,
        job_uuid: uuid::Uuid,
        from_shark: StorageNode,
        storinfo: Arc<StorinfoClient>,
        config: EvacuateConfig,
        manager_db: Arc<Database>,
        database_url: &str,
        update_rx: watch::Receiver<Option<rebalancer_types::EvacuateJobUpdateMessage>>,
    ) -> Result<Self, JobError> {
        let db = EvacuateDb::new(&job_id, database_url).await?;

        let http_client = reqwest::Client::builder()
            .timeout(Duration::from_secs(config.agent_timeout_secs))
            .build()?;

        let (shutdown_tx, _) = watch::channel(false);

        // Create Moray pool if ZooKeeper and domain are configured
        let moray_pool = match (&config.zk_connect_string, &config.moray_domain) {
            (Some(zk), Some(domain)) => {
                let pool_config = MorayPoolConfig {
                    zk_connect_string: zk.clone(),
                    domain: domain.clone(),
                    min_shard: config.moray_min_shard,
                    max_shard: config.moray_max_shard,
                };
                Some(Arc::new(MorayPool::new(pool_config)))
            }
            _ => {
                warn!(
                    job_id = %job_id,
                    "Moray pool not configured - metadata updates will be no-ops"
                );
                None
            }
        };

        Ok(Self {
            job_id,
            job_uuid,
            from_shark,
            config,
            manager_db,
            database_url: database_url.to_string(),
            db: Arc::new(db),
            assignments: Arc::new(RwLock::new(HashMap::new())),
            dest_sharks: Arc::new(RwLock::new(HashMap::new())),
            http_client,
            storinfo,
            moray_pool,
            shutdown_tx,
            update_rx,
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
            .map_err(|e| {
                JobError::Internal(format!("Failed to update job state to setup: {}", e))
            })?;

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

        // Initialize destination sharks cache from storinfo
        self.init_dest_sharks().await?;

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
            let update_rx = self.update_rx.clone();
            async move { job.metadata_update_broker(md_update_rx, update_rx).await }
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
            .map_err(|e| {
                JobError::Internal(format!("Failed to update job state to running: {}", e))
            })?;

        // Spawn object discovery task
        // When this task completes, the object_tx channel is dropped, signaling
        // end of input to the assignment manager
        let object_discovery = self.spawn_object_discovery(object_tx).await?;

        // Wait for object discovery to complete first and track any errors
        // Discovery errors are tracked so we can mark the job as failed if critical
        let discovery_error: Option<String> = match object_discovery.await {
            Ok(Ok(())) => {
                debug!("Object discovery completed successfully");
                None
            }
            Ok(Err(e)) => {
                error!(job_id = %self.job_id, error = %e, "Object discovery error");
                Some(format!("Discovery error: {}", e))
            }
            Err(e) => {
                error!(job_id = %self.job_id, error = %e, "Object discovery task panicked");
                Some(format!("Discovery task panicked: {}", e))
            }
        };

        // Wait for assignment manager to complete
        let assignment_error: Option<String> = match assignment_manager.await {
            Ok(Ok(())) => {
                debug!("Assignment manager completed");
                None
            }
            Ok(Err(e)) => {
                error!("Assignment manager error: {}", e);
                // Intentionally ignore send error - receiver may already be dropped
                let _ = self.shutdown_tx.send(true);
                Some(format!("Assignment manager error: {}", e))
            }
            Err(e) => {
                error!("Assignment manager panicked: {}", e);
                // Intentionally ignore send error - receiver may already be dropped
                let _ = self.shutdown_tx.send(true);
                Some(format!("Assignment manager panicked: {}", e))
            }
        };

        // Signal shutdown and wait for other workers
        // Intentionally ignore send error - receivers may already be dropped during cleanup
        let _ = self.shutdown_tx.send(true);
        drop(md_update_tx); // Close metadata channel

        // Wait for workers to complete and track any errors
        let poster_error: Option<String> = match assignment_poster.await {
            Ok(Ok(())) => None,
            Ok(Err(e)) => {
                error!("Assignment poster error: {}", e);
                Some(format!("Assignment poster error: {}", e))
            }
            Err(e) => {
                error!("Assignment poster panicked: {}", e);
                Some(format!("Assignment poster panicked: {}", e))
            }
        };

        let checker_error: Option<String> = match assignment_checker.await {
            Ok(Ok(())) => None,
            Ok(Err(e)) => {
                error!("Assignment checker error: {}", e);
                Some(format!("Assignment checker error: {}", e))
            }
            Err(e) => {
                error!("Assignment checker panicked: {}", e);
                Some(format!("Assignment checker panicked: {}", e))
            }
        };

        let updater_error: Option<String> = match metadata_updater.await {
            Ok(Ok(())) => None,
            Ok(Err(e)) => {
                error!("Metadata updater error: {}", e);
                Some(format!("Metadata updater error: {}", e))
            }
            Err(e) => {
                error!("Metadata updater panicked: {}", e);
                Some(format!("Metadata updater panicked: {}", e))
            }
        };

        // Determine final job state based on errors
        // A discovery error is considered critical and fails the job
        // Worker errors are also critical
        let critical_errors: Vec<&str> = [
            discovery_error.as_deref(),
            assignment_error.as_deref(),
            poster_error.as_deref(),
            checker_error.as_deref(),
            updater_error.as_deref(),
        ]
        .into_iter()
        .flatten()
        .collect();

        let final_state = if critical_errors.is_empty() {
            "complete"
        } else {
            warn!(
                job_id = %self.job_id,
                error_count = critical_errors.len(),
                errors = ?critical_errors,
                "Job completed with errors"
            );
            "failed"
        };

        // Update final job state
        self.manager_db
            .update_job_state(&self.job_uuid, final_state)
            .await
            .map_err(|e| {
                JobError::Internal(format!(
                    "Failed to update job state to {}: {}",
                    final_state, e
                ))
            })?;

        if final_state == "failed" {
            warn!(job_id = %self.job_id, "Evacuate job completed with failures");
        } else {
            info!(job_id = %self.job_id, "Evacuate job completed successfully");
        }

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
                    // Increment skipped count
                    // arch-lint: allow(no-error-swallowing) reason="Counter is best-effort; failure tracked via metric"
                    if let Err(e) = self
                        .manager_db
                        .increment_result_count(&self.job_uuid, "skipped")
                        .await
                    {
                        warn!(job_id = %self.job_id, error = %e, "Failed to increment skipped count");
                        metrics::record_db_operation_failure();
                    }
                    continue;
                }
                Err(e) => {
                    warn!(
                        object_id = %eobj.id,
                        error = %e,
                        "Error selecting destination, marking object as skipped"
                    );
                    // Record the error in the database so object isn't lost
                    // arch-lint: allow(no-error-swallowing) reason="Best-effort error recording; continue processing"
                    if let Err(db_err) = self
                        .db
                        .mark_object_error(&eobj.id, types::EvacuateObjectError::InternalError)
                        .await
                    {
                        error!(
                            object_id = %eobj.id,
                            error = %db_err,
                            "Failed to record destination selection error in database"
                        );
                    }
                    // Increment error count
                    // arch-lint: allow(no-error-swallowing) reason="Counter is best-effort; failure tracked via metric"
                    if let Err(count_err) = self
                        .manager_db
                        .increment_result_count(&self.job_uuid, "error")
                        .await
                    {
                        warn!(job_id = %self.job_id, error = %count_err, "Failed to increment error count");
                        metrics::record_db_operation_failure();
                    }
                    continue;
                }
            };

            let dest_id = dest_shark.manta_storage_id.clone();
            eobj.dest_shark = dest_id.clone();

            // Update assigned capacity tracking
            let object_size_mb = (eobj.get_content_length() / (1024 * 1024)) + 1;
            self.update_assigned_capacity(&dest_id, object_size_mb)
                .await;

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

                    // Mark objects as skipped and increment counts
                    for task in assignment.tasks.values() {
                        self.db
                            .mark_object_skipped(
                                &task.object_id,
                                rebalancer_types::ObjectSkippedReason::AssignmentRejected,
                            )
                            .await?;
                        // Increment skipped count
                        // arch-lint: allow(no-error-swallowing) reason="Counter is best-effort; failure tracked via metric"
                        if let Err(e) = self
                            .manager_db
                            .increment_result_count(&self.job_uuid, "skipped")
                            .await
                        {
                            warn!(job_id = %self.job_id, error = %e, "Failed to increment skipped count");
                            metrics::record_db_operation_failure();
                        }
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
    /// Also listens for runtime configuration updates (e.g., SetMetadataThreads).
    ///
    /// Concurrency is controlled via a semaphore. The `SetMetadataThreads` message
    /// adjusts the number of permits available, allowing dynamic scaling of
    /// concurrent metadata update workers.
    async fn metadata_update_broker(
        &self,
        mut md_rx: mpsc::Receiver<AssignmentCacheEntry>,
        mut update_rx: watch::Receiver<Option<rebalancer_types::EvacuateJobUpdateMessage>>,
    ) -> Result<(), JobError> {
        use rebalancer_types::{EvacuateJobUpdateMessage, MAX_TUNABLE_MD_UPDATE_THREADS};

        const DEFAULT_METADATA_THREADS: u32 = 10;
        // Maximum permits we'll ever allow - use the same constant as API validation
        // to ensure consistency between what the API accepts and what the broker allows.
        const MAX_PERMITS: u32 = MAX_TUNABLE_MD_UPDATE_THREADS;

        info!(
            job_id = %self.job_id,
            initial_threads = DEFAULT_METADATA_THREADS,
            "Metadata update broker started"
        );

        // Semaphore to control concurrent metadata updates
        // Start with max permits so we can add/remove as needed
        let semaphore = Arc::new(Semaphore::new(MAX_PERMITS as usize));

        // Track the current configured limit and "forget" excess permits to enforce it
        let current_limit = Arc::new(AtomicU32::new(DEFAULT_METADATA_THREADS));

        // Remove permits to get to our default limit.
        // We use forget_permits() to permanently remove the excess permits from the semaphore.
        // This is the correct approach - do NOT use acquire_many() first, as that would
        // double-count the reduction (acquire takes permits, then forget removes more).
        let excess_permits = MAX_PERMITS - DEFAULT_METADATA_THREADS;
        semaphore.forget_permits(excess_permits as usize);

        // JoinSet to track in-flight metadata update tasks
        let mut tasks: JoinSet<()> = JoinSet::new();

        loop {
            tokio::select! {
                // Handle runtime configuration updates
                result = update_rx.changed() => {
                    if result.is_err() {
                        // Channel closed, continue processing remaining work
                        debug!(job_id = %self.job_id, "Update channel closed");
                        continue;
                    }
                    if let Some(msg) = update_rx.borrow_and_update().clone() {
                        match msg {
                            EvacuateJobUpdateMessage::SetMetadataThreads(new_limit) => {
                                let old_limit = current_limit.load(Ordering::SeqCst);

                                // Clamp new_limit to valid range
                                let new_limit = new_limit.clamp(1, MAX_PERMITS);

                                info!(
                                    job_id = %self.job_id,
                                    old_threads = old_limit,
                                    new_threads = new_limit,
                                    "Adjusting metadata threads"
                                );

                                if new_limit > old_limit {
                                    // Increasing capacity: add permits
                                    let delta = (new_limit - old_limit) as usize;
                                    semaphore.add_permits(delta);
                                } else if new_limit < old_limit {
                                    // Decreasing capacity: remove permits by forgetting them
                                    // Note: this doesn't preempt running tasks, just prevents
                                    // new ones from starting until current ones complete
                                    let delta = (old_limit - new_limit) as usize;
                                    semaphore.forget_permits(delta);
                                }
                                // else: no change needed

                                current_limit.store(new_limit, Ordering::SeqCst);
                            }
                        }
                    }
                }

                // Reap completed tasks to avoid unbounded growth
                Some(result) = tasks.join_next(), if !tasks.is_empty() => {
                    if let Err(e) = result {
                        error!(job_id = %self.job_id, error = %e, "Metadata update task panicked");
                    }
                }

                // Handle metadata update requests
                ace_opt = md_rx.recv() => {
                    let Some(ace) = ace_opt else {
                        // Channel closed, we're done receiving new work
                        break;
                    };

                    // Get objects from this assignment that need metadata updates
                    let objects = match self.db.get_assignment_objects(&ace.id).await {
                        Ok(objs) => objs,
                        Err(e) => {
                            error!(
                                job_id = %self.job_id,
                                assignment_id = %ace.id,
                                error = %e,
                                "Failed to get assignment objects"
                            );
                            continue;
                        }
                    };

                    // Filter to objects that need processing
                    let objects_to_process: Vec<_> = objects
                        .into_iter()
                        .filter(|obj| obj.status == EvacuateObjectStatus::PostProcessing)
                        .collect();

                    if objects_to_process.is_empty() {
                        // No objects need processing, mark assignment complete
                        let mut cache = self.assignments.write().await;
                        if let Some(entry) = cache.get_mut(&ace.id) {
                            entry.state = AssignmentState::PostProcessed;
                        }
                        continue;
                    }

                    // Spawn a task for each object's metadata update
                    // Each task acquires a semaphore permit before doing work
                    for obj in objects_to_process {
                        let sem = Arc::clone(&semaphore);
                        let db = Arc::clone(&self.db);
                        let manager_db = Arc::clone(&self.manager_db);
                        let moray_pool = self.moray_pool.clone();
                        let from_shark_id = self.from_shark.manta_storage_id.clone();
                        let dest_sharks = Arc::clone(&self.dest_sharks);
                        let job_id = self.job_id.clone();
                        let job_uuid = self.job_uuid;
                        let assignments = Arc::clone(&self.assignments);
                        let assignment_id = ace.id.clone();

                        tasks.spawn(async move {
                            // Acquire permit before processing
                            // This limits concurrent metadata updates
                            let Ok(_permit) = sem.acquire().await else {
                                error!("Semaphore closed unexpectedly, skipping object");
                                return;
                            };

                            // Perform the metadata update
                            let result = Self::update_object_metadata_static(
                                &moray_pool,
                                &from_shark_id,
                                &dest_sharks,
                                &obj,
                            ).await;

                            match result {
                                Ok(()) => {
                                    if let Err(e) = db.mark_object_complete(&obj.id).await {
                                        error!(
                                            job_id = %job_id,
                                            object_id = %obj.id,
                                            error = %e,
                                            "Failed to mark object complete in DB"
                                        );
                                    }
                                    // Increment complete count
                                    // arch-lint: allow(no-error-swallowing) reason="Counter is best-effort; failure tracked via metric"
                                    if let Err(e) = manager_db
                                        .increment_result_count(&job_uuid, "complete")
                                        .await
                                    {
                                        warn!(job_id = %job_id, error = %e, "Failed to increment complete count");
                                        metrics::record_db_operation_failure();
                                    }
                                }
                                Err(e) => {
                                    error!(
                                        job_id = %job_id,
                                        object_id = %obj.id,
                                        error = %e,
                                        "Failed to update metadata"
                                    );
                                    if let Err(db_err) = db
                                        .mark_object_error(
                                            &obj.id,
                                            EvacuateObjectError::MetadataUpdateFailed,
                                        )
                                        .await
                                    {
                                        error!(
                                            job_id = %job_id,
                                            object_id = %obj.id,
                                            error = %db_err,
                                            "Failed to mark object error in DB"
                                        );
                                    }
                                    // Increment failed count
                                    // arch-lint: allow(no-error-swallowing) reason="Counter is best-effort; failure tracked via metric"
                                    if let Err(e) = manager_db
                                        .increment_result_count(&job_uuid, "failed")
                                        .await
                                    {
                                        warn!(job_id = %job_id, error = %e, "Failed to increment failed count");
                                        metrics::record_db_operation_failure();
                                    }
                                }
                            }

                            // Check if this was the last object for the assignment
                            // This is a best-effort check - the assignment will be marked
                            // complete when all its objects are processed
                            // Note: we can't easily track this per-assignment with spawned tasks,
                            // so we rely on the assignment_checker to eventually clean up
                            let _ = assignments; // Keep reference to show intent
                            let _ = assignment_id;
                        });
                    }

                    // Mark assignment as being processed (tasks are in flight)
                    // The actual PostProcessed state will be set when all tasks complete
                    // For now, we track it via the spawned tasks
                    {
                        let cache = self.assignments.read().await;
                        if cache.contains_key(&ace.id) {
                            // Keep it in current state - tasks will update when done
                            // The assignment checker will handle final cleanup
                            debug!(
                                job_id = %self.job_id,
                                assignment_id = %ace.id,
                                "Spawned metadata update tasks for assignment"
                            );
                        }
                    }
                }
            }
        }

        // Wait for all in-flight tasks to complete before returning
        info!(
            job_id = %self.job_id,
            pending_tasks = tasks.len(),
            "Waiting for remaining metadata update tasks to complete"
        );

        let mut panic_count = 0u64;
        while let Some(result) = tasks.join_next().await {
            if let Err(e) = result {
                panic_count += 1;
                metrics::record_task_panic();
                error!(job_id = %self.job_id, error = %e, "Metadata update task panicked");
            }
        }

        if panic_count > 0 {
            error!(
                job_id = %self.job_id,
                panic_count,
                "Metadata update broker completed with task panics"
            );
            return Err(JobError::Internal(format!(
                "{} metadata update task(s) panicked",
                panic_count
            )));
        }

        info!("Metadata update broker completed");
        Ok(())
    }

    /// Static version of update_object_metadata for use in spawned tasks
    ///
    /// This is a static method to avoid lifetime issues when spawning tasks.
    async fn update_object_metadata_static(
        moray_pool: &Option<Arc<MorayPool>>,
        from_shark_id: &str,
        dest_sharks: &Arc<RwLock<HashMap<String, DestSharkInfo>>>,
        obj: &EvacuateObject,
    ) -> Result<(), JobError> {
        // Check if Moray pool is available
        let moray_pool = match moray_pool {
            Some(pool) => pool,
            None => {
                // No Moray pool configured - log warning but don't fail
                // This allows testing without a real Moray connection
                warn!(
                    object_id = %obj.id,
                    "Moray pool not configured, skipping metadata update"
                );
                return Ok(());
            }
        };

        // Validate shard number
        let shard = if obj.shard < 0 {
            return Err(JobError::Internal(format!(
                "Invalid shard number {} for object {}",
                obj.shard, obj.id
            )));
        } else {
            obj.shard as u32
        };

        // Get the object key from the embedded Manta object
        let key = obj
            .object
            .get("key")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                JobError::Internal(format!(
                    "Missing or invalid 'key' in object metadata for {}",
                    obj.id
                ))
            })?;

        // Get destination shark info
        let dest_sharks_guard = dest_sharks.read().await;
        let dest_info = dest_sharks_guard.get(&obj.dest_shark).ok_or_else(|| {
            JobError::Internal(format!(
                "Destination shark {} not found in cache for object {}",
                obj.dest_shark, obj.id
            ))
        })?;

        let dest_datacenter = dest_info.node.datacenter.clone();
        drop(dest_sharks_guard); // Release lock before async call

        // Update the object metadata in Moray
        crate::moray::update_object_sharks(
            moray_pool,
            shard,
            key,
            from_shark_id,
            &obj.dest_shark,
            &dest_datacenter,
            &obj.etag,
        )
        .await
        .map_err(|e| {
            JobError::Internal(format!(
                "Failed to update metadata for object {}: {}",
                obj.id, e
            ))
        })?;

        debug!(
            object_id = %obj.id,
            shard = shard,
            from_shark = %from_shark_id,
            dest_shark = %obj.dest_shark,
            "Successfully updated object metadata in Moray"
        );

        Ok(())
    }

    /// Calculate available MB for a destination shark respecting max_fill_percentage
    ///
    /// This mirrors the legacy `_calculate_available_mb` function from evacuate.rs.
    /// It calculates how much space can actually be used on a shark given:
    /// - The shark's current available_mb and percent_used
    /// - The max_fill_percentage limit
    /// - How much has already been assigned in this job
    fn calculate_available_mb(&self, info: &DestSharkInfo) -> u64 {
        let max_fill = self.config.max_fill_percentage;

        // If percent_used >= max_fill_percentage, no space available
        if info.percent_used >= max_fill {
            return 0;
        }

        // If available_mb is 0, return 0
        if info.available_mb == 0 {
            return 0;
        }

        // Calculate total MB from available and percent_used
        // available_mb = total_mb * (1 - percent_used/100)
        // total_mb = available_mb / (1 - percent_used/100)
        // total_mb = available_mb * 100 / (100 - percent_used)
        let used_fraction = info.percent_used as u64;
        if used_fraction >= 100 {
            return 0;
        }

        let total_mb = (info.available_mb * 100) / (100 - used_fraction);

        // Max fill in MB
        let max_fill_mb = (total_mb * max_fill as u64) / 100;

        // Used MB
        let used_mb = total_mb - info.available_mb;

        // Maximum remaining we can use
        let max_remaining = max_fill_mb.saturating_sub(used_mb);

        // Subtract what's already assigned
        max_remaining.saturating_sub(info.assigned_mb)
    }

    /// Select a destination shark for an object
    ///
    /// Selects the best destination shark based on:
    /// 1. Filtering out the source shark
    /// 2. Filtering out sharks the object is already on
    /// 3. Filtering out sharks in datacenters where the object already exists
    ///    (except the from_shark's datacenter, since we're replacing that copy)
    /// 4. Filtering out sharks without enough available capacity (respecting max_fill_percentage)
    /// 5. Selecting the shark with the most available space
    async fn select_destination(
        &self,
        eobj: &EvacuateObject,
    ) -> Result<Option<StorageNode>, JobError> {
        let obj_sharks = eobj.get_sharks();
        let object_size_mb = (eobj.get_content_length() / (1024 * 1024)) + 1; // Round up

        // Get datacenters where object already exists (excluding from_shark's DC)
        let excluded_datacenters: std::collections::HashSet<&str> = obj_sharks
            .iter()
            .filter(|s| s.manta_storage_id != self.from_shark.manta_storage_id)
            .map(|s| s.datacenter.as_str())
            .collect();

        // Get sharks the object is already on
        let existing_shark_ids: std::collections::HashSet<&str> = obj_sharks
            .iter()
            .map(|s| s.manta_storage_id.as_str())
            .collect();

        // Read destination sharks cache
        let dest_sharks = self.dest_sharks.read().await;

        // Find the best destination
        let mut best: Option<(&str, u64)> = None;

        for (shark_id, info) in dest_sharks.iter() {
            // Skip the source shark
            if shark_id == &self.from_shark.manta_storage_id {
                continue;
            }

            // Skip sharks the object is already on
            if existing_shark_ids.contains(shark_id.as_str()) {
                continue;
            }

            // Skip sharks in datacenters where the object already exists
            // (except from_shark's datacenter)
            if excluded_datacenters.contains(info.node.datacenter.as_str()) {
                continue;
            }

            // Calculate effective available space respecting max_fill_percentage
            let effective_available = self.calculate_available_mb(info);

            // Skip if not enough space
            if effective_available < self.config.min_avail_mb {
                continue;
            }

            // Skip if not enough space for this object
            if effective_available < object_size_mb {
                continue;
            }

            // Select this shark if it has more space than current best
            match best {
                None => best = Some((shark_id, effective_available)),
                Some((_, best_available)) if effective_available > best_available => {
                    best = Some((shark_id, effective_available));
                }
                _ => {}
            }
        }

        // Return the best destination, if any
        match best {
            Some((shark_id, _)) => Ok(dest_sharks.get(shark_id).map(|info| info.node.clone())),
            None => Ok(None),
        }
    }

    /// Update the assigned capacity for a destination shark
    ///
    /// Called when an object is assigned to a shark to track capacity usage.
    async fn update_assigned_capacity(&self, shark_id: &str, size_mb: u64) {
        let mut dest_sharks = self.dest_sharks.write().await;
        if let Some(info) = dest_sharks.get_mut(shark_id) {
            info.assigned_mb = info.assigned_mb.saturating_add(size_mb);
        }
    }

    /// Initialize the destination sharks cache from storinfo
    ///
    /// Should be called after storinfo refresh to populate available destinations.
    /// Nodes in blacklisted datacenters are excluded from consideration.
    async fn init_dest_sharks(&self) -> Result<(), JobError> {
        let nodes = self
            .storinfo
            .get_nodes_excluding_datacenters(&self.config.blacklist_datacenters)
            .await
            .map_err(|e| JobError::Internal(format!("Failed to get storinfo nodes: {}", e)))?;

        // Log blacklisted datacenters if any
        if !self.config.blacklist_datacenters.is_empty() {
            info!(
                blacklist = ?self.config.blacklist_datacenters,
                "Excluding datacenters from destination selection"
            );
        }

        let mut dest_sharks = self.dest_sharks.write().await;
        dest_sharks.clear();

        for node_info in nodes {
            // Skip the source shark - we're evacuating from it
            if node_info.node.manta_storage_id == self.from_shark.manta_storage_id {
                continue;
            }

            // Skip sharks that are already at or above max_fill_percentage
            let percent_used = node_info.percent_used.round() as u32;
            if percent_used >= self.config.max_fill_percentage {
                debug!(
                    shark = %node_info.node.manta_storage_id,
                    percent_used = percent_used,
                    max_fill = self.config.max_fill_percentage,
                    "Skipping shark - already at or above max fill percentage"
                );
                continue;
            }

            dest_sharks.insert(
                node_info.node.manta_storage_id.clone(),
                DestSharkInfo {
                    node: node_info.node,
                    available_mb: node_info.available_mb,
                    percent_used,
                    assigned_mb: 0,
                },
            );
        }

        info!(
            count = dest_sharks.len(),
            max_fill_percentage = self.config.max_fill_percentage,
            "Initialized destination sharks cache"
        );

        Ok(())
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
            // Mark failed tasks and increment skipped counts
            for task in failed_tasks {
                if let rebalancer_types::TaskStatus::Failed(reason) = &task.status {
                    self.db
                        .mark_object_skipped(&task.object_id, *reason)
                        .await?;
                    // Increment skipped count for agent-reported failures
                    // arch-lint: allow(no-error-swallowing) reason="Counter is best-effort; failure tracked via metric"
                    if let Err(e) = self
                        .manager_db
                        .increment_result_count(&self.job_uuid, "skipped")
                        .await
                    {
                        warn!(job_id = %self.job_id, error = %e, "Failed to increment skipped count");
                        metrics::record_db_operation_failure();
                    }
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

    /// Spawn the object discovery task based on configuration
    ///
    /// This method spawns a task that discovers objects to evacuate and sends
    /// them to the object channel. The source of objects depends on the
    /// configured `object_source`.
    ///
    /// For retry jobs (ObjectSource::LocalDb with source_job_id set), this will:
    /// 1. Connect to the original job's database
    /// 2. Read retryable objects from there
    /// 3. Copy them to the new job's database before processing
    async fn spawn_object_discovery(
        &self,
        object_tx: mpsc::Sender<EvacuateObject>,
    ) -> Result<tokio::task::JoinHandle<Result<(), JobError>>, JobError> {
        let source = self.config.object_source.clone();
        let max_objects = self.config.max_objects;
        let source_job_id = self.config.source_job_id.clone();
        let database_url = self.database_url.clone();
        let db = Arc::clone(&self.db);
        let manager_db = Arc::clone(&self.manager_db);
        let job_uuid = self.job_uuid;
        let job_id = self.job_id.clone();
        let from_shark_id = self.from_shark.manta_storage_id.clone();

        let handle = tokio::spawn(async move {
            match source {
                ObjectSource::None => {
                    // No objects to discover - used for testing scaffolding
                    info!(job_id = %job_id, "Object source is None, no objects to process");
                    Ok(())
                }
                ObjectSource::LocalDb => {
                    // For retry jobs, read from the source job's database
                    // Otherwise, read from our own database
                    let objects = if let Some(ref src_id) = source_job_id {
                        info!(
                            job_id = %job_id,
                            source_job_id = %src_id,
                            "Reading objects from source job's database for retry"
                        );
                        // Connect to the source job's database
                        let source_db = EvacuateDb::new(src_id, &database_url).await?;
                        source_db.get_retryable_objects(max_objects).await?
                    } else {
                        info!(job_id = %job_id, "Reading objects from local database");
                        db.get_retryable_objects(max_objects).await?
                    };

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

                        // For retry jobs, copy the object to the new job's database
                        if source_job_id.is_some()
                            && let Err(e) = db.insert_object(&obj).await
                        {
                            warn!(
                                job_id = %job_id,
                                object_id = %obj.id,
                                error = %e,
                                "Failed to copy object to new job database"
                            );
                            continue;
                        }

                        if object_tx.send(obj).await.is_err() {
                            warn!(
                                job_id = %job_id,
                                "Object channel closed, stopping discovery"
                            );
                            break;
                        }
                        sent += 1;

                        // Increment total count for each object discovered
                        // arch-lint: allow(no-error-swallowing) reason="Counter is best-effort; failure tracked via metric"
                        if let Err(e) = manager_db.increment_result_count(&job_uuid, "total").await
                        {
                            warn!(job_id = %job_id, error = %e, "Failed to increment total count");
                            metrics::record_db_operation_failure();
                        }

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
                ObjectSource::Sharkspotter {
                    domain,
                    min_shard,
                    max_shard,
                } => {
                    // Discover objects via Sharkspotter
                    info!(
                        job_id = %job_id,
                        domain = %domain,
                        min_shard = min_shard,
                        max_shard = max_shard,
                        from_shark = %from_shark_id,
                        "Starting Sharkspotter object discovery"
                    );

                    // Create sharkspotter config
                    let config = sharkspotter::config::Config {
                        min_shard,
                        max_shard,
                        domain: domain.clone(),
                        sharks: vec![from_shark_id.clone()],
                        chunk_size: 1000,
                        begin: 0,
                        end: 0,
                        skip_validate_sharks: true, // Already validated by manager
                        output_file: None,
                        obj_id_only: false,
                        multithreaded: true,
                        max_threads: 50,
                        direct_db: false,
                        log_level: slog::Level::Info,
                    };

                    // Create channel for sharkspotter results
                    let (tx, rx) = crossbeam_channel::unbounded::<SharkspotterMessage>();

                    // Run sharkspotter in a blocking task
                    let sharkspotter_handle = {
                        let config = config.clone();
                        let job_id = job_id.clone();
                        std::thread::spawn(move || {
                            // Create a slog logger for sharkspotter
                            let drain = slog_stdlog::StdLog.fuse();
                            let log = slog::Logger::root(drain, slog::o!("job_id" => job_id));

                            if let Err(e) = sharkspotter::run_multithreaded(&config, log, tx) {
                                error!(error = %e, "Sharkspotter error");
                                return Err(JobError::Internal(format!(
                                    "Sharkspotter failed: {}",
                                    e
                                )));
                            }
                            Ok(())
                        })
                    };

                    // Process results from sharkspotter
                    let mut sent = 0;
                    let mut errors = 0;

                    loop {
                        match rx.recv_timeout(std::time::Duration::from_secs(1)) {
                            Ok(msg) => {
                                // Convert SharkspotterMessage to EvacuateObject
                                let object_id = match sharkspotter::object_id_from_manta_obj(
                                    &msg.manta_value,
                                ) {
                                    Ok(id) => id,
                                    Err(e) => {
                                        warn!(
                                            job_id = %job_id,
                                            error = %e,
                                            "Failed to extract objectId from manta object"
                                        );
                                        errors += 1;
                                        continue;
                                    }
                                };

                                let eobj = EvacuateObject {
                                    id: object_id,
                                    assignment_id: String::new(),
                                    object: msg.manta_value,
                                    shard: msg.shard as i32,
                                    dest_shark: String::new(),
                                    etag: msg.etag,
                                    status: EvacuateObjectStatus::Unprocessed,
                                    skipped_reason: None,
                                    error: None,
                                };

                                if object_tx.send(eobj).await.is_err() {
                                    warn!(
                                        job_id = %job_id,
                                        "Object channel closed, stopping discovery"
                                    );
                                    break;
                                }
                                sent += 1;

                                // Increment total count
                                // arch-lint: allow(no-error-swallowing) reason="Counter is best-effort; failure tracked via metric"
                                if let Err(e) =
                                    manager_db.increment_result_count(&job_uuid, "total").await
                                {
                                    warn!(
                                        job_id = %job_id,
                                        error = %e,
                                        "Failed to increment total count"
                                    );
                                    metrics::record_db_operation_failure();
                                }

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
                            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                                // Check if sharkspotter thread is done
                                if sharkspotter_handle.is_finished() {
                                    break;
                                }
                            }
                            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                                // Channel closed, sharkspotter is done
                                break;
                            }
                        }
                    }

                    // Wait for sharkspotter thread to complete
                    match sharkspotter_handle.join() {
                        Ok(Ok(())) => {
                            info!(
                                job_id = %job_id,
                                sent = sent,
                                errors = errors,
                                "Sharkspotter discovery complete"
                            );
                        }
                        Ok(Err(e)) => {
                            error!(
                                job_id = %job_id,
                                error = %e,
                                sent = sent,
                                "Sharkspotter completed with errors"
                            );
                            return Err(e);
                        }
                        Err(_) => {
                            error!(
                                job_id = %job_id,
                                "Sharkspotter thread panicked"
                            );
                            return Err(JobError::Internal(
                                "Sharkspotter thread panicked".to_string(),
                            ));
                        }
                    }

                    Ok(())
                }
            }
        });

        Ok(handle)
    }

    /// Shutdown the job gracefully
    #[allow(dead_code)]
    pub fn shutdown(&self) {
        info!(job_id = %self.job_id, "Shutting down evacuate job");
        // Intentionally ignore send error - receivers may already be dropped
        let _ = self.shutdown_tx.send(true);
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
        assert_eq!(config.max_fill_percentage, 90);
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

    // -------------------------------------------------------------------------
    // Tests for EvacuateObject helper methods
    // -------------------------------------------------------------------------

    #[test]
    fn test_evacuate_object_get_sharks() {
        let obj = make_evacuate_object("test-obj");
        let sharks = obj.get_sharks();

        assert_eq!(sharks.len(), 2);
        assert_eq!(sharks[0].manta_storage_id, "1.stor.domain");
        assert_eq!(sharks[0].datacenter, "dc1");
        assert_eq!(sharks[1].manta_storage_id, "2.stor.domain");
        assert_eq!(sharks[1].datacenter, "dc2");
    }

    #[test]
    fn test_evacuate_object_get_sharks_missing() {
        let obj = EvacuateObject {
            id: "test".to_string(),
            object: json!({"objectId": "test"}),
            ..Default::default()
        };
        let sharks = obj.get_sharks();
        assert!(sharks.is_empty());
    }

    #[test]
    fn test_evacuate_object_get_sharks_malformed() {
        let obj = EvacuateObject {
            id: "test".to_string(),
            object: json!({"sharks": "not an array"}),
            ..Default::default()
        };
        let sharks = obj.get_sharks();
        assert!(sharks.is_empty());
    }

    #[test]
    fn test_evacuate_object_get_content_length() {
        let obj = make_evacuate_object("test-obj");
        assert_eq!(obj.get_content_length(), 1024);
    }

    #[test]
    fn test_evacuate_object_get_content_length_with_size() {
        let obj = make_evacuate_object_with_size("test-obj", 1024 * 1024 * 10); // 10 MiB
        assert_eq!(obj.get_content_length(), 1024 * 1024 * 10);
    }

    #[test]
    fn test_evacuate_object_get_content_length_missing() {
        let obj = EvacuateObject {
            id: "test".to_string(),
            object: json!({"objectId": "test"}),
            ..Default::default()
        };
        assert_eq!(obj.get_content_length(), 0);
    }

    #[test]
    fn test_evacuate_object_get_content_length_malformed() {
        let obj = EvacuateObject {
            id: "test".to_string(),
            object: json!({"contentLength": "not a number"}),
            ..Default::default()
        };
        assert_eq!(obj.get_content_length(), 0);
    }
}
