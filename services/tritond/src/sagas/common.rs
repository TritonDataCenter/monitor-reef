// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Shared saga action helpers.
//!
//! Every catalog module reuses this small toolbox:
//!
//! * [`fence_check`] — D-Sg-8 epoch verification at the top of
//!   every action body.
//! * [`store_err_to_action_err`] — tag a `StoreError` into the
//!   structured `ActionError` payload the handler decodes back to
//!   the right HTTP status (`409`/`404`/`500`). Mirrors
//!   `instance_create::store_err_to_action_err`.
//! * [`decode_store_error_kind`] — handler-side inverse: pull a
//!   stable kind string out of an `ActionError::source_error` so
//!   the HTTP layer can preserve duplicate-name → 409 etc.
//! * [`no_op_undo`] — placeholder for actions whose forward
//!   effect is idempotent (`await_*`, `finish`, audit emission).
//! * [`AwaitJobParams`] / [`await_provisioning_job_terminal`] — the
//!   shared "wait for an enqueued `ProvisioningJob` to reach
//!   `Completed` / `Failed`" action. Used by every saga that
//!   dispatches CN-side work.
//!
//! Action bodies live here so the catalog modules don't drift on
//! shape (e.g. one module wraps in `with_action_timeout`, another
//! forgets — and we miss D-Sg-9 invariant 9 enforcement).

use std::time::Duration;

use tritond_saga::{ActionContext, ActionError, SagaContext, TritondSagaType};
use tritond_store::{JobStatusKind, ProvisioningJob, StoreError};

pub type Ctx = ActionContext<TritondSagaType>;

/// D-Sg-9 default for short store mutations. 30 s is far outside
/// what any single store call should ever take in practice but
/// catches a wedged FDB / hanging metadata write.
pub const ACTION_TIMEOUT_STORE: Duration = Duration::from_secs(30);

/// D-Sg-9 default for `await_*` actions that poll the agent.
/// Matches the existing `TRITOND_STALE_CLAIM_THRESHOLD_SECS`
/// default (600 s) so a wedged agent claim and the saga awaiting
/// it fail together.
pub const ACTION_TIMEOUT_AWAIT: Duration = Duration::from_secs(600);

/// best-effort fence check called at the top of
/// every saga action body before any externally-visible side
/// effect. If another SEC has adopted the saga since this action's
/// context was built, short-circuit the action so the unwind tail
/// runs in this process while the adopting SEC drives the saga
/// forward.
pub async fn fence_check(ctx: &SagaContext) -> Result<(), ActionError> {
    ctx.verify_fence().await.map_err(|e| {
        ActionError::action_failed(serde_json::json!({
            "kind": "fenced_out",
            "message": e.to_string(),
        }))
    })
}

/// Tag a [`StoreError`] into a structured `ActionError` payload
/// the handler can decode back into the right HTTP status. The
/// handler uses [`decode_store_error_kind`] to re-derive
/// `409`/`404`/`500` from the payload — without it, every saga
/// failure would land as `500` and we'd lose the existing
/// `duplicate-name → 409` invariant.
pub fn store_err_to_action_err(e: StoreError) -> ActionError {
    // StoreError is `#[non_exhaustive]`; the wildcard arm catches
    // every variant outside the well-known set so a future store
    // error doesn't break the build.
    let kind = match &e {
        StoreError::Conflict(_) => "conflict",
        StoreError::NotFound => "not_found",
        StoreError::Backend(_) => "store_backend",
        _ => "store_other",
    };
    ActionError::action_failed(serde_json::json!({
        "kind": kind,
        "message": e.to_string(),
    }))
}

/// Handler-side inverse of [`store_err_to_action_err`]. Returns the
/// stable kind string from an `ActionError::source_error`, so the
/// HTTP layer can map back to the right status. Returns `None`
/// when the payload didn't come from a store-tagged action body.
pub fn decode_store_error_kind(source: &serde_json::Value) -> Option<&'static str> {
    let kind = source.get("kind")?.as_str()?;
    match kind {
        "conflict" => Some("conflict"),
        "not_found" => Some("not_found"),
        "store_backend" => Some("store_backend"),
        "store_other" => Some("store_other"),
        "fenced_out" => Some("fenced_out"),
        "action_timeout" => Some("action_timeout"),
        _ => None,
    }
}

pub async fn no_op_undo(_ctx: Ctx) -> Result<(), anyhow::Error> {
    Ok(())
}

/// Poll a `ProvisioningJob` until it reaches a terminal status.
/// On `JobOutcome::Failed`, return an `ActionError` so the saga
/// unwinds. The action expects the job to live in the saga's
/// context under `job_node_name` — pass the name you used when
/// you appended the enqueue node.
///
/// Used by every saga that dispatches CN-side work — RFD 00004
/// D-Sg-2 "the agent-dispatch protocol is untouched; sagas are
/// the orchestration layer above the queue".
pub async fn await_provisioning_job_terminal(
    ctx: Ctx,
    job_node_name: &str,
    action_name: &'static str,
) -> Result<(), ActionError> {
    crate::sagas::with_action_timeout(action_name, ACTION_TIMEOUT_AWAIT, async move {
        let user_ctx = ctx.user_data();
        fence_check(user_ctx).await?;
        let store = user_ctx.store().clone();
        let job: ProvisioningJob = ctx.lookup(job_node_name)?;
        let job_id = job.id;
        const POLL: Duration = Duration::from_millis(50);
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
    })
    .await
}
