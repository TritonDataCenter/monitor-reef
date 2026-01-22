// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2020 Joyent, Inc.
// Copyright 2026 Edgecast Cloud LLC.

// Allow expect/unwrap in tests - they provide clear panic messages on failure
#![allow(clippy::expect_used, clippy::unwrap_used)]

//! Integration tests for the rebalancer agent HTTP API.
//!
//! These tests port the legacy tests from `libs/rebalancer-legacy/agent/src/main.rs`
//! to the new trait-based Dropshot architecture.
//!
//! Tests ported:
//! 1. `download` - Basic file download
//! 2. `replace_healthy` - Re-download with new UUID
//! 3. `object_not_found` - Handle 404 from source
//! 4. `failed_checksum` - MD5 mismatch detection
//! 5. `duplicate_assignment` - Reject duplicate UUID (409 Conflict)
//! 6. `delete_assignment` - Delete completed assignment

use std::mem;
use std::time::Duration;

use dropshot::{ConfigDropshot, ConfigLogging, ConfigLoggingLevel, HttpServerStarter};
use rebalancer_types::{
    AgentAssignmentState, Assignment, AssignmentPayload, ObjectSkippedReason, StorageNode, Task,
    TaskStatus,
};
use reqwest::StatusCode;
use tempfile::TempDir;
use wiremock::matchers::{method, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

// ============================================================================
// Test Infrastructure
// ============================================================================

/// Test context that holds both the agent server and the mock source server
struct TestContext {
    /// HTTP client for talking to the agent
    client: reqwest::Client,
    /// Base URL for the agent server
    agent_url: String,
    /// Mock server acting as the source storage node
    mock_source: MockServer,
    /// Temp directory for agent data (kept alive for test duration)
    _temp_dir: TempDir,
}

impl TestContext {
    /// Create a new test context with a running agent and mock source server
    async fn new() -> Self {
        // Create temp directory for agent data
        let temp_dir = TempDir::new().expect("failed to create temp dir");

        // Start mock source server
        let mock_source = MockServer::start().await;

        // Create agent config pointing to temp directory
        // Use temp_dir/manta as the manta_root for test isolation
        let config = rebalancer_agent::config::AgentConfig {
            data_dir: temp_dir.path().to_path_buf(),
            manta_root: temp_dir.path().join("manta"),
            concurrent_downloads: 4,
            download_timeout_secs: 30,
        };

        // Ensure manta root directory exists
        tokio::fs::create_dir_all(&config.manta_root)
            .await
            .expect("failed to create manta root dir");

        // Create API context
        let api_context = rebalancer_agent::context::ApiContext::new(config)
            .await
            .expect("failed to create API context");

        // Build API description
        let api = rebalancer_agent_api::rebalancer_agent_api_mod::api_description::<
            rebalancer_agent::RebalancerAgentImpl,
        >()
        .expect("failed to create API description");

        // Configure server
        let config_dropshot = ConfigDropshot {
            bind_address: "127.0.0.1:0".parse().unwrap(),
            default_request_body_max_bytes: 100 * 1024 * 1024,
            default_handler_task_mode: dropshot::HandlerTaskMode::Detached,
            ..Default::default()
        };

        let config_logging = ConfigLogging::StderrTerminal {
            level: ConfigLoggingLevel::Error,
        };
        let log = config_logging
            .to_logger("test-agent")
            .expect("failed to create logger");

        // Start server
        let server = HttpServerStarter::new(&config_dropshot, api, api_context, &log)
            .expect("failed to create server")
            .start();

        let agent_url = format!("http://{}", server.local_addr());

        // Leak the server handle to keep it running for the duration of the test
        // (The server will be cleaned up when the test process exits)
        std::mem::forget(server);

        let client = reqwest::Client::new();

        Self {
            client,
            agent_url,
            mock_source,
            _temp_dir: temp_dir,
        }
    }

    /// Get the source storage ID (mock server address without http://)
    fn source_storage_id(&self) -> String {
        self.mock_source.uri().replace("http://", "")
    }

    /// Create a task for downloading from the mock source
    fn create_task(&self, object_id: &str, owner: &str, content: &[u8]) -> Task {
        // Calculate MD5 checksum (base64 encoded)
        // Use the md-5 crate (same as the agent processor)
        use md5::{Digest, Md5};
        let hash = Md5::digest(content);
        let md5sum = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, hash);

        Task {
            object_id: object_id.to_string(),
            owner: owner.to_string(),
            md5sum,
            source: StorageNode {
                datacenter: "dc".to_string(),
                manta_storage_id: self.source_storage_id(),
            },
            status: TaskStatus::Pending,
        }
    }

    /// Create a task with a specified (possibly incorrect) md5sum
    fn create_task_with_md5(&self, object_id: &str, owner: &str, md5sum: &str) -> Task {
        Task {
            object_id: object_id.to_string(),
            owner: owner.to_string(),
            md5sum: md5sum.to_string(),
            source: StorageNode {
                datacenter: "dc".to_string(),
                manta_storage_id: self.source_storage_id(),
            },
            status: TaskStatus::Pending,
        }
    }

    /// Send an assignment and expect a specific status code
    async fn send_assignment_expect(
        &self,
        payload: &AssignmentPayload,
        expected_status: StatusCode,
    ) {
        let response = self
            .client
            .post(format!("{}/assignments", self.agent_url))
            .json(payload)
            .send()
            .await
            .expect("request failed");

        assert_eq!(
            response.status(),
            expected_status,
            "Expected status {}, got {}",
            expected_status,
            response.status()
        );
    }

    /// Send an assignment and expect success (200 OK)
    async fn send_assignment(&self, payload: &AssignmentPayload) {
        self.send_assignment_expect(payload, StatusCode::OK).await;
    }

    /// Get assignment status
    async fn get_assignment(&self, uuid: &str) -> Assignment {
        let response = self
            .client
            .get(format!("{}/assignments/{}", self.agent_url, uuid))
            .send()
            .await
            .expect("request failed");

        assert_eq!(response.status(), StatusCode::OK, "get assignment failed");

        response.json().await.expect("failed to parse assignment")
    }

    /// Delete an assignment and expect success
    async fn delete_assignment(&self, uuid: &str) {
        let response = self
            .client
            .delete(format!("{}/assignments/{}", self.agent_url, uuid))
            .send()
            .await
            .expect("delete request failed");

        assert_eq!(
            response.status(),
            StatusCode::NO_CONTENT,
            "delete should return 204"
        );
    }

    /// Poll assignment until complete
    async fn wait_for_completion(&self, uuid: &str) -> Assignment {
        loop {
            let assignment = self.get_assignment(uuid).await;

            if matches!(&assignment.stats.state, AgentAssignmentState::Complete(_)) {
                return assignment;
            }

            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Wait for assignment and verify all tasks have expected status
    async fn monitor_assignment(&self, uuid: &str, expected: TaskStatus) {
        let assignment = self.wait_for_completion(uuid).await;

        match &assignment.stats.state {
            AgentAssignmentState::Complete(opt) => match opt {
                None => {
                    if expected != TaskStatus::Complete {
                        panic!(
                            "Assignment succeeded when it should not. Expected: {:?}",
                            expected
                        );
                    }
                }
                Some(tasks) => {
                    for t in tasks.iter() {
                        // Compare discriminants for Failed variants
                        if mem::discriminant(&t.status) != mem::discriminant(&expected) {
                            panic!(
                                "Task status mismatch. Expected: {:?}, Got: {:?}",
                                expected, t.status
                            );
                        }
                        // For Failed status, also check the reason matches
                        if let (
                            TaskStatus::Failed(expected_reason),
                            TaskStatus::Failed(got_reason),
                        ) = (&expected, &t.status)
                        {
                            assert_eq!(
                                mem::discriminant(expected_reason),
                                mem::discriminant(got_reason),
                                "Failure reason mismatch. Expected: {:?}, Got: {:?}",
                                expected_reason,
                                got_reason
                            );
                        }
                    }
                }
            },
            other => {
                panic!("Assignment not complete. State: {:?}", other);
            }
        }
    }

    /// Setup mock to serve a file
    async fn mock_file(&self, owner: &str, object_id: &str, content: &[u8]) {
        Mock::given(method("GET"))
            .and(path_regex(format!(r"^/{}/{}$", owner, object_id)))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(content.to_vec()))
            .mount(&self.mock_source)
            .await;
    }

    /// Setup mock to return 404 for a file
    async fn mock_not_found(&self, owner: &str, object_id: &str) {
        Mock::given(method("GET"))
            .and(path_regex(format!(r"^/{}/{}$", owner, object_id)))
            .respond_with(ResponseTemplate::new(404))
            .mount(&self.mock_source)
            .await;
    }
}

// ============================================================================
// Tests
// ============================================================================

/// Test name: download
/// Description: Download a healthy file from a storage node that the agent
///              does not already have.
/// Expected: The operation should be a success. TaskStatus for any/all tasks
///           should appear as "Complete".
#[tokio::test]
async fn download() {
    let ctx = TestContext::new().await;

    // Setup mock file
    let content = b"Hello, this is test content for download!";
    let owner = "rebalancer";
    let object_id = "test-object-1";

    ctx.mock_file(owner, object_id, content).await;

    // Create assignment
    let uuid = uuid::Uuid::new_v4().to_string();
    let task = ctx.create_task(object_id, owner, content);
    let payload = AssignmentPayload {
        id: uuid.clone(),
        tasks: vec![task],
    };

    // Send assignment and wait for completion
    ctx.send_assignment(&payload).await;
    ctx.monitor_assignment(&uuid, TaskStatus::Complete).await;
}

/// Test name: replace_healthy
/// Description: First, download a known healthy file that the agent may or
///              may not already have. After successful completion of the
///              first download, repeat the process a second time with the
///              exact same assignment information but a different UUID.
/// Expected: TaskStatus for all tasks in both assignments should appear
///           as "Complete".
#[tokio::test]
async fn replace_healthy() {
    let ctx = TestContext::new().await;

    // Setup mock file
    let content = b"Content for replace_healthy test";
    let owner = "rebalancer";
    let object_id = "replace-test-object";

    ctx.mock_file(owner, object_id, content).await;

    // First download
    let uuid1 = uuid::Uuid::new_v4().to_string();
    let task = ctx.create_task(object_id, owner, content);
    let payload1 = AssignmentPayload {
        id: uuid1.clone(),
        tasks: vec![task.clone()],
    };

    ctx.send_assignment(&payload1).await;
    ctx.monitor_assignment(&uuid1, TaskStatus::Complete).await;

    // Second download with different UUID (same content)
    let uuid2 = uuid::Uuid::new_v4().to_string();
    let payload2 = AssignmentPayload {
        id: uuid2.clone(),
        tasks: vec![ctx.create_task(object_id, owner, content)],
    };

    ctx.send_assignment(&payload2).await;
    ctx.monitor_assignment(&uuid2, TaskStatus::Complete).await;
}

/// Test name: object_not_found
/// Description: Attempt to download an object from a storage node where
///              the object does not reside will cause a client error.
/// Expected: TaskStatus for all tasks in the assignment should appear
///           as "Failed(HTTPStatusCode(NotFound))".
#[tokio::test]
async fn object_not_found() {
    let ctx = TestContext::new().await;

    let owner = "rebalancer";
    let object_id = "nonexistent-object";

    // Setup mock to return 404
    ctx.mock_not_found(owner, object_id).await;

    // Create assignment for non-existent object
    let uuid = uuid::Uuid::new_v4().to_string();
    let task = ctx.create_task_with_md5(object_id, owner, "fake-md5");
    let payload = AssignmentPayload {
        id: uuid.clone(),
        tasks: vec![task],
    };

    ctx.send_assignment(&payload).await;
    ctx.monitor_assignment(
        &uuid,
        TaskStatus::Failed(ObjectSkippedReason::HTTPStatusCode(404)),
    )
    .await;
}

/// Test name: failed_checksum
/// Description: Download a file in order to replace a known damaged copy.
///              Upon completion of the download, the checksum of the file
///              should fail. This tests that in a situation where the
///              calculated hash does not match the expected value, such an
///              event is made known in the records of failed tasks.
/// Expected: TaskStatus for all tasks in the assignment should appear
///           as Failed(MD5Mismatch).
#[tokio::test]
async fn failed_checksum() {
    let ctx = TestContext::new().await;

    // Setup mock file with actual content
    let content = b"Real content for checksum test";
    let owner = "rebalancer";
    let object_id = "checksum-test-object";

    ctx.mock_file(owner, object_id, content).await;

    // Create assignment with WRONG checksum
    let uuid = uuid::Uuid::new_v4().to_string();
    let task = ctx.create_task_with_md5(object_id, owner, "deliberately-wrong-md5");
    let payload = AssignmentPayload {
        id: uuid.clone(),
        tasks: vec![task],
    };

    ctx.send_assignment(&payload).await;
    ctx.monitor_assignment(&uuid, TaskStatus::Failed(ObjectSkippedReason::MD5Mismatch))
        .await;
}

/// Test name: duplicate_assignment
/// Description: First, successfully process an assignment. Upon completion
///              reissue the exact same assignment (including the uuid) to
///              the agent. Any time that an agent receives an assignment
///              uuid that it knows it has already received -- regardless of
///              the state of that assignment (complete or not) -- the
///              request should be rejected.
/// Expected: When we send the assignment for the second time, the server
///           should return a response of 409 (CONFLICT).
#[tokio::test]
async fn duplicate_assignment() {
    let ctx = TestContext::new().await;

    // Setup mock file
    let content = b"Content for duplicate test";
    let owner = "rebalancer";
    let object_id = "duplicate-test-object";

    ctx.mock_file(owner, object_id, content).await;

    // First successful assignment
    let uuid = uuid::Uuid::new_v4().to_string();
    let task = ctx.create_task(object_id, owner, content);
    let payload = AssignmentPayload {
        id: uuid.clone(),
        tasks: vec![task],
    };

    ctx.send_assignment(&payload).await;
    ctx.monitor_assignment(&uuid, TaskStatus::Complete).await;

    // Try to send the same assignment again (same UUID)
    // Should get 409 Conflict
    ctx.send_assignment_expect(&payload, StatusCode::CONFLICT)
        .await;
}

/// Test name: delete_assignment
/// Description: First generate an assignment and post it to the agent. Once
///              it has been observed that the assignment has been completely
///              processed, issue a request to the agent, telling it to
///              delete it.
/// Expected: After issuing the delete, we should receive a response of
///           204 indicating that the agent successfully located the
///           assignment and deleted it.
#[tokio::test]
async fn delete_assignment() {
    let ctx = TestContext::new().await;

    // Setup mock file
    let content = b"Content for delete test";
    let owner = "rebalancer";
    let object_id = "delete-test-object";

    ctx.mock_file(owner, object_id, content).await;

    // Post an assignment
    let uuid = uuid::Uuid::new_v4().to_string();
    let task = ctx.create_task(object_id, owner, content);
    let payload = AssignmentPayload {
        id: uuid.clone(),
        tasks: vec![task],
    };

    ctx.send_assignment(&payload).await;

    // Wait for the agent to finish it
    ctx.monitor_assignment(&uuid, TaskStatus::Complete).await;

    // Issue a request to delete it
    ctx.delete_assignment(&uuid).await;

    // Verify it's gone by trying to get it (should 404)
    let response = ctx
        .client
        .get(format!("{}/assignments/{}", ctx.agent_url, uuid))
        .send()
        .await
        .expect("request failed");

    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "Expected 404 after deletion"
    );
}

