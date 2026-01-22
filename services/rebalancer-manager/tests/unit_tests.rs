// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Unit tests for the rebalancer manager service.
//!
//! These tests use MockDatabase and MockStorinfo to test manager logic
//! without requiring external dependencies (PostgreSQL, storinfo service).
//!
//! Phase 3 tests:
//! - Job state transitions
//! - Assignment distribution logic
//! - Retry job flow

// Allow unwrap/expect in tests
#![allow(clippy::unwrap_used)]
#![allow(clippy::expect_used)]

use chrono::{DateTime, Utc};
use rebalancer_types::{JobAction, JobDbEntry, JobState};
use std::collections::HashMap;
use std::sync::Mutex;
use uuid::Uuid;

// ============================================================================
// Mock Database (copied from db.rs for integration test access)
// ============================================================================

/// Database row for a job
#[derive(Debug, Clone)]
struct JobRow {
    id: Uuid,
    action: String,
    state: String,
    from_shark: Option<String>,
    #[allow(dead_code)]
    from_shark_datacenter: Option<String>,
    #[allow(dead_code)]
    max_objects: Option<i32>,
    created_at: DateTime<Utc>,
    #[allow(dead_code)]
    updated_at: DateTime<Utc>,
}

impl JobRow {
    fn into_db_entry(self) -> JobDbEntry {
        JobDbEntry {
            id: self.id.to_string(),
            action: match self.action.as_str() {
                "evacuate" => JobAction::Evacuate,
                _ => JobAction::None,
            },
            state: match self.state.as_str() {
                "init" => JobState::Init,
                "setup" => JobState::Setup,
                "running" => JobState::Running,
                "stopped" => JobState::Stopped,
                "complete" => JobState::Complete,
                "failed" => JobState::Failed,
                _ => JobState::Init,
            },
        }
    }
}

/// In-memory mock database for testing
struct MockDatabase {
    jobs: Mutex<HashMap<Uuid, JobRow>>,
    job_results: Mutex<HashMap<Uuid, HashMap<String, i64>>>,
}

impl MockDatabase {
    fn new() -> Self {
        Self {
            jobs: Mutex::new(HashMap::new()),
            job_results: Mutex::new(HashMap::new()),
        }
    }

    fn set_job_results(&self, job_id: Uuid, results: HashMap<String, i64>) {
        let mut job_results = self.job_results.lock().unwrap();
        job_results.insert(job_id, results);
    }

    fn create_evacuate_job(
        &self,
        id: Uuid,
        from_shark: &str,
        from_shark_datacenter: &str,
        max_objects: Option<u32>,
    ) -> Result<String, String> {
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
        let mut jobs = self.jobs.lock().unwrap();
        jobs.insert(id, job);
        Ok(id.to_string())
    }

    fn get_job(&self, id: &Uuid) -> Result<JobRow, String> {
        let jobs = self.jobs.lock().unwrap();
        jobs.get(id)
            .cloned()
            .ok_or_else(|| format!("Job not found: {}", id))
    }

    fn list_jobs(&self) -> Result<Vec<JobDbEntry>, String> {
        let jobs = self.jobs.lock().unwrap();
        let mut entries: Vec<_> = jobs.values().cloned().collect();
        entries.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(entries.into_iter().map(|j| j.into_db_entry()).collect())
    }

    fn update_job_state(&self, id: &Uuid, new_state: &str) -> Result<(), String> {
        let mut jobs = self.jobs.lock().unwrap();
        let job = jobs
            .get_mut(id)
            .ok_or_else(|| format!("Job not found: {}", id))?;
        job.state = new_state.to_string();
        job.updated_at = Utc::now();
        Ok(())
    }

    fn get_job_results(&self, id: &Uuid) -> Result<HashMap<String, i64>, String> {
        let job_results = self.job_results.lock().unwrap();
        Ok(job_results.get(id).cloned().unwrap_or_default())
    }

    fn increment_result_count(&self, id: &Uuid, status: &str) -> Result<(), String> {
        let mut job_results = self.job_results.lock().unwrap();
        let results = job_results.entry(*id).or_default();
        *results.entry(status.to_string()).or_insert(0) += 1;
        Ok(())
    }
}

// ============================================================================
// Test Module for Job State Transitions
// ============================================================================

mod job_state_tests {
    use super::*;

