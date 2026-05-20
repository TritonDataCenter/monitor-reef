// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `/v2/migrations` + `/v2/instances/{id}/migrations` handlers (LM-1).
//!
//! Read-only at LM-1: list, get-one, page progress, per-instance
//! history. The mutating endpoint
//! (`POST /v2/instances/{id}/actions/migrate`) lands with the
//! migration saga (LM-5) so the handler can dispatch on
//! `MigrationAction`.
//!
//! Auth: operator-only for the fleet-wide list. Per-instance
//! migration history is gated by the existing instance-read
//! permission (LM-1 keeps it operator-only for simplicity; LM-5
//! adds the tenant-scoped path).

use dropshot::{HttpError, HttpResponseOk, Path, Query, RequestContext};
use tritond_api::{
    InstanceMigrationsPath, ListMigrationProgressQuery, ListMigrationsQuery, MigrationPath,
};
use tritond_store::{MigrationProgressEvent, MigrationRecord, StoreError};

use crate::auth::{Action, authenticate_and_authorize};
use crate::context::ApiContext;

/// Server-side caps so a wild `limit` can't trigger a giant scan.
const MAX_LIMIT: usize = 200;
const DEFAULT_LIMIT: usize = 50;
const MAX_PROGRESS_LIMIT: usize = 1_000;
const DEFAULT_PROGRESS_LIMIT: usize = 200;

pub(crate) async fn list_migrations(
    rqctx: RequestContext<ApiContext>,
    query: Query<ListMigrationsQuery>,
) -> Result<HttpResponseOk<Vec<MigrationRecord>>, HttpError> {
    let ctx = rqctx.context();
    authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::MigrationList,
    )
    .await?;
    let q = query.into_inner();
    let limit = q
        .limit
        .map(|l| (l as usize).clamp(1, MAX_LIMIT))
        .unwrap_or(DEFAULT_LIMIT);
    let rows = ctx
        .store
        .list_migrations(q.after_id, limit)
        .await
        .map_err(store_error_to_http)?;
    Ok(HttpResponseOk(rows))
}

pub(crate) async fn get_migration(
    rqctx: RequestContext<ApiContext>,
    path: Path<MigrationPath>,
) -> Result<HttpResponseOk<MigrationRecord>, HttpError> {
    let ctx = rqctx.context();
    authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::MigrationGet,
    )
    .await?;
    let MigrationPath { migration_id } = path.into_inner();
    let record = ctx
        .store
        .get_migration(migration_id)
        .await
        .map_err(store_error_to_http)?;
    Ok(HttpResponseOk(record))
}

pub(crate) async fn list_migration_progress(
    rqctx: RequestContext<ApiContext>,
    path: Path<MigrationPath>,
    query: Query<ListMigrationProgressQuery>,
) -> Result<HttpResponseOk<Vec<MigrationProgressEvent>>, HttpError> {
    let ctx = rqctx.context();
    authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::MigrationGet,
    )
    .await?;
    let MigrationPath { migration_id } = path.into_inner();
    let q = query.into_inner();
    let limit = q
        .limit
        .map(|l| (l as usize).clamp(1, MAX_PROGRESS_LIMIT))
        .unwrap_or(DEFAULT_PROGRESS_LIMIT);
    let after_seq = q.after_seq.unwrap_or(0);
    let rows = ctx
        .store
        .list_migration_progress(migration_id, after_seq, limit)
        .await
        .map_err(store_error_to_http)?;
    Ok(HttpResponseOk(rows))
}

pub(crate) async fn list_instance_migrations(
    rqctx: RequestContext<ApiContext>,
    path: Path<InstanceMigrationsPath>,
) -> Result<HttpResponseOk<Vec<MigrationRecord>>, HttpError> {
    let ctx = rqctx.context();
    authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::MigrationList,
    )
    .await?;
    let InstanceMigrationsPath { instance_id } = path.into_inner();
    let rows = ctx
        .store
        .list_migrations_for_instance(instance_id)
        .await
        .map_err(store_error_to_http)?;
    Ok(HttpResponseOk(rows))
}

fn store_error_to_http(err: StoreError) -> HttpError {
    match err {
        StoreError::NotFound => HttpError::for_not_found(None, "migration not found".to_string()),
        StoreError::Conflict(msg) => HttpError::for_client_error(
            Some("Conflict".to_string()),
            dropshot::ClientErrorStatusCode::CONFLICT,
            msg,
        ),
        other => HttpError::for_internal_error(format!("store error: {other}")),
    }
}