// ============================================================================
// ApiContext unit tests
// ============================================================================

/// Test that ApiContext cleanup_temp_files removes .tmp files from manta root
#[tokio::test]
async fn test_cleanup_temp_files_on_startup() {
    let temp_dir = TempDir::new().expect("failed to create temp dir");

    // Create manta_root directory structure with some .tmp files
    let manta_root = temp_dir.path().join("manta");
    let owner_dir = manta_root.join("test-owner");
    tokio::fs::create_dir_all(&owner_dir)
        .await
        .expect("failed to create owner dir");

    // Create a .tmp file (should be cleaned up)
    let tmp_file = owner_dir.join("partial-download.tmp");
    tokio::fs::write(&tmp_file, b"partial content")
        .await
        .expect("failed to write tmp file");

    // Create a real file (should NOT be cleaned up)
    let real_file = owner_dir.join("real-object");
    tokio::fs::write(&real_file, b"real content")
        .await
        .expect("failed to write real file");

    // Verify tmp file exists before context creation
    assert!(
        tokio::fs::try_exists(&tmp_file).await.unwrap_or(false),
        ".tmp file should exist before context creation"
    );

    // Create API context - this should clean up .tmp files
    let config = rebalancer_agent::config::AgentConfig {
        data_dir: temp_dir.path().to_path_buf(),
        manta_root: manta_root.clone(),
        concurrent_downloads: 4,
        download_timeout_secs: 30,
    };

    let _api_context = rebalancer_agent::context::ApiContext::new(config)
        .await
        .expect("failed to create API context");

    // Verify tmp file was cleaned up
    assert!(
        !tokio::fs::try_exists(&tmp_file).await.unwrap_or(true),
        ".tmp file should be cleaned up on startup"
    );

    // Verify real file was NOT cleaned up
    assert!(
        tokio::fs::try_exists(&real_file).await.unwrap_or(false),
        "real file should NOT be cleaned up"
    );
}

