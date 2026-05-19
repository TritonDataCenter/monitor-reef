// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `designate` saga action (RFD 00005 PL-5d).
//!
//! Wraps [`crate::placement::pick`] in a steno `ActionFunc` and
//! registers it in the catalog. The action body reads a
//! `PlacementRequest` from the saga's params, runs the chain
//! against the live store, and on success writes the
//! `CnReservation` + pins `Instance.host_cn_uuid`. The `undesignate`
//! undo releases both.
//!
//! ## Wire shape
//!
//! For PL-5d the action body reads its placement request from
//! the saga's params as a [`tritond_placement::PlacementRequest`].
//! Standalone tests drive it via a one-action saga over that
//! shape; the upcoming integration with `instance-create`
//! (PL-5e) wires the same action body via a small wrapper that
//! synthesises the `PlacementRequest` from
//! `InstanceCreateParams` + the previously-created `Instance`
//! row.
//!
//! ## Action output
//!
//! The do-fn returns the chosen `Uuid` (the host CN). The
//! `ExplainReport` is dropped on the saga state by design --
//! the audit row carries the bounded projection (per RFD 00005
//! invariant 4), not the full report. PL-5e routes the report
//! into the audit log; PL-5d ships the action with the report
//! discarded.
//!
//! ## Fencing
//!
//! The reservation row carries `(sec_id, epoch)` for audit per
//! RFD 00004 D-Sg-8. The catalog-action boundary fence check
//! (the SagaContext::verify_fence call existing actions perform)
//! sits one level up; PL-5d adopts the existing pattern.

use serde::{Deserialize, Serialize};
use tritond_placement::PlacementRequest;
use tritond_saga::{ActionContext, ActionError, ActionFunc, ActionRegistry, TritondSagaType};
use uuid::Uuid;

use crate::placement::{Commit, PickError, pick, release_reservation};

type Ctx = ActionContext<TritondSagaType>;

/// Catalog name. Used by saga DAG builders to reference this
/// action; also surfaces in `ExplainReport` and audit rows.
pub const ACTION_DESIGNATE: &str = "designate";

/// Standalone `designate` saga -- one action, params shape is
/// [`PlacementRequest`]. PL-5d ships this so the action body has
/// an exercised path; PL-5e integrates the same action body into
/// `instance-create` via a small in-DAG wrapper.
pub const STANDALONE_SAGA_NAME: &str = "designate";
pub const STANDALONE_SAGA_VERSION: u32 = 1;

/// The exact value the standalone-saga DAG hangs off of: a saga
/// whose params deserialise as a `PlacementRequest`. The action
/// body looks up the request from `ctx.saga_params()` and runs
/// the placement chain.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StandaloneParams(pub PlacementRequest);

/// Register the `designate` action in the catalog registry.
///
/// Called by `crate::sagas::register_all_actions` at executor
/// build time. The action is referenced by the standalone
/// designate saga (PL-5d) and by the future `instance-create`
/// DAG (PL-5e).
pub fn register(reg: &mut ActionRegistry) {
    reg.register(ActionFunc::new_action(
        ACTION_DESIGNATE,
        sd_designate,
        sd_undesignate,
    ));
}

async fn sd_designate(ctx: Ctx) -> Result<Uuid, ActionError> {
    crate::sagas::with_action_timeout(
        "designate",
        std::time::Duration::from_secs(30),
        async move {
            let user_ctx = ctx.user_data();
            let store = user_ctx.store().clone();
            let StandaloneParams(request) = ctx.saga_params()?;
            let saga_id_uuid: Uuid = user_ctx.saga_id().0;
            let sec_id_uuid = user_ctx.sec_id().0;
            let sec_epoch_u64 = user_ctx.sec_epoch().0;

            match pick(
                &store,
                request,
                Commit::Yes {
                    saga_id: saga_id_uuid,
                    sec_id: sec_id_uuid,
                    sec_epoch: sec_epoch_u64,
                },
            )
            .await
            {
                Ok(outcome) => {
                    let chosen = outcome.chosen.ok_or_else(|| {
                        ActionError::action_failed(serde_json::json!({
                            "kind": "designate.no_eligible_cn",
                            "reason": "internal: chosen was None on a commit-success path",
                        }))
                    })?;
                    Ok(chosen)
                }
                Err(PickError::NoEligibleCn { report }) => {
                    Err(ActionError::action_failed(serde_json::json!({
                        "kind": "designate.no_eligible_cn",
                        "audit": report.bounded_for_audit(),
                    })))
                }
                Err(PickError::Store(e)) => Err(ActionError::action_failed(serde_json::json!({
                    "kind": "designate.store_error",
                    "message": e.to_string(),
                }))),
            }
        },
    )
    .await
}

async fn sd_undesignate(ctx: Ctx) -> Result<(), anyhow::Error> {
    let user_ctx = ctx.user_data();
    let store = user_ctx.store().clone();
    let StandaloneParams(request) = ctx.saga_params()?;
    let saga_id_uuid: Uuid = user_ctx.saga_id().0;
    // The do-fn returns the chosen CN; the undo looks it up via
    // ctx.lookup. The lookup uses the action's *node name*; the
    // standalone DAG names the node `host_cn_uuid` for symmetry
    // with the embedded shape in `instance-create` (PL-5e).
    let cn: Uuid = ctx.lookup("host_cn_uuid").unwrap_or(Uuid::nil());
    if cn == Uuid::nil() {
        // do-fn never produced a chosen CN -- nothing to release.
        return Ok(());
    }
    release_reservation(&store, cn, saga_id_uuid, request.instance_id)
        .await
        .map_err(|e| anyhow::anyhow!("release_reservation: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_compiles_with_register_callable() {
        // Trivial test ensuring the surface compiles. Driving
        // the action via the full SagaExecutor needs the test
        // harness pieces the in-process executor wires up; PL-5d
        // ships the registration + body so PL-5e can compose
        // both into a DAG.
        let _ = ACTION_DESIGNATE;
        assert_eq!(STANDALONE_SAGA_NAME, "designate");
    }
}
