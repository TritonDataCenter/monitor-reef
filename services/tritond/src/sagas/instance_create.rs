// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `instance-create` saga.
//!
//! Replaces the imperative body of `create_project_instance`
//! (`handlers/instances.rs:116`). The previous flow allocated the
//! instance record + every NIC + every IP + the boot disk in one
//! atomic store transaction, then enqueued a `Provision` job and
//! returned `Pending` to the caller. A failure between the create
//! and the enqueue leaked the records; a failure between the enqueue
//! and the agent's ack left the instance perpetually `Pending` with
//! no way to roll back.
//!
//! As a saga, the chain has explicit per-action undo:
//!
//! | # | Action                       | Output      | Undo                                                       |
//! |---|------------------------------|-------------|------------------------------------------------------------|
//! | 1 | `create_instance_record`     | `Instance`  | `delete_instance(force=true)` — releases NICs/IPs/disks    |
//! | 2 | `pin_host_cn` (optional)     | `Instance`  | `set_instance_host_cn(None)` — clears the pin              |
//! | 3 | `persist_root_pw_meta`       | `()`        | (none — meta survives an instance delete; cleared with it) |
//! | 4 | `enqueue_provision_job`      | `Instance`  | enqueue `Delete` job (best-effort)                         |
//!
//! On any failure between actions 1 and 4, the saga unwinds through
//! action 1's undo and zero rows leak. After enqueue (action 4) the
//! agent owns the lifecycle; the saga returns the just-created
//! `Instance` (still in `Pending`) and the existing operator-poll
//! flow drives the instance to `Running`. The lifecycle field is
//! never CAS-forced by the saga; the agent's classifier is the
//! source of truth (invariant 2).
//!
//! SG-2 keeps the catalog minimal: extra-NIC subsagas, the
//! `select_host_cn` action, and the `await_provision_terminal`
//! action all stay deferred. The bright line for SG-2 is "every
//! step from `create_instance` through `enqueue_job` unwinds
//! cleanly on failure"; the long-tail "await the agent" piece
//! lands in a follow-up slice once the operator-poll surface
//! (SG-4) and the integration-test fixtures are updated to expect
//! a Running-on-return response.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tritond_auth::generate_random_password;
use tritond_saga::{
    ActionContext, ActionError, ActionFunc, ActionRegistry, DagBuilder, Node, ResourceRef,
    ResourceScope, SagaDag, SagaError, SagaName, SagaResult, TritondSagaType,
};
use tritond_store::{
    Instance, InstanceCreateResult, JobKind, JobStatusKind, MetaScope, MetaValue, NewInstance,
    NewJob,
};
use uuid::Uuid;

/// Saga `NAME` (kebab-case, matches Steno's `SagaName` convention).
pub const SAGA_NAME: &str = "instance-create";

/// Saga `VERSION`. Bump on any change to the
/// action sequence, action ids, or `Params` shape. The registry
/// keeps the previous N=2 versions registered so a rolling deploy
/// and crash recovery against the prior version both work.
///
/// `2` adds the `await_provision_terminal` + `finish` actions and
/// the `await_provision_terminal: bool` param field (defaults
/// to `true`).
pub const SAGA_VERSION: u32 = 2;

/// Parameters the handler hands to `SagaExecutor::saga_execute`.
/// Carries everything that doesn't change during the saga: the
/// destination tenant/project, the validated request, and the
/// pre-selected host CN (chosen by the handler before the saga
/// starts so placement-failure surfaces as a `409`/`503` and not as
/// a partial-saga unwind).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceCreateParams {
    pub tenant_id: Uuid,
    pub project_id: Uuid,
    pub request: NewInstance,
    /// `Some(cn)` if the handler picked a tenant CN. `None` when
    /// the in-process stub provisioner is the only consumer (which
    /// happens in `make docker-up` and most integration tests; the
    /// stub claims unrouted jobs).
    pub target_cn_uuid: Option<Uuid>,
    /// Idempotency-Key carried through to a future replay-dedup
    /// table (SG-4). Threading it now keeps the wire shape stable so
    /// SG-4 doesn't bump the saga version.
    #[serde(default)]
    pub idempotency_key: Option<String>,
    /// Whether the saga should block on the agent acking the
    /// Provision job's terminal status. `true` in production so a
    /// Provision-failed outcome triggers the unwind tail; `false`
    /// in test fixtures that drive the agent protocol manually
    /// after the create POST returns.
    #[serde(default = "default_true")]
    pub await_provision_terminal: bool,
}

