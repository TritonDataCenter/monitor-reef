// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `/v2/operations` HTTP handlers.
//!
//! The operator-visible projection of `tritond-saga`'s catalog.
//! Reads from the SecStore via `SagaExecutor::list_sagas` /
//! `get_saga`; maps `SagaRecord` to the public `OperationSummary` /
//! `OperationDetail` shapes defined in `tritond-api`.

use std::collections::HashMap;

use dropshot::{HttpError, HttpResponseOk, Path, Query, RequestContext};
use tritond_api::{
    AbandonResponse, ListOperationsQuery, OperationDetail, OperationPath, OperationProgress,
    OperationState, OperationStep, OperationSummary, ResourceReference,
    ResourceScope as ApiResourceScope, StepStatus,
};
use tritond_saga::{
    ResourceScope as SagaResourceScope, SagaCachedStatePersist, SagaError, SagaId, SagaNodeEvent,
    SagaNodeEventType, SagaNodeId, SagaRecord,
};

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

    // Resource-scoped path: both query params
    // must be present together. Passing one without the other is
    // a 400 — silent fall-through to unfiltered would be a
    // footgun for callers building per-resource saga pages.
    let records = match (q.resource_scope, q.resource_id) {
        (Some(api_scope), Some(rid)) => {
            let saga_scope = api_scope_to_saga(api_scope);
            ctx.saga
                .list_sagas_by_reference(saga_scope, rid, marker, limit)
                .await
                .map_err(saga_error_to_http)?
        }
        (None, None) => ctx
            .saga
            .list_sagas(marker, limit)
            .await
            .map_err(saga_error_to_http)?,
        _ => {
            return Err(HttpError::for_bad_request(
                Some("InvalidRequest".to_string()),
                "resource_scope and resource_id must be provided together".to_string(),
            ));
        }
    };
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
    let saga_id = SagaId(operation_id);
    let record = ctx
        .saga
        .get_saga(saga_id)
        .await
        .map_err(saga_error_to_http)?;
    // progress is computed from the persisted
    // DAG + node-event log so the value survives a tritond restart.
    // `load_events` paginates internally under the FDB 10 MB
    // single-txn limit (SG-0 acceptance).
    let events = ctx
        .saga
        .get_saga_events(saga_id)
        .await
        .map_err(saga_error_to_http)?;
    let progress = compute_progress(&record, &events);
    let refined = fine_state(&record, &events);
    let steps = compute_steps(&record, &events);
    Ok(HttpResponseOk(record_to_detail(
        record, progress, refined, steps,
    )))
}

pub(crate) async fn abandon_operation(
    rqctx: RequestContext<ApiContext>,
    path: Path<OperationPath>,
) -> Result<HttpResponseOk<AbandonResponse>, HttpError> {
    let ctx = rqctx.context();
    // operator-only. Gated by the
    // root-allows-all Cedar rule on Action::OperationsAbandon —
    // no per-silo or per-tenant principal can drive an unwind.
    authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::OperationsAbandon,
    )
    .await?;
    let OperationPath { operation_id } = path.into_inner();
    let poked = ctx
        .saga
        .abandon_saga(SagaId(operation_id))
        .await
        .map_err(saga_error_to_http)?;
    Ok(HttpResponseOk(AbandonResponse {
        id: operation_id,
        poked_nodes: poked as u64,
    }))
}

fn record_to_summary(r: SagaRecord) -> OperationSummary {
    let state = state_from_record(&r);
    let references = r
        .references
        .iter()
        .map(|x| ResourceReference {
            scope: saga_scope_to_api(x.scope),
            id: x.id,
        })
        .collect();
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
        references,
    }
}