/// Test that resume_failed() returns false on clean startup
#[tokio::test]
async fn test_resume_failed_false_on_clean_startup() {
    let temp_dir = TempDir::new().expect("failed to create temp dir");

    let config = rebalancer_agent::config::AgentConfig {
        data_dir: temp_dir.path().to_path_buf(),
        manta_root: temp_dir.path().join("manta"),
        concurrent_downloads: 4,
        download_timeout_secs: 30,
    };

    let api_context = rebalancer_agent::context::ApiContext::new(config)
        .await
        .expect("failed to create API context");

    // On a clean startup with no prior assignments, resume_failed should be false
    assert!(
        !api_context.resume_failed(),
        "resume_failed should be false on clean startup"
    );
}

// ============================================================================
// Phase 1: Missing Agent Tests
// ============================================================================

/// Test name: existing_file_checksum_match
/// Description: When a file already exists locally with the correct MD5 checksum,
///              the agent should skip the download and report success.
///              This tests the CRIT-9 optimization from the legacy agent.
/// Expected: TaskStatus should be Complete without actually downloading.
#[tokio::test]
async fn existing_file_checksum_match() {
    let ctx = TestContext::new().await;

    // Setup mock file (the mock server shouldn't actually be called)
    let content = b"Pre-existing content with correct checksum";
    let owner = "rebalancer";
    let object_id = "preexisting-object";

    // Pre-create the file at the expected location in the agent's manta_root
    // First we need to access the temp_dir to write the file
    let manta_file_path = ctx._temp_dir.path().join("manta").join(owner).join(object_id);
    tokio::fs::create_dir_all(manta_file_path.parent().unwrap())
        .await
        .expect("failed to create owner dir");
    tokio::fs::write(&manta_file_path, content)
        .await
        .expect("failed to write pre-existing file");

    // Mock the source to return 500 if called - we shouldn't reach this
    Mock::given(method("GET"))
        .and(path_regex(format!(r"^/{}/{}$", owner, object_id)))
        .respond_with(ResponseTemplate::new(500).set_body_string("Should not be called!"))
        .mount(&ctx.mock_source)
        .await;

    // Create assignment with the same content checksum
    let uuid = uuid::Uuid::new_v4().to_string();
    let task = ctx.create_task(object_id, owner, content);
    let payload = AssignmentPayload {
        id: uuid.clone(),
        tasks: vec![task],
    };

    // Send assignment and wait for completion
    ctx.send_assignment(&payload).await;

    // The task should complete successfully by detecting the existing file
    // with matching checksum (CRIT-9 skip logic)
    ctx.monitor_assignment(&uuid, TaskStatus::Complete).await;
}

