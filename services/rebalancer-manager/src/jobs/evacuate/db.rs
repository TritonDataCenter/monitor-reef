// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2020 Joyent, Inc.
// Copyright 2026 Edgecast Cloud LLC.

//! PostgreSQL database layer for evacuate job state
//!
//! Each evacuate job gets its own database to track object evacuation progress.
//! This allows for crash recovery and provides a persistent record of the job.

use deadpool_postgres::{Config, Pool, Runtime};
use tokio_postgres::NoTls;
use tracing::{debug, warn};

use rebalancer_types::ObjectSkippedReason;

use super::JobError;
use super::types::{EvacuateObject, EvacuateObjectError, EvacuateObjectStatus};

/// Database layer for evacuate object tracking
pub struct EvacuateDb {
    pool: Pool,
    #[allow(dead_code)]
    db_name: String,
}

#[allow(dead_code)]
impl EvacuateDb {
    /// Create a new evacuate database connection
    ///
    /// Creates the database and tables if they don't exist.
    pub async fn new(job_id: &str, database_url: &str) -> Result<Self, JobError> {
        // Parse the database URL
        let pg_config: tokio_postgres::Config = database_url
            .parse()
            .map_err(|e| JobError::Database(format!("Invalid database URL: {}", e)))?;

        // Build deadpool config
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
        // Use job_id as database name for isolation
        cfg.dbname = Some(format!("evacuate_{}", job_id.replace('-', "_")));

        let pool = cfg
            .create_pool(Some(Runtime::Tokio1), NoTls)
            .map_err(|e| JobError::Database(format!("Failed to create pool: {}", e)))?;

        let db = Self {
            pool,
            db_name: job_id.to_string(),
        };

        // Initialize schema
        db.init_schema().await?;

        Ok(db)
    }

    /// Initialize the database schema
    async fn init_schema(&self) -> Result<(), JobError> {
        let client = self.pool.get().await?;

        // Create evacuateobjects table
        client
            .batch_execute(
                r#"
                CREATE TABLE IF NOT EXISTS evacuateobjects (
                    id TEXT PRIMARY KEY,
                    assignment_id TEXT NOT NULL DEFAULT '',
                    object JSONB NOT NULL,
                    shard INTEGER NOT NULL,
                    dest_shark TEXT NOT NULL DEFAULT '',
                    etag TEXT NOT NULL DEFAULT '',
                    status TEXT NOT NULL DEFAULT 'unprocessed'
                        CHECK(status IN ('unprocessed', 'assigned', 'skipped', 'error', 'post_processing', 'complete')),
                    skipped_reason TEXT,
                    error TEXT
                );

                CREATE INDEX IF NOT EXISTS idx_evacuateobjects_assignment
                    ON evacuateobjects(assignment_id);
                CREATE INDEX IF NOT EXISTS idx_evacuateobjects_status
                    ON evacuateobjects(status);

                CREATE TABLE IF NOT EXISTS config (
                    id INTEGER PRIMARY KEY,
                    from_shark JSONB NOT NULL
                );

                CREATE TABLE IF NOT EXISTS duplicates (
                    id TEXT PRIMARY KEY,
                    key TEXT NOT NULL,
                    shards INTEGER[] NOT NULL
                );
                "#,
            )
            .await?;

        debug!("Evacuate database schema initialized");
        Ok(())
    }

    /// Insert a new evacuate object
    pub async fn insert_object(&self, obj: &EvacuateObject) -> Result<(), JobError> {
        let client = self.pool.get().await?;

        let skipped_reason = obj.skipped_reason.as_ref().map(|r| r.to_string());
        let error = obj.error.as_ref().map(|e| e.to_string());

        client
            .execute(
                r#"
                INSERT INTO evacuateobjects
                    (id, assignment_id, object, shard, dest_shark, etag, status, skipped_reason, error)
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
                ON CONFLICT (id) DO UPDATE SET
                    assignment_id = EXCLUDED.assignment_id,
                    dest_shark = EXCLUDED.dest_shark,
                    status = EXCLUDED.status,
                    skipped_reason = EXCLUDED.skipped_reason,
                    error = EXCLUDED.error
                "#,
                &[
                    &obj.id,
                    &obj.assignment_id,
                    &obj.object,
                    &obj.shard,
                    &obj.dest_shark,
                    &obj.etag,
                    &obj.status.to_string(),
                    &skipped_reason,
                    &error,
                ],
            )
            .await?;

        Ok(())
    }

    /// Get all objects for an assignment
    pub async fn get_assignment_objects(
        &self,
        assignment_id: &str,
    ) -> Result<Vec<EvacuateObject>, JobError> {
        let client = self.pool.get().await?;

        let rows = client
            .query(
                r#"
                SELECT id, assignment_id, object, shard, dest_shark, etag, status, skipped_reason, error
                FROM evacuateobjects
                WHERE assignment_id = $1
                "#,
                &[&assignment_id],
            )
            .await?;

        let mut objects = Vec::with_capacity(rows.len());
        for row in rows {
            objects.push(row_to_evacuate_object(&row)?);
        }

        Ok(objects)
    }