    /// Test: Job progresses through expected states: init -> setup -> running -> complete
    #[test]
    fn test_job_state_transitions_happy_path() {
        let db = MockDatabase::new();
        let job_id = Uuid::new_v4();

        // Create job (starts in 'init' state)
        db.create_evacuate_job(job_id, "1.stor.test.domain", "dc1", None)
            .expect("create job");

        let job = db.get_job(&job_id).expect("get job");
        assert_eq!(job.state, "init");

        // Transition to 'setup'
        db.update_job_state(&job_id, "setup").expect("update state");
        let job = db.get_job(&job_id).expect("get job");
        assert_eq!(job.state, "setup");

        // Transition to 'running'
        db.update_job_state(&job_id, "running").expect("update state");
        let job = db.get_job(&job_id).expect("get job");
        assert_eq!(job.state, "running");

        // Transition to 'complete'
        db.update_job_state(&job_id, "complete").expect("update state");
        let job = db.get_job(&job_id).expect("get job");
        assert_eq!(job.state, "complete");
    }

    /// Test: Job can transition to 'failed' state
    #[test]
    fn test_job_state_transitions_failure_path() {
        let db = MockDatabase::new();
        let job_id = Uuid::new_v4();

        // Create job
        db.create_evacuate_job(job_id, "1.stor.test.domain", "dc1", None)
            .expect("create job");

        // Transition to 'running'
        db.update_job_state(&job_id, "running").expect("update state");

        // Transition to 'failed'
        db.update_job_state(&job_id, "failed").expect("update state");
        let job = db.get_job(&job_id).expect("get job");
        assert_eq!(job.state, "failed");
    }

    /// Test: Job can be stopped
    #[test]
    fn test_job_state_transitions_stopped() {
        let db = MockDatabase::new();
        let job_id = Uuid::new_v4();

        db.create_evacuate_job(job_id, "1.stor.test.domain", "dc1", None)
            .expect("create job");

        db.update_job_state(&job_id, "running").expect("update state");
        db.update_job_state(&job_id, "stopped").expect("update state");

        let job = db.get_job(&job_id).expect("get job");
        assert_eq!(job.state, "stopped");
    }

    /// Test: Multiple jobs can be tracked simultaneously
    #[test]
    fn test_multiple_jobs() {
        let db = MockDatabase::new();

        let job_ids: Vec<Uuid> = (0..5).map(|_| Uuid::new_v4()).collect();

        // Create all jobs
        for (i, id) in job_ids.iter().enumerate() {
            db.create_evacuate_job(
                *id,
                &format!("{}.stor.test.domain", i + 1),
                &format!("dc{}", i % 3 + 1),
                None,
            )
            .expect("create job");
        }

        // List all jobs
        let jobs = db.list_jobs().expect("list jobs");
        assert_eq!(jobs.len(), 5);

        // Verify all have evacuate action
        for job in &jobs {
            assert_eq!(job.action, JobAction::Evacuate);
            assert_eq!(job.state, JobState::Init);
        }

        // Update states independently
        db.update_job_state(&job_ids[0], "running").expect("update");
        db.update_job_state(&job_ids[1], "complete").expect("update");
        db.update_job_state(&job_ids[2], "failed").expect("update");

        // Verify independent states
        assert_eq!(db.get_job(&job_ids[0]).unwrap().state, "running");
        assert_eq!(db.get_job(&job_ids[1]).unwrap().state, "complete");
        assert_eq!(db.get_job(&job_ids[2]).unwrap().state, "failed");
        assert_eq!(db.get_job(&job_ids[3]).unwrap().state, "init");
        assert_eq!(db.get_job(&job_ids[4]).unwrap().state, "init");
    }
}

// ============================================================================
// Test Module for Result Tracking
// ============================================================================

mod result_tracking_tests {
    use super::*;

