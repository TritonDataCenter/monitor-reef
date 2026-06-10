// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `/v1/migrations` + `/v1/instances/{id}/migrations` handlers
//! (LM-1) and the flat operator migrate/abort route
//! `POST /v1/instances/{id}/migrate` (LM-5b).
//!
//! Read surface: list, get-one, page progress, per-instance
//! history. Mutating surface: `action=begin` starts the
//! migrate-instance saga via [`start_migration`] (shared with the
//! tenant route in `handlers::instances`); `action=abort` flags
//! the active migration's `action_requested` so the saga's abort
//! poll unwinds it.
//!
//! Auth: operator-only across the board — the fleet-wide list,
//! the flat begin, and abort are all granted only by the
//! root-allows-all Cedar rule. The tenant-scoped begin path lives
//! on `POST /v1/tenants/.../instances/{id}/migrate`.

use dropshot::{
    ClientErrorStatusCode, HttpError, HttpResponseCreated, HttpResponseOk, Path, Query,
    RequestContext, TypedBody,
};
use tritond_api::{
    InstanceMigrationsPath, ListMigrationProgressQuery, ListMigrationsQuery, MigrateInstanceBody,
    MigrateInstanceResponse, MigrationPath,
};
use tritond_audit::Outcome as AuditOutcome;
use tritond_store::{
    LifecycleStateKind, MigrationAction, MigrationPhase, MigrationProgressEvent, MigrationRecord,
    StoreError,
};
use uuid::Uuid;

use crate::auth::{Action, Principal, authenticate_and_authorize};
use crate::context::ApiContext;
use crate::validate::parse_request_id;

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

// ──────────────────────────────────────────────────────────────────
// LM-5b — flat operator route: POST /v1/instances/{id}/migrate
// ──────────────────────────────────────────────────────────────────

pub(crate) async fn migrate_instance_v1(
    rqctx: RequestContext<ApiContext>,
    path: Path<tritond_api::v1::InstancePath>,
    body: TypedBody<MigrateInstanceBody>,
) -> Result<HttpResponseCreated<MigrateInstanceResponse>, HttpError> {
    let ctx = rqctx.context();
    let tritond_api::v1::InstancePath { instance_id } = path.into_inner();
    let body = body.into_inner();
    // Authorize under the verb-specific action so the audit log
    // distinguishes begin from abort. Both are operator-only
    // (granted by root-allows-all, mirroring MigrationList).
    let audit_action = match body.action {
        MigrationAction::Abort => Action::MigrationAbort,
        _ => Action::InstanceMigrate,
    };
    let principal =
        authenticate_and_authorize(&rqctx, &ctx.auth, &ctx.audit, &ctx.store, audit_action).await?;
    let request_id = parse_request_id(&rqctx);
    match body.action {
        MigrationAction::Begin => {
            let instance = ctx
                .store
                .get_instance(instance_id)
                .await
                .map_err(crate::error::store_error_to_http)?;
            let response = start_migration(
                ctx,
                &principal,
                Action::InstanceMigrate,
                request_id,
                &instance,
                body.target_server_uuid,
                body.cold,
                body.automatic,
            )
            .await?;
            Ok(HttpResponseCreated(response))
        }
        MigrationAction::Abort => {
            let response = abort_migration(ctx, &principal, request_id, instance_id).await?;
            Ok(HttpResponseCreated(response))
        }
        other => Err(HttpError::for_client_error(
            Some("UnsupportedAction".to_string()),
            ClientErrorStatusCode::BAD_REQUEST,
            format!(
                "migrate: action={other:?} is intentionally not supported in this \
                 release; supported actions are begin and abort",
            ),
        )),
    }
}

