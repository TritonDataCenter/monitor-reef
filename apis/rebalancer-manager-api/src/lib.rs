// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2020 Joyent, Inc.
// Copyright 2026 Edgecast Cloud LLC.

//! Dropshot API trait for the rebalancer manager service.
//!
//! The rebalancer manager coordinates object movement across storage nodes.
//! It is responsible for:
//!
//! - Creating evacuation jobs to move objects off a storage node
//! - Distributing work to rebalancer agents on destination storage nodes
//! - Tracking job progress and handling failures
//! - Providing status information about running and completed jobs
//!
//! ## Endpoints
//!
//! - `POST /jobs` - Create a new job
//! - `GET /jobs` - List all jobs
//! - `GET /jobs/{uuid}` - Get job status
//! - `PUT /jobs/{uuid}` - Update a running job's configuration
//! - `POST /jobs/{uuid}/retry` - Retry a failed job

use dropshot::{HttpError, HttpResponseOk, HttpResponseUpdatedNoContent, Path, RequestContext};
use rebalancer_types::{EvacuateJobUpdateMessage, JobDbEntry, JobPayload, JobStatus};
use schemars::JsonSchema;
use serde::Deserialize;

/// Path parameters for job-specific endpoints.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct JobPath {
    /// The job UUID
    pub uuid: String,
}

/// Rebalancer Manager API
///
/// This API is used to create and manage rebalancer jobs. Currently supports
/// evacuation jobs, which move all objects off a storage node to other nodes.
#[dropshot::api_description]
pub trait RebalancerManagerApi {
    /// Context type for request handlers
    type Context: Send + Sync + 'static;

    /// Create a new job
    ///
    /// Create a new rebalancer job. The job type is specified by the "action"
    /// field in the payload. Currently supported actions:
    ///
    /// - `evacuate`: Move all objects off a storage node
    ///
    /// The job runs asynchronously. Use the returned UUID to check status.
    ///
    /// Returns 500 if snaplink cleanup is required or job creation fails.
    #[endpoint {
        method = POST,
        path = "/jobs",
        tags = ["jobs"],
    }]
    async fn create_job(
        rqctx: RequestContext<Self::Context>,
        body: dropshot::TypedBody<JobPayload>,
    ) -> Result<HttpResponseOk<String>, HttpError>;

    /// List all jobs
    ///
    /// Returns a list of all jobs with their ID, action type, and current state.
    #[endpoint {
        method = GET,
        path = "/jobs",
        tags = ["jobs"],
    }]
    async fn list_jobs(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseOk<Vec<JobDbEntry>>, HttpError>;

    /// Get job status
    ///
    /// Returns detailed status for a job, including:
    /// - Configuration (e.g., which storage node is being evacuated)
    /// - Current state
    /// - Result counts by status category
    ///
    /// Returns 400 if the UUID is invalid or job not found.
    /// Returns 500 if the job is still initializing.
    #[endpoint {
        method = GET,
        path = "/jobs/{uuid}",
        tags = ["jobs"],
    }]
    async fn get_job(
        rqctx: RequestContext<Self::Context>,
        path: Path<JobPath>,
    ) -> Result<HttpResponseOk<JobStatus>, HttpError>;

    /// Update a running job
    ///
    /// Dynamically update configuration of a running job. The update message
    /// type depends on the job action. For evacuate jobs, you can update:
    ///
    /// - `SetMetadataThreads`: Change the number of metadata update threads
    ///
    /// Returns 400 if:
    /// - The job is not found
    /// - The job is not in the Running state
    /// - The job doesn't support dynamic updates
    /// - The update message is invalid
    #[endpoint {
        method = PUT,
        path = "/jobs/{uuid}",
        tags = ["jobs"],
    }]
    async fn update_job(
        rqctx: RequestContext<Self::Context>,
        path: Path<JobPath>,
        body: dropshot::TypedBody<EvacuateJobUpdateMessage>,
    ) -> Result<HttpResponseUpdatedNoContent, HttpError>;

    /// Retry a failed job
    ///
    /// Create a new job that retries the work from a previously failed job.
    /// Only objects that were not successfully processed will be retried.
    ///
    /// Returns the new job's UUID.
    ///
    /// Returns 500 if the original job is not found or retry fails.
    #[endpoint {
        method = POST,
        path = "/jobs/{uuid}/retry",
        tags = ["jobs"],
    }]
    async fn retry_job(
        rqctx: RequestContext<Self::Context>,
        path: Path<JobPath>,
    ) -> Result<HttpResponseOk<String>, HttpError>;
}