fn default_true() -> bool {
    true
}

/// Per-action timeouts. The `await` cap mirrors
/// `TRITOND_STALE_CLAIM_THRESHOLD_SECS` so a wedged agent claim and
/// the saga awaiting it fail together. Other actions are short store
/// mutations; 30 s is far outside their practical envelope but
/// catches a wedged FDB / hanging metadata write.
const ACTION_TIMEOUT_STORE: std::time::Duration = std::time::Duration::from_secs(30);
const ACTION_TIMEOUT_AWAIT_PROVISION: std::time::Duration = std::time::Duration::from_secs(600);

type Ctx = ActionContext<TritondSagaType>;

/// Register every action in this saga onto the executor's
/// [`ActionRegistry`]. Called by [`crate::sagas::register_all_actions`].
pub fn register(reg: &mut ActionRegistry) {
    reg.register(ActionFunc::new_action(
        "instance_create.create_record",
        create_instance_record,
        create_instance_record_undo,
    ));
    reg.register(ActionFunc::new_action(
        "instance_create.pin_host_cn",
        pin_host_cn,
        pin_host_cn_undo,
    ));
    reg.register(ActionFunc::new_action(
        "instance_create.persist_root_pw_meta",
        persist_root_pw_meta,
        no_op_undo,
    ));
    reg.register(ActionFunc::new_action(
        "instance_create.enqueue_provision_job",
        enqueue_provision_job,
        enqueue_provision_job_undo,
    ));
    reg.register(ActionFunc::new_action(
        "instance_create.await_provision_terminal",
        await_provision_terminal,
        no_op_undo,
    ));
    reg.register(ActionFunc::new_action(
        "instance_create.finish",
        finish,
        no_op_undo,
    ));
}

/// Build the saga DAG from `Params`. Linear 4-action chain; SG-2
/// does not yet fan out to subsagas for extra NICs / disks, nor
/// await the agent's terminal job status.
pub fn build_dag(params: &InstanceCreateParams) -> SagaResult<Arc<SagaDag>> {
    let name = SagaName::new(SAGA_NAME);
    let mut b = DagBuilder::new(name);

    // The Steno DAG references actions by *name*. Each
    // `Node::action(..., &*ActionFunc::new_action(name, ..., ...))`
    // call constructs a throwaway `Arc<dyn Action>` whose only
    // purpose is to surface the action's name to the dag builder;
    // the registry holds the canonical instance.
    b.append(Node::action(
        "instance",
        "create_instance_record",
        &*ActionFunc::new_action(
            "instance_create.create_record",
            create_instance_record,
            create_instance_record_undo,
        ),
    ));
    b.append(Node::action(
        "instance_after_pin",
        "pin_host_cn",
        &*ActionFunc::new_action("instance_create.pin_host_cn", pin_host_cn, pin_host_cn_undo),
    ));
    b.append(Node::action(
        "root_pw",
        "persist_root_pw_meta",
        &*ActionFunc::new_action(
            "instance_create.persist_root_pw_meta",
            persist_root_pw_meta,
            no_op_undo,
        ),
    ));
    b.append(Node::action(
        "provision_job",
        "enqueue_provision_job",
        &*ActionFunc::new_action(
            "instance_create.enqueue_provision_job",
            enqueue_provision_job,
            enqueue_provision_job_undo,
        ),
    ));
    b.append(Node::action(
        "provisioned",
        "await_provision_terminal",
        &*ActionFunc::new_action(
            "instance_create.await_provision_terminal",
            await_provision_terminal,
            no_op_undo,
        ),
    ));
    b.append(Node::action(
        "final_instance",
        "finish",
        &*ActionFunc::new_action("instance_create.finish", finish, no_op_undo),
    ));

    let dag = b
        .build()
        .map_err(|e| SagaError::Backend(format!("dag build: {e}")))?;
    let params_json = serde_json::to_value(params)
        .map_err(|e| SagaError::Backend(format!("params serialize: {e}")))?;
    Ok(Arc::new(SagaDag::new(dag, params_json)))
}

