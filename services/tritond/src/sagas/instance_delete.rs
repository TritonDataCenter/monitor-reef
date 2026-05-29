// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `instance-delete` saga.
//!
//! Promotes the imperative `delete_project_instance` (today:
//! `delete_instance(force=true)` + best-effort `JobKind::Delete`
//! enqueue) to an agent-ack'd chain:
//!
//! | # | Action                | Output             | Undo                                                 |
//! |---|-----------------------|--------------------|------------------------------------------------------|
//! | 1 | `snapshot_attachments`| `DeleteSnapshot`   | (no side effect — just reads)                        |
//! | 2 | `detach_fips`         | `()`               | re-attach each FIP to its prior NIC (best-effort)    |
//! | 3 | `enqueue_delete_job`  | `ProvisioningJob`  | (no-op — by unwind time the job is terminal)         |
//! | 4 | `await_delete_terminal`| `()`              | (no-op)                                              |
//! | 5 | `release_record`      | `()`               | (cannot undo a store-side release — Stuck on err)    |
//! | 6 | `finish`              | `()`               | (no-op)                                              |
//!
//! The store-side `delete_instance(force=true)` in action 5
//! releases every NIC / IPv4 / IPv6 / Disk / DhcpLease in a single
//! transaction. If that mutation fails after the agent already
//! tore down the zone, the saga lands `Stuck` (Done + the undo
//! sweep can't undo a successful zone delete) and the operator
//! sees the record still exists for forensic cleanup.
//!
//! v2p invalidations (PROTEUS §11.7.1 item 8) are pushed from the
//! `release_record` action body before `delete_instance` clears
//! the NIC rows — the broadcast cost is bounded by the saga.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tritond_saga::{
    ActionContext, ActionError, ActionFunc, ActionRegistry, DagBuilder, Node, ResourceRef,
    ResourceScope, SagaDag, SagaError, SagaName, SagaResult, TritondSagaType,
};
use tritond_store::{FloatingIp, JobKind, NewJob, ProvisioningJob};
use uuid::Uuid;

use super::common::{
    ACTION_TIMEOUT_STORE, Ctx, await_provisioning_job_terminal, fence_check, no_op_undo,
    store_err_to_action_err,
};

pub const SAGA_NAME: &str = "instance-delete";
pub const SAGA_VERSION: u32 = 1;

/// Params handed to `SagaExecutor::saga_execute`. Carries the
/// already-resolved instance + scope so action bodies don't have
/// to re-validate path parentage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceDeleteParams {
    pub tenant_id: Uuid,
    pub project_id: Uuid,
    pub instance_id: Uuid,
    /// Host CN the instance is pinned to (populated by the
    /// handler from the instance record; carried into the enqueue
    /// action so the Delete job is routed to the right agent).
    pub target_cn_uuid: Option<Uuid>,
    /// `true` in production so a Delete job failure stuck-fails
    /// the saga; `false` in test fixtures that drive the agent
    /// manually.
    #[serde(default = "default_true")]
    pub await_delete_terminal: bool,
}

fn default_true() -> bool {
    true
}

/// Output of `snapshot_attachments` — what we need to restore on
/// undo of the detach action.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct DeleteSnapshot {
    /// Every FIP currently attached to a NIC on this instance.
    /// Stored as `(fip_id, nic_id)` pairs so the undo can re-bind.
    attached_fips: Vec<(Uuid, Uuid)>,
    /// FIPs that were also *hosted* on a CN (CN-terminated 1:1 NAT) at
    /// snapshot time. Each needs a dataplane withdraw (FipRelease)
    /// pinned to its hosting CN so the instance delete does not leak an
    /// ipadm `<fip>/32` alias + a stale `hosted_fips` entry (invariant
    /// 14). A detached / un-hosted FIP never lands here.
    hosted_fips: Vec<HostedFipWithdraw>,
}

/// One hosted FIP to withdraw during instance delete (invariant 14).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct HostedFipWithdraw {
    fip_id: Uuid,
    hosted_cn: Uuid,
    fip_addr: String,
    external_nic_tag: Option<String>,
}

type LocalCtx = Ctx;

pub fn register(reg: &mut ActionRegistry) {
    reg.register(ActionFunc::new_action(
        "instance_delete.snapshot",
        snapshot_attachments,
        no_op_undo,
    ));
    reg.register(ActionFunc::new_action(
        "instance_delete.detach_fips",
        detach_fips,
        detach_fips_undo,
    ));
    reg.register(ActionFunc::new_action(
        "instance_delete.enqueue_delete_job",
        enqueue_delete_job,
        no_op_undo,
    ));
    reg.register(ActionFunc::new_action(
        "instance_delete.await_delete_terminal",
        await_delete_terminal,
        no_op_undo,
    ));
    reg.register(ActionFunc::new_action(
        "instance_delete.release_record",
        release_record,
        no_op_undo,
    ));
    reg.register(ActionFunc::new_action(
        "instance_delete.finish",
        finish,
        no_op_undo,
    ));
}