/// Test name: partial_assignment_failure
/// Description: An assignment with multiple tasks where some succeed and some fail.
///              This tests that partial failures are properly tracked.
/// Expected: Some tasks Complete, some Failed - assignment still completes.
#[tokio::test]
async fn partial_assignment_failure() {
    let ctx = TestContext::new().await;

    let owner = "rebalancer";

    // Setup task 1 - will succeed
    let content1 = b"Content for successful download";
    let object_id1 = "success-object";
    ctx.mock_file(owner, object_id1, content1).await;

    // Setup task 2 - will fail with 404
    let object_id2 = "not-found-object";
    ctx.mock_not_found(owner, object_id2).await;

    // Setup task 3 - will succeed
    let content3 = b"Another successful content";
    let object_id3 = "another-success-object";
    ctx.mock_file(owner, object_id3, content3).await;

    // Create assignment with mixed tasks
    let uuid = uuid::Uuid::new_v4().to_string();
    let payload = AssignmentPayload {
        id: uuid.clone(),
        tasks: vec![
            ctx.create_task(object_id1, owner, content1),
            ctx.create_task_with_md5(object_id2, owner, "fake-md5"), // Will fail
            ctx.create_task(object_id3, owner, content3),
        ],
    };

    ctx.send_assignment(&payload).await;

    // Wait for completion
    let assignment = ctx.wait_for_completion(&uuid).await;

    // Verify assignment completed with partial failure
    match &assignment.stats.state {
        AgentAssignmentState::Complete(Some(failed_tasks)) => {
            // Should have exactly one failed task
            assert_eq!(failed_tasks.len(), 1, "Expected exactly 1 failed task");
            let failed = &failed_tasks[0];
            assert_eq!(failed.object_id, object_id2);
            assert!(matches!(
                failed.status,
                TaskStatus::Failed(ObjectSkippedReason::HTTPStatusCode(404))
            ));
        }
        AgentAssignmentState::Complete(None) => {
            panic!("Expected partial failure, but all tasks succeeded");
        }
        other => {
            panic!("Assignment not complete: {:?}", other);
        }
    }

    // Verify stats reflect partial failure
    assert_eq!(assignment.stats.total, 3);
    assert_eq!(assignment.stats.complete, 3); // All processed (complete includes failed)
    assert_eq!(assignment.stats.failed, 1); // One failed
}

