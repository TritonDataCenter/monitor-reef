// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! HTTP API integration tests for the rebalancer manager service.
//!
//! These tests verify the HTTP endpoints work correctly by spinning up
//! a test server with mocked dependencies.

// Allow unwrap/expect in tests - panicking on setup failures is acceptable
#![allow(clippy::unwrap_used)]
#![allow(clippy::expect_used)]

use dropshot::{
    ConfigDropshot, ConfigLogging, ConfigLoggingLevel, HttpResponseOk,
    HttpResponseUpdatedNoContent, HttpServerStarter,
};
use rebalancer_manager_api::RebalancerManagerApi;
use rebalancer_types::{
    EvacuateJobUpdateMessage, JobAction, JobDbEntry, JobPayload, JobState, JobStatus,
};
use reqwest::StatusCode;
use serde_json::json;
use std::collections::HashMap;
use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

/// Test context that provides mock implementations for testing
struct TestContext {
    jobs: Arc<RwLock<Vec<TestJob>>>,
}

#[derive(Clone)]
struct TestJob {
    id: Uuid,
    action: JobAction,
    state: JobState,
    from_shark: String,
    datacenter: String,
}

impl TestContext {
    fn new() -> Self {
        Self {
            jobs: Arc::new(RwLock::new(Vec::new())),
        }
    }
}

/// Test implementation of the RebalancerManagerApi
enum TestRebalancerManagerImpl {}

impl RebalancerManagerApi for TestRebalancerManagerImpl {
    type Context = TestContext;

    async fn create_job(
        rqctx: dropshot::RequestContext<Self::Context>,
        body: dropshot::TypedBody<JobPayload>,
    ) -> Result<HttpResponseOk<String>, dropshot::HttpError> {
        let ctx = rqctx.context();
        let payload = body.into_inner();

        let job = match payload {
            JobPayload::Evacuate(params) => TestJob {
                id: Uuid::new_v4(),
                action: JobAction::Evacuate,
                state: JobState::Init,
                from_shark: params.from_shark,
                datacenter: "test-dc".to_string(),
            },
        };

        let job_id = job.id.to_string();
        ctx.jobs.write().await.push(job);

        Ok(HttpResponseOk(job_id))
    }

    async fn list_jobs(
        rqctx: dropshot::RequestContext<Self::Context>,
    ) -> Result<HttpResponseOk<Vec<JobDbEntry>>, dropshot::HttpError> {
        let ctx = rqctx.context();
        let jobs = ctx.jobs.read().await;

        let entries: Vec<JobDbEntry> = jobs
            .iter()
            .map(|j| JobDbEntry {
                id: j.id.to_string(),
                action: j.action.clone(),
                state: j.state.clone(),
            })
            .collect();

        Ok(HttpResponseOk(entries))
    }

    async fn get_job(
        rqctx: dropshot::RequestContext<Self::Context>,
        path_params: dropshot::Path<rebalancer_manager_api::JobPath>,
    ) -> Result<HttpResponseOk<JobStatus>, dropshot::HttpError> {
        let ctx = rqctx.context();
        let uuid_str = path_params.into_inner().uuid;

        // Validate UUID format
        let uuid = Uuid::parse_str(&uuid_str).map_err(|_| {
            dropshot::HttpError::for_bad_request(None, format!("Invalid UUID format: {}", uuid_str))
        })?;

        let jobs = ctx.jobs.read().await;
        let job = jobs.iter().find(|j| j.id == uuid).ok_or_else(|| {
            dropshot::HttpError::for_bad_request(None, format!("Job {} not found", uuid_str))
        })?;

        // Build results as a HashMap<String, i64>
        let mut results_map = HashMap::new();
        results_map.insert("total".to_string(), 0i64);
        results_map.insert("complete".to_string(), 0i64);
        results_map.insert("skipped".to_string(), 0i64);
        results_map.insert("error".to_string(), 0i64);

        let status = JobStatus {
            config: rebalancer_types::JobStatusConfig::Evacuate(
                rebalancer_types::JobConfigEvacuate {
                    from_shark: rebalancer_types::StorageNode {
                        manta_storage_id: job.from_shark.clone(),
                        datacenter: job.datacenter.clone(),
                    },
                },
            ),
            results: rebalancer_types::JobStatusResults::Evacuate(results_map),
            state: job.state.clone(),
        };

        Ok(HttpResponseOk(status))
    }