    /// Mark an object as skipped
    pub async fn mark_object_skipped(
        &self,
        object_id: &str,
        reason: ObjectSkippedReason,
    ) -> Result<(), JobError> {
        let client = self.pool.get().await?;

        client
            .execute(
                r#"
                UPDATE evacuateobjects
                SET status = 'skipped', skipped_reason = $1
                WHERE id = $2
                "#,
                &[&reason.to_string(), &object_id],
            )
            .await?;

        Ok(())
    }

    /// Mark an object as having an error
    pub async fn mark_object_error(
        &self,
        object_id: &str,
        error: EvacuateObjectError,
    ) -> Result<(), JobError> {
        let client = self.pool.get().await?;

        client
            .execute(
                r#"
                UPDATE evacuateobjects
                SET status = 'error', error = $1
                WHERE id = $2
                "#,
                &[&error.to_string(), &object_id],
            )
            .await?;

        Ok(())
    }

    /// Mark an object as complete
    pub async fn mark_object_complete(&self, object_id: &str) -> Result<(), JobError> {
        let client = self.pool.get().await?;

        client
            .execute(
                "UPDATE evacuateobjects SET status = 'complete' WHERE id = $1",
                &[&object_id],
            )
            .await?;

        Ok(())
    }

    /// Set object status
    pub async fn set_object_status(
        &self,
        object_id: &str,
        status: EvacuateObjectStatus,
    ) -> Result<(), JobError> {
        let client = self.pool.get().await?;

        client
            .execute(
                "UPDATE evacuateobjects SET status = $1 WHERE id = $2",
                &[&status.to_string(), &object_id],
            )
            .await?;

        Ok(())
    }

    /// Get objects with a specific status
    #[allow(dead_code)]
    pub async fn get_objects_by_status(
        &self,
        status: EvacuateObjectStatus,
    ) -> Result<Vec<EvacuateObject>, JobError> {
        let client = self.pool.get().await?;

        let rows = client
            .query(
                r#"
                SELECT id, assignment_id, object, shard, dest_shark, etag, status, skipped_reason, error
                FROM evacuateobjects
                WHERE status = $1
                "#,
                &[&status.to_string()],
            )
            .await?;

        let mut objects = Vec::with_capacity(rows.len());
        for row in rows {
            objects.push(row_to_evacuate_object(&row)?);
        }

        Ok(objects)
    }

    /// Get count of objects by status
    #[allow(dead_code)]
    pub async fn get_status_counts(
        &self,
    ) -> Result<std::collections::HashMap<EvacuateObjectStatus, i64>, JobError> {
        let client = self.pool.get().await?;

        let rows = client
            .query(
                "SELECT status, COUNT(*) FROM evacuateobjects GROUP BY status",
                &[],
            )
            .await?;

        let mut counts = std::collections::HashMap::new();
        for row in rows {
            let status_str: String = row.get(0);
            let count: i64 = row.get(1);
            if let Ok(status) = status_str.parse() {
                counts.insert(status, count);
            }
        }

        Ok(counts)
    }

    /// Get objects that can be retried (unprocessed, skipped, or error status)
    ///
    /// Returns objects in batches for memory efficiency. Pass limit=0 for all objects.
    pub async fn get_retryable_objects(
        &self,
        limit: Option<u32>,
    ) -> Result<Vec<EvacuateObject>, JobError> {
        let client = self.pool.get().await?;

        let query = match limit {
            Some(n) if n > 0 => format!(
                r#"
                SELECT id, assignment_id, object, shard, dest_shark, etag, status, skipped_reason, error
                FROM evacuateobjects
                WHERE status IN ('unprocessed', 'skipped', 'error')
                ORDER BY id
                LIMIT {}
                "#,
                n
            ),
            _ => r#"
                SELECT id, assignment_id, object, shard, dest_shark, etag, status, skipped_reason, error
                FROM evacuateobjects
                WHERE status IN ('unprocessed', 'skipped', 'error')
                ORDER BY id
                "#
            .to_string(),
        };

        let rows = client.query(&query, &[]).await?;

        let mut objects = Vec::with_capacity(rows.len());
        for row in rows {
            objects.push(row_to_evacuate_object(&row)?);
        }

        Ok(objects)
    }
}

/// Convert a database row to an EvacuateObject
fn row_to_evacuate_object(row: &tokio_postgres::Row) -> Result<EvacuateObject, JobError> {
    let status_str: String = row.get(6);
    let status = status_str
        .parse()
        .map_err(|e: String| JobError::Database(e))?;

    let skipped_reason: Option<String> = row.get(7);
    let skipped_reason = skipped_reason.and_then(|s| {
        serde_json::from_str(&format!("\"{}\"", s))
            .map_err(|e| {
                warn!("Failed to parse skipped reason '{}': {}", s, e);
                e
            })
            .ok()
    });

    let error: Option<String> = row.get(8);
    let error = error.and_then(|s| s.parse().ok());

    Ok(EvacuateObject {
        id: row.get(0),
        assignment_id: row.get(1),
        object: row.get(2),
        shard: row.get(3),
        dest_shark: row.get(4),
        etag: row.get(5),
        status,
        skipped_reason,
        error,
    })
}