/// Test name: concurrent_downloads
/// Description: Verify that multiple downloads can be processed concurrently
///              within an assignment, up to the semaphore limit.
/// Expected: All tasks should complete, potentially in parallel.
#[tokio::test]
async fn concurrent_downloads() {
    let ctx = TestContext::new().await;

    let owner = "rebalancer";
    let num_tasks = 8; // More than default concurrent_downloads (4)

    // Setup mocks for all tasks
    let mut tasks = Vec::with_capacity(num_tasks);
    for i in 0..num_tasks {
        let object_id = format!("concurrent-object-{}", i);
        let content = format!("Content for concurrent test {}", i);
        let content_bytes = content.as_bytes();

        ctx.mock_file(owner, &object_id, content_bytes).await;
        tasks.push(ctx.create_task(&object_id, owner, content_bytes));
    }

    // Create assignment with many tasks
    let uuid = uuid::Uuid::new_v4().to_string();
    let payload = AssignmentPayload {
        id: uuid.clone(),
        tasks,
    };

    ctx.send_assignment(&payload).await;

    // All tasks should complete successfully
    let assignment = ctx.wait_for_completion(&uuid).await;

    match &assignment.stats.state {
        AgentAssignmentState::Complete(None) => {
            // All succeeded
        }
        AgentAssignmentState::Complete(Some(failed_tasks)) => {
            panic!("Some tasks failed: {:?}", failed_tasks);
        }
        other => {
            panic!("Assignment not complete: {:?}", other);
        }
    }

    assert_eq!(assignment.stats.total, num_tasks);
    assert_eq!(assignment.stats.complete, num_tasks);
    assert_eq!(assignment.stats.failed, 0);
}

