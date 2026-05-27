// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `floating-ip-allocate` / `floating-ip-attach` /
//! `floating-ip-detach` sagas.
//!
//! Single-store-call operations on the surface — saga-shape is
//! light. Their value is the resource-reference index entries
//! (tenant / project / fip / nic) so the per-FIP and per-VM saga
//! views can surface them, and the standard saga unwind story
//! (record the original state, restore on failure).

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tritond_saga::{
    ActionContext, ActionError, ActionFunc, ActionRegistry, DagBuilder, Node, ResourceRef,
    ResourceScope, SagaDag, SagaError, SagaName, SagaResult, TritondSagaType,
};
use tritond_store::{FloatingIp, NewFloatingIp};
use uuid::Uuid;

use super::common::{ACTION_TIMEOUT_STORE, Ctx, fence_check, no_op_undo, store_err_to_action_err};

pub const SAGA_NAME_ALLOCATE: &str = "floating-ip-allocate";
pub const SAGA_NAME_ATTACH: &str = "floating-ip-attach";
pub const SAGA_NAME_DETACH: &str = "floating-ip-detach";
pub const SAGA_VERSION: u32 = 1;

pub fn register(reg: &mut ActionRegistry) {
    reg.register(ActionFunc::new_action(
        "floating_ip.allocate",
        allocate,
        allocate_undo,
    ));
    reg.register(ActionFunc::new_action(
        "floating_ip.attach",
        attach,
        attach_undo,
    ));
    reg.register(ActionFunc::new_action(
        "floating_ip.detach",
        detach,
        detach_undo,
    ));
}

// ===============================================================
// floating-ip-allocate
// ===============================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FloatingIpAllocateParams {
    pub tenant_id: Uuid,
    pub project_id: Uuid,
    pub request: NewFloatingIp,
}

pub fn build_allocate_dag(params: &FloatingIpAllocateParams) -> SagaResult<Arc<SagaDag>> {
    let mut b = DagBuilder::new(SagaName::new(SAGA_NAME_ALLOCATE));
    b.append(Node::action(
        "fip",
        "allocate",
        &*ActionFunc::new_action("floating_ip.allocate", allocate, allocate_undo),
    ));
    let dag = b
        .build()
        .map_err(|e| SagaError::Backend(format!("dag build: {e}")))?;
    let params_json = serde_json::to_value(params)
        .map_err(|e| SagaError::Backend(format!("params serialize: {e}")))?;
    Ok(Arc::new(SagaDag::new(dag, params_json)))
}

pub fn build_allocate_references(params: &FloatingIpAllocateParams) -> Vec<ResourceRef> {
    vec![
        ResourceRef::new(ResourceScope::Tenant, params.tenant_id),
        ResourceRef::new(ResourceScope::Project, params.project_id),
    ]
}

async fn allocate(ctx: Ctx) -> Result<FloatingIp, ActionError> {
    crate::sagas::with_action_timeout("floating_ip.allocate", ACTION_TIMEOUT_STORE, async move {
        let user_ctx = ctx.user_data();
        fence_check(user_ctx).await?;
        let store = user_ctx.store().clone();
        let params: FloatingIpAllocateParams = ctx.saga_params()?;
        store
            .create_floating_ip(params.tenant_id, params.project_id, params.request)
            .await
            .map_err(store_err_to_action_err)
    })
    .await
}

async fn allocate_undo(ctx: Ctx) -> Result<(), anyhow::Error> {
    let store = ctx.user_data().store().clone();
    let Ok(fip) = ctx.lookup::<FloatingIp>("fip") else {
        return Ok(());
    };
    match store.delete_floating_ip(fip.id).await {
        Ok(()) | Err(tritond_store::StoreError::NotFound) => Ok(()),
        Err(e) => Err(anyhow::anyhow!("delete_floating_ip during undo: {e}")),
    }
}

// ===============================================================
// floating-ip-attach
// ===============================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FloatingIpAttachParams {
    pub tenant_id: Uuid,
    pub project_id: Uuid,
    pub fip_id: Uuid,
    pub target_nic_id: Uuid,
    /// Captured by the handler before the saga runs so the undo
    /// can restore the prior binding. `None` = the FIP was
    /// floating; undo detaches.
    #[serde(default)]
    pub prior_nic_id: Option<Uuid>,
    /// The instance the target NIC belongs to (resolved by the
    /// handler from the NIC record). Goes into the saga's
    /// reference index so the per-instance saga view picks the
    /// attach up.
    #[serde(default)]
    pub target_instance_id: Option<Uuid>,
}

pub fn build_attach_dag(params: &FloatingIpAttachParams) -> SagaResult<Arc<SagaDag>> {
    let mut b = DagBuilder::new(SagaName::new(SAGA_NAME_ATTACH));
    b.append(Node::action(
        "attached",
        "attach",
        &*ActionFunc::new_action("floating_ip.attach", attach, attach_undo),
    ));
    let dag = b
        .build()
        .map_err(|e| SagaError::Backend(format!("dag build: {e}")))?;
    let params_json = serde_json::to_value(params)
        .map_err(|e| SagaError::Backend(format!("params serialize: {e}")))?;
    Ok(Arc::new(SagaDag::new(dag, params_json)))
}