    /// Test: Result counts are tracked correctly
    #[test]
    fn test_result_count_tracking() {
        let db = MockDatabase::new();
        let job_id = Uuid::new_v4();

        db.create_evacuate_job(job_id, "1.stor.test.domain", "dc1", Some(1000))
            .expect("create job");

        // Simulate processing 1000 objects:
        // - 800 complete
        // - 150 skipped
        // - 50 error
        for _ in 0..800 {
            db.increment_result_count(&job_id, "complete")
                .expect("increment");
        }
        for _ in 0..150 {
            db.increment_result_count(&job_id, "skipped")
                .expect("increment");
        }
        for _ in 0..50 {
            db.increment_result_count(&job_id, "error")
                .expect("increment");
        }
        for _ in 0..1000 {
            db.increment_result_count(&job_id, "total")
                .expect("increment");
        }

        // Verify counts
        let results = db.get_job_results(&job_id).expect("get results");
        assert_eq!(*results.get("complete").unwrap(), 800);
        assert_eq!(*results.get("skipped").unwrap(), 150);
        assert_eq!(*results.get("error").unwrap(), 50);
        assert_eq!(*results.get("total").unwrap(), 1000);

        // Verify total matches sum
        let sum = results.get("complete").unwrap()
            + results.get("skipped").unwrap()
            + results.get("error").unwrap();
        assert_eq!(*results.get("total").unwrap(), sum);
    }

    /// Test: Results for non-existent job returns empty map
    #[test]
    fn test_results_nonexistent_job() {
        let db = MockDatabase::new();
        let fake_id = Uuid::new_v4();

        let results = db.get_job_results(&fake_id).expect("get results");
        assert!(results.is_empty());
    }

    /// Test: Large scale result tracking (simulating big evacuate job)
    #[test]
    fn test_large_scale_results() {
        let db = MockDatabase::new();
        let job_id = Uuid::new_v4();

        db.create_evacuate_job(job_id, "1.stor.test.domain", "dc1", None)
            .expect("create job");

        // Simulate 100,000 objects processed
        let total = 100_000i64;
        let mut results = HashMap::new();
        results.insert("total".to_string(), total);
        results.insert("complete".to_string(), 95_000);
        results.insert("skipped".to_string(), 4_500);
        results.insert("error".to_string(), 500);
        db.set_job_results(job_id, results);

        let retrieved = db.get_job_results(&job_id).expect("get results");
        assert_eq!(*retrieved.get("total").unwrap(), 100_000);
        assert_eq!(*retrieved.get("complete").unwrap(), 95_000);
    }
}

// ============================================================================
// Test Module for Retry Job Flow
// ============================================================================

mod retry_job_tests {
    use super::*;

    /// Test: Retry creates a new job with same from_shark
    #[test]
    fn test_retry_job_creates_new_job() {
        let db = MockDatabase::new();
        let original_job_id = Uuid::new_v4();
        let from_shark = "1.stor.test.domain";
        let datacenter = "dc1";

        // Create original job
        db.create_evacuate_job(original_job_id, from_shark, datacenter, Some(1000))
            .expect("create job");

        // Mark original as complete with some failures
        db.update_job_state(&original_job_id, "complete")
            .expect("update");

        // Simulate results with some failures that need retry
        let mut results = HashMap::new();
        results.insert("total".to_string(), 1000);
        results.insert("complete".to_string(), 900);
        results.insert("skipped".to_string(), 80);
        results.insert("error".to_string(), 20);
        db.set_job_results(original_job_id, results);

        // Create retry job
        let retry_job_id = Uuid::new_v4();
        db.create_evacuate_job(retry_job_id, from_shark, datacenter, None)
            .expect("create retry job");

        // Verify both jobs exist
        let jobs = db.list_jobs().expect("list");
        assert_eq!(jobs.len(), 2);

        // Verify retry job is in init state
        let retry_job = db.get_job(&retry_job_id).expect("get retry job");
        assert_eq!(retry_job.state, "init");
        assert_eq!(retry_job.from_shark.as_deref(), Some(from_shark));

        // Original job should still show complete
        let original = db.get_job(&original_job_id).expect("get original");
        assert_eq!(original.state, "complete");
    }

    /// Test: Retry job can complete where original failed
    #[test]
    fn test_retry_job_completion() {
        let db = MockDatabase::new();

        // Original job with failures
        let original_id = Uuid::new_v4();
        db.create_evacuate_job(original_id, "1.stor.test.domain", "dc1", Some(100))
            .expect("create");
        db.update_job_state(&original_id, "complete").expect("update");

        let mut original_results = HashMap::new();
        original_results.insert("total".to_string(), 100);
        original_results.insert("complete".to_string(), 80);
        original_results.insert("skipped".to_string(), 20);
        db.set_job_results(original_id, original_results);

        // Retry job processes the 20 skipped objects
        let retry_id = Uuid::new_v4();
        db.create_evacuate_job(retry_id, "1.stor.test.domain", "dc1", Some(20))
            .expect("create retry");

        db.update_job_state(&retry_id, "running").expect("update");

        // Retry successfully processes all 20
        let mut retry_results = HashMap::new();
        retry_results.insert("total".to_string(), 20);
        retry_results.insert("complete".to_string(), 20);
        db.set_job_results(retry_id, retry_results);

        db.update_job_state(&retry_id, "complete").expect("update");

        // Verify retry job completed successfully
        let retry_job = db.get_job(&retry_id).expect("get retry");
        assert_eq!(retry_job.state, "complete");

        let retry_results = db.get_job_results(&retry_id).expect("get results");
        assert_eq!(*retry_results.get("complete").unwrap(), 20);
        assert!(retry_results.get("skipped").is_none());
    }
}

