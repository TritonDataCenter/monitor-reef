// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `nat-gateway-create` / `nat-gateway-delete` sagas
//!.
//!
//! SG-5 (per the RFD) ultimately wants the create chain to
//! allocate the public IP, select an edge CN, materialize the
//! `EdgeCluster` + `EdgeClusterInstance`, render the manifest,
//! enqueue `EdgeApply`, and await realization. The current
//! imperative path materialises the edge lazily (on route create
//! via `ensure_nat_gateway_edges_for_routes`). This first cut
//! wraps the existing store-level NAT gateway record CRUD in a
//! saga so:
//!
//! * the operation has an `Operation` handle + audit-chain entry,
//! * resource-reference index entries make it discoverable from
//!   per-tenant / per-project / per-VPC saga views,
//! * a future expansion of `nat-gateway-create` to drive the full
//!   edge-materialize flow doesn't change the wire shape — only
//!   adds nodes to the DAG.
//!
//! The edge materialisation is left where it is (route_create's
//! follow-on `ensure_nat_gateway_edge_materialized`) until SG-5b
//! folds it in.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tritond_saga::{
    ActionContext, ActionError, ActionFunc, ActionRegistry, DagBuilder, Node, ResourceRef,
    ResourceScope, SagaDag, SagaError, SagaName, SagaResult, TritondSagaType,
};
use tritond_store::{NatGateway, NewNatGateway};
use uuid::Uuid;

use super::common::{ACTION_TIMEOUT_STORE, Ctx, fence_check, no_op_undo, store_err_to_action_err};

pub const SAGA_NAME_CREATE: &str = "nat-gateway-create";
pub const SAGA_NAME_DELETE: &str = "nat-gateway-delete";
pub const SAGA_VERSION: u32 = 1;

pub fn register(reg: &mut ActionRegistry) {
    reg.register(ActionFunc::new_action(
        "nat_gateway.create_record",
        create_record,
        create_record_undo,
    ));
    reg.register(ActionFunc::new_action(
        "nat_gateway.delete_record",
        delete_record,
        no_op_undo,
    ));
}

// ===============================================================
// nat-gateway-create
// ===============================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NatGatewayCreateParams {
    pub tenant_id: Uuid,
    pub project_id: Uuid,
    pub vpc_id: Uuid,
    pub request: NewNatGateway,
}

pub fn build_create_dag(params: &NatGatewayCreateParams) -> SagaResult<Arc<SagaDag>> {
    let mut b = DagBuilder::new(SagaName::new(SAGA_NAME_CREATE));
    b.append(Node::action(
        "nat_gateway",
        "create_record",
        &*ActionFunc::new_action(
            "nat_gateway.create_record",
            create_record,
            create_record_undo,
        ),
    ));
    let dag = b
        .build()
        .map_err(|e| SagaError::Backend(format!("dag build: {e}")))?;
    let params_json = serde_json::to_value(params)
        .map_err(|e| SagaError::Backend(format!("params serialize: {e}")))?;
    Ok(Arc::new(SagaDag::new(dag, params_json)))
}

pub fn build_create_references(params: &NatGatewayCreateParams) -> Vec<ResourceRef> {
    vec![
        ResourceRef::new(ResourceScope::Tenant, params.tenant_id),
        ResourceRef::new(ResourceScope::Project, params.project_id),
        ResourceRef::new(ResourceScope::Vpc, params.vpc_id),
    ]
}

async fn create_record(ctx: Ctx) -> Result<NatGateway, ActionError> {
    crate::sagas::with_action_timeout(
        "nat_gateway.create_record",
        ACTION_TIMEOUT_STORE,
        async move {
            let user_ctx = ctx.user_data();
            fence_check(user_ctx).await?;
            let store = user_ctx.store().clone();
            let params: NatGatewayCreateParams = ctx.saga_params()?;
            store
                .create_nat_gateway(
                    params.tenant_id,
                    params.project_id,
                    params.vpc_id,
                    params.request,
                )
                .await
                .map_err(store_err_to_action_err)
        },
    )
    .await
}

async fn create_record_undo(ctx: Ctx) -> Result<(), anyhow::Error> {
    let store = ctx.user_data().store().clone();
    let Ok(nat) = ctx.lookup::<NatGateway>("nat_gateway") else {
        return Ok(());
    };
    match store.delete_nat_gateway(nat.id).await {
        Ok(()) | Err(tritond_store::StoreError::NotFound) => Ok(()),
        Err(e) => Err(anyhow::anyhow!("delete_nat_gateway during undo: {e}")),
    }
}

// ===============================================================
// nat-gateway-delete
// ===============================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NatGatewayDeleteParams {
    pub tenant_id: Uuid,
    pub project_id: Uuid,
    pub vpc_id: Uuid,
    pub nat_gateway_id: Uuid,
}

pub fn build_delete_dag(params: &NatGatewayDeleteParams) -> SagaResult<Arc<SagaDag>> {
    let mut b = DagBuilder::new(SagaName::new(SAGA_NAME_DELETE));
    b.append(Node::action(
        "deleted",
        "delete_record",
        &*ActionFunc::new_action("nat_gateway.delete_record", delete_record, no_op_undo),
    ));
    let dag = b
        .build()
        .map_err(|e| SagaError::Backend(format!("dag build: {e}")))?;
    let params_json = serde_json::to_value(params)
        .map_err(|e| SagaError::Backend(format!("params serialize: {e}")))?;
    Ok(Arc::new(SagaDag::new(dag, params_json)))
}

pub fn build_delete_references(params: &NatGatewayDeleteParams) -> Vec<ResourceRef> {
    vec![
        ResourceRef::new(ResourceScope::Tenant, params.tenant_id),
        ResourceRef::new(ResourceScope::Project, params.project_id),
        ResourceRef::new(ResourceScope::Vpc, params.vpc_id),
        ResourceRef::new(ResourceScope::NatGateway, params.nat_gateway_id),
    ]
}

async fn delete_record(ctx: Ctx) -> Result<(), ActionError> {
    crate::sagas::with_action_timeout(
        "nat_gateway.delete_record",
        ACTION_TIMEOUT_STORE,
        async move {
            let user_ctx = ctx.user_data();
            fence_check(user_ctx).await?;
            let store = user_ctx.store().clone();
            let params: NatGatewayDeleteParams = ctx.saga_params()?;
            match store.delete_nat_gateway(params.nat_gateway_id).await {
                Ok(()) | Err(tritond_store::StoreError::NotFound) => Ok(()),
                Err(e) => Err(store_err_to_action_err(e)),
            }
        },
    )
    .await
}

pub fn decode_store_error_kind(source: &serde_json::Value) -> Option<&'static str> {
    super::common::decode_store_error_kind(source)
}