/// Resources this saga touches, known at create time. Used by
/// `SagaExecutor::saga_execute` to populate the by_ref index so a
/// future per-resource view (the VM detail page's "operations"
/// subtab, a CN's saga log, etc.) can resolve sagas without
/// scanning every record.
///
/// **Deferred:** the instance UUID isn't on this list because
/// `create_instance_record` allocates it. Pre-allocating the UUID
/// in the handler would let us include it; doing so is a small
/// follow-up. Until then, an Instance's saga page shows sagas that
/// named the instance in their params (delete / start / stop /
/// restart / fip-attach), not the create itself.
pub fn build_references(params: &InstanceCreateParams) -> Vec<ResourceRef> {
    let mut out = Vec::new();
    out.push(ResourceRef::new(ResourceScope::Tenant, params.tenant_id));
    out.push(ResourceRef::new(ResourceScope::Project, params.project_id));
    if let Some(cn) = params.target_cn_uuid {
        out.push(ResourceRef::new(ResourceScope::Cn, cn));
    }
    out.push(ResourceRef::new(
        ResourceScope::Image,
        params.request.image_id,
    ));
    out.push(ResourceRef::new(
        ResourceScope::Subnet,
        params.request.primary_subnet_id,
    ));
    out
}

// ---------------------------------------------------------------
// Actions
// ---------------------------------------------------------------

async fn create_instance_record(ctx: Ctx) -> Result<Instance, ActionError> {
    crate::sagas::with_action_timeout(
        "instance_create.create_record",
        ACTION_TIMEOUT_STORE,
        async move {
            let user_ctx = ctx.user_data();
            fence_check(user_ctx).await?;
            let store = user_ctx.store().clone();
            let params: InstanceCreateParams = ctx.saga_params()?;
            let result: InstanceCreateResult = store
                .create_instance(params.tenant_id, params.project_id, params.request)
                .await
                .map_err(store_err_to_action_err)?;
            Ok(result.instance)
        },
    )
    .await
}

async fn create_instance_record_undo(ctx: Ctx) -> Result<(), anyhow::Error> {
    let store = ctx.user_data().store().clone();
    let instance: Instance = ctx.lookup("instance")?;
    // force=true: the zone never came up if we're unwinding here;
    // we want every NIC / IP / Disk / DhcpLease alloc released.
    match store.delete_instance(instance.id, /* force */ true).await {
        Ok(()) | Err(tritond_store::StoreError::NotFound) => Ok(()),
        Err(e) => Err(anyhow::anyhow!("delete_instance during undo: {e}")),
    }
}

async fn pin_host_cn(ctx: Ctx) -> Result<Instance, ActionError> {
    crate::sagas::with_action_timeout(
        "instance_create.pin_host_cn",
        ACTION_TIMEOUT_STORE,
        async move {
            let user_ctx = ctx.user_data();
            fence_check(user_ctx).await?;
            let store = user_ctx.store().clone();
            let params: InstanceCreateParams = ctx.saga_params()?;
            let instance: Instance = ctx.lookup("instance")?;
            match params.target_cn_uuid {
                Some(cn) => {
                    let updated: Instance = store
                        .set_instance_host_cn(instance.id, Some(cn))
                        .await
                        .map_err(store_err_to_action_err)?;
                    Ok(updated)
                }
                None => Ok(instance),
            }
        },
    )
    .await
}

async fn pin_host_cn_undo(ctx: Ctx) -> Result<(), anyhow::Error> {
    let store = ctx.user_data().store().clone();
    let params: InstanceCreateParams = ctx.saga_params()?;
    if params.target_cn_uuid.is_none() {
        return Ok(());
    }
    let instance: Instance = ctx.lookup("instance")?;
    match store.set_instance_host_cn(instance.id, None).await {
        Ok(_) => Ok(()),
        Err(tritond_store::StoreError::NotFound) => Ok(()),
        Err(e) => Err(anyhow::anyhow!("set_instance_host_cn(None): {e}")),
    }
}

