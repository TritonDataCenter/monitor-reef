// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2020 Joyent, Inc.
// Copyright 2026 Edgecast Cloud LLC.

//! SQLite-based assignment storage
//!
//! Provides persistent storage for assignments and their tasks, enabling
//! crash recovery and tracking of assignment progress.

use std::path::Path;
use std::sync::Arc;

use rusqlite::{Connection, params};
use thiserror::Error;
use tokio::sync::Mutex;
use tracing::warn;

use rebalancer_types::{
    AgentAssignmentState, AgentAssignmentStats, Assignment, ObjectSkippedReason, StorageNode, Task,
    TaskStatus,
};

#[derive(Error, Debug)]
pub enum StorageError {
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("Assignment not found: {0}")]
    NotFound(String),
    #[error("Assignment not complete: {0}")]
    NotComplete(String),
    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}

/// SQLite-based storage for assignments
pub struct AssignmentStorage {
    conn: Arc<Mutex<Connection>>,
}

impl AssignmentStorage {
    /// Create a new storage instance, initializing the database schema
    pub fn new(db_path: &Path) -> Result<Self, StorageError> {
        let conn = Connection::open(db_path)?;

        // Create tables if they don't exist
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS assignments (
                uuid TEXT PRIMARY KEY,
                state TEXT NOT NULL DEFAULT 'scheduled',
                total_tasks INTEGER NOT NULL,
                completed_tasks INTEGER NOT NULL DEFAULT 0,
                failed_tasks INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE TABLE IF NOT EXISTS tasks (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                assignment_uuid TEXT NOT NULL,
                object_id TEXT NOT NULL,
                owner TEXT NOT NULL,
                md5sum TEXT NOT NULL,
                source_datacenter TEXT NOT NULL,
                source_storage_id TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'pending',
                failure_reason TEXT,
                FOREIGN KEY (assignment_uuid) REFERENCES assignments(uuid)
            );

            CREATE INDEX IF NOT EXISTS idx_tasks_assignment ON tasks(assignment_uuid);
            CREATE INDEX IF NOT EXISTS idx_tasks_status ON tasks(assignment_uuid, status);
            "#,
        )?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Check if an assignment exists
    pub async fn has_assignment(&self, uuid: &str) -> Result<bool, StorageError> {
        let conn = self.conn.lock().await;
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM assignments WHERE uuid = ?",
            params![uuid],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    /// Create a new assignment with its tasks
    pub async fn create(&self, uuid: &str, tasks: &[Task]) -> Result<(), StorageError> {
        let conn = self.conn.lock().await;

        conn.execute(
            "INSERT INTO assignments (uuid, total_tasks) VALUES (?, ?)",
            params![uuid, tasks.len() as i64],
        )?;

        let mut stmt = conn.prepare(
            r#"INSERT INTO tasks
               (assignment_uuid, object_id, owner, md5sum, source_datacenter, source_storage_id)
               VALUES (?, ?, ?, ?, ?, ?)"#,
        )?;

        for task in tasks {
            stmt.execute(params![
                uuid,
                task.object_id,
                task.owner,
                task.md5sum,
                task.source.datacenter,
                task.source.manta_storage_id,
            ])?;
        }

        Ok(())
    }

    /// Get assignment status
    pub async fn get(&self, uuid: &str) -> Result<Assignment, StorageError> {
        let conn = self.conn.lock().await;

        // Get assignment metadata
        let (state_str, total, completed, failed): (String, i64, i64, i64) = conn
            .query_row(
                "SELECT state, total_tasks, completed_tasks, failed_tasks FROM assignments WHERE uuid = ?",
                params![uuid],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => StorageError::NotFound(uuid.to_string()),
                other => StorageError::Sqlite(other),
            })?;

        // Convert state string to enum
        let state = match state_str.as_str() {
            "scheduled" => AgentAssignmentState::Scheduled,
            "running" => AgentAssignmentState::Running,
            "complete" => {
                // Get failed tasks if complete
                let failed_tasks = self.get_failed_tasks_inner(&conn, uuid)?;
                if failed_tasks.is_empty() {
                    AgentAssignmentState::Complete(None)
                } else {
                    AgentAssignmentState::Complete(Some(failed_tasks))
                }
            }
            _ => AgentAssignmentState::Scheduled,
        };

        Ok(Assignment {
            uuid: uuid.to_string(),
            stats: AgentAssignmentStats {
                state,
                failed: failed as usize,
                complete: completed as usize,
                total: total as usize,
            },
        })
    }

    /// Get failed tasks for an assignment (internal, requires holding lock)
    fn get_failed_tasks_inner(
        &self,
        conn: &Connection,
        uuid: &str,
    ) -> Result<Vec<Task>, StorageError> {
        let mut stmt = conn.prepare(
            r#"SELECT object_id, owner, md5sum, source_datacenter, source_storage_id, failure_reason
               FROM tasks WHERE assignment_uuid = ? AND status = 'failed'"#,
        )?;

        let tasks = stmt
            .query_map(params![uuid], |row| {
                let failure_reason: Option<String> = row.get(5)?;
                Ok(Task {
                    object_id: row.get(0)?,
                    owner: row.get(1)?,
                    md5sum: row.get(2)?,
                    source: StorageNode {
                        datacenter: row.get(3)?,
                        manta_storage_id: row.get(4)?,
                    },
                    status: match failure_reason {
                        Some(ref reason_str) => {
                            // Try to parse the reason, default to NetworkError if unknown
                            let reason: ObjectSkippedReason = serde_json::from_str(reason_str)
                                .unwrap_or_else(|e| {
                                    warn!(
                                        raw_reason = %reason_str,
                                        error = %e,
                                        "Failed to parse failure reason, defaulting to NetworkError"
                                    );
                                    ObjectSkippedReason::NetworkError
                                });
                            TaskStatus::Failed(reason)
                        }
                        None => TaskStatus::Failed(ObjectSkippedReason::NetworkError),
                    },
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(tasks)
    }

    /// Get pending tasks for an assignment
    pub async fn get_pending_tasks(&self, uuid: &str) -> Result<Vec<Task>, StorageError> {
        let conn = self.conn.lock().await;

        let mut stmt = conn.prepare(
            r#"SELECT object_id, owner, md5sum, source_datacenter, source_storage_id
               FROM tasks WHERE assignment_uuid = ? AND status = 'pending'"#,
        )?;

        let tasks = stmt
            .query_map(params![uuid], |row| {
                Ok(Task {
                    object_id: row.get(0)?,
                    owner: row.get(1)?,
                    md5sum: row.get(2)?,
                    source: StorageNode {
                        datacenter: row.get(3)?,
                        manta_storage_id: row.get(4)?,
                    },
                    status: TaskStatus::Pending,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(tasks)
    }

    /// Update assignment state
    pub async fn set_state(&self, uuid: &str, state: &str) -> Result<(), StorageError> {
        let conn = self.conn.lock().await;
        let rows_affected = conn.execute(
            "UPDATE assignments SET state = ? WHERE uuid = ?",
            params![state, uuid],
        )?;
        if rows_affected == 0 {
            return Err(StorageError::NotFound(uuid.to_string()));
        }
        Ok(())
    }

    /// Mark a task as complete
    pub async fn mark_task_complete(
        &self,
        uuid: &str,
        object_id: &str,
    ) -> Result<(), StorageError> {
        let conn = self.conn.lock().await;

        conn.execute(
            "UPDATE tasks SET status = 'complete' WHERE assignment_uuid = ? AND object_id = ?",
            params![uuid, object_id],
        )?;

        conn.execute(
            "UPDATE assignments SET completed_tasks = completed_tasks + 1 WHERE uuid = ?",
            params![uuid],
        )?;

        Ok(())
    }

    /// Mark a task as failed
    pub async fn mark_task_failed(
        &self,
        uuid: &str,
        object_id: &str,
        reason: &ObjectSkippedReason,
    ) -> Result<(), StorageError> {
        let conn = self.conn.lock().await;
        let reason_json = serde_json::to_string(reason)?;

        conn.execute(
            "UPDATE tasks SET status = 'failed', failure_reason = ? WHERE assignment_uuid = ? AND object_id = ?",
            params![reason_json, uuid, object_id],
        )?;

        conn.execute(
            "UPDATE assignments SET completed_tasks = completed_tasks + 1, failed_tasks = failed_tasks + 1 WHERE uuid = ?",
            params![uuid],
        )?;

        Ok(())
    }

    /// Check if all tasks are complete
    #[allow(dead_code)]
    pub async fn is_complete(&self, uuid: &str) -> Result<bool, StorageError> {
        let conn = self.conn.lock().await;
        let (total, completed): (i64, i64) = conn.query_row(
            "SELECT total_tasks, completed_tasks FROM assignments WHERE uuid = ?",
            params![uuid],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        Ok(completed >= total)
    }

    /// Get all incomplete assignments (scheduled or running)
    ///
    /// Used on startup to resume interrupted assignments.
    pub async fn get_incomplete_assignments(&self) -> Result<Vec<String>, StorageError> {
        let conn = self.conn.lock().await;
        let mut stmt =
            conn.prepare("SELECT uuid FROM assignments WHERE state IN ('scheduled', 'running')")?;

        let uuids = stmt
            .query_map([], |row| row.get(0))?
            .collect::<Result<Vec<String>, _>>()?;

        Ok(uuids)
    }

    /// Delete a completed assignment
    pub async fn delete(&self, uuid: &str) -> Result<(), StorageError> {
        let conn = self.conn.lock().await;

        // Check if assignment exists and is complete
        let state: String = conn
            .query_row(
                "SELECT state FROM assignments WHERE uuid = ?",
                params![uuid],
                |row| row.get(0),
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => StorageError::NotFound(uuid.to_string()),
                other => StorageError::Sqlite(other),
            })?;

        if state != "complete" {
            return Err(StorageError::NotComplete(uuid.to_string()));
        }

        // Delete tasks first (foreign key)
        conn.execute("DELETE FROM tasks WHERE assignment_uuid = ?", params![uuid])?;
        conn.execute("DELETE FROM assignments WHERE uuid = ?", params![uuid])?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn create_test_task(object_id: &str) -> Task {
        Task {
            object_id: object_id.to_string(),
            owner: "test-owner".to_string(),
            md5sum: "abc123".to_string(),
            source: StorageNode {
                datacenter: "dc1".to_string(),
                manta_storage_id: "1.stor.domain.com".to_string(),
            },
            status: TaskStatus::Pending,
        }
    }

    #[tokio::test]
    async fn test_create_and_get_assignment() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let storage = AssignmentStorage::new(&db_path).unwrap();

        let uuid = "test-uuid-123";
        let tasks = vec![create_test_task("obj1"), create_test_task("obj2")];

        storage.create(uuid, &tasks).await.unwrap();

        let assignment = storage.get(uuid).await.unwrap();
        assert_eq!(assignment.uuid, uuid);
        assert_eq!(assignment.stats.total, 2);
        assert_eq!(assignment.stats.complete, 0);
        assert_eq!(assignment.stats.failed, 0);
    }

    #[tokio::test]
    async fn test_mark_task_complete() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let storage = AssignmentStorage::new(&db_path).unwrap();

        let uuid = "test-uuid-456";
        let tasks = vec![create_test_task("obj1"), create_test_task("obj2")];

        storage.create(uuid, &tasks).await.unwrap();
        storage.mark_task_complete(uuid, "obj1").await.unwrap();

        let assignment = storage.get(uuid).await.unwrap();
        assert_eq!(assignment.stats.complete, 1);
        assert_eq!(assignment.stats.failed, 0);
    }

    #[tokio::test]
    async fn test_mark_task_failed() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let storage = AssignmentStorage::new(&db_path).unwrap();

        let uuid = "test-uuid-789";
        let tasks = vec![create_test_task("obj1")];

        storage.create(uuid, &tasks).await.unwrap();
        storage
            .mark_task_failed(uuid, "obj1", &ObjectSkippedReason::MD5Mismatch)
            .await
            .unwrap();

        let assignment = storage.get(uuid).await.unwrap();
        assert_eq!(assignment.stats.complete, 1);
        assert_eq!(assignment.stats.failed, 1);
    }

    #[tokio::test]
    async fn test_delete_requires_complete() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let storage = AssignmentStorage::new(&db_path).unwrap();

        let uuid = "test-uuid-delete";
        let tasks = vec![create_test_task("obj1")];

        storage.create(uuid, &tasks).await.unwrap();

        // Should fail because assignment is not complete
        let result = storage.delete(uuid).await;
        assert!(matches!(result, Err(StorageError::NotComplete(_))));

        // Mark complete and set state
        storage.mark_task_complete(uuid, "obj1").await.unwrap();
        storage.set_state(uuid, "complete").await.unwrap();

        // Now delete should succeed
        storage.delete(uuid).await.unwrap();

        // Should not exist anymore
        assert!(!storage.has_assignment(uuid).await.unwrap());
    }
}