/// Test name: network_timeout
/// Description: Test handling of network timeouts during download.
///              Uses wiremock delay to simulate a slow source.
/// Expected: Task should fail with NetworkError.
#[tokio::test]
async fn network_timeout() {
    let ctx = TestContext::new().await;

    let owner = "rebalancer";
    let object_id = "timeout-object";

    // Setup mock to delay longer than the agent's timeout
    // The test context uses download_timeout_secs = 30, but we use a shorter delay
    // and a custom mock that delays. We'll use wiremock's Delay feature.
    Mock::given(method("GET"))
        .and(path_regex(format!(r"^/{}/{}$", owner, object_id)))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(b"This response is delayed".to_vec())
                .set_delay(Duration::from_secs(35)), // Longer than 30s timeout
        )
        .mount(&ctx.mock_source)
        .await;

    let uuid = uuid::Uuid::new_v4().to_string();
    let task = ctx.create_task_with_md5(object_id, owner, "fake-md5");
    let payload = AssignmentPayload {
        id: uuid.clone(),
        tasks: vec![task],
    };

    ctx.send_assignment(&payload).await;
    ctx.monitor_assignment(&uuid, TaskStatus::Failed(ObjectSkippedReason::NetworkError))
        .await;
}

// ============================================================================
// ApiContext unit tests (continued)
// ============================================================================