async fn persist_root_pw_meta(ctx: Ctx) -> Result<(), ActionError> {
    crate::sagas::with_action_timeout(
        "instance_create.persist_root_pw_meta",
        ACTION_TIMEOUT_STORE,
        async move {
            let user_ctx = ctx.user_data();
            fence_check(user_ctx).await?;
            let log = user_ctx.log().clone();
            let store = user_ctx.store().clone();
            let instance: Instance = ctx.lookup("instance")?;
            // Auto-generate the initial root password and persist it as
            // `instance/root_pw` at instance scope with `guest_visible=false`.
            // See the original handler comment block for the full rationale.
            let pw = generate_random_password();
            let meta = MetaValue {
                value: serde_json::Value::String(pw.expose().to_string()),
                guest_visible: false,
                guest_writable: false,
                updated_by: "system".to_string(),
                updated_at: chrono::Utc::now(),
            };
            match store
                .set_meta(MetaScope::Instance, instance.id, "instance/root_pw", meta)
                .await
            {
                Ok(_) => Ok(()),
                Err(e) => {
                    // Mirror the original "WARN, don't fail" behaviour: a
                    // transient FDB blip writing the meta should not roll
                    // the entire create back. Log and move on; operator can
                    // re-set the password manually via `tcadm meta set`.
                    slog::warn!(
                        log,
                        "instance-create: failed to persist auto-generated root_pw; operator must set manually";
                        "instance_id" => %instance.id,
                        "error" => %e,
                    );
                    Ok(())
                }
            }
        },
    )
    .await
}

async fn enqueue_provision_job(ctx: Ctx) -> Result<tritond_store::ProvisioningJob, ActionError> {
    crate::sagas::with_action_timeout(
        "instance_create.enqueue_provision_job",
        ACTION_TIMEOUT_STORE,
        async move {
            let user_ctx = ctx.user_data();
            fence_check(user_ctx).await?;
            let store = user_ctx.store().clone();
            let params: InstanceCreateParams = ctx.saga_params()?;
            let instance: Instance = ctx.lookup("instance_after_pin")?;
            let job: tritond_store::ProvisioningJob = store
                .enqueue_job(NewJob {
                    kind: JobKind::Provision {
                        instance_id: instance.id,
                    },
                    target_cn_uuid: params.target_cn_uuid,
                })
                .await
                .map_err(store_err_to_action_err)?;
            Ok(job)
        },
    )
    .await
}

async fn enqueue_provision_job_undo(ctx: Ctx) -> Result<(), anyhow::Error> {
    let log = ctx.user_data().log().clone();
    let store = ctx.user_data().store().clone();
    let params: InstanceCreateParams = ctx.saga_params()?;
    let instance: Instance = ctx.lookup("instance_after_pin")?;
    // Best-effort: enqueue a Delete job so the agent (or stub) tears
    // any half-started zone down. SG-2's saga doesn't await the
    // Delete; the `create_instance_record` undo (one step earlier in
    // the unwind chain) deletes the record + every alloc so a
    // wedged Delete job is operator-visible but not blocking.
    if let Err(e) = store
        .enqueue_job(NewJob {
            kind: JobKind::Delete {
                instance_id: instance.id,
            },
            target_cn_uuid: params.target_cn_uuid,
        })
        .await
    {
        slog::warn!(
            log,
            "instance-create undo: enqueue Delete failed; relying on record cleanup";
            "instance_id" => %instance.id,
            "error" => %e,
        );
    }
    Ok(())
}

/// Poll the Provision job until it reaches a terminal status, or
/// short-circuit if the saga's params asked to skip the wait. On
/// `JobOutcome::Failed`, return an `ActionError` so the saga
/// unwinds back through the create / pin / record steps (RFD 00004
/// SG-2 unwind story).
async fn await_provision_terminal(ctx: Ctx) -> Result<(), ActionError> {
    let params: InstanceCreateParams = ctx.saga_params()?;
    if !params.await_provision_terminal {
        // Tests that drive the agent protocol manually skip the
        // wait so the POST returns immediately and the test can
        // then issue claim+complete via the agent client. The
        // existing fire-and-forget behaviour is preserved.
        return Ok(());
    }
    // D-Sg-9: the outer timeout wraps the entire poll loop. When
    // it fires the saga unwinds and the existing enqueue-Delete
    // undo cleans up the half-started instance. The poll cadence
    // itself stays short (50 ms) so the in-process stub provisioner
    // doesn't run integration tests slowly.
    crate::sagas::with_action_timeout(
        "instance_create.await_provision_terminal",
        ACTION_TIMEOUT_AWAIT_PROVISION,
        async move {
            let user_ctx = ctx.user_data();
            fence_check(user_ctx).await?;
            let store = user_ctx.store().clone();
            let job: tritond_store::ProvisioningJob = ctx.lookup("provision_job")?;
            let job_id = job.id;
            const POLL: std::time::Duration = std::time::Duration::from_millis(50);
            loop {
                let current = store
                    .get_job(job_id)
                    .await
                    .map_err(store_err_to_action_err)?;
                match current.status.kind() {
                    JobStatusKind::Completed => return Ok(()),
                    JobStatusKind::Failed => {
                        return Err(ActionError::action_failed(serde_json::json!({
                            "kind": "provision_failed",
                            "job_id": job_id.to_string(),
                            "reason": match &current.status {
                                tritond_store::JobStatus::Failed { reason } => reason.clone(),
                                _ => "(no reason)".to_string(),
                            },
                        })));
                    }
                    _ => tokio::time::sleep(POLL).await,
                }
            }
        },
    )
    .await
}