    async fn update_job(
        rqctx: dropshot::RequestContext<Self::Context>,
        path_params: dropshot::Path<rebalancer_manager_api::JobPath>,
        body: dropshot::TypedBody<EvacuateJobUpdateMessage>,
    ) -> Result<HttpResponseUpdatedNoContent, dropshot::HttpError> {
        let ctx = rqctx.context();
        let uuid_str = path_params.into_inner().uuid;
        let msg = body.into_inner();

        // Validate UUID format
        let uuid = Uuid::parse_str(&uuid_str).map_err(|_| {
            dropshot::HttpError::for_bad_request(None, format!("Invalid UUID format: {}", uuid_str))
        })?;

        // Validate the update message
        msg.validate().map_err(|e| {
            dropshot::HttpError::for_bad_request(None, format!("Invalid update: {}", e))
        })?;

        let jobs = ctx.jobs.read().await;
        let job = jobs.iter().find(|j| j.id == uuid).ok_or_else(|| {
            dropshot::HttpError::for_bad_request(None, format!("Job {} not found", uuid_str))
        })?;

        // Only allow updates when job is running
        if job.state != JobState::Running {
            return Err(dropshot::HttpError::for_bad_request(
                None,
                format!("Cannot update job in '{}' state", job.state),
            ));
        }

        Ok(HttpResponseUpdatedNoContent())
    }

    async fn retry_job(
        rqctx: dropshot::RequestContext<Self::Context>,
        path_params: dropshot::Path<rebalancer_manager_api::JobPath>,
    ) -> Result<HttpResponseOk<String>, dropshot::HttpError> {
        let ctx = rqctx.context();
        let uuid_str = path_params.into_inner().uuid;

        // Validate UUID format
        let uuid = Uuid::parse_str(&uuid_str).map_err(|_| {
            dropshot::HttpError::for_bad_request(None, format!("Invalid UUID format: {}", uuid_str))
        })?;

        let jobs = ctx.jobs.read().await;
        let job = jobs.iter().find(|j| j.id == uuid).ok_or_else(|| {
            dropshot::HttpError::for_internal_error(format!("Job {} not found", uuid_str))
        })?;

        // Create a new job for the retry
        let new_job = TestJob {
            id: Uuid::new_v4(),
            action: job.action.clone(),
            state: JobState::Init,
            from_shark: job.from_shark.clone(),
            datacenter: job.datacenter.clone(),
        };
        let new_job_id = new_job.id.to_string();

        drop(jobs);
        ctx.jobs.write().await.push(new_job);

        Ok(HttpResponseOk(new_job_id))
    }
}

/// Helper to find an available port
fn find_available_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

/// Helper to start a test server
async fn start_test_server() -> (String, tokio::task::JoinHandle<()>) {
    let port = find_available_port();
    let bind_address: SocketAddr = format!("127.0.0.1:{}", port).parse().unwrap();

    let api = rebalancer_manager_api::rebalancer_manager_api_mod::api_description::<
        TestRebalancerManagerImpl,
    >()
    .expect("Failed to create API description");

    let ctx = TestContext::new();

    let config_dropshot = ConfigDropshot {
        bind_address,
        default_request_body_max_bytes: 1024 * 1024,
        default_handler_task_mode: dropshot::HandlerTaskMode::Detached,
        ..Default::default()
    };

    let config_logging = ConfigLogging::StderrTerminal {
        level: ConfigLoggingLevel::Error,
    };

    let log = config_logging
        .to_logger("test-server")
        .expect("Failed to create logger");

    let server = HttpServerStarter::new(&config_dropshot, api, ctx, &log)
        .expect("Failed to create server")
        .start();

    let base_url = format!("http://127.0.0.1:{}", port);

    let handle = tokio::spawn(async move {
        server.await.ok();
    });

    // Give the server a moment to start
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    (base_url, handle)
}

// ============================================================================
// Tests
// ============================================================================

#[tokio::test]
async fn test_list_jobs_empty() {
    let (base_url, _handle) = start_test_server().await;
    let client = reqwest::Client::new();

    let response = client
        .get(format!("{}/jobs", base_url))
        .send()
        .await
        .expect("Request failed");

    assert_eq!(response.status(), StatusCode::OK);

    let jobs: Vec<JobDbEntry> = response.json().await.expect("Failed to parse response");
    assert!(jobs.is_empty(), "Expected empty job list");
}

#[tokio::test]
async fn test_create_job() {
    let (base_url, _handle) = start_test_server().await;
    let client = reqwest::Client::new();

    let payload = json!({
        "action": "evacuate",
        "params": {
            "from_shark": "1.stor.test.domain"
        }
    });

    let response = client
        .post(format!("{}/jobs", base_url))
        .json(&payload)
        .send()
        .await
        .expect("Request failed");

    assert_eq!(response.status(), StatusCode::OK);

    let job_id: String = response.json().await.expect("Failed to parse response");
    assert!(
        Uuid::parse_str(&job_id).is_ok(),
        "Expected valid UUID, got: {}",
        job_id
    );
}

