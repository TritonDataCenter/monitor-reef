// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! cn-agent API trait definition.
//!
//! This crate defines the Dropshot trait for the Triton Compute Node Agent, a
//! per-compute-node HTTP service that CNAPI dispatches tasks to. Tasks cover
//! VM lifecycle, ZFS operations, image management, agent installs, and more.
//!
//! # Wire compatibility
//!
//! CNAPI speaks to cn-agent by POSTing `{task, params}` to `/tasks` and
//! expecting the task's result as a 200 body (or an error as a 500 body).
//! This shape is preserved verbatim so existing CNAPI deployments can target
//! the Rust agent without protocol changes.
//!
//! # Endpoints
//!
//! | Method | Path       | Purpose                                  |
//! |--------|------------|------------------------------------------|
//! | GET    | /ping      | Health check + basic agent metadata      |
//! | POST   | /tasks     | Dispatch a task                          |
//! | GET    | /history   | Last N tasks (default 16) for debugging  |
//! | POST   | /pause     | Stop accepting new tasks                 |
//! | POST   | /resume    | Resume accepting tasks                   |

use dropshot::{
    HttpError, HttpResponseOk, HttpResponseUpdatedNoContent, RequestContext, TypedBody,
};

pub mod history;
pub mod tasks;
pub mod types;

pub use history::{TaskHistoryEntry, TaskHistoryResponse, TaskStatus};
pub use tasks::{MachineUuidParams, SleepParams, TaskError, TaskName, TaskRequest, TaskResult};
pub use types::{PingResponse, Uuid};

/// Compute Node Agent API.
///
/// Implementors of this trait provide the platform-specific task dispatcher
/// (SmartOS, or a test/dummy backend).
#[dropshot::api_description]
pub trait CnAgentApi {
    /// Context type for request handlers.
    type Context: Send + Sync + 'static;

    /// Health check and agent metadata.
    #[endpoint {
        method = GET,
        path = "/ping",
        tags = ["system"],
    }]
    async fn ping(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseOk<PingResponse>, HttpError>;

    /// Dispatch a task.
    ///
    /// Runs the named task with the given params. On success the task's result
    /// is returned as a JSON body; on failure a [`TaskError`] is returned with
    /// HTTP 500. If the agent is paused, returns HTTP 503.
    #[endpoint {
        method = POST,
        path = "/tasks",
        tags = ["tasks"],
    }]
    async fn dispatch_task(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<TaskRequest>,
    ) -> Result<HttpResponseOk<TaskResult>, HttpError>;

    /// Return the in-memory task history (most recent first).
    #[endpoint {
        method = GET,
        path = "/history",
        tags = ["tasks"],
    }]
    async fn get_history(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseOk<TaskHistoryResponse>, HttpError>;

    /// Stop accepting new tasks.
    ///
    /// Pausing is used during agent self-update and CN reboots to drain work
    /// before the service exits. Tasks already in flight are not interrupted.
    #[endpoint {
        method = POST,
        path = "/pause",
        tags = ["system"],
    }]
    async fn pause(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseUpdatedNoContent, HttpError>;

    /// Resume accepting tasks after a prior pause.
    #[endpoint {
        method = POST,
        path = "/resume",
        tags = ["system"],
    }]
    async fn resume(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseUpdatedNoContent, HttpError>;
}