/// Wire-form ↔ saga-form scope conversion. Wide enums on both
/// sides; kept verbose so adding a scope is one match arm in each
/// direction with no chance of silent mismatch.
fn api_scope_to_saga(s: ApiResourceScope) -> SagaResourceScope {
    match s {
        ApiResourceScope::Fleet => SagaResourceScope::Fleet,
        ApiResourceScope::Silo => SagaResourceScope::Silo,
        ApiResourceScope::Tenant => SagaResourceScope::Tenant,
        ApiResourceScope::Project => SagaResourceScope::Project,
        ApiResourceScope::Vpc => SagaResourceScope::Vpc,
        ApiResourceScope::Subnet => SagaResourceScope::Subnet,
        ApiResourceScope::Cn => SagaResourceScope::Cn,
        ApiResourceScope::Instance => SagaResourceScope::Instance,
        ApiResourceScope::Nic => SagaResourceScope::Nic,
        ApiResourceScope::Disk => SagaResourceScope::Disk,
        ApiResourceScope::Image => SagaResourceScope::Image,
        ApiResourceScope::FloatingIp => SagaResourceScope::FloatingIp,
        ApiResourceScope::NatGateway => SagaResourceScope::NatGateway,
        ApiResourceScope::Route => SagaResourceScope::Route,
        ApiResourceScope::RouteTable => SagaResourceScope::RouteTable,
        ApiResourceScope::EdgeCluster => SagaResourceScope::EdgeCluster,
        ApiResourceScope::Job => SagaResourceScope::Job,
    }
}
fn saga_scope_to_api(s: SagaResourceScope) -> ApiResourceScope {
    match s {
        SagaResourceScope::Fleet => ApiResourceScope::Fleet,
        SagaResourceScope::Silo => ApiResourceScope::Silo,
        SagaResourceScope::Tenant => ApiResourceScope::Tenant,
        SagaResourceScope::Project => ApiResourceScope::Project,
        SagaResourceScope::Vpc => ApiResourceScope::Vpc,
        SagaResourceScope::Subnet => ApiResourceScope::Subnet,
        SagaResourceScope::Cn => ApiResourceScope::Cn,
        SagaResourceScope::Instance => ApiResourceScope::Instance,
        SagaResourceScope::Nic => ApiResourceScope::Nic,
        SagaResourceScope::Disk => ApiResourceScope::Disk,
        SagaResourceScope::Image => ApiResourceScope::Image,
        SagaResourceScope::FloatingIp => ApiResourceScope::FloatingIp,
        SagaResourceScope::NatGateway => ApiResourceScope::NatGateway,
        SagaResourceScope::Route => ApiResourceScope::Route,
        SagaResourceScope::RouteTable => ApiResourceScope::RouteTable,
        SagaResourceScope::EdgeCluster => ApiResourceScope::EdgeCluster,
        SagaResourceScope::Job => ApiResourceScope::Job,
    }
}

/// Extract the u32 backing a `SagaNodeId`. The newtype's inner
/// field is private; we round-trip through serde the same way
/// `fdb.rs::node_id_u32` does so the projection matches what was
/// written to the event log.
fn node_id_u32(node_id: SagaNodeId) -> u32 {
    serde_json::to_value(node_id)
        .ok()
        .and_then(|v| v.as_u64())
        .map(|v| v as u32)
        .unwrap_or(0)
}

fn record_to_detail(
    r: SagaRecord,
    progress: OperationProgress,
    refined_state: OperationState,
    steps: Vec<OperationStep>,
) -> OperationDetail {
    let references = r
        .references
        .iter()
        .map(|x| ResourceReference {
            scope: saga_scope_to_api(x.scope),
            id: x.id,
        })
        .collect();
    OperationDetail {
        summary: OperationSummary {
            id: r.id.0,
            kind: r.name.clone(),
            version: r.version,
            state: refined_state,
            creator_sec: r.creator_sec.as_uuid(),
            current_sec: r.current_sec.as_uuid(),
            time_created: r.time_created,
            time_done: r.time_done,
            stuck_reason: r.stuck_reason.clone(),
            references,
        },
        current_epoch: r.current_epoch.0,
        adopt_generation: r.adopt_generation,
        dag: r.dag,
        progress,
        steps,
    }
}