pub fn build_attach_references(params: &FloatingIpAttachParams) -> Vec<ResourceRef> {
    let mut out = vec![
        ResourceRef::new(ResourceScope::Tenant, params.tenant_id),
        ResourceRef::new(ResourceScope::Project, params.project_id),
        ResourceRef::new(ResourceScope::FloatingIp, params.fip_id),
        ResourceRef::new(ResourceScope::Nic, params.target_nic_id),
    ];
    if let Some(iid) = params.target_instance_id {
        out.push(ResourceRef::new(ResourceScope::Instance, iid));
    }
    out
}

async fn attach(ctx: Ctx) -> Result<FloatingIp, ActionError> {
    crate::sagas::with_action_timeout("floating_ip.attach", ACTION_TIMEOUT_STORE, async move {
        let user_ctx = ctx.user_data();
        fence_check(user_ctx).await?;
        let store = user_ctx.store().clone();
        let params: FloatingIpAttachParams = ctx.saga_params()?;
        store
            .attach_floating_ip(params.fip_id, params.target_nic_id)
            .await
            .map_err(store_err_to_action_err)
    })
    .await
}

async fn attach_undo(ctx: Ctx) -> Result<(), anyhow::Error> {
    let store = ctx.user_data().store().clone();
    let log = ctx.user_data().log().clone();
    let Ok(params) = ctx.saga_params::<FloatingIpAttachParams>() else {
        return Ok(());
    };
    match params.prior_nic_id {
        Some(prior) => match store.attach_floating_ip(params.fip_id, prior).await {
            Ok(_) => Ok(()),
            Err(tritond_store::StoreError::NotFound) => Ok(()),
            Err(e) => {
                slog::warn!(
                    log,
                    "floating-ip-attach undo: restore-prior-binding failed";
                    "fip_id" => %params.fip_id,
                    "prior_nic_id" => %prior,
                    "error" => %e,
                );
                Ok(())
            }
        },
        None => match store.detach_floating_ip(params.fip_id).await {
            Ok(_) | Err(tritond_store::StoreError::NotFound) => Ok(()),
            Err(e) => Err(anyhow::anyhow!("detach during attach-undo: {e}")),
        },
    }
}

// ===============================================================
// floating-ip-detach
// ===============================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FloatingIpDetachParams {
    pub tenant_id: Uuid,
    pub project_id: Uuid,
    pub fip_id: Uuid,
    /// Captured by the handler before the saga runs so the undo
    /// can re-attach. `None` means the FIP was already floating
    /// — detach is a no-op and the undo has nothing to restore.
    #[serde(default)]
    pub prior_nic_id: Option<Uuid>,
    #[serde(default)]
    pub prior_instance_id: Option<Uuid>,
}

pub fn build_detach_dag(params: &FloatingIpDetachParams) -> SagaResult<Arc<SagaDag>> {
    let mut b = DagBuilder::new(SagaName::new(SAGA_NAME_DETACH));
    b.append(Node::action(
        "detached",
        "detach",
        &*ActionFunc::new_action("floating_ip.detach", detach, detach_undo),
    ));
    let dag = b
        .build()
        .map_err(|e| SagaError::Backend(format!("dag build: {e}")))?;
    let params_json = serde_json::to_value(params)
        .map_err(|e| SagaError::Backend(format!("params serialize: {e}")))?;
    Ok(Arc::new(SagaDag::new(dag, params_json)))
}

pub fn build_detach_references(params: &FloatingIpDetachParams) -> Vec<ResourceRef> {
    let mut out = vec![
        ResourceRef::new(ResourceScope::Tenant, params.tenant_id),
        ResourceRef::new(ResourceScope::Project, params.project_id),
        ResourceRef::new(ResourceScope::FloatingIp, params.fip_id),
    ];
    if let Some(nic) = params.prior_nic_id {
        out.push(ResourceRef::new(ResourceScope::Nic, nic));
    }
    if let Some(iid) = params.prior_instance_id {
        out.push(ResourceRef::new(ResourceScope::Instance, iid));
    }
    out
}

async fn detach(ctx: Ctx) -> Result<FloatingIp, ActionError> {
    crate::sagas::with_action_timeout("floating_ip.detach", ACTION_TIMEOUT_STORE, async move {
        let user_ctx = ctx.user_data();
        fence_check(user_ctx).await?;
        let store = user_ctx.store().clone();
        let params: FloatingIpDetachParams = ctx.saga_params()?;
        store
            .detach_floating_ip(params.fip_id)
            .await
            .map_err(store_err_to_action_err)
    })
    .await
}

async fn detach_undo(ctx: Ctx) -> Result<(), anyhow::Error> {
    let store = ctx.user_data().store().clone();
    let log = ctx.user_data().log().clone();
    let Ok(params) = ctx.saga_params::<FloatingIpDetachParams>() else {
        return Ok(());
    };
    let Some(prior) = params.prior_nic_id else {
        return Ok(());
    };
    match store.attach_floating_ip(params.fip_id, prior).await {
        Ok(_) => Ok(()),
        Err(tritond_store::StoreError::NotFound) => Ok(()),
        Err(e) => {
            slog::warn!(
                log,
                "floating-ip-detach undo: restore-prior-binding failed";
                "fip_id" => %params.fip_id,
                "prior_nic_id" => %prior,
                "error" => %e,
            );
            Ok(())
        }
    }
}

// no_op stand-ins so the un-used import lints stay quiet in builds
// that compile only allocate / attach / detach in isolation.
#[allow(dead_code)]
async fn _unused(_ctx: Ctx) -> Result<(), anyhow::Error> {
    no_op_undo(_ctx).await
}

pub fn decode_store_error_kind(source: &serde_json::Value) -> Option<&'static str> {
    super::common::decode_store_error_kind(source)
}
