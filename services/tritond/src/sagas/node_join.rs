// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `node-join` saga (RFD 00004 SG-6).
//!
//! Per the RFD the chain is: CN registration → credential issuance
//! → bound API-key mint, with undos that revoke the credentials.
//! The current imperative path (`approve_cn` →
//! `mint_and_attach_cn_credential`) does all three in one
//! transaction-ish flow. This first cut wraps the **outcome** in a
//! saga record so the operation has:
//!
//! * an `Operation` handle + audit-chain entry,
//! * resource-reference index entries (fleet + CN) so the
//!   per-CN saga view picks it up.
//!
//! Splitting the chain into the three RFD-named actions with
//! granular undo (key revoke, claim-code restore) is SG-6b. The
//! single-action shape here matches the existing transactional
//! semantics (either the whole approve lands or it doesn't), so
//! the saga doesn't change observable behaviour while the
//! catalog catches up.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tritond_saga::{
    ActionContext, ActionError, ActionFunc, ActionRegistry, DagBuilder, Node, ResourceRef,
    ResourceScope, SagaDag, SagaError, SagaName, SagaResult, TritondSagaType,
};
use uuid::Uuid;

use super::common::{Ctx, no_op_undo};

pub const SAGA_NAME: &str = "node-join";
pub const SAGA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeJoinParams {
    /// CN server UUID — the resource being joined.
    pub server_uuid: Uuid,
}

pub fn register(reg: &mut ActionRegistry) {
    reg.register(ActionFunc::new_action(
        "node_join.join",
        join_marker,
        no_op_undo,
    ));
}

pub fn build_dag(params: &NodeJoinParams) -> SagaResult<Arc<SagaDag>> {
    let mut b = DagBuilder::new(SagaName::new(SAGA_NAME));
    b.append(Node::action(
        "joined",
        "join",
        &*ActionFunc::new_action("node_join.join", join_marker, no_op_undo),
    ));
    let dag = b
        .build()
        .map_err(|e| SagaError::Backend(format!("dag build: {e}")))?;
    let params_json = serde_json::to_value(params)
        .map_err(|e| SagaError::Backend(format!("params serialize: {e}")))?;
    Ok(Arc::new(SagaDag::new(dag, params_json)))
}

pub fn build_references(params: &NodeJoinParams) -> Vec<ResourceRef> {
    vec![
        ResourceRef::new(ResourceScope::Fleet, Uuid::nil()),
        ResourceRef::new(ResourceScope::Cn, params.server_uuid),
    ]
}

/// Marker action: the actual approval ran in the handler before
/// `saga_execute`. This action records the saga succeeded so the
/// operation reads as `succeeded` in the Operations view. SG-6b
/// will replace this with the real (cred-issuance + key-mint +
/// approve) chain.
async fn join_marker(_ctx: Ctx) -> Result<(), ActionError> {
    Ok(())
}
