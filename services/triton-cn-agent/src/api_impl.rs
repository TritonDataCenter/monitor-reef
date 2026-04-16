// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! [`CnAgentApi`] trait implementation backed by an [`AgentContext`].

use std::sync::Arc;

use chrono::Utc;
use cn_agent_api::{
    CnAgentApi, PingResponse, TaskError, TaskHistoryEntry, TaskHistoryResponse, TaskName,
    TaskRequest, TaskResult, TaskStatus,
};
use dropshot::{
    ClientErrorStatusCode, ErrorStatusCode, HttpError, HttpResponseOk,
    HttpResponseUpdatedNoContent, RequestContext, TypedBody,
};

use crate::context::AgentContext;

/// Marker type that implements [`CnAgentApi`] with an [`AgentContext`].
pub enum CnAgentApiImpl {}

impl CnAgentApi for CnAgentApiImpl {
    type Context = Arc<AgentContext>;

    async fn ping(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseOk<PingResponse>, HttpError> {
        let ctx = rqctx.context();
        Ok(HttpResponseOk(PingResponse {
            name: ctx.metadata.name.clone(),
            version: ctx.metadata.version.clone(),
            server_uuid: ctx.metadata.server_uuid,
            backend: ctx.metadata.backend.clone(),
            paused: ctx.is_paused(),
        }))
    }

    async fn dispatch_task(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<TaskRequest>,
    ) -> Result<HttpResponseOk<TaskResult>, HttpError> {
        let ctx = rqctx.context().clone();
        let req = body.into_inner();

        if ctx.is_paused() {
            return Err(HttpError::for_unavail(
                None,
                "cn-agent is paused and not accepting new tasks".to_string(),
            ));
        }

        if matches!(req.task, TaskName::Unknown) {
            return Err(HttpError::for_client_error(
                None,
                ClientErrorStatusCode::BAD_REQUEST,
                "unknown task name".to_string(),
            ));
        }

        let Some(handler) = ctx.registry.get(req.task) else {
            return Err(HttpError::for_not_found(
                None,
                format!(
                    "no handler registered for task '{}' on backend '{}'",
                    req.task, ctx.metadata.backend
                ),
            ));
        };

        let started_at = Utc::now();
        let mut entry = TaskHistoryEntry {
            started_at: started_at.to_rfc3339(),
            finished_at: None,
            task: req.task,
            params: req.params.clone(),
            status: TaskStatus::Active,
            error_count: 0,
        };
        ctx.push_history(entry.clone());

        let outcome = handler.run(req.params).await;

        entry.finished_at = Some(Utc::now().to_rfc3339());
        match outcome {
            Ok(result) => {
                entry.status = TaskStatus::Finished;
                record_final_entry(&ctx, entry);
                Ok(HttpResponseOk(result))
            }
            Err(task_err) => {
                entry.status = TaskStatus::Failed;
                entry.error_count = 1;
                record_final_entry(&ctx, entry);
                Err(task_error_to_http(task_err))
            }
        }
    }

    async fn get_history(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseOk<TaskHistoryResponse>, HttpError> {
        let ctx = rqctx.context();
        Ok(HttpResponseOk(TaskHistoryResponse {
            entries: ctx.snapshot_history(),
        }))
    }

    async fn pause(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseUpdatedNoContent, HttpError> {
        rqctx.context().set_paused(true);
        Ok(HttpResponseUpdatedNoContent())
    }

    async fn resume(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseUpdatedNoContent, HttpError> {
        rqctx.context().set_paused(false);
        Ok(HttpResponseUpdatedNoContent())
    }
}

/// Replace the last history entry (the active one we pushed on dispatch) with
/// the final status.
///
/// Why not update in place: we push under a brief lock before running the
/// task so the history is visible mid-run. After the task completes we rewrite
/// the same slot. Other tasks may have been pushed in the meantime, so we
/// match by `started_at` (pushed once from this handler, unique within a
/// single history window of 16 entries).
fn record_final_entry(ctx: &AgentContext, entry: TaskHistoryEntry) {
    if let Ok(mut history) = ctx.history.lock() {
        for existing in history.iter_mut() {
            if existing.started_at == entry.started_at
                && existing.task == entry.task
                && existing.status == TaskStatus::Active
            {
                *existing = entry;
                return;
            }
        }
        // Couldn't find the active entry (evicted?); push the final one.
        if history.len() >= crate::TASK_HISTORY_SIZE {
            history.pop_front();
        }
        history.push_back(entry);
    }
}

/// Translate a task failure into an HTTP 500 whose body carries the task's
/// actual error message.
///
/// [`HttpError::for_internal_error`] hides the internal message behind a
/// generic "Internal Server Error" body. cn-agent's task errors are expected
/// to be client-visible (CNAPI reads them to decide what to do next), so we
/// construct the error directly and put the task message in both
/// `external_message` and `internal_message`.
fn task_error_to_http(err: TaskError) -> HttpError {
    HttpError {
        status_code: ErrorStatusCode::INTERNAL_SERVER_ERROR,
        error_code: err.rest_code,
        external_message: err.error.clone(),
        internal_message: err.error,
        headers: None,
    }
}