/// Shared "begin" lane for the tenant route
/// (`POST /v1/tenants/.../instances/{id}/migrate`) and the flat
/// operator route: create the `MigrationRecord` (atomic
/// `migration/active/<instance>` guard against concurrent
/// migrations of the same VM), kick off the `migrate-instance`
/// saga, audit, and return both ids. The caller has already
/// authenticated, authorized, and scope-checked the instance.
///
/// Only bhyve has a live lane (pause + RAM stream); the server
/// forces `cold = true` for every other brand regardless of what
/// the caller asked for.
#[allow(clippy::too_many_arguments)] // route-handler glue; a params struct adds ceremony
pub(crate) async fn start_migration(
    ctx: &ApiContext,
    principal: &Principal,
    audit_action: Action,
    request_id: Option<Uuid>,
    instance: &tritond_store::Instance,
    target_cn_hint: Option<Uuid>,
    cold_requested: bool,
    automatic: bool,
) -> Result<MigrateInstanceResponse, HttpError> {
    let instance_id = instance.id;
    let source_cn = instance.host_cn_uuid.ok_or_else(|| {
        HttpError::for_client_error(
            Some("Conflict".to_string()),
            ClientErrorStatusCode::CONFLICT,
            "instance has no host_cn_uuid (was it ever provisioned?)".to_string(),
        )
    })?;
    // Captured at submission so the saga restores the guest's
    // pre-migration power state (quiesce undo, activate_target)
    // even after the saga itself has been mutating the lifecycle.
    let was_running = instance.lifecycle.kind() == LifecycleStateKind::Running;
    let cold = cold_requested || !matches!(instance.brand, tritond_store::InstanceBrand::Bhyve);

    let record = ctx
        .store
        .create_migration(tritond_store::NewMigration {
            instance_id,
            tenant_id: instance.tenant_id,
            project_id: instance.project_id,
            source_cn,
            action_requested: MigrationAction::Begin,
            automatic,
        })
        .await
        .map_err(crate::error::store_error_to_http)?;

    let saga_params = crate::sagas::migration::MigrationSagaParams {
        migration_id: record.id,
        instance_id,
        tenant_id: instance.tenant_id,
        project_id: instance.project_id,
        source_cn,
        target_cn_hint,
        automatic,
        cold,
        was_running,
    };
    let saga_dag = crate::sagas::migration::build_dag(&saga_params)
        .map_err(|e| HttpError::for_internal_error(format!("migrate saga dag build: {e}")))?;
    let saga_refs = crate::sagas::migration::build_references(&saga_params);
    let saga_id = tritond_saga::SagaId(Uuid::new_v4());

    // saga_execute drives the saga; the operator polls
    // /v1/operations/{id} for saga state and
    // /v1/migrations/{id}/progress for the per-phase event log.
    let _ = ctx
        .saga
        .saga_execute(
            saga_id,
            crate::sagas::migration::SAGA_NAME,
            crate::sagas::migration::SAGA_VERSION,
            saga_dag,
            &saga_refs,
        )
        .await
        .map_err(|e| HttpError::for_internal_error(format!("migrate saga execute: {e}")))?;

    ctx.audit
        .record_mutation(
            principal,
            audit_action,
            request_id,
            Some(format!("Instance::\"{instance_id}\"")),
            AuditOutcome::Success {
                resource: Some(format!("Instance::\"{instance_id}\"")),
            },
            serde_json::json!({
                "tenant_id": instance.tenant_id,
                "project_id": instance.project_id,
                "migration_id": record.id.to_string(),
                "operation_id": saga_id.0.to_string(),
                "source_cn": source_cn.to_string(),
                "target_cn_hint": target_cn_hint.map(|u| u.to_string()),
                "cold": cold,
                "automatic": automatic,
            }),
        )
        .await;

    Ok(MigrateInstanceResponse {
        migration_id: record.id,
        operation_id: saga_id.0,
    })
}

/// `action=abort`: flag the instance's active migration so the
/// saga's abort poll converts it into an unwind. Refused with 409
/// once the record is terminal or the cutover CAS has committed
/// (`Instance.host_cn_uuid` already points at the target — the
/// target copy is canonical and the unwind tail would destroy it).
async fn abort_migration(
    ctx: &ApiContext,
    principal: &Principal,
    request_id: Option<Uuid>,
    instance_id: Uuid,
) -> Result<MigrateInstanceResponse, HttpError> {
    let mut record = ctx
        .store
        .get_active_migration(instance_id)
        .await
        .map_err(store_error_to_http)?
        .ok_or_else(|| conflict("no active migration for this instance".to_string()))?;
    if record.state.is_terminal() {
        return Err(conflict(format!(
            "migration {} is already terminal ({:?}); nothing to abort",
            record.id, record.state,
        )));
    }
    if record.phase == MigrationPhase::Switch {
        let instance = ctx
            .store
            .get_instance(instance_id)
            .await
            .map_err(crate::error::store_error_to_http)?;
        if record.target_cn.is_some() && instance.host_cn_uuid == record.target_cn {
            return Err(conflict(format!(
                "migration {} has committed its cutover; abort is no longer \
                 possible (use a fresh migration to move the instance back)",
                record.id,
            )));
        }
    }

    record.action_requested = MigrationAction::Abort;
    let record = ctx
        .store
        .put_migration(record)
        .await
        .map_err(store_error_to_http)?;

    ctx.audit
        .record_mutation(
            principal,
            Action::MigrationAbort,
            request_id,
            Some(format!("Instance::\"{instance_id}\"")),
            AuditOutcome::Success {
                resource: Some(format!("Instance::\"{instance_id}\"")),
            },
            serde_json::json!({
                "migration_id": record.id.to_string(),
                "operation_id": record.saga_id.map(|u| u.to_string()),
            }),
        )
        .await;

    Ok(MigrateInstanceResponse {
        migration_id: record.id,
        // The saga binds its id in its first node; an abort that
        // lands in the tiny window before that still takes effect
        // via the abort poll, but has no operation to point at.
        operation_id: record.saga_id.unwrap_or_else(Uuid::nil),
    })
}

fn conflict(msg: String) -> HttpError {
    HttpError::for_client_error(
        Some("Conflict".to_string()),
        ClientErrorStatusCode::CONFLICT,
        msg,
    )
}

fn store_error_to_http(err: StoreError) -> HttpError {
    match err {
        StoreError::NotFound => HttpError::for_not_found(None, "migration not found".to_string()),
        StoreError::Conflict(msg) => HttpError::for_client_error(
            Some("Conflict".to_string()),
            ClientErrorStatusCode::CONFLICT,
            msg,
        ),
        other => HttpError::for_internal_error(format!("store error: {other}")),
    }
}
