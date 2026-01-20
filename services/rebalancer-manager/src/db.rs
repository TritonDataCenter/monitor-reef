// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2020 Joyent, Inc.
// Copyright 2026 Edgecast Cloud LLC.

//! PostgreSQL database layer using tokio-postgres
//!
//! Provides async database operations for job management using a pure-Rust
//! PostgreSQL driver (no libpq dependency).

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use deadpool_postgres::{Config, Pool, Runtime};
use thiserror::Error;
use tokio_postgres::NoTls;

use rebalancer_types::{JobAction, JobDbEntry, JobState};

/// Database errors
#[derive(Debug, Error)]
pub enum DbError {
    #[error("Database connection error: {0}")]
    Connection(String),

    #[error("Query error: {0}")]
    Query(String),

    #[error("Job not found: {0}")]
    NotFound(String),

    #[error("Cannot update job: {0}")]
    CannotUpdate(String),

    #[error("Cannot retry job: {0}")]
    CannotRetry(String),
}

impl From<tokio_postgres::Error> for DbError {
    fn from(e: tokio_postgres::Error) -> Self {
        DbError::Query(e.to_string())
    }
}

impl From<deadpool_postgres::PoolError> for DbError {
    fn from(e: deadpool_postgres::PoolError) -> Self {
        DbError::Connection(e.to_string())
    }
}