pub fn build_dag(params: &InstanceDeleteParams) -> SagaResult<Arc<SagaDag>> {
    let name = SagaName::new(SAGA_NAME);
    let mut b = DagBuilder::new(name);
    b.append(Node::action(
        "snapshot",
        "snapshot_attachments",
        &*ActionFunc::new_action("instance_delete.snapshot", snapshot_attachments, no_op_undo),
    ));
    b.append(Node::action(
        "fips_detached",
        "detach_fips",
        &*ActionFunc::new_action("instance_delete.detach_fips", detach_fips, detach_fips_undo),
    ));
    b.append(Node::action(
        "delete_job",
        "enqueue_delete_job",
        &*ActionFunc::new_action(
            "instance_delete.enqueue_delete_job",
            enqueue_delete_job,
            no_op_undo,
        ),
    ));
    b.append(Node::action(
        "deleted_on_agent",
        "await_delete_terminal",
        &*ActionFunc::new_action(
            "instance_delete.await_delete_terminal",
            await_delete_terminal,
            no_op_undo,
        ),
    ));
    b.append(Node::action(
        "record_released",
        "release_record",
        &*ActionFunc::new_action("instance_delete.release_record", release_record, no_op_undo),
    ));
    b.append(Node::action(
        "final",
        "finish",
        &*ActionFunc::new_action("instance_delete.finish", finish, no_op_undo),
    ));
    let dag = b
        .build()
        .map_err(|e| SagaError::Backend(format!("dag build: {e}")))?;
    let params_json = serde_json::to_value(params)
        .map_err(|e| SagaError::Backend(format!("params serialize: {e}")))?;
    Ok(Arc::new(SagaDag::new(dag, params_json)))
}

/// Resources this saga touches. The Instance ref makes the saga
/// findable from the VM detail page; tenant / project / cn
/// support the per-tenant / per-project / per-CN views.
pub fn build_references(
    params: &InstanceDeleteParams,
    target_cn_uuid: Option<Uuid>,
) -> Vec<ResourceRef> {
    let mut out = Vec::new();
    out.push(ResourceRef::new(ResourceScope::Tenant, params.tenant_id));
    out.push(ResourceRef::new(ResourceScope::Project, params.project_id));
    out.push(ResourceRef::new(
        ResourceScope::Instance,
        params.instance_id,
    ));
    if let Some(cn) = target_cn_uuid.or(params.target_cn_uuid) {
        out.push(ResourceRef::new(ResourceScope::Cn, cn));
    }
    out
}

// ---------------------------------------------------------------
// Actions
// ---------------------------------------------------------------

async fn snapshot_attachments(ctx: LocalCtx) -> Result<DeleteSnapshot, ActionError> {
    crate::sagas::with_action_timeout(
        "instance_delete.snapshot",
        ACTION_TIMEOUT_STORE,
        async move {
            let user_ctx = ctx.user_data();
            fence_check(user_ctx).await?;
            let store = user_ctx.store().clone();
            let params: InstanceDeleteParams = ctx.saga_params()?;
            // Phase-0 simplification: walk the project's FIPs and
            // filter to those bound to this instance. The store
            // doesn't yet have a per-instance FIP lookup; one
            // project-scan per delete is acceptable given the size of
            // the operation.
            let project_fips: Vec<FloatingIp> = store
                .list_floating_ips_in_project(params.project_id)
                .await
                .map_err(store_err_to_action_err)?;
            let instance_fips: Vec<&FloatingIp> = project_fips
                .iter()
                .filter(|f| {
                    f.attached_to
                        .as_ref()
                        .is_some_and(|a| a.instance_id == params.instance_id)
                })
                .collect();
            let attached = instance_fips
                .iter()
                .filter_map(|f| f.attached_to.as_ref().map(|a| (f.id, a.nic_id)))
                .collect();
            // Capture the hosted (CN-terminated) FIPs separately so the
            // detach step can withdraw the dataplane on the hosting CN
            // (invariant 14). Resolve the external nic_tag name here so
            // the FipRelease job is complete after the binding clears.
            let mut hosted_fips = Vec::new();
            for f in &instance_fips {
                if let Some(hosted_cn) = f.hosted_cn {
                    let external_nic_tag = match f.external_nic_tag {
                        Some(tag_id) => {
                            store.get_nic_tag(tag_id).await.ok().map(|t| t.name)
                        }
                        None => None,
                    };
                    hosted_fips.push(HostedFipWithdraw {
                        fip_id: f.id,
                        hosted_cn,
                        fip_addr: f.address.to_string(),
                        external_nic_tag,
                    });
                }
            }
            Ok(DeleteSnapshot {
                attached_fips: attached,
                hosted_fips,
            })
        },
    )
    .await
}