/// One Action node enumerated from the persisted DAG.
struct ActionNodeRef {
    index: u32,
    name: String,
    label: String,
    action_name: String,
}

/// Walk the persisted DAG to enumerate Action nodes (Steno's
/// `SagaDag` JSON has `graph.nodes[i] = { Action: { name, label, action_name } }`
/// for action nodes, plus `Start` / `End` markers we skip).
fn enumerate_action_nodes_full(dag: &serde_json::Value) -> Vec<ActionNodeRef> {
    let Some(nodes) = dag
        .get("graph")
        .and_then(|g| g.get("nodes"))
        .and_then(|n| n.as_array())
    else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (i, n) in nodes.iter().enumerate() {
        if let Some(action) = n.get("Action") {
            let name = action
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let label = action
                .get("label")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let action_name = action
                .get("action_name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            out.push(ActionNodeRef {
                index: i as u32,
                name,
                label,
                action_name,
            });
        }
    }
    out
}

/// Legacy projection used by progress computation — `(index,
/// label)` only. Built on top of the full enumerator so they don't
/// drift.
fn enumerate_action_nodes(dag: &serde_json::Value) -> Vec<(u32, String)> {
    enumerate_action_nodes_full(dag)
        .into_iter()
        .map(|n| (n.index, n.label))
        .collect()
}

/// Build the per-step debug projection used by the Operations
/// detail panel. For each Action node we collapse its event log
/// down to:
///
/// - a single `StepStatus` (pending / running / succeeded / failed /
///   undo_running / undone / undo_failed)
/// - the action's output JSON if it succeeded
/// - the failing `ActionError` JSON if it failed (Steno's
///   `ActionError` serialises as a tagged enum with a `kind` and
///   payload; the structured value is what catalog modules pack via
///   `ActionError::action_failed(json!({"kind": ..., ...}))`)
/// - a short `error_message` for tooltip / one-line display
/// - the structured `UndoActionError` JSON if the undo itself failed
///
/// Steno's `SagaNodeEvent` carries no timestamp (per-step timing
/// needs a wrapped event-record schema; deferred). What we ship now
/// is enough to triage: "step N failed with this error" /
/// "step M's compensation also failed".
fn compute_steps(r: &SagaRecord, events: &[SagaNodeEvent]) -> Vec<OperationStep> {
    let nodes = enumerate_action_nodes_full(&r.dag);
    if nodes.is_empty() {
        return Vec::new();
    }
    let action_indices: std::collections::HashSet<u32> = nodes.iter().map(|n| n.index).collect();

    // Bucket events by node_id, keeping the order we read them.
    let mut by_node: HashMap<u32, Vec<&SagaNodeEvent>> = HashMap::new();
    for ev in events {
        let id = node_id_u32(ev.node_id);
        if action_indices.contains(&id) {
            by_node.entry(id).or_default().push(ev);
        }
    }

    nodes
        .into_iter()
        .map(|n| {
            let evs: &[&SagaNodeEvent] = by_node.get(&n.index).map(|v| v.as_slice()).unwrap_or(&[]);
            let (status, output, error, undo_error) = project_step(evs);
            let error_message = error.as_ref().and_then(extract_error_message);
            OperationStep {
                index: n.index,
                name: n.name,
                label: n.label,
                action_name: n.action_name,
                status,
                output,
                error,
                error_message,
                undo_error,
            }
        })
        .collect()
}

