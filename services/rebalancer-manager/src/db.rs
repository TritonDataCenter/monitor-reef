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
    pub async fn get_job_results(
        &self,
        id: &uuid::Uuid,
    ) -> Result<HashMap<String, i64>, DbError> {
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
