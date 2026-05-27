// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `image-import` saga.
//!
//! Per the RFD: tritond-side image record + scope → enqueue agent
//! fetch/import job → await. The current imperative path
//! (`create_*_image*`) creates the image record in one store call;
//! there's no agent-side fetch step today.
//!
//! This first cut wraps the record creation in a saga so the
//! operation gets an `Operation` handle + audit row + per-image /
//! per-silo / per-tenant / per-project reference index entries.
//! Adding the agent fetch chain is SG-6b — the wire shape stays
//! stable because that's a new node, not a param-shape bump.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tritond_saga::{
    ActionContext, ActionError, ActionFunc, ActionRegistry, DagBuilder, Node, ResourceRef,
    ResourceScope, SagaDag, SagaError, SagaName, SagaResult, TritondSagaType,
};
use uuid::Uuid;

use super::common::{Ctx, no_op_undo};

pub const SAGA_NAME: &str = "image-import";
pub const SAGA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageImportParams {
    pub image_id: Uuid,
    /// Scope the image is being imported into. One of fleet / silo
    /// / tenant / project. The scope owner id is the matching
    /// `*_id` field; the saga's reference list mirrors the choice.
    #[serde(default)]
    pub silo_id: Option<Uuid>,
    #[serde(default)]
    pub tenant_id: Option<Uuid>,
    #[serde(default)]
    pub project_id: Option<Uuid>,
}

pub fn register(reg: &mut ActionRegistry) {
    reg.register(ActionFunc::new_action(
        "image_import.record",
        record_marker,
        no_op_undo,
    ));
}

pub fn build_dag(params: &ImageImportParams) -> SagaResult<Arc<SagaDag>> {
    let mut b = DagBuilder::new(SagaName::new(SAGA_NAME));
    b.append(Node::action(
        "image",
        "record",
        &*ActionFunc::new_action("image_import.record", record_marker, no_op_undo),
    ));
    let dag = b
        .build()
        .map_err(|e| SagaError::Backend(format!("dag build: {e}")))?;
    let params_json = serde_json::to_value(params)
        .map_err(|e| SagaError::Backend(format!("params serialize: {e}")))?;
    Ok(Arc::new(SagaDag::new(dag, params_json)))
}

pub fn build_references(params: &ImageImportParams) -> Vec<ResourceRef> {
    let mut out = vec![ResourceRef::new(ResourceScope::Image, params.image_id)];
    if let Some(s) = params.silo_id {
        out.push(ResourceRef::new(ResourceScope::Silo, s));
    }
    if let Some(t) = params.tenant_id {
        out.push(ResourceRef::new(ResourceScope::Tenant, t));
    }
    if let Some(p) = params.project_id {
        out.push(ResourceRef::new(ResourceScope::Project, p));
    }
    if out.iter().all(|r| {
        !matches!(
            r.scope,
            ResourceScope::Silo | ResourceScope::Tenant | ResourceScope::Project
        )
    }) {
        // Fleet-scoped image — register the fleet ref so the
        // fleet view picks it up.
        out.push(ResourceRef::new(ResourceScope::Fleet, Uuid::nil()));
    }
    out
}

/// Marker action: the actual image record creation ran in the
/// handler before `saga_execute`. SG-6b will replace this with
/// the real (create record → enqueue fetch → await) chain.
async fn record_marker(_ctx: Ctx) -> Result<(), ActionError> {
    Ok(())
}
