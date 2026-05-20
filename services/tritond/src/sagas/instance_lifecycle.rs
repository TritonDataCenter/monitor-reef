// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `instance-start` / `instance-stop` / `instance-restart` sagas
//! (RFD 00004 SG-3).
//!
//! Each is the same shape — a CAS lifecycle transition followed by
//! a job enqueue and (optionally) an agent-terminal await:
//!
//! | # | Action                | Output             | Undo                                                |
//! |---|-----------------------|--------------------|-----------------------------------------------------|
//! | 1 | `transition_lifecycle`| `Instance`         | CAS back from the transitional state if we can      |
//! | 2 | `enqueue_job`         | `ProvisioningJob`  | (no-op — by unwind time the job is terminal)        |
//! | 3 | `await_terminal`      | `()`               | (no-op)                                             |
//! | 4 | `finish`              | `Instance`         | (no-op)                                             |
//!
//! The three sagas differ only in their `LifecycleOp` (the
//! action / from-state set / to-state / `JobKind` template) so
//! all three share one params type + one DAG builder.
//!
//! Undo of action 1 is "best-effort revert" — if the agent has
//! already moved the instance past the transitional state we
//! accept the new state rather than racing the agent. The CAS
//! `expected_from` is the *transitional* state we wrote in action 1,
//! and `to` is the *original* state.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tritond_saga::{
    ActionContext, ActionError, ActionFunc, ActionRegistry, DagBuilder, Node, ResourceRef,
    ResourceScope, SagaDag, SagaError, SagaName, SagaResult, TritondSagaType,
};
use tritond_store::{
    Instance, JobKind, LifecycleState, LifecycleStateKind, NewJob, ProvisioningJob,
};

/// Serializable mirror of `tritond_store::LifecycleStateKind`. The
/// upstream enum isn't serde-derived, so the saga params can't
/// carry it directly. This wrapper exists purely so the recovered
/// saga can name the original state on undo.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OriginalLifecycle {
    Pending,
    Provisioning,
    Running,
    Stopping,
    Stopped,
    Failed,
}

impl OriginalLifecycle {
    pub fn from_kind(k: LifecycleStateKind) -> Self {
        match k {
            LifecycleStateKind::Pending => OriginalLifecycle::Pending,
            LifecycleStateKind::Provisioning => OriginalLifecycle::Provisioning,
            LifecycleStateKind::Running => OriginalLifecycle::Running,
            LifecycleStateKind::Stopping => OriginalLifecycle::Stopping,
            LifecycleStateKind::Stopped => OriginalLifecycle::Stopped,
            LifecycleStateKind::Failed => OriginalLifecycle::Failed,
            _ => OriginalLifecycle::Failed,
        }
    }

    pub fn to_state(self) -> Option<LifecycleState> {
        Some(match self {
            OriginalLifecycle::Pending => LifecycleState::Pending,
            OriginalLifecycle::Provisioning => LifecycleState::Provisioning,
            OriginalLifecycle::Running => LifecycleState::Running,
            OriginalLifecycle::Stopping => LifecycleState::Stopping,
            OriginalLifecycle::Stopped => LifecycleState::Stopped,
            // Failed{reason} can't be reconstructed without the
            // string — return None so the undo skips rather than
            // fabricating a fake reason.
            OriginalLifecycle::Failed => return None,
        })
    }
}
use uuid::Uuid;

use super::common::{
    ACTION_TIMEOUT_STORE, Ctx, await_provisioning_job_terminal, fence_check, no_op_undo,
    store_err_to_action_err,
};

pub const SAGA_NAME_START: &str = "instance-start";
pub const SAGA_NAME_STOP: &str = "instance-stop";
pub const SAGA_NAME_RESTART: &str = "instance-restart";
pub const SAGA_VERSION: u32 = 1;

/// Which lifecycle operation this saga is. Serialised on the
/// params so a recovered saga can re-resolve its (from, to, job
/// kind) without ambiguity.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleOp {
    Start,
    Stop,
    Restart,
}