/// Re-read the just-provisioned instance so the response carries
/// its current lifecycle (now `Running` after the agent drove
/// Pending → Provisioning → Running). The saga has no `Instance`
/// output before this action because action 4's output became the
/// `ProvisioningJob` once SG-2b added the await step.
async fn finish(ctx: Ctx) -> Result<Instance, ActionError> {
    crate::sagas::with_action_timeout("instance_create.finish", ACTION_TIMEOUT_STORE, async move {
        let user_ctx = ctx.user_data();
        fence_check(user_ctx).await?;
        let store = user_ctx.store().clone();
        let instance: Instance = ctx.lookup("instance_after_pin")?;
        let refreshed: Instance = store
            .get_instance(instance.id)
            .await
            .map_err(store_err_to_action_err)?;
        Ok(refreshed)
    })
    .await
}

async fn no_op_undo(_ctx: Ctx) -> Result<(), anyhow::Error> {
    Ok(())
}

/// best-effort fence check called at the top of
/// every saga action body before any externally-visible side effect.
/// If another SEC has adopted the saga since this action's context
/// was built (heartbeat-driven reassignment, etc.), short-circuit
/// the action with an `ActionError` so the unwind tail runs in this
/// process while the adopting SEC drives the saga forward.
async fn fence_check(ctx: &tritond_saga::SagaContext) -> Result<(), ActionError> {
    ctx.verify_fence().await.map_err(|e| {
        ActionError::action_failed(serde_json::json!({
            "kind": "fenced_out",
            "message": e.to_string(),
        }))
    })
}

/// Tag a [`StoreError`] into a structured `ActionError` payload the
/// handler can decode back into the right HTTP status. The handler
/// uses [`store_error_kind_from_action_error`] (see this module's
/// `lookup` helpers) to re-derive `409`/`404`/`500` from the payload
/// — without it, every saga failure would land as `500` and we'd
/// lose the existing `duplicate-name → 409` invariant.
fn store_err_to_action_err(e: tritond_store::StoreError) -> ActionError {
    let kind = match &e {
        tritond_store::StoreError::Conflict(_) => "conflict",
        tritond_store::StoreError::NotFound => "not_found",
        tritond_store::StoreError::Backend(_) => "backend",
        tritond_store::StoreError::FencedOut { .. } => "fenced_out",
        // variants. PinConflict tags as conflict so
        // the existing 409 mapping picks it up; CapacityExhausted
        // and AlreadyExists ride the backend tag (500 / retry-able)
        // since the placement saga action lands in PL-5.
        tritond_store::StoreError::PinConflict { .. } => "conflict",
        tritond_store::StoreError::CapacityExhausted { .. } => "backend",
        tritond_store::StoreError::AlreadyExists(_) => "backend",
        // ScanLimitExceeded should never reach a saga (sagas operate
        // on bounded sets by uuid). Surfaces as `backend` for the
        // unreachable case; a debugger will spot it in the saga log.
        tritond_store::StoreError::ScanLimitExceeded { .. } => "backend",
    };
    let payload = serde_json::json!({
        "kind": "store_error",
        "store_error_kind": kind,
        "message": e.to_string(),
    });
    ActionError::action_failed(payload)
}

/// Decode the payload an `action_failed` carries back into the
/// [`tritond_store::StoreError`] variant it wrapped, when the action
/// used [`store_err_to_action_err`]. Returns `None` for any payload
/// shape we don't recognise (the handler then defaults to `500`).
pub fn decode_store_error_kind(value: &serde_json::Value) -> Option<&'static str> {
    if value.get("kind")?.as_str()? != "store_error" {
        return None;
    }
    match value.get("store_error_kind")?.as_str()? {
        "conflict" => Some("conflict"),
        "not_found" => Some("not_found"),
        "backend" => Some("backend"),
        _ => None,
    }
}
