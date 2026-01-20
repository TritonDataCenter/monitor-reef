// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2020 Joyent, Inc.
// Copyright 2026 Edgecast Cloud LLC.

//! Dropshot API trait for the rebalancer agent service.
//!
//! The rebalancer agent runs on each storage node and processes object
//! download assignments sent by the rebalancer manager. The agent:
//!
//! - Receives assignments (batches of objects to download)
//! - Downloads objects from source storage nodes
//! - Verifies checksums
//! - Reports assignment status back to the manager
//!
//! ## Endpoints
//!
//! - `POST /assignments` - Submit a new assignment
//! - `GET /assignments/{uuid}` - Get assignment status
//! - `DELETE /assignments/{uuid}` - Delete a completed assignment

use dropshot::{HttpError, HttpResponseDeleted, HttpResponseOk, Path, RequestContext};
use rebalancer_types::{Assignment, AssignmentPayload};
use schemars::JsonSchema;
use serde::Deserialize;

/// Path parameters for assignment-specific endpoints.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct AssignmentPath {
    /// The assignment UUID
    pub uuid: String,
}

/// Rebalancer Agent API
///
/// This API is used by the rebalancer manager to submit object download
/// assignments to storage node agents and track their progress.
#[dropshot::api_description]
pub trait RebalancerAgentApi {
    /// Context type for request handlers
    type Context: Send + Sync + 'static;

    /// Create a new assignment
    ///
    /// Submit a batch of objects to be downloaded from source storage nodes.
    /// The agent will process the assignment asynchronously.
    ///
    /// Returns the assignment UUID on success. Returns 409 Conflict if an
    /// assignment with the same UUID already exists.
    #[endpoint {
        method = POST,
        path = "/assignments",
        tags = ["assignments"],
    }]
    async fn create_assignment(
        rqctx: RequestContext<Self::Context>,
        body: dropshot::TypedBody<AssignmentPayload>,
    ) -> Result<HttpResponseOk<String>, HttpError>;

    /// Get assignment status
    ///
    /// Returns the current status and statistics for an assignment, including:
    /// - State (Scheduled, Running, Complete)
    /// - Number of tasks completed/failed
    /// - Failed tasks (if the assignment is complete)
    ///
    /// Returns 404 if the assignment is not found.
    /// Returns 400 if the UUID is malformed.
    #[endpoint {
        method = GET,
        path = "/assignments/{uuid}",
        tags = ["assignments"],
    }]
    async fn get_assignment(
        rqctx: RequestContext<Self::Context>,
        path: Path<AssignmentPath>,
    ) -> Result<HttpResponseOk<Assignment>, HttpError>;

    /// Delete a completed assignment
    ///
    /// Remove a completed assignment from disk. Only assignments in the
    /// "completed" state can be deleted; attempting to delete a scheduled
    /// or running assignment returns 403 Forbidden.
    ///
    /// Returns 404 if the assignment is not found.
    /// Returns 403 if the assignment is not yet complete.
    /// Returns 400 if the UUID is malformed.
    #[endpoint {
        method = DELETE,
        path = "/assignments/{uuid}",
        tags = ["assignments"],
    }]
    async fn delete_assignment(
        rqctx: RequestContext<Self::Context>,
        path: Path<AssignmentPath>,
    ) -> Result<HttpResponseDeleted, HttpError>;
}
