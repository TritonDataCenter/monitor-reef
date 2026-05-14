// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `/v2/operations` HTTP handlers (RFD 00004 SG-4).
//!
//! The operator-visible projection of `tritond-saga`'s catalog.
//! Reads from the SecStore via `SagaExecutor::list_sagas` /
//! `get_saga`; maps `SagaRecord` to the public `OperationSummary` /
//! `OperationDetail` shapes defined in `tritond-api`.

use dropshot::{HttpError, HttpResponseOk, Path, Query, RequestContext};
use tritond_api::{
    ListOperationsQuery, OperationDetail, OperationPath, OperationState, OperationSummary,
};
use tritond_saga::{SagaCachedStatePersist, SagaError, SagaId, SagaRecord};

use crate::auth::{Action, authenticate_and_authorize};
use crate::context::ApiContext;

/// Server-side cap on the page size. Keeps responses bounded even
/// if a client sends a wild `limit`.
const MAX_LIMIT: usize = 200;
const DEFAULT_LIMIT: usize = 50;

pub(crate) async fn list_operations(
    rqctx: RequestContext<ApiContext>,
    query: Query<ListOperationsQuery>,
) -> Result<HttpResponseOk<Vec<OperationSummary>>, HttpError> {
    let ctx = rqctx.context();
    // SG-4 is operator-only; SG-4b will add tenant scoping once the
    // catalog has tenant-resource references on the SagaRecord.
    authenticate_and_authorize(&rqctx, &ctx.auth, &ctx.audit, &ctx.store, Action::AuditList)
        .await?;
    let q = query.into_inner();
    let limit = q
        .limit
        .map(|l| (l as usize).clamp(1, MAX_LIMIT))
        .unwrap_or(DEFAULT_LIMIT);
    let marker = q.after_id.map(SagaId);
    let records = ctx
        .saga
        .list_sagas(marker, limit)
        .await
        .map_err(saga_error_to_http)?;
    Ok(HttpResponseOk(
        records.into_iter().map(record_to_summary).collect(),
    ))
}

pub(crate) async fn get_operation(
    rqctx: RequestContext<ApiContext>,
    path: Path<OperationPath>,
) -> Result<HttpResponseOk<OperationDetail>, HttpError> {
    let ctx = rqctx.context();
    authenticate_and_authorize(&rqctx, &ctx.auth, &ctx.audit, &ctx.store, Action::AuditList)
        .await?;
    let OperationPath { operation_id } = path.into_inner();
    let record = ctx
        .saga
        .get_saga(SagaId(operation_id))
        .await
        .map_err(saga_error_to_http)?;
    Ok(HttpResponseOk(record_to_detail(record)))
}

fn record_to_summary(r: SagaRecord) -> OperationSummary {
    let state = state_from_record(&r);
    OperationSummary {
        id: r.id.0,
        kind: r.name,
        version: r.version,
        state,
        creator_sec: r.creator_sec.as_uuid(),
        current_sec: r.current_sec.as_uuid(),
        time_created: r.time_created,
        time_done: r.time_done,
        stuck_reason: r.stuck_reason,
    }
}

fn record_to_detail(r: SagaRecord) -> OperationDetail {
    OperationDetail {
        summary: OperationSummary {
            id: r.id.0,
            kind: r.name.clone(),
            version: r.version,
            state: state_from_record(&r),
            creator_sec: r.creator_sec.as_uuid(),
            current_sec: r.current_sec.as_uuid(),
            time_created: r.time_created,
            time_done: r.time_done,
            stuck_reason: r.stuck_reason.clone(),
        },
        current_epoch: r.current_epoch.0,
        adopt_generation: r.adopt_generation,
        dag: r.dag,
    }
}

fn state_from_record(r: &SagaRecord) -> OperationState {
    if r.stuck_reason.is_some() {
        return OperationState::Stuck;
    }
    match r.state {
        SagaCachedStatePersist::Running | SagaCachedStatePersist::Unwinding => {
            OperationState::Running
        }
        SagaCachedStatePersist::Done => OperationState::Done,
    }
}

fn saga_error_to_http(e: SagaError) -> HttpError {
    match e {
        SagaError::NotFound => HttpError::for_not_found(None, "operation not found".to_string()),
        SagaError::Conflict(msg) => HttpError::for_client_error(
            Some("Conflict".to_string()),
            dropshot::ClientErrorStatusCode::CONFLICT,
            msg,
        ),
        SagaError::UnknownVersion { name, version } => HttpError::for_internal_error(format!(
            "saga {name}@{version} has no registered handler"
        )),
        // Fence violation on a read path is itself a 500: the
        // operator-visible endpoint only reads, it doesn't write,
        // so the fence shouldn't trip here. If it does, log it as
        // an internal error so we notice.
        SagaError::FencedOut { .. } => HttpError::for_internal_error(e.to_string()),
        SagaError::Steno(msg) | SagaError::Backend(msg) => HttpError::for_internal_error(msg),
    }
}