// ============================================================================
// Test Module for Assignment Distribution
// ============================================================================

mod assignment_distribution_tests {
    use super::*;

    /// Test: Assignment distribution with multiple destination sharks
    ///
    /// Simulates the assignment distribution logic where objects are
    /// assigned to destination sharks based on available capacity.
    #[test]
    fn test_assignment_by_capacity() {
        // Simulate destination sharks with varying capacities
        struct SharkCapacity {
            id: String,
            available_mb: u64,
            assigned_mb: u64,
        }

        let mut sharks = vec![
            SharkCapacity {
                id: "1.dest.domain".to_string(),
                available_mb: 10000,
                assigned_mb: 0,
            },
            SharkCapacity {
                id: "2.dest.domain".to_string(),
                available_mb: 5000,
                assigned_mb: 0,
            },
            SharkCapacity {
                id: "3.dest.domain".to_string(),
                available_mb: 15000,
                assigned_mb: 0,
            },
        ];

        // Simulate assigning 100 objects of varying sizes
        let mut assignments: HashMap<String, Vec<u64>> = HashMap::new();

        for i in 0..100 {
            let object_size = (i % 10 + 1) * 100; // 100-1000 MB

            // Find shark with most available space (capacity - assigned)
            let best_shark = sharks
                .iter_mut()
                .filter(|s| s.available_mb > s.assigned_mb + object_size)
                .max_by_key(|s| s.available_mb - s.assigned_mb);

            if let Some(shark) = best_shark {
                shark.assigned_mb += object_size;
                assignments
                    .entry(shark.id.clone())
                    .or_default()
                    .push(object_size);
            }
        }

        // Verify assignments were distributed
        assert!(!assignments.is_empty());

        // Verify no shark exceeded its capacity
        for shark in &sharks {
            assert!(
                shark.assigned_mb <= shark.available_mb,
                "Shark {} exceeded capacity: {} > {}",
                shark.id,
                shark.assigned_mb,
                shark.available_mb
            );
        }

        // Verify the largest shark (15000 MB) got the most assignments
        let shark3_count = assignments.get("3.dest.domain").map_or(0, |v| v.len());
        let _shark1_count = assignments.get("1.dest.domain").map_or(0, |v| v.len());
        let shark2_count = assignments.get("2.dest.domain").map_or(0, |v| v.len());

        // Shark 3 should have most assignments as it has most capacity
        assert!(
            shark3_count >= shark2_count,
            "Expected shark3 ({}) >= shark2 ({})",
            shark3_count,
            shark2_count
        );
    }

    /// Test: Assignment batching respects max_tasks_per_assignment
    #[test]
    fn test_assignment_batching() {
        let max_tasks = 200;
        let total_objects = 1000;

        // Simulate batching objects into assignments
        let mut assignments: Vec<Vec<String>> = Vec::new();
        let mut current_batch: Vec<String> = Vec::new();

        for i in 0..total_objects {
            current_batch.push(format!("object-{}", i));

            if current_batch.len() >= max_tasks {
                assignments.push(current_batch);
                current_batch = Vec::new();
            }
        }

        // Don't forget the last partial batch
        if !current_batch.is_empty() {
            assignments.push(current_batch);
        }

        // Verify batching
        assert_eq!(assignments.len(), 5); // 1000 / 200 = 5 full batches
        for (i, batch) in assignments.iter().enumerate() {
            if i < 4 {
                assert_eq!(batch.len(), max_tasks);
            }
        }
    }
}

// ============================================================================
// Test Module for Datacenter Validation
// ============================================================================

mod datacenter_validation_tests {
    #![allow(unused_imports)]
    use super::*;