/// Database row for a job
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct JobRow {
    pub id: uuid::Uuid,
    pub action: String,
    pub state: String,
    pub from_shark: Option<String>,
    pub from_shark_datacenter: Option<String>,
    pub max_objects: Option<i32>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl JobRow {
    /// Convert to API JobDbEntry type (for listing)
    pub fn into_db_entry(self) -> JobDbEntry {
        JobDbEntry {
            id: self.id.to_string(),
            action: parse_job_action(&self.action),
            state: parse_job_state(&self.state),
        }
    }
}

fn parse_job_action(s: &str) -> JobAction {
    match s {
        "evacuate" => JobAction::Evacuate,
        _ => JobAction::None,
    }
}

fn parse_job_state(s: &str) -> JobState {
    match s {
        "init" => JobState::Init,
        "setup" => JobState::Setup,
        "running" => JobState::Running,
        "stopped" => JobState::Stopped,
        "complete" => JobState::Complete,
        "failed" => JobState::Failed,
        _ => JobState::Init,
    }
}

/// Database operations
pub struct Database {
    pool: Pool,
}

impl Database {
    /// Create a new database connection pool from a connection URL
    pub async fn new(database_url: &str) -> Result<Self, DbError> {
        // Parse the database URL using tokio-postgres
        let pg_config: tokio_postgres::Config = database_url
            .parse()
            .map_err(|e| DbError::Connection(format!("Invalid database URL: {}", e)))?;

        // Build deadpool config from tokio-postgres config
        let mut cfg = Config::new();
        if let Some(hosts) = pg_config.get_hosts().first() {
            match hosts {
                tokio_postgres::config::Host::Tcp(host) => {
                    cfg.host = Some(host.clone());
                }
                tokio_postgres::config::Host::Unix(path) => {
                    cfg.host = Some(path.to_string_lossy().to_string());
                }
            }
        }
        if let Some(ports) = pg_config.get_ports().first() {
            cfg.port = Some(*ports);
        }
        if let Some(user) = pg_config.get_user() {
            cfg.user = Some(user.to_string());
        }
        if let Some(password) = pg_config.get_password() {
            cfg.password = Some(String::from_utf8_lossy(password).to_string());
        }
        if let Some(dbname) = pg_config.get_dbname() {
            cfg.dbname = Some(dbname.to_string());
        }

        let pool = cfg
            .create_pool(Some(Runtime::Tokio1), NoTls)
            .map_err(|e| DbError::Connection(format!("Failed to create pool: {}", e)))?;

        // Test the connection
        let client = pool.get().await?;
        client
            .execute("SELECT 1", &[])
            .await
            .map_err(|e| DbError::Connection(format!("Failed to connect to database: {}", e)))?;

        Ok(Self { pool })
    }

    /// Create a new evacuate job
    pub async fn create_evacuate_job(
        &self,
        id: uuid::Uuid,
        from_shark: &str,
        from_shark_datacenter: &str,
        max_objects: Option<u32>,
    ) -> Result<String, DbError> {
        let client = self.pool.get().await?;
        let now = Utc::now();

        client
            .execute(
                "INSERT INTO jobs (id, action, state, from_shark, from_shark_datacenter, max_objects, created_at, updated_at)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
                &[
                    &id,
                    &"evacuate",
                    &"init",
                    &from_shark,
                    &from_shark_datacenter,
                    &max_objects.map(|n| n as i32),
                    &now,
                    &now,
                ],
            )
            .await?;

        Ok(id.to_string())
    }

    /// Get a job by ID
    pub async fn get_job(&self, id: &uuid::Uuid) -> Result<JobRow, DbError> {
        let client = self.pool.get().await?;

        let row = client
            .query_opt(
                "SELECT id, action, state, from_shark, from_shark_datacenter, max_objects, created_at, updated_at
                 FROM jobs WHERE id = $1",
                &[id],
            )
            .await?
            .ok_or_else(|| DbError::NotFound(id.to_string()))?;

        Ok(JobRow {
            id: row.get(0),
            action: row.get(1),
            state: row.get(2),
            from_shark: row.get(3),
            from_shark_datacenter: row.get(4),
            max_objects: row.get(5),
            created_at: row.get(6),
            updated_at: row.get(7),
        })
    }

    /// List all jobs
    pub async fn list_jobs(&self) -> Result<Vec<JobDbEntry>, DbError> {
        let client = self.pool.get().await?;

        let rows = client
            .query(
                "SELECT id, action, state, from_shark, from_shark_datacenter, max_objects, created_at, updated_at
                 FROM jobs ORDER BY created_at DESC",
                &[],
            )
            .await?;

        Ok(rows
            .into_iter()
            .map(|row| {
                JobRow {
                    id: row.get(0),
                    action: row.get(1),
                    state: row.get(2),
                    from_shark: row.get(3),
                    from_shark_datacenter: row.get(4),
                    max_objects: row.get(5),
                    created_at: row.get(6),
                    updated_at: row.get(7),
                }
                .into_db_entry()
            })
            .collect())
    }

    /// Update job state
    #[allow(dead_code)]
    pub async fn update_job_state(&self, id: &uuid::Uuid, new_state: &str) -> Result<(), DbError> {
        let client = self.pool.get().await?;
        let now = Utc::now();

        let updated = client
            .execute(
                "UPDATE jobs SET state = $1, updated_at = $2 WHERE id = $3",
                &[&new_state, &now, id],
            )
            .await?;

        if updated == 0 {
            return Err(DbError::NotFound(id.to_string()));
        }

        Ok(())
    }

    /// Get job result counts
    pub async fn get_job_results(&self, id: &uuid::Uuid) -> Result<HashMap<String, i64>, DbError> {
        let client = self.pool.get().await?;

        let rows = client
            .query(
                "SELECT status, count FROM job_results WHERE job_id = $1",
                &[id],
            )
            .await?;

        Ok(rows
            .into_iter()
            .map(|row| {
                let status: String = row.get(0);
                let count: i64 = row.get(1);
                (status, count)
            })
            .collect())
    }

    /// Increment a result count for a job
    #[allow(dead_code)]
    pub async fn increment_result_count(
        &self,
        id: &uuid::Uuid,
        status: &str,
    ) -> Result<(), DbError> {
        let client = self.pool.get().await?;

        client
            .execute(
                "INSERT INTO job_results (job_id, status, count) VALUES ($1, $2, 1)
                 ON CONFLICT (job_id, status) DO UPDATE SET count = job_results.count + 1",
                &[id, &status],
            )
            .await?;

        Ok(())
    }
}

// ============================================================================
// Mock Database for Testing
// ============================================================================

/// Mock database for testing without PostgreSQL.
///
/// This provides an in-memory implementation of the database operations
/// that can be used in unit tests without requiring a real database.
#[cfg(test)]
pub mod mock {
    use super::*;
    use std::sync::Mutex;

    /// In-memory mock database
    pub struct MockDatabase {
        jobs: Mutex<HashMap<uuid::Uuid, JobRow>>,
        job_results: Mutex<HashMap<uuid::Uuid, HashMap<String, i64>>>,
    }

    impl MockDatabase {
        /// Create a new empty mock database
        pub fn new() -> Self {
            Self {
                jobs: Mutex::new(HashMap::new()),
                job_results: Mutex::new(HashMap::new()),
            }
        }

        /// Insert a job directly (for test setup)
        pub fn insert_job(&self, job: JobRow) {
            let mut jobs = self.jobs.lock().unwrap();
            jobs.insert(job.id, job);
        }

        /// Set result counts for a job (for test setup)
        pub fn set_job_results(&self, job_id: uuid::Uuid, results: HashMap<String, i64>) {
            let mut job_results = self.job_results.lock().unwrap();
            job_results.insert(job_id, results);
        }

        /// Create a new evacuate job
        pub fn create_evacuate_job(
            &self,
            id: uuid::Uuid,
            from_shark: &str,
            from_shark_datacenter: &str,
            max_objects: Option<u32>,
        ) -> Result<String, DbError> {
            let now = Utc::now();
            let job = JobRow {
                id,
                action: "evacuate".to_string(),
                state: "init".to_string(),
                from_shark: Some(from_shark.to_string()),
                from_shark_datacenter: Some(from_shark_datacenter.to_string()),
                max_objects: max_objects.map(|n| n as i32),
                created_at: now,
                updated_at: now,
            };
            self.insert_job(job);
            Ok(id.to_string())
        }

        /// Get a job by ID
        pub fn get_job(&self, id: &uuid::Uuid) -> Result<JobRow, DbError> {
            let jobs = self.jobs.lock().unwrap();
            jobs.get(id)
                .cloned()
                .ok_or_else(|| DbError::NotFound(id.to_string()))
        }

        /// List all jobs
        pub fn list_jobs(&self) -> Result<Vec<JobDbEntry>, DbError> {
            let jobs = self.jobs.lock().unwrap();
            let mut entries: Vec<_> = jobs.values().cloned().collect();
            // Sort by created_at descending
            entries.sort_by(|a, b| b.created_at.cmp(&a.created_at));
            Ok(entries.into_iter().map(|j| j.into_db_entry()).collect())
        }

        /// Update job state
        pub fn update_job_state(&self, id: &uuid::Uuid, new_state: &str) -> Result<(), DbError> {
            let mut jobs = self.jobs.lock().unwrap();
            let job = jobs
                .get_mut(id)
                .ok_or_else(|| DbError::NotFound(id.to_string()))?;
            job.state = new_state.to_string();
            job.updated_at = Utc::now();
            Ok(())
        }

        /// Get job result counts
        pub fn get_job_results(&self, id: &uuid::Uuid) -> Result<HashMap<String, i64>, DbError> {
            let job_results = self.job_results.lock().unwrap();
            Ok(job_results.get(id).cloned().unwrap_or_default())
        }

        /// Increment a result count for a job
        pub fn increment_result_count(&self, id: &uuid::Uuid, status: &str) -> Result<(), DbError> {
            let mut job_results = self.job_results.lock().unwrap();
            let results = job_results.entry(*id).or_default();
            *results.entry(status.to_string()).or_insert(0) += 1;
            Ok(())
        }
    }

    impl Default for MockDatabase {
        fn default() -> Self {
            Self::new()
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::mock::MockDatabase;
    use super::*;

    /// Test: list_jobs returns successfully (even if empty)
    ///
    /// Port of legacy `list_job_test` from libs/rebalancer-legacy/manager/src/jobs/status.rs
    #[test]
    fn list_jobs_test() {
        let db = MockDatabase::new();

        // Empty list should be OK
        let result = db.list_jobs();
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());

        // Add a job and verify it appears in the list
        let job_id = uuid::Uuid::new_v4();
        db.create_evacuate_job(job_id, "fake_shark", "dc1", Some(100))
            .expect("create job");

        let jobs = db.list_jobs().expect("list jobs");
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].id, job_id.to_string());
        assert_eq!(jobs[0].action, JobAction::Evacuate);
        assert_eq!(jobs[0].state, JobState::Init);
    }

    /// Test: get_job returns error for non-existent job UUID
    ///
    /// Port of legacy `bad_job_id` from libs/rebalancer-legacy/manager/src/jobs/status.rs
    #[test]
    fn bad_job_id() {
        let db = MockDatabase::new();
        let uuid = uuid::Uuid::new_v4();

        // Should fail for non-existent job
        let result = db.get_job(&uuid);
        assert!(result.is_err());
        match result {
            Err(DbError::NotFound(id)) => assert_eq!(id, uuid.to_string()),
            _ => panic!("Expected NotFound error"),
        }
    }

    /// Test: get_job_status returns correct result counts
    ///
    /// Port of legacy `get_status_test` from libs/rebalancer-legacy/manager/src/jobs/status.rs
    #[test]
    fn get_status_test() {
        const NUM_OBJS: i64 = 200;

        let db = MockDatabase::new();
        let job_id = uuid::Uuid::new_v4();

        // Create an evacuate job
        db.create_evacuate_job(job_id, "fake_shark", "dc1", Some(NUM_OBJS as u32))
            .expect("create job");

        // Update job state to running (so it's past init)
        db.update_job_state(&job_id, "running")
            .expect("update state");

        // Set up result counts simulating object processing
        let mut results = HashMap::new();
        results.insert("Total".to_string(), NUM_OBJS);
        results.insert("Complete".to_string(), 150);
        results.insert("Failed".to_string(), 50);
        db.set_job_results(job_id, results);

        // Verify job exists and has correct state
        let job = db.get_job(&job_id).expect("get job");
        assert_eq!(job.action, "evacuate");
        assert_eq!(job.state, "running");

        // Verify result counts
        let job_results = db.get_job_results(&job_id).expect("get job results");
        let total_count = *job_results.get("Total").expect("Total count");
        assert_eq!(total_count, NUM_OBJS);
    }

    /// Test: get_job_status handles zero values correctly
    ///
    /// Port of legacy `get_status_zero_value_test` from libs/rebalancer-legacy/manager/src/jobs/status.rs
    ///
    /// This tests that when a status category has zero objects, it either:
    /// - Doesn't appear in the results (not inserted)
    /// - Or appears with a count of 0
    #[test]
    fn get_status_zero_value_test() {
        const NUM_OBJS: i64 = 200;

        let db = MockDatabase::new();
        let job_id = uuid::Uuid::new_v4();

        // Create an evacuate job
        db.create_evacuate_job(job_id, "fake_shark", "dc1", Some(NUM_OBJS as u32))
            .expect("create job");

        // Update job state to running
        db.update_job_state(&job_id, "running")
            .expect("update state");

        // Set up result counts with some statuses having zero
        // In the legacy test, Post Processing was set to 0 by avoiding that status
        let mut results = HashMap::new();
        results.insert("Total".to_string(), NUM_OBJS);
        results.insert("Complete".to_string(), 100);
        results.insert("Assigned".to_string(), 100);
        results.insert("Post Processing".to_string(), 0);
        db.set_job_results(job_id, results);

        // Verify result counts
        let job_results = db.get_job_results(&job_id).expect("get job results");

        let total_count = *job_results.get("Total").expect("Total count");
        assert_eq!(total_count, NUM_OBJS);

        let post_processing_count = *job_results
            .get("Post Processing")
            .expect("Post Processing count");
        assert_eq!(post_processing_count, 0);
    }

    /// Test: JobRow converts to JobDbEntry correctly
    #[test]
    fn job_row_into_db_entry() {
        let job_id = uuid::Uuid::new_v4();
        let now = Utc::now();

        let row = JobRow {
            id: job_id,
            action: "evacuate".to_string(),
            state: "running".to_string(),
            from_shark: Some("1.stor.domain.com".to_string()),
            from_shark_datacenter: Some("dc1".to_string()),
            max_objects: Some(100),
            created_at: now,
            updated_at: now,
        };

        let entry = row.into_db_entry();
        assert_eq!(entry.id, job_id.to_string());
        assert_eq!(entry.action, JobAction::Evacuate);
        assert_eq!(entry.state, JobState::Running);
    }

    /// Test: parse_job_action handles all known actions
    #[test]
    fn test_parse_job_action() {
        assert_eq!(parse_job_action("evacuate"), JobAction::Evacuate);
        assert_eq!(parse_job_action("unknown"), JobAction::None);
        assert_eq!(parse_job_action(""), JobAction::None);
    }

    /// Test: parse_job_state handles all known states
    #[test]
    fn test_parse_job_state() {
        assert_eq!(parse_job_state("init"), JobState::Init);
        assert_eq!(parse_job_state("setup"), JobState::Setup);
        assert_eq!(parse_job_state("running"), JobState::Running);
        assert_eq!(parse_job_state("stopped"), JobState::Stopped);
        assert_eq!(parse_job_state("complete"), JobState::Complete);
        assert_eq!(parse_job_state("failed"), JobState::Failed);
        assert_eq!(parse_job_state("unknown"), JobState::Init);
    }

    /// Test: increment_result_count works correctly
    #[test]
    fn test_increment_result_count() {
        let db = MockDatabase::new();
        let job_id = uuid::Uuid::new_v4();

        // Create a job first
        db.create_evacuate_job(job_id, "fake_shark", "dc1", None)
            .expect("create job");

        // Increment a status count multiple times
        db.increment_result_count(&job_id, "Complete")
            .expect("increment");
        db.increment_result_count(&job_id, "Complete")
            .expect("increment");
        db.increment_result_count(&job_id, "Failed")
            .expect("increment");

        let results = db.get_job_results(&job_id).expect("get results");
        assert_eq!(*results.get("Complete").unwrap(), 2);
        assert_eq!(*results.get("Failed").unwrap(), 1);
    }
}