#[tokio::test]
async fn test_list_jobs_after_create() {
    let (base_url, _handle) = start_test_server().await;
    let client = reqwest::Client::new();

    // Create a job
    let payload = json!({
        "action": "evacuate",
        "params": {
            "from_shark": "1.stor.test.domain"
        }
    });

    let create_response = client
        .post(format!("{}/jobs", base_url))
        .json(&payload)
        .send()
        .await
        .expect("Create request failed");

    assert_eq!(create_response.status(), StatusCode::OK);
    let created_job_id: String = create_response
        .json()
        .await
        .expect("Failed to parse response");

    // List jobs
    let list_response = client
        .get(format!("{}/jobs", base_url))
        .send()
        .await
        .expect("List request failed");

    assert_eq!(list_response.status(), StatusCode::OK);

    let jobs: Vec<JobDbEntry> = list_response
        .json()
        .await
        .expect("Failed to parse response");
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].id, created_job_id);
    assert_eq!(jobs[0].action, JobAction::Evacuate);
}

#[tokio::test]
async fn test_get_job() {
    let (base_url, _handle) = start_test_server().await;
    let client = reqwest::Client::new();

    // Create a job first
    let payload = json!({
        "action": "evacuate",
        "params": {
            "from_shark": "1.stor.test.domain"
        }
    });

    let create_response = client
        .post(format!("{}/jobs", base_url))
        .json(&payload)
        .send()
        .await
        .expect("Create request failed");

    let job_id: String = create_response
        .json()
        .await
        .expect("Failed to parse response");

    // Get job status
    let get_response = client
        .get(format!("{}/jobs/{}", base_url, job_id))
        .send()
        .await
        .expect("Get request failed");

    assert_eq!(get_response.status(), StatusCode::OK);

    let status: JobStatus = get_response.json().await.expect("Failed to parse response");
    assert_eq!(status.state, JobState::Init);

    // Verify the from_shark is correct
    let rebalancer_types::JobStatusConfig::Evacuate(config) = status.config;
    assert_eq!(config.from_shark.manta_storage_id, "1.stor.test.domain");
}

#[tokio::test]
async fn test_get_job_invalid_uuid() {
    let (base_url, _handle) = start_test_server().await;
    let client = reqwest::Client::new();

    let response = client
        .get(format!("{}/jobs/not-a-uuid", base_url))
        .send()
        .await
        .expect("Request failed");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_get_job_not_found() {
    let (base_url, _handle) = start_test_server().await;
    let client = reqwest::Client::new();

    let fake_uuid = Uuid::new_v4();
    let response = client
        .get(format!("{}/jobs/{}", base_url, fake_uuid))
        .send()
        .await
        .expect("Request failed");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_retry_job() {
    let (base_url, _handle) = start_test_server().await;
    let client = reqwest::Client::new();

    // Create a job first
    let payload = json!({
        "action": "evacuate",
        "params": {
            "from_shark": "1.stor.test.domain"
        }
    });

    let create_response = client
        .post(format!("{}/jobs", base_url))
        .json(&payload)
        .send()
        .await
        .expect("Create request failed");

    let job_id: String = create_response
        .json()
        .await
        .expect("Failed to parse response");

    // Retry the job
    let retry_response = client
        .post(format!("{}/jobs/{}/retry", base_url, job_id))
        .send()
        .await
        .expect("Retry request failed");

    assert_eq!(retry_response.status(), StatusCode::OK);

    let new_job_id: String = retry_response
        .json()
        .await
        .expect("Failed to parse response");
    assert!(
        Uuid::parse_str(&new_job_id).is_ok(),
        "Expected valid UUID for new job"
    );
    assert_ne!(new_job_id, job_id, "Retry should create a new job");
}

#[tokio::test]
async fn test_retry_job_invalid_uuid() {
    let (base_url, _handle) = start_test_server().await;
    let client = reqwest::Client::new();

    let response = client
        .post(format!("{}/jobs/not-a-uuid/retry", base_url))
        .send()
        .await
        .expect("Request failed");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_create_job_bad_request() {
    let (base_url, _handle) = start_test_server().await;
    let client = reqwest::Client::new();

    // Send malformed JSON
    let response = client
        .post(format!("{}/jobs", base_url))
        .header("Content-Type", "application/json")
        .body("{invalid json}")
        .send()
        .await
        .expect("Request failed");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}