/// Collapse the events for one node into a status + optional
/// output / error / undo_error. Forward + undo events are scanned
/// independently so a node that succeeded *and* later had its undo
/// run is `Undone` (or `UndoFailed`), not a hybrid.
fn project_step(
    events: &[&SagaNodeEvent],
) -> (
    StepStatus,
    Option<serde_json::Value>,
    Option<serde_json::Value>,
    Option<serde_json::Value>,
) {
    let mut status = StepStatus::Pending;
    let mut output: Option<serde_json::Value> = None;
    let mut error: Option<serde_json::Value> = None;
    let mut undo_error: Option<serde_json::Value> = None;
    let mut saw_undo_started = false;
    let mut saw_undo_finished = false;

    for ev in events {
        match &ev.event_type {
            SagaNodeEventType::Started => {
                if matches!(status, StepStatus::Pending) {
                    status = StepStatus::Running;
                }
            }
            SagaNodeEventType::Succeeded(v) => {
                status = StepStatus::Succeeded;
                output = Some((**v).clone());
            }
            SagaNodeEventType::Failed(e) => {
                status = StepStatus::Failed;
                error = serde_json::to_value(e).ok();
            }
            SagaNodeEventType::UndoStarted => {
                saw_undo_started = true;
                status = StepStatus::UndoRunning;
            }
            SagaNodeEventType::UndoFinished => {
                saw_undo_finished = true;
                status = StepStatus::Undone;
            }
            SagaNodeEventType::UndoFailed(e) => {
                status = StepStatus::UndoFailed;
                undo_error = serde_json::to_value(e).ok();
            }
        }
    }
    // `saw_undo_started` + `saw_undo_finished` exist to keep the
    // status machine readable; the final assignment above already
    // captures the right state.
    let _ = (saw_undo_started, saw_undo_finished);
    (status, output, error, undo_error)
}

/// Pull a short human message out of a serialised `ActionError`
/// for one-line display. Our catalog packs payloads as
/// `{"kind": "...", "message": "..."}` (`instance_create.rs`); for
/// anything we don't recognise we render the top-level "message"
/// field if it exists, else stringify the whole error.
fn extract_error_message(value: &serde_json::Value) -> Option<String> {
    // Steno's ActionError is an externally-tagged enum. The common
    // variant is `ActionFailed { source_error: <our-json> }`. Try
    // that first, then any nested `message` field, then a final
    // stringification fallback.
    if let Some(action_failed) = value.get("ActionFailed") {
        if let Some(src) = action_failed.get("source_error") {
            if let Some(msg) = src.get("message").and_then(|m| m.as_str()) {
                return Some(msg.to_string());
            }
            if let Some(kind) = src.get("kind").and_then(|k| k.as_str()) {
                return Some(kind.to_string());
            }
        }
    }
    if let Some(msg) = value.get("message").and_then(|m| m.as_str()) {
        return Some(msg.to_string());
    }
    // Last-resort: serde-print the whole thing so the operator
    // sees *something*. Capped to a reasonable display length.
    let s = serde_json::to_string(value).ok()?;
    Some(if s.len() > 200 {
        format!("{}…", &s[..200])
    } else {
        s
    })
}