/// Test that temp file cleanup handles nested directories
#[tokio::test]
async fn test_cleanup_temp_files_nested_directories() {
    let temp_dir = TempDir::new().expect("failed to create temp dir");

    // Create manta_root with nested directory structure
    let manta_root = temp_dir.path().join("manta");
    let nested_dir = manta_root.join("owner1").join("subdir").join("deep");
    tokio::fs::create_dir_all(&nested_dir)
        .await
        .expect("failed to create nested dir");

    // Create .tmp files at various levels
    let tmp_file1 = manta_root.join("level1.tmp");
    let tmp_file2 = manta_root.join("owner1").join("level2.tmp");
    let tmp_file3 = nested_dir.join("level4.tmp");

    tokio::fs::write(&tmp_file1, b"tmp1")
        .await
        .expect("failed to write tmp file 1");
    tokio::fs::write(&tmp_file2, b"tmp2")
        .await
        .expect("failed to write tmp file 2");
    tokio::fs::write(&tmp_file3, b"tmp3")
        .await
        .expect("failed to write tmp file 3");

    // Create API context
    let config = rebalancer_agent::config::AgentConfig {
        data_dir: temp_dir.path().to_path_buf(),
        manta_root,
        concurrent_downloads: 4,
        download_timeout_secs: 30,
    };

    let _api_context = rebalancer_agent::context::ApiContext::new(config)
        .await
        .expect("failed to create API context");

    // Verify all .tmp files were cleaned up
    assert!(
        !tokio::fs::try_exists(&tmp_file1).await.unwrap_or(true),
        "level 1 .tmp file should be cleaned up"
    );
    assert!(
        !tokio::fs::try_exists(&tmp_file2).await.unwrap_or(true),
        "level 2 .tmp file should be cleaned up"
    );
    assert!(
        !tokio::fs::try_exists(&tmp_file3).await.unwrap_or(true),
        "level 4 .tmp file should be cleaned up"
    );
}