impl LifecycleOp {
    pub fn saga_name(self) -> &'static str {
        match self {
            LifecycleOp::Start => SAGA_NAME_START,
            LifecycleOp::Stop => SAGA_NAME_STOP,
            LifecycleOp::Restart => SAGA_NAME_RESTART,
        }
    }

    /// Allowed source lifecycle kinds for this op. Matches the
    /// existing `instance_lifecycle_transition` `expected_from`.
    pub fn from_kinds(self) -> &'static [LifecycleStateKind] {
        match self {
            LifecycleOp::Start => &[LifecycleStateKind::Stopped],
            LifecycleOp::Stop => &[LifecycleStateKind::Running],
            LifecycleOp::Restart => &[LifecycleStateKind::Running],
        }
    }

    /// Target transitional state — what the CAS writes.
    pub fn to_state(self) -> LifecycleState {
        match self {
            LifecycleOp::Start => LifecycleState::Pending,
            LifecycleOp::Stop => LifecycleState::Stopping,
            // Restart drives Running → Stopping → Stopped →
            // Running. The Stopping CAS happens here; the agent
            // owns the rest.
            LifecycleOp::Restart => LifecycleState::Stopping,
        }
    }

    /// Concrete `JobKind` for the agent. The dispatcher matches
    /// on this kind to know whether to power on, off, or cycle.
    pub fn job_kind(self, instance_id: Uuid) -> JobKind {
        match self {
            LifecycleOp::Start => JobKind::Provision { instance_id },
            LifecycleOp::Stop => JobKind::Stop { instance_id },
            LifecycleOp::Restart => JobKind::Restart { instance_id },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceLifecycleParams {
    pub op: LifecycleOp,
    pub tenant_id: Uuid,
    pub project_id: Uuid,
    pub instance_id: Uuid,
    /// Host CN the instance is pinned to; passed in so the job is
    /// routed to the right agent.
    pub target_cn_uuid: Option<Uuid>,
    /// `true` in production so a job failure stuck-fails the saga;
    /// `false` in test fixtures that drive the agent manually.
    #[serde(default = "default_true")]
    pub await_job_terminal: bool,
    /// Original lifecycle state before the CAS. Captured by the
    /// handler so the undo can CAS back. `None` is acceptable —
    /// the undo just becomes a no-op.
    #[serde(default)]
    pub original_state_kind: Option<OriginalLifecycle>,
}

fn default_true() -> bool {
    true
}

pub fn register(reg: &mut ActionRegistry) {
    reg.register(ActionFunc::new_action(
        "instance_lifecycle.transition",
        transition_lifecycle,
        transition_lifecycle_undo,
    ));
    reg.register(ActionFunc::new_action(
        "instance_lifecycle.enqueue_job",
        enqueue_job,
        no_op_undo,
    ));
    reg.register(ActionFunc::new_action(
        "instance_lifecycle.await_terminal",
        await_terminal,
        no_op_undo,
    ));
    reg.register(ActionFunc::new_action(
        "instance_lifecycle.finish",
        finish,
        no_op_undo,
    ));
}

pub fn build_dag(params: &InstanceLifecycleParams) -> SagaResult<Arc<SagaDag>> {
    let name = SagaName::new(params.op.saga_name());
    let mut b = DagBuilder::new(name);
    b.append(Node::action(
        "transitioned",
        "transition_lifecycle",
        &*ActionFunc::new_action(
            "instance_lifecycle.transition",
            transition_lifecycle,
            transition_lifecycle_undo,
        ),
    ));
    b.append(Node::action(
        "job",
        "enqueue_job",
        &*ActionFunc::new_action("instance_lifecycle.enqueue_job", enqueue_job, no_op_undo),
    ));
    b.append(Node::action(
        "agent_terminal",
        "await_terminal",
        &*ActionFunc::new_action(
            "instance_lifecycle.await_terminal",
            await_terminal,
            no_op_undo,
        ),
    ));
    b.append(Node::action(
        "final",
        "finish",
        &*ActionFunc::new_action("instance_lifecycle.finish", finish, no_op_undo),
    ));
    let dag = b
        .build()
        .map_err(|e| SagaError::Backend(format!("dag build: {e}")))?;
    let params_json = serde_json::to_value(params)
        .map_err(|e| SagaError::Backend(format!("params serialize: {e}")))?;
    Ok(Arc::new(SagaDag::new(dag, params_json)))
}

pub fn build_references(params: &InstanceLifecycleParams) -> Vec<ResourceRef> {
    let mut out = Vec::new();
    out.push(ResourceRef::new(ResourceScope::Tenant, params.tenant_id));
    out.push(ResourceRef::new(ResourceScope::Project, params.project_id));
    out.push(ResourceRef::new(
        ResourceScope::Instance,
        params.instance_id,
    ));
    if let Some(cn) = params.target_cn_uuid {
        out.push(ResourceRef::new(ResourceScope::Cn, cn));
    }
    out
}

// ---------------------------------------------------------------
// Actions
// ---------------------------------------------------------------

async fn transition_lifecycle(ctx: Ctx) -> Result<Instance, ActionError> {
    crate::sagas::with_action_timeout(
        "instance_lifecycle.transition",
        ACTION_TIMEOUT_STORE,
        async move {
            let user_ctx = ctx.user_data();
            fence_check(user_ctx).await?;
            let store = user_ctx.store().clone();
            let params: InstanceLifecycleParams = ctx.saga_params()?;
            let updated = store
                .transition_instance_lifecycle(
                    params.instance_id,
                    params.op.from_kinds(),
                    params.op.to_state(),
                )
                .await
                .map_err(store_err_to_action_err)?;
            Ok(updated)
        },
    )
    .await
}

async fn transition_lifecycle_undo(ctx: Ctx) -> Result<(), anyhow::Error> {
    let store = ctx.user_data().store().clone();
    let log = ctx.user_data().log().clone();
    let params: InstanceLifecycleParams = ctx
        .saga_params()
        .map_err(|e| anyhow::anyhow!("saga_params: {e}"))?;
    // We can only CAS back if the handler captured the original
    // state. If it didn't, the undo is a no-op (the agent will
    // observe an enqueued job for a state it can't act on, but
    // job dispatch already handles unknown lifecycle gracefully).
    let Some(original_kind) = params.original_state_kind else {
        return Ok(());
    };
    let Some(original) = original_kind.to_state() else {
        // Failed{reason} can't be reconstructed.
        return Ok(());
    };
    let from_transitional = params.op.to_state().kind();
    match store
        .transition_instance_lifecycle(params.instance_id, &[from_transitional], original)
        .await
    {
        Ok(_) => Ok(()),
        Err(tritond_store::StoreError::Conflict(_)) | Err(tritond_store::StoreError::NotFound) => {
            // Agent has already moved the instance past the
            // transitional state (or deleted it); accept and move on.
            slog::info!(
                log,
                "instance-lifecycle undo: agent moved past transitional state; skipping revert";
                "instance_id" => %params.instance_id,
            );
            Ok(())
        }
        Err(e) => Err(anyhow::anyhow!("lifecycle revert during undo: {e}")),
    }
}

async fn enqueue_job(ctx: Ctx) -> Result<ProvisioningJob, ActionError> {
    crate::sagas::with_action_timeout(
        "instance_lifecycle.enqueue_job",
        ACTION_TIMEOUT_STORE,
        async move {
            let user_ctx = ctx.user_data();
            fence_check(user_ctx).await?;
            let store = user_ctx.store().clone();
            let params: InstanceLifecycleParams = ctx.saga_params()?;
            let job = store
                .enqueue_job(NewJob {
                    kind: params.op.job_kind(params.instance_id),
                    target_cn_uuid: params.target_cn_uuid,
                })
                .await
                .map_err(store_err_to_action_err)?;
            Ok(job)
        },
    )
    .await
}

async fn await_terminal(ctx: Ctx) -> Result<(), ActionError> {
    let params: InstanceLifecycleParams = ctx.saga_params()?;
    if !params.await_job_terminal {
        return Ok(());
    }
    await_provisioning_job_terminal(ctx, "job", "instance_lifecycle.await_terminal").await
}

async fn finish(ctx: Ctx) -> Result<Instance, ActionError> {
    crate::sagas::with_action_timeout(
        "instance_lifecycle.finish",
        ACTION_TIMEOUT_STORE,
        async move {
            let user_ctx = ctx.user_data();
            fence_check(user_ctx).await?;
            let store = user_ctx.store().clone();
            let params: InstanceLifecycleParams = ctx.saga_params()?;
            // Re-read so the response reflects the agent's final
            // transition (Pending → Running, Stopping → Stopped,
            // etc.).
            store
                .get_instance(params.instance_id)
                .await
                .map_err(store_err_to_action_err)
        },
    )
    .await
}

pub fn decode_store_error_kind(source: &serde_json::Value) -> Option<&'static str> {
    super::common::decode_store_error_kind(source)
}