/// Project per-step progress from the DAG (total Action nodes) +
/// the node-event log (which have a terminal event). Forward pass
/// only: undos don't push `completed_steps` up — that's by design,
/// an unwinding saga's progress should regress on the wire.
fn compute_progress(r: &SagaRecord, events: &[SagaNodeEvent]) -> OperationProgress {
    let action_nodes = enumerate_action_nodes(&r.dag);
    let total_steps = action_nodes.len() as u32;
    if total_steps == 0 {
        return OperationProgress {
            completed_steps: 0,
            total_steps: 0,
            current_step: None,
        };
    }
    let action_node_ids: std::collections::HashSet<u32> =
        action_nodes.iter().map(|(i, _)| *i).collect();
    let label_for: HashMap<u32, &str> =
        action_nodes.iter().map(|(i, l)| (*i, l.as_str())).collect();

    // Latest forward-direction event-kind per Action node. Only
    // forward kinds count toward "completed_steps"; undo events
    // don't bump it.
    let mut latest_forward: HashMap<u32, &'static str> = HashMap::new();
    for ev in events {
        let node_id = node_id_u32(ev.node_id);
        if !action_node_ids.contains(&node_id) {
            continue;
        }
        match ev.event_type {
            SagaNodeEventType::Started => {
                latest_forward.entry(node_id).or_insert("started");
            }
            SagaNodeEventType::Succeeded(_) => {
                latest_forward.insert(node_id, "succeeded");
            }
            SagaNodeEventType::Failed(_) => {
                latest_forward.insert(node_id, "failed");
            }
            // Undo events don't change forward progress.
            SagaNodeEventType::UndoStarted
            | SagaNodeEventType::UndoFinished
            | SagaNodeEventType::UndoFailed(_) => {}
        }
    }

    let mut completed_steps: u32 = 0;
    let mut current_step: Option<String> = None;
    // Walk action nodes in declared order so `current_step` is
    // the first "started-but-not-terminal" we see — stable for a
    // sequential DAG and a reasonable choice for parallel ones.
    for (node_id, label) in &action_nodes {
        match latest_forward.get(node_id).copied() {
            Some("succeeded") | Some("failed") => completed_steps += 1,
            Some("started") if current_step.is_none() => {
                current_step = Some(label.clone());
            }
            _ => {}
        }
    }
    // No `started` seen? Then the next pending node is "current".
    if current_step.is_none()
        && completed_steps < total_steps
        && !matches!(r.state, SagaCachedStatePersist::Done)
    {
        if let Some((id, _)) = action_nodes.get(completed_steps as usize) {
            current_step = label_for.get(id).map(|s| (*s).to_string());
        }
    }
    OperationProgress {
        completed_steps,
        total_steps,
        current_step,
    }
}

/// Coarse projection used by the list endpoint. Derived from the
/// saga record alone (no event-log read) so a page of 50 rows is
/// cheap. The detail endpoint refines `Done` into one of
/// Succeeded / Failed / Unwound. Pending is *not* projected here
/// because distinguishing Pending from Running requires the event
/// log; the list shows the cheap "is it cached as Running" view.
fn state_from_record(r: &SagaRecord) -> OperationState {
    if r.stuck_reason.is_some() {
        return OperationState::Stuck;
    }
    match r.state {
        SagaCachedStatePersist::Running => OperationState::Running,
        SagaCachedStatePersist::Unwinding => OperationState::Unwinding,
        SagaCachedStatePersist::Done => OperationState::Done,
    }
}

/// Fine projection used by the detail endpoint. Walks the
/// node-event log to refine the coarse state:
///
/// * `Running` + zero forward events → `Pending`.
/// * `Done` + any `Failed` + any undo event → `Unwound`.
/// * `Done` + any `Failed` + no undo events → `Failed`.
/// * `Done` + no failures → `Succeeded`.
///
/// Cost: O(events). For SG-2's instance-create the event log is
/// well under a kilobyte; for longer sagas `load_events` paginates.
fn fine_state(r: &SagaRecord, events: &[SagaNodeEvent]) -> OperationState {
    if r.stuck_reason.is_some() {
        return OperationState::Stuck;
    }
    match r.state {
        SagaCachedStatePersist::Unwinding => OperationState::Unwinding,
        SagaCachedStatePersist::Running => {
            let started = events
                .iter()
                .any(|e| matches!(e.event_type, SagaNodeEventType::Started));
            if started {
                OperationState::Running
            } else {
                OperationState::Pending
            }
        }
        SagaCachedStatePersist::Done => {
            let any_failed = events
                .iter()
                .any(|e| matches!(e.event_type, SagaNodeEventType::Failed(_)));
            let any_undo = events.iter().any(|e| {
                matches!(
                    e.event_type,
                    SagaNodeEventType::UndoStarted
                        | SagaNodeEventType::UndoFinished
                        | SagaNodeEventType::UndoFailed(_)
                )
            });
            if !any_failed {
                OperationState::Succeeded
            } else if any_undo {
                OperationState::Unwound
            } else {
                OperationState::Failed
            }
        }
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