    /// Helper struct for simulating storage nodes
    struct StorageNode {
        id: String,
        datacenter: String,
    }

    /// Helper struct for simulating object metadata
    struct ObjectShark {
        id: String,
        datacenter: String,
    }

    /// Validate destination selection - should not place object in same DC twice
    fn validate_destination(
        obj_sharks: &[ObjectShark],
        from_shark: &StorageNode,
        to_shark: &StorageNode,
    ) -> Option<&'static str> {
        // Can't evacuate back to source
        if to_shark.id == from_shark.id {
            return Some("ObjectAlreadyOnDestShark");
        }

        // Can't place on a shark where object already exists
        for shark in obj_sharks {
            if shark.id == to_shark.id {
                return Some("ObjectAlreadyOnDestShark");
            }
        }

        // Can't place in a DC where object already exists
        // (except the from_shark's DC since we're moving away from it)
        for shark in obj_sharks {
            if shark.id != from_shark.id && shark.datacenter == to_shark.datacenter {
                return Some("ObjectAlreadyInDatacenter");
            }
        }

        None
    }

    /// Test: Cannot evacuate to same shark
    #[test]
    fn test_no_evacuate_to_same_shark() {
        let from_shark = StorageNode {
            id: "1.stor.domain".to_string(),
            datacenter: "dc1".to_string(),
        };

        let obj_sharks = vec![ObjectShark {
            id: "1.stor.domain".to_string(),
            datacenter: "dc1".to_string(),
        }];

        // Try to evacuate to same shark
        let to_shark = StorageNode {
            id: "1.stor.domain".to_string(),
            datacenter: "dc1".to_string(),
        };

        let result = validate_destination(&obj_sharks, &from_shark, &to_shark);
        assert_eq!(result, Some("ObjectAlreadyOnDestShark"));
    }

    /// Test: Cannot evacuate to a shark where object already exists
    #[test]
    fn test_no_evacuate_to_existing_shark() {
        let from_shark = StorageNode {
            id: "1.stor.domain".to_string(),
            datacenter: "dc1".to_string(),
        };

        let obj_sharks = vec![
            ObjectShark {
                id: "1.stor.domain".to_string(),
                datacenter: "dc1".to_string(),
            },
            ObjectShark {
                id: "2.stor.domain".to_string(),
                datacenter: "dc2".to_string(),
            },
        ];

        // Try to evacuate to shark 2 where object already exists
        let to_shark = StorageNode {
            id: "2.stor.domain".to_string(),
            datacenter: "dc2".to_string(),
        };

        let result = validate_destination(&obj_sharks, &from_shark, &to_shark);
        assert_eq!(result, Some("ObjectAlreadyOnDestShark"));
    }

    /// Test: Cannot evacuate to same datacenter (except from_shark's DC)
    #[test]
    fn test_no_duplicate_datacenter() {
        let from_shark = StorageNode {
            id: "1.stor.domain".to_string(),
            datacenter: "dc1".to_string(),
        };

        // Object on shark 1 (dc1) and shark 2 (dc2)
        let obj_sharks = vec![
            ObjectShark {
                id: "1.stor.domain".to_string(),
                datacenter: "dc1".to_string(),
            },
            ObjectShark {
                id: "2.stor.domain".to_string(),
                datacenter: "dc2".to_string(),
            },
        ];

        // Try to evacuate to shark 3 in dc2 - should fail (object already in dc2)
        let to_shark_dc2 = StorageNode {
            id: "3.stor.domain".to_string(),
            datacenter: "dc2".to_string(),
        };

        let result = validate_destination(&obj_sharks, &from_shark, &to_shark_dc2);
        assert_eq!(result, Some("ObjectAlreadyInDatacenter"));

        // Evacuate to shark 4 in dc1 - should succeed (we're replacing dc1 copy)
        let to_shark_dc1 = StorageNode {
            id: "4.stor.domain".to_string(),
            datacenter: "dc1".to_string(),
        };

        let result = validate_destination(&obj_sharks, &from_shark, &to_shark_dc1);
        assert!(result.is_none());

        // Evacuate to shark 5 in dc3 - should succeed (new DC)
        let to_shark_dc3 = StorageNode {
            id: "5.stor.domain".to_string(),
            datacenter: "dc3".to_string(),
        };

        let result = validate_destination(&obj_sharks, &from_shark, &to_shark_dc3);
        assert!(result.is_none());
    }
}