async fn detach_fips(ctx: LocalCtx) -> Result<(), ActionError> {
    crate::sagas::with_action_timeout(
        "instance_delete.detach_fips",
        ACTION_TIMEOUT_STORE,
        async move {
            let user_ctx = ctx.user_data();
            fence_check(user_ctx).await?;
            let store = user_ctx.store().clone();
            let snap: DeleteSnapshot = ctx.lookup("snapshot")?;
            // Invariant 14: withdraw each hosted (CN-terminated) FIP's
            // dataplane on its hosting CN BEFORE clearing the binding
            // (release-before-detach), so the instance delete does not
            // leak an ipadm alias + stale hosted_fips entry. Pinned to
            // the hosting CN. A detached / un-hosted FIP enqueues
            // nothing (it never reached `hosted_fips`).
            for hosted in &snap.hosted_fips {
                store
                    .enqueue_job(NewJob {
                        kind: JobKind::FipRelease {
                            floating_ip_id: hosted.fip_id,
                            fip_addr: hosted.fip_addr.clone(),
                            external_nic_tag: hosted.external_nic_tag.clone(),
                            hosted_cn: hosted.hosted_cn,
                        },
                        target_cn_uuid: Some(hosted.hosted_cn),
                    })
                    .await
                    .map_err(store_err_to_action_err)?;
            }
            for (fip_id, _nic_id) in &snap.attached_fips {
                // detach is idempotent — no-op when already
                // detached, returns the record either way.
                store
                    .detach_floating_ip(*fip_id)
                    .await
                    .map_err(store_err_to_action_err)?;
            }
            Ok(())
        },
    )
    .await
}

async fn detach_fips_undo(ctx: LocalCtx) -> Result<(), anyhow::Error> {
    let store = ctx.user_data().store().clone();
    let log = ctx.user_data().log().clone();
    // If the snapshot wasn't taken (action 1 failed) the lookup
    // errors; nothing to undo.
    let Ok(snap) = ctx.lookup::<DeleteSnapshot>("snapshot") else {
        return Ok(());
    };
    for (fip_id, nic_id) in &snap.attached_fips {
        match store.attach_floating_ip(*fip_id, *nic_id).await {
            Ok(_) => {}
            Err(e) => {
                slog::warn!(
                    log,
                    "instance-delete undo: re-attach FIP failed; operator may need to re-attach manually";
                    "fip_id" => %fip_id,
                    "nic_id" => %nic_id,
                    "error" => %e,
                );
            }
        }
    }
    Ok(())
}

async fn enqueue_delete_job(ctx: LocalCtx) -> Result<ProvisioningJob, ActionError> {
    crate::sagas::with_action_timeout(
        "instance_delete.enqueue_delete_job",
        ACTION_TIMEOUT_STORE,
        async move {
            let user_ctx = ctx.user_data();
            fence_check(user_ctx).await?;
            let store = user_ctx.store().clone();
            let params: InstanceDeleteParams = ctx.saga_params()?;
            let job = store
                .enqueue_job(NewJob {
                    kind: JobKind::Delete {
                        instance_id: params.instance_id,
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

async fn await_delete_terminal(ctx: LocalCtx) -> Result<(), ActionError> {
    let params: InstanceDeleteParams = ctx.saga_params()?;
    if !params.await_delete_terminal {
        return Ok(());
    }
    await_provisioning_job_terminal(ctx, "delete_job", "instance_delete.await_delete_terminal")
        .await
}

async fn release_record(ctx: LocalCtx) -> Result<(), ActionError> {
    crate::sagas::with_action_timeout(
        "instance_delete.release_record",
        ACTION_TIMEOUT_STORE,
        async move {
            let user_ctx = ctx.user_data();
            fence_check(user_ctx).await?;
            let store = user_ctx.store().clone();
            let params: InstanceDeleteParams = ctx.saga_params()?;
            // v2p invalidation broadcast (PROTEUS §11.7.1 item 8)
            // lives in the handler before saga_execute — the global
            // ring isn't reachable from a SagaContext yet. A
            // follow-up can add a `PeerInvalidations` handle to
            // `SagaContext` and move it here.

            // force=true: the zone has been told to delete via the
            // agent; release every NIC/IP/Disk/DhcpLease row.
            // NotFound is benign — a retry that reached here again
            // after the record was already cleared.
            match store.delete_instance(params.instance_id, true).await {
                Ok(()) | Err(tritond_store::StoreError::NotFound) => Ok(()),
                Err(e) => Err(store_err_to_action_err(e)),
            }
        },
    )
    .await
}

async fn finish(_ctx: LocalCtx) -> Result<(), ActionError> {
    Ok(())
}

pub fn decode_store_error_kind(source: &serde_json::Value) -> Option<&'static str> {
    super::common::decode_store_error_kind(source)
}
