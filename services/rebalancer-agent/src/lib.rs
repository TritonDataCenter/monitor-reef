// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2020 Joyent, Inc.
// Copyright 2026 Edgecast Cloud LLC.

//! Rebalancer Agent Library
//!
//! This library provides the core functionality for the rebalancer agent service.
//! The agent runs on storage nodes and processes object download assignments
//! sent by the rebalancer manager.
//!
//! # Modules
//!
//! - [`config`] - Agent configuration (data directory, concurrency settings)
//! - [`context`] - API context for request handlers
//! - [`processor`] - Task processing logic (downloads and checksum verification)
//! - [`storage`] - SQLite-based assignment persistence

pub mod config;
pub mod context;
pub mod processor;
pub mod storage;

use dropshot::{
    ClientErrorStatusCode, HttpError, HttpResponseDeleted, HttpResponseOk, Path, RequestContext,
};
use rebalancer_agent_api::{AssignmentPath, RebalancerAgentApi};
use rebalancer_types::{Assignment, AssignmentPayload};

use crate::context::ApiContext;

/// Rebalancer Agent API implementation
///
/// This enum serves as the implementation type for the `RebalancerAgentApi` trait.
/// It contains no data - all state is stored in the `ApiContext`.
pub enum RebalancerAgentImpl {}

impl RebalancerAgentApi for RebalancerAgentImpl {
    type Context = ApiContext;

    async fn create_assignment(
        rqctx: RequestContext<Self::Context>,
        body: dropshot::TypedBody<AssignmentPayload>,
    ) -> Result<HttpResponseOk<String>, HttpError> {
        let ctx = rqctx.context();
        let payload = body.into_inner();
        let uuid = payload.id.clone();

        tracing::info!(
            assignment_id = %uuid,
            task_count = payload.tasks.len(),
            "Received new assignment"
        );

        // Check if assignment already exists
        if ctx.assignment_exists(&uuid).await {
            tracing::warn!(assignment_id = %uuid, "Assignment already exists");
            return Err(HttpError::for_client_error(
                None,
                ClientErrorStatusCode::CONFLICT,
                format!("Assignment {} already exists", uuid),
            ));
        }

        // Store and start processing the assignment
        ctx.create_assignment(payload).await.map_err(|e| {
            HttpError::for_internal_error(format!("Failed to create assignment: {}", e))
        })?;

        Ok(HttpResponseOk(uuid))
    }

    async fn get_assignment(
        rqctx: RequestContext<Self::Context>,
        path: Path<AssignmentPath>,
    ) -> Result<HttpResponseOk<Assignment>, HttpError> {
        let ctx = rqctx.context();
        let uuid = path.into_inner().uuid;

        // Validate UUID format
        uuid::Uuid::parse_str(&uuid).map_err(|_| {
            HttpError::for_bad_request(None, format!("Invalid UUID format: {}", uuid))
        })?;

        let assignment = ctx.get_assignment(&uuid).await.ok_or_else(|| {
            HttpError::for_not_found(None, format!("Assignment {} not found", uuid))
        })?;

        Ok(HttpResponseOk(assignment))
    }

    async fn delete_assignment(
        rqctx: RequestContext<Self::Context>,
        path: Path<AssignmentPath>,
    ) -> Result<HttpResponseDeleted, HttpError> {
        let ctx = rqctx.context();
        let uuid = path.into_inner().uuid;

        // Validate UUID format
        uuid::Uuid::parse_str(&uuid).map_err(|_| {
            HttpError::for_bad_request(None, format!("Invalid UUID format: {}", uuid))
        })?;

        ctx.delete_assignment(&uuid).await.map_err(|e| {
            let msg = e.to_string();
            if msg.contains("not found") {
                HttpError::for_not_found(None, format!("Assignment {} not found", uuid))
            } else if msg.contains("not complete") {
                HttpError::for_client_error(
                    None,
                    ClientErrorStatusCode::FORBIDDEN,
                    format!("Assignment {} is not complete and cannot be deleted", uuid),
                )
            } else {
                HttpError::for_internal_error(format!("Failed to delete assignment: {}", e))
            }
        })?;

        tracing::info!(assignment_id = %uuid, "Deleted assignment");
        Ok(HttpResponseDeleted())
    }
}
