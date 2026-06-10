// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `migrate-instance` saga. One static 15-node DAG serves both
//! the cold and the live lanes — live-only nodes no-op when
//! `params.cold` and vice versa. The v2 sequence fixes the v1 cold
//! data-loss bug: the source guest is now stopped (cold) or paused
//! (live) in `quiesce_source` *before* `final_zfs_increment` runs,
//! so writes after the final snapshot can no longer be lost. v3
//! inserts `mount_target` after the final receive: the receives run
//! `zfs recv -u`, so the zoneroot lands unmounted and both cold
//! activation and live listen-mode boot fail to write `/startvm`
//! until it is mounted. `switch_ownership` is the atomic FDB CAS —
//! the point of no return; nodes after it are best-effort and never
//! unwind.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tritond_saga::{
    ActionContext, ActionError, ActionFunc, ActionRegistry, DagBuilder, Node, ResourceRef,
    ResourceScope, SagaDag, SagaError, SagaName, SagaResult, TritondSagaType,
};
use tritond_store::{
    LifecycleState, LifecycleStateKind, MigrationPhase, MigrationRecord, MigrationState,
    ProvisioningJob, QuotaDanceSaveResult, ZfsSendResult,
};
use uuid::Uuid;

use crate::sagas::common::{
    ACTION_TIMEOUT_AWAIT, ACTION_TIMEOUT_STORE, fence_check, no_op_undo, store_err_to_action_err,
};

pub const SAGA_NAME: &str = "migrate-instance";

/// Steno saga version. Bump on any change to the action sequence,
/// action ids, or [`MigrationSagaParams`] shape. v2 adds the
/// quota-dance / sync-convergence / quiesce / activate nodes and
/// the `was_running` param. v3 inserts `migration.mount_target`
/// between `final_zfs_increment` and `stream_vmm`. The superseded
/// action names (`migration.snapshot_source_quota`,
/// `migration.quiesce_and_stream`) stay registered in
/// [`register`] per the deprecation window so a persisted older
/// DAG can still resolve its actions; note the executor's version
/// table currently holds one version per saga name, so old-version
/// recovery additionally needs that table widened (tracked for the
/// saga lib).
pub const SAGA_VERSION: u32 = 3;

/// Per-action timeout for short store mutations. 30 s catches a
/// wedged FDB write and nothing else.
const ACTION_TIMEOUT: std::time::Duration = ACTION_TIMEOUT_STORE;

/// Per-action timeout for the bulk data-transfer nodes
/// (`initial_zfs_send`, `sync_convergence`, `final_zfs_increment`,
/// `stream_vmm`). A multi-TB dataset over a saturated link is a
/// day-scale transfer; the abort poll in [`await_jobs_terminal`]
/// is the responsive cancel path, not this ceiling.
const ACTION_TIMEOUT_TRANSFER: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

type Ctx = ActionContext<TritondSagaType>;

/// Parameters the handler hands to `SagaExecutor::saga_execute`.
///
/// The migration record is **pre-created** by the handler before
/// the saga starts (so the operator's `POST` immediately surfaces
/// it on `/v1/migrations`). The saga's first action associates
/// the assigned saga_id back onto the record; the actual record
/// lives in FDB the whole time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationSagaParams {
    pub migration_id: Uuid,
    pub instance_id: Uuid,
    pub tenant_id: Uuid,
    pub project_id: Uuid,
    pub source_cn: Uuid,
    /// Operator-supplied target CN hint. When `Some`, the
    /// designate action force-places to that CN (subject to the
    /// `avoid_cn` + the migration filters still rejecting an
    /// incompatible target). When `None`, the designate action
    /// runs the full placement chain.
    #[serde(default)]
    pub target_cn_hint: Option<Uuid>,
    /// `true` for migrations triggered by the (future) rebalance
    /// / evacuate driver, `false` for operator-initiated. Only
    /// used for audit labels + metrics.
    #[serde(default)]
    pub automatic: bool,
    /// `true` for the cold-migrate path: `quiesce_source` stops
    /// the source guest (if running) before the final incremental
    /// send, and no vmm wire handshake happens. `false` runs the
    /// live path (pause + RAM/device-state stream). The handler
    /// forces `cold: true` for non-bhyve brands; only bhyve has a
    /// live lane.
    #[serde(default)]
    pub cold: bool,
    /// Whether the instance was Running when the operator
    /// submitted the migration. Captured by the create handler so
    /// `quiesce_source`'s undo and `activate_target` restore the
    /// guest to its pre-migration power state rather than guessing
    /// from a lifecycle that the saga itself has been mutating.
    #[serde(default)]
    pub was_running: bool,
}

/// Register every catalog action onto the executor's
/// [`ActionRegistry`]. Called by [`crate::sagas::register_all_actions`].
pub fn register(reg: &mut ActionRegistry) {
    reg.register(ActionFunc::new_action(
        "migration.associate_record",
        associate_record,
        associate_record_undo,
    ));
    reg.register(ActionFunc::new_action(
        "migration.designate_target",
        designate_target,
        designate_target_undo,
    ));
    reg.register(ActionFunc::new_action(
        "migration.save_source_quota",
        save_source_quota,
        save_source_quota_undo,
    ));
    reg.register(ActionFunc::new_action(
        "migration.create_target_zone",
        create_target_zone,
        create_target_zone_undo,
    ));
    reg.register(ActionFunc::new_action(
        "migration.reserve_target_nics",
        reserve_target_nics,
        reserve_target_nics_undo,
    ));
    reg.register(ActionFunc::new_action(
        "migration.initial_zfs_send",
        initial_zfs_send,
        no_op_undo,
    ));
    reg.register(ActionFunc::new_action(
        "migration.sync_convergence",
        sync_convergence,
        no_op_undo,
    ));
    reg.register(ActionFunc::new_action(
        "migration.quiesce_source",
        quiesce_source,
        quiesce_source_undo,
    ));
    reg.register(ActionFunc::new_action(
        "migration.final_zfs_increment",
        final_zfs_increment,
        no_op_undo,
    ));
    reg.register(ActionFunc::new_action(
        "migration.mount_target",
        mount_target,
        no_op_undo,
    ));
    reg.register(ActionFunc::new_action(
        "migration.stream_vmm",
        stream_vmm,
        no_op_undo,
    ));
    reg.register(ActionFunc::new_action(
        "migration.switch_ownership",
        switch_ownership,
        switch_ownership_undo,
    ));
    reg.register(ActionFunc::new_action(
        "migration.activate_target",
        activate_target,
        no_op_undo,
    ));
    reg.register(ActionFunc::new_action(
        "migration.cleanup_source",
        cleanup_source,
        no_op_undo,
    ));
    reg.register(ActionFunc::new_action(
        "migration.finish",
        finish,
        no_op_undo,
    ));

    // v1 (N=2 deprecation window): action names that v2 renamed or
    // dropped stay registered so a persisted v1 DAG can still
    // resolve every action on recovery.
    reg.register(ActionFunc::new_action(
        "migration.snapshot_source_quota",
        snapshot_source_quota_v1,
        no_op_undo,
    ));
    reg.register(ActionFunc::new_action(
        "migration.quiesce_and_stream",
        quiesce_and_stream_v1,
        no_op_undo,
    ));
}

/// Build the saga DAG. Linear 15-action chain.
pub fn build_dag(params: &MigrationSagaParams) -> SagaResult<Arc<SagaDag>> {
    let name = SagaName::new(SAGA_NAME);
    let mut b = DagBuilder::new(name);

    // We deliberately spell out every node with the canonical
    // pair of (forward, undo) names so a future grep for
    // `"migration.switch_ownership"` lands on both halves.
    b.append(Node::action(
        "record",
        "associate_record",
        &*ActionFunc::new_action(
            "migration.associate_record",
            associate_record,
            associate_record_undo,
        ),
    ));
    b.append(Node::action(
        "designated_target_cn",
        "designate_target",
        &*ActionFunc::new_action(
            "migration.designate_target",
            designate_target,
            designate_target_undo,
        ),
    ));
    b.append(Node::action(
        "source_quota",
        "save_source_quota",
        &*ActionFunc::new_action(
            "migration.save_source_quota",
            save_source_quota,
            save_source_quota_undo,
        ),
    ));
    b.append(Node::action(
        "target_zone",
        "create_target_zone",
        &*ActionFunc::new_action(
            "migration.create_target_zone",
            create_target_zone,
            create_target_zone_undo,
        ),
    ));
    b.append(Node::action(
        "reserved_nics",
        "reserve_target_nics",
        &*ActionFunc::new_action(
            "migration.reserve_target_nics",
            reserve_target_nics,
            reserve_target_nics_undo,
        ),
    ));
    b.append(Node::action(
        "initial_zfs",
        "initial_zfs_send",
        &*ActionFunc::new_action("migration.initial_zfs_send", initial_zfs_send, no_op_undo),
    ));
    b.append(Node::action(
        "synced",
        "sync_convergence",
        &*ActionFunc::new_action("migration.sync_convergence", sync_convergence, no_op_undo),
    ));
    b.append(Node::action(
        "quiesced",
        "quiesce_source",
        &*ActionFunc::new_action(
            "migration.quiesce_source",
            quiesce_source,
            quiesce_source_undo,
        ),
    ));
    b.append(Node::action(
        "final_zfs",
        "final_zfs_increment",
        &*ActionFunc::new_action(
            "migration.final_zfs_increment",
            final_zfs_increment,
            no_op_undo,
        ),
    ));
    b.append(Node::action(
        "target_mounted",
        "mount_target",
        &*ActionFunc::new_action("migration.mount_target", mount_target, no_op_undo),
    ));
    b.append(Node::action(
        "vmm_streamed",
        "stream_vmm",
        &*ActionFunc::new_action("migration.stream_vmm", stream_vmm, no_op_undo),
    ));
    b.append(Node::action(
        "switched",
        "switch_ownership",
        &*ActionFunc::new_action(
            "migration.switch_ownership",
            switch_ownership,
            switch_ownership_undo,
        ),
    ));
    b.append(Node::action(
        "target_activated",
        "activate_target",
        &*ActionFunc::new_action("migration.activate_target", activate_target, no_op_undo),
    ));
    b.append(Node::action(
        "source_cleaned",
        "cleanup_source",
        &*ActionFunc::new_action("migration.cleanup_source", cleanup_source, no_op_undo),
    ));
    b.append(Node::action(
        "final_record",
        "finish",
        &*ActionFunc::new_action("migration.finish", finish, no_op_undo),
    ));

    let dag = b
        .build()
        .map_err(|e| SagaError::Backend(format!("dag build: {e}")))?;
    let params_json = serde_json::to_value(params)
        .map_err(|e| SagaError::Backend(format!("params serialize: {e}")))?;
    Ok(Arc::new(SagaDag::new(dag, params_json)))
}

/// Resources this saga touches, known at create time. The
/// per-resource saga views surface migration sagas on the
/// migrating instance's page + on the source/target CN pages
/// without scanning the whole catalog.
pub fn build_references(params: &MigrationSagaParams) -> Vec<ResourceRef> {
    let mut out = Vec::new();
    out.push(ResourceRef::new(ResourceScope::Tenant, params.tenant_id));
    out.push(ResourceRef::new(ResourceScope::Project, params.project_id));
    out.push(ResourceRef::new(
        ResourceScope::Instance,
        params.instance_id,
    ));
    out.push(ResourceRef::new(ResourceScope::Cn, params.source_cn));
    if let Some(target) = params.target_cn_hint {
        // Operator force-placed; the chosen CN is known at
        // create-time. When the designate action picks it
        // dynamically (the common path), the per-CN view gets
        // the target only after the action runs — out of scope
        // for the initial reference list.
        out.push(ResourceRef::new(ResourceScope::Cn, target));
    }
    out
}

// ───────────────────────── Actions ─────────────────────────────

async fn associate_record(ctx: Ctx) -> Result<MigrationRecord, ActionError> {
    crate::sagas::with_action_timeout("migration.associate_record", ACTION_TIMEOUT, async move {
        let user_ctx = ctx.user_data();
        fence_check(user_ctx).await?;
        let store = user_ctx.store().clone();
        let params: MigrationSagaParams = ctx.saga_params()?;

        // Read the pre-created record. The handler created it
        // before saga_execute; we stamp the saga_id on it so the
        // /v1/migrations surface can link to the operation.
        let mut record = store
            .get_migration(params.migration_id)
            .await
            .map_err(store_err_to_action_err)?;
        record.saga_id = Some(user_ctx.saga_id().0);
        record.started_at = Some(chrono::Utc::now());
        // The handler created the record in Begin/Begin; we leave
        // that until designate_target picks a target_cn.
        let stored = store
            .put_migration(record)
            .await
            .map_err(store_err_to_action_err)?;
        Ok(stored)
    })
    .await
}

async fn associate_record_undo(ctx: Ctx) -> Result<(), anyhow::Error> {
    let store = ctx.user_data().store().clone();
    let params: MigrationSagaParams = ctx.saga_params()?;
    // Clear the saga_id pointer + transition to a terminal state
    // so the active-migration guard releases. An operator-
    // requested Abort lands as Aborted; everything else as
    // Failed. Tolerant of a missing record (race with an
    // out-of-band cleanup).
    if let Ok(mut r) = store.get_migration(params.migration_id).await {
        r.saga_id = None;
        r.state = if r.action_requested == tritond_store::MigrationAction::Abort {
            MigrationState::Aborted
        } else {
            MigrationState::Failed
        };
        r.finished_at = Some(chrono::Utc::now());
        if r.error.is_none() {
            r.error = Some("saga unwound during associate_record".to_string());
        }
        if let Err(e) = store.put_migration(r).await {
            // Best-effort: a follow-up sweeper task will reap
            // stuck records. Avoid panicking the unwind path.
            tracing::warn!(error = %e, "migration.associate_record undo: put_migration failed");
        }
    }
    Ok(())
}

async fn designate_target(ctx: Ctx) -> Result<Uuid, ActionError> {
    crate::sagas::with_action_timeout("migration.designate_target", ACTION_TIMEOUT, async move {
        let user_ctx = ctx.user_data();
        fence_check(user_ctx).await?;
        let store = user_ctx.store().clone();
        let params: MigrationSagaParams = ctx.saga_params()?;
        let saga_id_uuid: Uuid = user_ctx.saga_id().0;
        let sec_id_uuid = user_ctx.sec_id().0;
        let sec_epoch_u64 = user_ctx.sec_epoch().0;

        // Read the source Instance for its shape + affinity rules
        // so the PlacementRequest reflects the actual workload.
        let instance = store
            .get_instance(params.instance_id)
            .await
            .map_err(store_err_to_action_err)?;

        // Build the compat fingerprint from the SOURCE CN's stored
        // CnCapacity row (published by the agent capability probe,
        // LM-0b) instead of the v1 hard-coded literal. A live
        // migration without probe data cannot pick a compatible
        // target, so it fails here with an actionable error; a
        // cold migration tolerates an absent row (the live-only
        // filters Skip on the empty fields and the ZFS fingerprint
        // check Skips on the empty map).
        let capacity = match store.get_cn_capacity(params.source_cn).await {
            Ok(c) => Some(c),
            Err(tritond_store::StoreError::NotFound) => None,
            Err(e) => return Err(store_err_to_action_err(e)),
        };
        if !params.cold {
            let probe_ok = capacity
                .as_ref()
                .is_some_and(|c| c.vmm_protocol_version.is_some());
            if !probe_ok {
                return Err(ActionError::action_failed(serde_json::json!({
                    "kind": "migration.designate.source_probe_missing",
                    "reason": "live migration requires the source CN's capability probe \
                               (vmm_protocol_version on its cn-capacity row); the agent has \
                               not reported one — retry with cold: true or upgrade the agent",
                    "source_cn": params.source_cn.to_string(),
                })));
            }
        }
        let migration_compat = match capacity {
            Some(c) => tritond_placement::types::MigrationCompat {
                vmm_protocol_version: c.vmm_protocol_version.unwrap_or_default(),
                cpu_features: c.cpu_features,
                tsc_offset_ns: c.tsc_offset_ns.unwrap_or(0),
                zpool_props: c
                    .zpool_props
                    .into_iter()
                    .map(|(pool, props)| {
                        (
                            pool,
                            tritond_placement::types::ZpoolPropFingerprint {
                                encryption: props.encryption,
                                compression: props.compression,
                                recordsize_bytes: props.recordsize_bytes,
                            },
                        )
                    })
                    .collect(),
                source_dataset_encrypted: false,
                cold: params.cold,
            },
            // Cold + no capacity row: empty fingerprint, every
            // migration filter Skips. Reachable only for CNs that
            // never published capacity (which placement would
            // reject as a target anyway).
            None => tritond_placement::types::MigrationCompat {
                vmm_protocol_version: String::new(),
                cpu_features: Vec::new(),
                tsc_offset_ns: 0,
                zpool_props: std::collections::BTreeMap::new(),
                source_dataset_encrypted: false,
                cold: params.cold,
            },
        };
        let request = tritond_placement::PlacementRequest {
            instance_id: params.instance_id,
            silo_uuid: uuid::Uuid::nil(),
            tenant_uuid: params.tenant_id,
            project_uuid: params.project_id,
            role: tritond_placement::types::CnRoleView::Tenant,
            // cpu_units convention: 1 vCPU = 100 cpu_units (DAPI
            // legacy convention picked up by tritond-placement).
            cpu_units: (instance.cpu as u32) * 100,
            ram_mb: (instance.memory_bytes / (1024 * 1024)) as u64,
            disk: std::collections::BTreeMap::new(),
            required_traits: std::collections::BTreeMap::new(),
            required_nic_tags: Vec::new(),
            required_underlay: tritond_placement::types::UnderlayCapability {
                ipv4: true,
                ipv6: false,
            },
            required_devices: Vec::new(),
            needs_hvm: matches!(instance.brand, tritond_store::InstanceBrand::Bhyve),
            min_platform: None,
            affinity: tritond_store::InstanceAffinity::empty(
                params.instance_id,
                params.tenant_id,
                chrono::Utc::now(),
            ),
            strategy_override: None,
            force_cn: params.target_cn_hint,
            ignore_scope_pin: false,
            deadline: chrono::Utc::now() + chrono::Duration::hours(6),
            avoid_cn: vec![params.source_cn],
            migration: Some(migration_compat),
        };

        let chosen = match crate::placement::pick(
            &store,
            request,
            crate::placement::Commit::ReservationOnly {
                saga_id: saga_id_uuid,
                sec_id: sec_id_uuid,
                sec_epoch: sec_epoch_u64,
            },
        )
        .await
        {
            Ok(outcome) => outcome.chosen.ok_or_else(|| {
                ActionError::action_failed(serde_json::json!({
                    "kind": "migration.designate.no_eligible_cn",
                    "reason": "internal: chosen was None on commit-success",
                }))
            })?,
            Err(crate::placement::PickError::NoEligibleCn { report }) => {
                return Err(ActionError::action_failed(serde_json::json!({
                    "kind": "migration.designate.no_eligible_cn",
                    "audit": report.bounded_for_audit(),
                })));
            }
            Err(crate::placement::PickError::Store(e)) => {
                return Err(store_err_to_action_err(e));
            }
        };

        // Persist target_cn on the migration record + advance
        // phase. The Instance pin stays on source_cn — switch
        // happens in `switch_ownership`.
        let mut record = store
            .get_migration(params.migration_id)
            .await
            .map_err(store_err_to_action_err)?;
        record.target_cn = Some(chosen);
        record.phase = MigrationPhase::Sync;
        record.state = MigrationState::Sync;
        store
            .put_migration(record)
            .await
            .map_err(store_err_to_action_err)?;
        Ok(chosen)
    })
    .await
}

async fn designate_target_undo(ctx: Ctx) -> Result<(), anyhow::Error> {
    let user_ctx = ctx.user_data();
    let store = user_ctx.store().clone();
    let params: MigrationSagaParams = ctx.saga_params()?;
    let saga_id_uuid: Uuid = user_ctx.saga_id().0;
    // The forward fn returns the chosen CN; the unwind path
    // looks it up via ctx.lookup against the *node* name. We
    // wired the DAG to expose this output as
    // `designated_target_cn`.
    let cn: Uuid = ctx.lookup("designated_target_cn").unwrap_or(Uuid::nil());
    if cn == Uuid::nil() {
        // do-fn never produced a chosen CN — nothing to release.
        return Ok(());
    }
    // Release only the reservation row. The shared
    // placement::release_reservation helper also clears
    // Instance.host_cn_uuid, which fits the Commit::Yes pins of the
    // create/designate sagas but would clobber the source pin here:
    // ReservationOnly never set the pin, and the instance must stay
    // on source_cn until switch_ownership.
    match store.release_cn_reservation(cn, saga_id_uuid).await {
        Ok(()) | Err(tritond_store::StoreError::NotFound) => {}
        Err(e) => {
            return Err(anyhow::anyhow!(
                "migration.designate undo: release_cn_reservation: {e}"
            ));
        }
    }
    // Also clear target_cn on the record so the audit view
    // shows the migration as un-designated rather than
    // pointing at a CN whose reservation is gone.
    if let Ok(mut r) = store.get_migration(params.migration_id).await {
        r.target_cn = None;
        if let Err(e) = store.put_migration(r).await {
            tracing::warn!(error = %e, "migration.designate undo: clear target_cn failed");
        }
    }
    Ok(())
}

async fn save_source_quota(ctx: Ctx) -> Result<QuotaDanceSaveResult, ActionError> {
    crate::sagas::with_action_timeout(
        "migration.save_source_quota",
        ACTION_TIMEOUT_AWAIT,
        async move {
            let user_ctx = ctx.user_data();
            fence_check(user_ctx).await?;
            let store = user_ctx.store().clone();
            let params: MigrationSagaParams = ctx.saga_params()?;

            // Derive the dataset name from the SmartOS convention:
            // `zones/<instance_uuid>`. The convention is
            // load-bearing on every current SmartOS deploy.
            let dataset = format!("zones/{}", params.instance_id);

            // A dataset with quota/refreservation set can fail at
            // `zfs recv` time, so clear them on the source up
            // front. The agent reports the original values back in
            // the job result; the saga keeps them on the record so
            // `activate_target` (success) or this node's undo
            // (abort) can restore them.
            let job = store
                .enqueue_job(tritond_store::NewJob {
                    kind: tritond_store::JobKind::MigrateQuotaDance {
                        migration_id: params.migration_id,
                        instance_id: params.instance_id,
                        dataset: dataset.clone(),
                        op: tritond_store::QuotaDanceOp::SaveAndClear,
                    },
                    target_cn_uuid: Some(params.source_cn),
                })
                .await
                .map_err(store_err_to_action_err)?;
            let finals = await_jobs_terminal(
                store.clone(),
                Some(params.migration_id),
                &[job.id],
                "migration.save_source_quota",
            )
            .await?;
            let saved: QuotaDanceSaveResult = match finals
                .first()
                .and_then(|j| j.result.clone())
            {
                Some(value) => serde_json::from_value(value).map_err(|e| {
                    ActionError::action_failed(serde_json::json!({
                        "kind": "migration.save_source_quota.bad_result",
                        "reason": format!("agent result did not parse as QuotaDanceSaveResult: {e}"),
                        "job_id": job.id.to_string(),
                    }))
                })?,
                // Agent predates the result field: treat as
                // "nothing was set" — Restore of (None, None) is a
                // no-op, matching v1 behavior.
                None => QuotaDanceSaveResult::default(),
            };

            let mut record = store
                .get_migration(params.migration_id)
                .await
                .map_err(store_err_to_action_err)?;
            record.source_filesystem_details = Some(tritond_store::SourceFilesystemDetails {
                dataset: dataset.clone(),
                original_quota_bytes: saved.quota_bytes,
                original_refreservation_bytes: saved.refreservation_bytes,
                snapshots: Vec::new(),
                encrypted: false,
            });
            store
                .put_migration(record)
                .await
                .map_err(store_err_to_action_err)?;
            tracing::info!(
                migration_id = %params.migration_id,
                %dataset,
                quota_bytes = ?saved.quota_bytes,
                refreservation_bytes = ?saved.refreservation_bytes,
                "migration.save_source_quota: source quota saved and cleared",
            );
            Ok(saved)
        },
    )
    .await
}

async fn save_source_quota_undo(ctx: Ctx) -> Result<(), anyhow::Error> {
    let store = ctx.user_data().store().clone();
    let params: MigrationSagaParams = ctx.saga_params()?;
    // Restore the values the forward action saved. Prefer the node
    // output (durable in the saga log even if the record write
    // raced); fall back to the record's details.
    let saved: Option<QuotaDanceSaveResult> = ctx.lookup("source_quota").ok();
    let (quota_bytes, refreservation_bytes, dataset) = match saved {
        Some(s) => (
            s.quota_bytes,
            s.refreservation_bytes,
            format!("zones/{}", params.instance_id),
        ),
        None => match store
            .get_migration(params.migration_id)
            .await
            .ok()
            .and_then(|r| r.source_filesystem_details)
        {
            Some(d) => (
                d.original_quota_bytes,
                d.original_refreservation_bytes,
                d.dataset,
            ),
            // Forward action never recorded anything — nothing
            // was cleared, nothing to restore.
            None => return Ok(()),
        },
    };
    if quota_bytes.is_none() && refreservation_bytes.is_none() {
        return Ok(());
    }
    // Best-effort: the job sits in the queue until the source
    // agent picks it up; the unwind must not block on it.
    if let Err(e) = store
        .enqueue_job(tritond_store::NewJob {
            kind: tritond_store::JobKind::MigrateQuotaDance {
                migration_id: params.migration_id,
                instance_id: params.instance_id,
                dataset,
                op: tritond_store::QuotaDanceOp::Restore {
                    quota_bytes,
                    refreservation_bytes,
                },
            },
            target_cn_uuid: Some(params.source_cn),
        })
        .await
    {
        tracing::warn!(
            error = %e,
            migration_id = %params.migration_id,
            "migration.save_source_quota undo: enqueue Restore failed; source quota needs manual restore",
        );
    }
    Ok(())
}

async fn create_target_zone(ctx: Ctx) -> Result<ProvisioningJob, ActionError> {
    crate::sagas::with_action_timeout(
        "migration.create_target_zone",
        ACTION_TIMEOUT_AWAIT,
        async move {
            let user_ctx = ctx.user_data();
            fence_check(user_ctx).await?;
            let store = user_ctx.store().clone();
            let params: MigrationSagaParams = ctx.saga_params()?;
            let target_cn: Uuid = ctx.lookup("designated_target_cn")?;

            // Migration-specific provision: `vmadm create` with
            // autoboot=false + no image ensure + Proteus ports
            // paused, then destroy the vmadm-created datasets so
            // the first `zfs recv` lands clean. A plain Provision
            // would boot a duplicate-MAC guest while the source
            // still owns the identity.
            let job = store
                .enqueue_job(tritond_store::NewJob {
                    kind: tritond_store::JobKind::MigrationProvisionTarget {
                        migration_id: params.migration_id,
                        instance_id: params.instance_id,
                    },
                    target_cn_uuid: Some(target_cn),
                })
                .await
                .map_err(store_err_to_action_err)?;
            tracing::info!(
                migration_id = %params.migration_id,
                target_cn = %target_cn,
                job_id = %job.id,
                "migration.create_target_zone: enqueued MigrationProvisionTarget job; awaiting terminal",
            );
            await_jobs_terminal(
                store,
                Some(params.migration_id),
                &[job.id],
                "migration.create_target_zone",
            )
            .await?;
            tracing::info!(
                migration_id = %params.migration_id,
                target_cn = %target_cn,
                job_id = %job.id,
                "migration.create_target_zone: target zone shell created",
            );
            Ok(job)
        },
    )
    .await
}

async fn create_target_zone_undo(ctx: Ctx) -> Result<(), anyhow::Error> {
    let store = ctx.user_data().store().clone();
    let params: MigrationSagaParams = ctx.saga_params()?;
    // Prefer the designate node's output; fall back to the record's
    // persisted target_cn (designate stamped it before this node ran,
    // and designate's own undo, which clears it, runs after this
    // one). The cleanup job MUST be pinned: an unrouted job would be
    // claimed by the unbound stub instead of the target CN's agent.
    let target_cn: Uuid = match ctx.lookup::<Uuid>("designated_target_cn") {
        Ok(cn) if cn != Uuid::nil() => cn,
        _ => match store.get_migration(params.migration_id).await {
            Ok(r) => match r.target_cn {
                Some(cn) => cn,
                None => return Ok(()),
            },
            // Can't resolve a target CN to pin the cleanup to;
            // nothing useful to enqueue.
            Err(_) => return Ok(()),
        },
    };
    // If the ownership CAS already committed, the "target" zone is
    // the canonical instance — destroying it would be data loss.
    // An unwind reaching here post-switch (a store error in the
    // post-switch tail) must leave it alone and let the operator
    // resolve from the record's error field.
    match store.get_instance(params.instance_id).await {
        Ok(i) if i.host_cn_uuid == Some(target_cn) => {
            tracing::warn!(
                migration_id = %params.migration_id,
                "migration.create_target_zone undo: ownership already switched; refusing to clean up the canonical target zone",
            );
            return Ok(());
        }
        Ok(_) => {}
        Err(e) => {
            // Can't prove the switch didn't happen — refuse the
            // destructive cleanup and leave it to the operator.
            tracing::warn!(
                migration_id = %params.migration_id,
                error = %e,
                "migration.create_target_zone undo: instance read failed; skipping target cleanup",
            );
            return Ok(());
        }
    }
    // An ambiguous live-stream failure means the target may be
    // RUNNING the guest (import fence completed, handshake didn't);
    // deleting it could destroy the only live copy. Leave it for
    // the operator alongside the paused source.
    if live_failure_is_ambiguous(&store, params.migration_id).await {
        tracing::warn!(
            migration_id = %params.migration_id,
            "migration.create_target_zone undo: ambiguous live failure; \
             refusing to tear down the possibly-activated target zone",
        );
        return Ok(());
    }
    // Best-effort: enqueue a MigrationCleanupTarget job so the
    // target agent vmadm-deletes the half-started zone. If the
    // target CN is unreachable, the job sits in the queue until
    // the agent comes back (operator-visible via /v1/migrations
    // showing the failed migration).
    if let Err(e) = store
        .enqueue_job(tritond_store::NewJob {
            kind: tritond_store::JobKind::MigrationCleanupTarget {
                migration_id: params.migration_id,
                instance_id: params.instance_id,
            },
            target_cn_uuid: Some(target_cn),
        })
        .await
    {
        tracing::warn!(
            error = %e,
            migration_id = %params.migration_id,
            "migration.create_target_zone undo: enqueue MigrationCleanupTarget failed",
        );
    }
    Ok(())
}

async fn reserve_target_nics(ctx: Ctx) -> Result<(), ActionError> {
    crate::sagas::with_action_timeout(
        "migration.reserve_target_nics",
        ACTION_TIMEOUT,
        async move {
            let user_ctx = ctx.user_data();
            fence_check(user_ctx).await?;
            let store = user_ctx.store().clone();
            let params: MigrationSagaParams = ctx.saga_params()?;

            // Read the source instance's NICs and copy their
            // uuids onto the migration record. The MACs travel
            // with the VM (per-VM in FDB, not per-CN), so
            // reservation here is bookkeeping for the
            // operator-visible record + the switch action's
            // sanity check; the actual Proteus
            // `create_port(paused)` lands when the agent learns
            // to dispatch ProteusActivate / ProteusDeactivate
            // jobs (LM-7's cutover fence).
            let nics = store
                .list_nics_for_instance(params.instance_id)
                .await
                .map_err(store_err_to_action_err)?;
            let nic_ids: Vec<Uuid> = nics.iter().map(|n| n.id).collect();
            let mut record = store
                .get_migration(params.migration_id)
                .await
                .map_err(store_err_to_action_err)?;
            record.reserved_nics = nic_ids.clone();
            store
                .put_migration(record)
                .await
                .map_err(store_err_to_action_err)?;
            tracing::info!(
                migration_id = %params.migration_id,
                nic_count = nic_ids.len(),
                "migration.reserve_target_nics: recorded NICs; Proteus pre-bind pending LM-7",
            );
            Ok(())
        },
    )
    .await
}

async fn reserve_target_nics_undo(_ctx: Ctx) -> Result<(), anyhow::Error> {
    Ok(())
}

async fn initial_zfs_send(ctx: Ctx) -> Result<(), ActionError> {
    crate::sagas::with_action_timeout(
        "migration.initial_zfs_send",
        ACTION_TIMEOUT_TRANSFER,
        async move {
            let user_ctx = ctx.user_data();
            fence_check(user_ctx).await?;
            let store = user_ctx.store().clone();
            let params: MigrationSagaParams = ctx.saga_params()?;
            let target_cn: Uuid = ctx.lookup("designated_target_cn")?;

            let dataset = format!("zones/{}", params.instance_id);
            let base_snap = format!("{dataset}@migration-base");

            enqueue_zfs_pair(
                &store,
                &params,
                target_cn,
                &dataset,
                None,
                &base_snap,
                "migration.initial_zfs_send",
            )
            .await?;
            tracing::info!(
                migration_id = %params.migration_id,
                "migration.initial_zfs_send: base send completed",
            );
            Ok(())
        },
    )
    .await
}

/// Saga-side convergence loop: incremental `MigrateZfsSend` pairs
/// (`@migration-sync-N`) until a round streams fewer bytes than
/// `migration.sync_delta_threshold_bytes` or
/// `migration.max_sync_rounds` is hit. Returns the name of the
/// last snapshot that exists on both sides — the incremental base
/// for `final_zfs_increment`.
async fn sync_convergence(ctx: Ctx) -> Result<String, ActionError> {
    crate::sagas::with_action_timeout(
        "migration.sync_convergence",
        ACTION_TIMEOUT_TRANSFER,
        async move {
            let user_ctx = ctx.user_data();
            fence_check(user_ctx).await?;
            let store = user_ctx.store().clone();
            let params: MigrationSagaParams = ctx.saga_params()?;
            let target_cn: Uuid = ctx.lookup("designated_target_cn")?;

            // Read live so an operator can tune the knobs while a
            // migration is in flight.
            let settings = store.get_settings().await.map_err(store_err_to_action_err)?;
            let threshold = settings.migration_sync_delta_threshold_bytes;
            let max_rounds = settings.migration_max_sync_rounds.max(1);

            let dataset = format!("zones/{}", params.instance_id);
            let mut prev_snap = format!("{dataset}@migration-base");
            for round in 1..=max_rounds {
                let to_snap = format!("{dataset}@migration-sync-{round}");
                let finals = enqueue_zfs_pair(
                    &store,
                    &params,
                    target_cn,
                    &dataset,
                    Some(prev_snap.clone()),
                    &to_snap,
                    "migration.sync_convergence",
                )
                .await?;
                prev_snap = to_snap;
                // The source side reports bytes_streamed; a
                // missing/garbled result can't prove convergence,
                // so it counts as "keep syncing" and the round cap
                // bounds the loop.
                let bytes_streamed = finals
                    .iter()
                    .find_map(|j| {
                        matches!(
                            j.kind,
                            tritond_store::JobKind::MigrateZfsSend {
                                role: tritond_store::MigrationJobRole::Source,
                                ..
                            }
                        )
                        .then(|| j.result.clone())
                        .flatten()
                    })
                    .and_then(|v| serde_json::from_value::<ZfsSendResult>(v).ok())
                    .map(|r| r.bytes_streamed);
                tracing::info!(
                    migration_id = %params.migration_id,
                    round,
                    bytes_streamed = ?bytes_streamed,
                    threshold,
                    "migration.sync_convergence: round complete",
                );
                append_sync_round_progress(
                    store.as_ref(),
                    params.migration_id,
                    round,
                    bytes_streamed,
                )
                .await;
                match bytes_streamed {
                    Some(b) if b < threshold => break,
                    Some(_) => {}
                    None => {
                        tracing::warn!(
                            migration_id = %params.migration_id,
                            round,
                            "migration.sync_convergence: source job reported no bytes_streamed; cannot prove convergence",
                        );
                    }
                }
            }
            Ok(prev_snap)
        },
    )
    .await
}

/// Per-round timeline entry for the operator progress log (LM-3).
/// Saga-side observability only: an append failure is logged and
/// dropped, never allowed to unwind a migration whose data plane
/// is healthy.
async fn append_sync_round_progress(
    store: &dyn tritond_store::Store,
    migration_id: Uuid,
    round: u64,
    bytes_streamed: Option<u64>,
) {
    let event = tritond_store::MigrationProgressEvent {
        // The store CAS-assigns the real seq on append.
        seq: 0,
        kind: "phase_transition".to_string(),
        phase: Some(MigrationPhase::Sync),
        state: Some(MigrationState::Sync),
        percentage: None,
        transferred_bytes: bytes_streamed,
        total_bytes: None,
        eta_ms: None,
        message: Some(format!("sync round {round} complete")),
        error: None,
        timestamp: chrono::Utc::now(),
    };
    if let Err(e) = store.append_migration_progress(migration_id, event).await {
        tracing::warn!(
            %migration_id,
            round,
            error = %e,
            "migration.sync_convergence: progress append failed; continuing",
        );
    }
}

/// Quiesce the source guest before the final incremental send —
/// the v1 data-loss fix. Cold: stop the guest via a `Stop` job
/// (skipped if it is already stopped). Live: pause it via
/// `MigratePauseSource`, leaving the guest frozen for the RAM
/// stream. Either way, every byte the guest will ever write on the
/// source exists before `final_zfs_increment` snapshots.
async fn quiesce_source(ctx: Ctx) -> Result<(), ActionError> {
    crate::sagas::with_action_timeout(
        "migration.quiesce_source",
        ACTION_TIMEOUT_AWAIT,
        async move {
            let user_ctx = ctx.user_data();
            fence_check(user_ctx).await?;
            let store = user_ctx.store().clone();
            let params: MigrationSagaParams = ctx.saga_params()?;

            // The cutover window opens here: the guest is about to
            // stop serving on the source.
            let mut record = store
                .get_migration(params.migration_id)
                .await
                .map_err(store_err_to_action_err)?;
            record.phase = MigrationPhase::Switch;
            record.state = MigrationState::Switch;
            store
                .put_migration(record)
                .await
                .map_err(store_err_to_action_err)?;

            if params.cold {
                let instance = store
                    .get_instance(params.instance_id)
                    .await
                    .map_err(store_err_to_action_err)?;
                if instance.lifecycle.kind() == LifecycleStateKind::Stopped {
                    tracing::info!(
                        migration_id = %params.migration_id,
                        "migration.quiesce_source: source already stopped; nothing to quiesce",
                    );
                    return Ok(());
                }
                // Mirror the stop handler's transitional CAS so the
                // agent's job-complete drive (Stopping → Stopped)
                // lands. A CAS conflict means the agent moved the
                // state first — vmadm stop is idempotent, proceed.
                if let Err(e) = store
                    .transition_instance_lifecycle(
                        params.instance_id,
                        &[LifecycleStateKind::Running],
                        LifecycleState::Stopping,
                    )
                    .await
                {
                    tracing::info!(
                        migration_id = %params.migration_id,
                        error = %e,
                        "migration.quiesce_source: Running → Stopping CAS skipped",
                    );
                }
                let job = store
                    .enqueue_job(tritond_store::NewJob {
                        kind: tritond_store::JobKind::Stop {
                            instance_id: params.instance_id,
                        },
                        target_cn_uuid: Some(params.source_cn),
                    })
                    .await
                    .map_err(store_err_to_action_err)?;
                tracing::info!(
                    migration_id = %params.migration_id,
                    job_id = %job.id,
                    "migration.quiesce_source: enqueued Stop on source; awaiting terminal",
                );
                await_jobs_terminal(
                    store,
                    Some(params.migration_id),
                    &[job.id],
                    "migration.quiesce_source",
                )
                .await?;
                return Ok(());
            }

            // Live lane: pause-first (pause-devices → pause-vm →
            // drain-devices). The guest stays paused until the
            // target's SwitchComplete fence or this node's undo.
            let job = store
                .enqueue_job(tritond_store::NewJob {
                    kind: tritond_store::JobKind::MigratePauseSource {
                        migration_id: params.migration_id,
                        instance_id: params.instance_id,
                    },
                    target_cn_uuid: Some(params.source_cn),
                })
                .await
                .map_err(store_err_to_action_err)?;
            tracing::info!(
                migration_id = %params.migration_id,
                job_id = %job.id,
                "migration.quiesce_source: enqueued MigratePauseSource; awaiting terminal",
            );
            await_jobs_terminal(
                store,
                Some(params.migration_id),
                &[job.id],
                "migration.quiesce_source",
            )
            .await?;
            Ok(())
        },
    )
    .await
}

async fn quiesce_source_undo(ctx: Ctx) -> Result<(), anyhow::Error> {
    let store = ctx.user_data().store().clone();
    let params: MigrationSagaParams = ctx.saga_params()?;
    if params.cold {
        // Only restart what the saga itself stopped: an instance
        // that was already stopped at submission stays stopped.
        if !params.was_running {
            return Ok(());
        }
        match store.get_instance(params.instance_id).await {
            // The guest never actually stopped (failure landed
            // before the Stop job ran) — nothing to undo.
            Ok(i) if i.lifecycle.kind() == LifecycleStateKind::Running => return Ok(()),
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(
                    migration_id = %params.migration_id,
                    error = %e,
                    "migration.quiesce_source undo: instance read failed; skipping restart",
                );
                return Ok(());
            }
        }
        // Mirror the start handler's transitional CAS (the agent's
        // Start completion drives Pending → Running). Best-effort.
        if let Err(e) = store
            .transition_instance_lifecycle(
                params.instance_id,
                &[LifecycleStateKind::Stopped, LifecycleStateKind::Stopping],
                LifecycleState::Pending,
            )
            .await
        {
            tracing::info!(
                migration_id = %params.migration_id,
                error = %e,
                "migration.quiesce_source undo: lifecycle CAS to Pending skipped",
            );
        }
        if let Err(e) = store
            .enqueue_job(tritond_store::NewJob {
                kind: tritond_store::JobKind::Start {
                    instance_id: params.instance_id,
                },
                target_cn_uuid: Some(params.source_cn),
            })
            .await
        {
            tracing::warn!(
                migration_id = %params.migration_id,
                error = %e,
                "migration.quiesce_source undo: enqueue Start failed; source guest needs manual start",
            );
        }
        return Ok(());
    }
    // Live lane: resume the paused guest, UNLESS the stream
    // failure landed inside the Finish window (the target may have
    // imported and resumed the guest already; resuming the source
    // too would split-brain the identity). stream_vmm stamps the
    // record with the ambiguous marker before unwinding.
    if live_failure_is_ambiguous(&store, params.migration_id).await {
        tracing::warn!(
            migration_id = %params.migration_id,
            "migration.quiesce_source undo: ambiguous live failure; \
             leaving source guest paused for the operator",
        );
        return Ok(());
    }
    if let Err(e) = store
        .enqueue_job(tritond_store::NewJob {
            kind: tritond_store::JobKind::MigrateResumeSource {
                migration_id: params.migration_id,
                instance_id: params.instance_id,
            },
            target_cn_uuid: Some(params.source_cn),
        })
        .await
    {
        tracing::warn!(
            migration_id = %params.migration_id,
            error = %e,
            "migration.quiesce_source undo: enqueue MigrateResumeSource failed; source guest left paused",
        );
    }
    Ok(())
}

async fn final_zfs_increment(ctx: Ctx) -> Result<(), ActionError> {
    crate::sagas::with_action_timeout(
        "migration.final_zfs_increment",
        ACTION_TIMEOUT_TRANSFER,
        async move {
            let user_ctx = ctx.user_data();
            fence_check(user_ctx).await?;
            let store = user_ctx.store().clone();
            let params: MigrationSagaParams = ctx.saga_params()?;
            let target_cn: Uuid = ctx.lookup("designated_target_cn")?;
            // Incremental base = the last sync-round snapshot, not
            // @migration-base: the target's newest snapshot is the
            // last sync round's, and an incremental stream must
            // start from the receiver's newest snapshot.
            let from_snap: String = ctx.lookup("synced")?;

            let dataset = format!("zones/{}", params.instance_id);
            let final_snap = format!("{dataset}@migration-final");

            // Runs strictly after quiesce_source: the guest is
            // stopped (cold) or paused (live), so this snapshot is
            // the last byte the source will ever write.
            enqueue_zfs_pair(
                &store,
                &params,
                target_cn,
                &dataset,
                Some(from_snap),
                &final_snap,
                "migration.final_zfs_increment",
            )
            .await?;
            tracing::info!(
                migration_id = %params.migration_id,
                "migration.final_zfs_increment: final increment completed",
            );
            Ok(())
        },
    )
    .await
}

/// Mount the received zoneroot tree on the target after the final
/// `zfs recv`. The receives ran `-u` (no automount) so the datasets
/// land unmounted; both the cold activation (`vmadm start`) and the
/// live listen-mode boot (`MigrateTargetListen`) write `/startvm`
/// into the zoneroot and fail if it is not mounted. Runs strictly
/// after `final_zfs_increment` (the last receive) and before
/// `stream_vmm`/`switch_ownership`, so a mount failure unwinds while
/// the source is still canonical. Abort is still honored. Applies to
/// both lanes.
async fn mount_target(ctx: Ctx) -> Result<(), ActionError> {
    crate::sagas::with_action_timeout("migration.mount_target", ACTION_TIMEOUT_AWAIT, async move {
        let user_ctx = ctx.user_data();
        fence_check(user_ctx).await?;
        let store = user_ctx.store().clone();
        let params: MigrationSagaParams = ctx.saga_params()?;
        let target_cn: Uuid = ctx.lookup("designated_target_cn")?;

        let job = store
            .enqueue_job(tritond_store::NewJob {
                kind: tritond_store::JobKind::MigrateMountTarget {
                    migration_id: params.migration_id,
                    instance_id: params.instance_id,
                },
                target_cn_uuid: Some(target_cn),
            })
            .await
            .map_err(store_err_to_action_err)?;
        tracing::info!(
            migration_id = %params.migration_id,
            target_cn = %target_cn,
            job_id = %job.id,
            "migration.mount_target: enqueued MigrateMountTarget; awaiting terminal",
        );
        await_jobs_terminal(
            store.clone(),
            Some(params.migration_id),
            &[job.id],
            "migration.mount_target",
        )
        .await?;
        Ok(())
    })
    .await
}

/// Live lane: stream RAM + device state from the paused source to
/// the listening target. Cold lane: nothing to stream; the
/// dataset transferred in the ZFS nodes is canonical once the
/// guest is stopped.
///
/// Sequence: boot the target zone in bhyve listen mode
/// (`MigrateTargetListen`, awaited terminal) and only then hand the
/// source agent the dial bundle (`MigrateVmmStream { Source }`), so
/// the dial always finds a registered listener.
///
/// Failure policy (the job's `result.last_phase`, reported by the
/// source agent, encodes the distinction):
/// * the source never entered the wire's Finish phase: the target
///   cannot have imported state, so failing the action is safe: the
///   unwind resumes the paused source (`quiesce_source` undo) and
///   tears down the target (`create_target_zone` undo);
/// * the source entered Finish (or the result is missing /
///   unparseable): the target holds complete device state and may
///   already be running the guest. Auto-resume could split-brain, so
///   the record gets a structured ambiguous-failure marker that the
///   undo chain reads to leave BOTH sides alone for the operator.
async fn stream_vmm(ctx: Ctx) -> Result<(), ActionError> {
    crate::sagas::with_action_timeout(
        "migration.stream_vmm",
        ACTION_TIMEOUT_TRANSFER,
        async move {
            let user_ctx = ctx.user_data();
            fence_check(user_ctx).await?;
            let store = user_ctx.store().clone();
            let params: MigrationSagaParams = ctx.saga_params()?;

            if params.cold {
                tracing::info!(
                    migration_id = %params.migration_id,
                    "migration.stream_vmm: cold lane, nothing to stream",
                );
                return Ok(());
            }
            let target_cn: Uuid = ctx.lookup("designated_target_cn")?;

            // Boot the target zone in listen mode. Abort is still
            // honored here: the source is paused but nothing
            // irreversible has happened (quiesce undo resumes it).
            let listen_job = store
                .enqueue_job(tritond_store::NewJob {
                    kind: tritond_store::JobKind::MigrateTargetListen {
                        migration_id: params.migration_id,
                        instance_id: params.instance_id,
                    },
                    target_cn_uuid: Some(target_cn),
                })
                .await
                .map_err(store_err_to_action_err)?;
            tracing::info!(
                migration_id = %params.migration_id,
                target_cn = %target_cn,
                job_id = %listen_job.id,
                "migration.stream_vmm: enqueued MigrateTargetListen; awaiting terminal",
            );
            await_jobs_terminal(
                store.clone(),
                Some(params.migration_id),
                &[listen_job.id],
                "migration.stream_vmm",
            )
            .await?;

            // Fresh Outbound ticket per attempt so the 10-min TTL
            // covers the stream's handshake regardless of how long
            // the ZFS legs took.
            let peer = resolve_migrate_peer(
                &store,
                target_cn,
                params.source_cn,
                params.instance_id,
                params.migration_id,
                tritond_auth::MigrateRole::Outbound,
            )
            .await?;
            let stream_job = store
                .enqueue_job(tritond_store::NewJob {
                    kind: tritond_store::JobKind::MigrateVmmStream {
                        migration_id: params.migration_id,
                        instance_id: params.instance_id,
                        role: tritond_store::MigrationJobRole::Source,
                        peer_endpoint: Some(peer.endpoint),
                        peer_spki_sha256_hex: Some(peer.spki_hex),
                        ticket: Some(peer.ticket),
                    },
                    target_cn_uuid: Some(params.source_cn),
                })
                .await
                .map_err(store_err_to_action_err)?;
            tracing::info!(
                migration_id = %params.migration_id,
                job_id = %stream_job.id,
                "migration.stream_vmm: enqueued source MigrateVmmStream; awaiting terminal",
            );

            // Deliberately NO abort poll for the stream itself: the
            // agents have no cancel channel, so an unwind racing the
            // target's import fence could resume the source while
            // the target activates (split brain). An abort requested
            // mid-stream is simply too late.
            let final_row = await_job_final_row(&store, stream_job.id).await?;
            match final_row.status.kind() {
                tritond_store::JobStatusKind::Completed => {
                    tracing::info!(
                        migration_id = %params.migration_id,
                        result = ?final_row.result,
                        "migration.stream_vmm: live stream complete; target activated",
                    );
                    Ok(())
                }
                _ => {
                    let reason = match &final_row.status {
                        tritond_store::JobStatus::Failed { reason } => reason.clone(),
                        other => format!("unexpected terminal status {other:?}"),
                    };
                    let last_phase = final_row
                        .result
                        .as_ref()
                        .and_then(|v| v.get("last_phase"))
                        .and_then(|v| v.as_str())
                        .map(str::to_string);
                    if live_failure_is_pre_finish(last_phase.as_deref()) {
                        return Err(ActionError::action_failed(serde_json::json!({
                            "kind": "migration.stream_vmm.failed",
                            "reason": reason,
                            "last_phase": last_phase,
                        })));
                    }
                    mark_live_failure_ambiguous(
                        &store,
                        &params,
                        target_cn,
                        last_phase.as_deref(),
                        &reason,
                    )
                    .await;
                    Err(ActionError::action_failed(serde_json::json!({
                        "kind": "migration.stream_vmm.ambiguous_failure",
                        "reason": reason,
                        "last_phase": last_phase,
                        "policy": "source left paused, target zone left in place; operator must resolve",
                    })))
                }
            }
        },
    )
    .await
}

async fn switch_ownership(ctx: Ctx) -> Result<(), ActionError> {
    crate::sagas::with_action_timeout("migration.switch_ownership", ACTION_TIMEOUT, async move {
        let user_ctx = ctx.user_data();
        fence_check(user_ctx).await?;
        let store = user_ctx.store().clone();
        let params: MigrationSagaParams = ctx.saga_params()?;

        // **POINT OF NO RETURN.** Read the chosen target_cn off
        // the migration record (designate_target wrote it).
        let record = store
            .get_migration(params.migration_id)
            .await
            .map_err(store_err_to_action_err)?;
        let target_cn = record.target_cn.ok_or_else(|| {
            ActionError::action_failed(serde_json::json!({
                "kind": "migration.switch.no_target_cn",
                "reason": "migration record missing target_cn — designate_target must have failed silently",
            }))
        })?;

        // Atomic CAS of Instance.host_cn_uuid from source_cn to
        // target_cn. On the live lane the guest is already running
        // on the target (stream_vmm's SwitchComplete fence); this
        // write makes the control plane agree. On the cold lane the
        // guest is stopped (quiesce_source) so there is nothing to
        // fence and the FDB write is the cutover.
        let instance = store
            .get_instance(params.instance_id)
            .await
            .map_err(store_err_to_action_err)?;
        if instance.host_cn_uuid != Some(params.source_cn) {
            // Either someone moved the instance out from under
            // us, or recovery is re-running switch on an
            // already-flipped instance. The second case is fine
            // (idempotent); the first is a programming error we
            // surface so the audit log catches it.
            if instance.host_cn_uuid == Some(target_cn) {
                tracing::info!(
                    migration_id = %params.migration_id,
                    "migration.switch_ownership: idempotent re-entry (host_cn already target)",
                );
                return Ok(());
            }
            return Err(ActionError::action_failed(serde_json::json!({
                "kind": "migration.switch.host_cn_mismatch",
                "reason": "Instance.host_cn_uuid was neither source nor target",
                "expected_source": params.source_cn.to_string(),
                "expected_target": target_cn.to_string(),
                "observed": instance.host_cn_uuid.map(|u| u.to_string()),
            })));
        }
        let _updated = store
            .set_instance_host_cn(params.instance_id, Some(target_cn))
            .await
            .map_err(store_err_to_action_err)?;
        tracing::info!(
            migration_id = %params.migration_id,
            from = %params.source_cn,
            to = %target_cn,
            "migration.switch_ownership: cutover committed",
        );
        Ok(())
    })
    .await
}

async fn switch_ownership_undo(_ctx: Ctx) -> Result<(), anyhow::Error> {
    // After this node's forward action commits, the target is
    // canonical and there is no clean undo — recovery is via
    // the explicit `migrate-rollback` sub-saga which schedules a
    // new migration with source/target swapped. We log + return
    // Ok so the unwind tail completes and the operator's audit
    // view shows "rolled past switch; rollback required".
    tracing::warn!(
        "migration.switch_ownership undo invoked: cutover already committed, requires explicit rollback saga",
    );
    Ok(())
}

/// Bring the migrated instance up on the target: power it on (cold,
/// iff it was running at submission) and restore the saved
/// quota/refreservation onto the target dataset. Live targets are
/// already running (the RAM stream resumed them), so only the
/// quota restore applies.
///
/// Runs AFTER switch_ownership, so failures here must NOT unwind
/// the saga — the unwind tail would tear down the now-canonical
/// target zone. Failures land on the record's `error` field for
/// the operator instead.
async fn activate_target(ctx: Ctx) -> Result<(), ActionError> {
    crate::sagas::with_action_timeout(
        "migration.activate_target",
        ACTION_TIMEOUT_AWAIT,
        async move {
            let user_ctx = ctx.user_data();
            fence_check(user_ctx).await?;
            let store = user_ctx.store().clone();
            let params: MigrationSagaParams = ctx.saga_params()?;
            let target_cn: Uuid = ctx.lookup("designated_target_cn")?;
            let mut problems: Vec<String> = Vec::new();

            if params.cold && params.was_running {
                // Mirror the start handler's transitional CAS (the
                // agent's Start completion drives Pending →
                // Running). Best-effort.
                if let Err(e) = store
                    .transition_instance_lifecycle(
                        params.instance_id,
                        &[LifecycleStateKind::Stopped, LifecycleStateKind::Stopping],
                        LifecycleState::Pending,
                    )
                    .await
                {
                    tracing::info!(
                        migration_id = %params.migration_id,
                        error = %e,
                        "migration.activate_target: lifecycle CAS to Pending skipped",
                    );
                }
                match store
                    .enqueue_job(tritond_store::NewJob {
                        kind: tritond_store::JobKind::Start {
                            instance_id: params.instance_id,
                        },
                        target_cn_uuid: Some(target_cn),
                    })
                    .await
                {
                    Ok(job) => {
                        // No abort poll: post-switch, abort is
                        // meaningless and must not skip activation.
                        if let Err(e) = await_jobs_terminal(
                            store.clone(),
                            None,
                            &[job.id],
                            "migration.activate_target",
                        )
                        .await
                        {
                            problems.push(format!("target Start job failed: {e:?}"));
                        }
                    }
                    Err(e) => problems.push(format!("enqueue target Start failed: {e}")),
                }
            }

            // Quota restore on the target dataset (recv ran with
            // `-x quota -x refreservation`, so the properties did
            // not travel with the stream).
            let details = store
                .get_migration(params.migration_id)
                .await
                .map_err(store_err_to_action_err)?
                .source_filesystem_details;
            if let Some(d) = details
                && (d.original_quota_bytes.is_some() || d.original_refreservation_bytes.is_some())
            {
                match store
                    .enqueue_job(tritond_store::NewJob {
                        kind: tritond_store::JobKind::MigrateQuotaDance {
                            migration_id: params.migration_id,
                            instance_id: params.instance_id,
                            dataset: d.dataset,
                            op: tritond_store::QuotaDanceOp::Restore {
                                quota_bytes: d.original_quota_bytes,
                                refreservation_bytes: d.original_refreservation_bytes,
                            },
                        },
                        target_cn_uuid: Some(target_cn),
                    })
                    .await
                {
                    Ok(job) => {
                        if let Err(e) = await_jobs_terminal(
                            store.clone(),
                            None,
                            &[job.id],
                            "migration.activate_target",
                        )
                        .await
                        {
                            problems.push(format!("target quota restore failed: {e:?}"));
                        }
                    }
                    Err(e) => problems.push(format!("enqueue target quota restore failed: {e}")),
                }
            }

            if !problems.is_empty() {
                let summary = problems.join("; ");
                tracing::warn!(
                    migration_id = %params.migration_id,
                    %summary,
                    "migration.activate_target: post-switch activation problems; operator action required",
                );
                if let Ok(mut record) = store.get_migration(params.migration_id).await {
                    record.error = Some(format!("activate_target: {summary}"));
                    if let Err(e) = store.put_migration(record).await {
                        tracing::warn!(
                            migration_id = %params.migration_id,
                            error = %e,
                            "migration.activate_target: recording problems on the record failed",
                        );
                    }
                }
            }
            Ok(())
        },
    )
    .await
}

async fn cleanup_source(ctx: Ctx) -> Result<(), ActionError> {
    crate::sagas::with_action_timeout("migration.cleanup_source", ACTION_TIMEOUT, async move {
        let user_ctx = ctx.user_data();
        fence_check(user_ctx).await?;
        let store = user_ctx.store().clone();
        let params: MigrationSagaParams = ctx.saga_params()?;

        // Best-effort: enqueue the source-side cleanup job
        // (vmadm delete on the source + zfs destroy of the
        // migration snapshots + release of source-side NIC
        // bindings). This runs AFTER switch_ownership has
        // committed — the target is canonical at this point,
        // so a failed cleanup is an operator alert (stranded
        // dataset on the source CN), not a saga failure.
        // The saga returns Ok regardless; the job sits in
        // the queue for the source agent whenever it's
        // available.
        match store
            .enqueue_job(tritond_store::NewJob {
                kind: tritond_store::JobKind::MigrationCleanupSource {
                    migration_id: params.migration_id,
                    instance_id: params.instance_id,
                },
                target_cn_uuid: Some(params.source_cn),
            })
            .await
        {
            Ok(job) => {
                tracing::info!(
                    migration_id = %params.migration_id,
                    job_id = %job.id,
                    "migration.cleanup_source: enqueued source teardown job",
                );
            }
            Err(e) => {
                // Don't fail the saga — cutover already
                // committed. Operator-visible via the
                // migration record's error field + audit log.
                tracing::warn!(
                    migration_id = %params.migration_id,
                    error = %e,
                    "migration.cleanup_source: enqueue failed; source needs manual cleanup",
                );
            }
        }
        Ok(())
    })
    .await
}

async fn finish(ctx: Ctx) -> Result<MigrationRecord, ActionError> {
    crate::sagas::with_action_timeout("migration.finish", ACTION_TIMEOUT, async move {
        let user_ctx = ctx.user_data();
        fence_check(user_ctx).await?;
        let store = user_ctx.store().clone();
        let params: MigrationSagaParams = ctx.saga_params()?;
        let mut record = store
            .get_migration(params.migration_id)
            .await
            .map_err(store_err_to_action_err)?;
        record.state = MigrationState::Successful;
        record.finished_at = Some(chrono::Utc::now());
        let stored = store
            .put_migration(record)
            .await
            .map_err(store_err_to_action_err)?;
        Ok(stored)
    })
    .await
}

// ───────────────────── v1 recovery stubs ───────────────────────
//
// v2 renamed `snapshot_source_quota` → `save_source_quota` and
// `quiesce_and_stream` → `stream_vmm`. The old names stay
// registered (N=2 deprecation window) so a v1 DAG recovered after
// a deploy can still resolve its actions; the bodies are inert
// no-ops because a recovered v1 saga is only ever driven to
// unwind, not forward.

async fn snapshot_source_quota_v1(_ctx: Ctx) -> Result<(), ActionError> {
    Err(ActionError::action_failed(serde_json::json!({
        "kind": "migration.v1_action_deprecated",
        "action": "migration.snapshot_source_quota",
        "reason": "migrate-instance v1 is deprecated; the recovered saga unwinds",
    })))
}

async fn quiesce_and_stream_v1(_ctx: Ctx) -> Result<(), ActionError> {
    Err(ActionError::action_failed(serde_json::json!({
        "kind": "migration.v1_action_deprecated",
        "action": "migration.quiesce_and_stream",
        "reason": "migrate-instance v1 is deprecated; the recovered saga unwinds",
    })))
}

// ───────────────────────── Helpers ─────────────────────────────

/// Marker prefix on `MigrationRecord.error` for live-stream
/// failures inside the Finish/SwitchComplete window. The undo
/// chain ([`quiesce_source_undo`], [`create_target_zone_undo`])
/// reads it to skip the source resume and the target teardown,
/// the one non-self-healing state, left for the operator.
const LIVE_AMBIGUOUS_ERROR_PREFIX: &str = "live migration ambiguous failure";

/// Wire-phase labels the source agent reports in the
/// `MigrateVmmStream` job result (`last_phase`); mirrors
/// `tritonagent::migrate_vmm::phase_label`. Only phases that provably
/// precede the device-state bytes leaving the source are safe to
/// resume: the target runs its irreversible cutover (import-state +
/// resume) the instant it has consumed those bytes, and the source
/// stamps "device_state" *before* the send with no ack before
/// "finish", so a delivered-then-errored send (or source death in the
/// gap) can leave last_phase=="device_state" after the target is
/// already live. "device_state"/"finish"/"complete"/missing/garbage
/// are therefore all treated as ambiguous: fail closed on the side
/// that cannot split-brain.
fn live_failure_is_pre_finish(last_phase: Option<&str>) -> bool {
    matches!(
        last_phase,
        Some("sync" | "pause" | "ram_push" | "ram_hash" | "time_data")
    )
}

/// Whether the record carries the ambiguous-failure marker.
async fn live_failure_is_ambiguous(
    store: &Arc<dyn tritond_store::Store>,
    migration_id: Uuid,
) -> bool {
    store
        .get_migration(migration_id)
        .await
        .ok()
        .and_then(|r| r.error)
        .is_some_and(|e| e.starts_with(LIVE_AMBIGUOUS_ERROR_PREFIX))
}

/// Stamp the ambiguous-failure marker + the operator-facing detail
/// onto the record BEFORE the action error unwinds, so the undo
/// chain (which runs next) sees it. Best-effort: if this write
/// fails the unwind degrades to the resume+cleanup path, which is
/// the pre-existing behavior.
async fn mark_live_failure_ambiguous(
    store: &Arc<dyn tritond_store::Store>,
    params: &MigrationSagaParams,
    target_cn: Uuid,
    last_phase: Option<&str>,
    reason: &str,
) {
    let detail = format!(
        "{LIVE_AMBIGUOUS_ERROR_PREFIX}: source reported last phase {last_phase:?} ({reason}); \
         source guest left PAUSED on CN {source}; target zone left in place on CN {target}; \
         verify which side holds the guest, then resume one side and delete the other",
        source = params.source_cn,
        target = target_cn,
    );
    match store.get_migration(params.migration_id).await {
        Ok(mut record) => {
            record.error = Some(detail);
            if let Err(e) = store.put_migration(record).await {
                tracing::warn!(
                    migration_id = %params.migration_id,
                    error = %e,
                    "migration.stream_vmm: writing ambiguous-failure marker failed; \
                     unwind will fall back to resume+cleanup",
                );
            }
        }
        Err(e) => {
            tracing::warn!(
                migration_id = %params.migration_id,
                error = %e,
                "migration.stream_vmm: record read for ambiguous marker failed",
            );
        }
    }
}

/// Source-side migrate-job peer info bundle: the wss base URL, the
/// pinned TLS SPKI, and the freshly-minted migrate ticket the
/// source agent needs to dial the target's listener. Built once per
/// transfer leg (each ZFS pass and the vmm stream mint a fresh
/// ticket so the 10-min TTL covers each leg independently) and
/// embedded into the Source job's payload.
struct MigratePeerInfo {
    endpoint: String,
    spki_hex: String,
    ticket: String,
}

/// Default agent migrate-listener port. Mirrors
/// `tritonagent::migrate::DEFAULT_MIGRATE_LISTEN_PORT = 4568`
/// (plan §D.3). A future slice persists the per-CN port on the
/// `Cn` record (analogous to `console_listen_port`) for
/// dynamically-bound deployments; for LM-6 we rely on the
/// invariant that every agent runs with the documented default.
const DEFAULT_AGENT_MIGRATE_PORT: u16 = 4568;

/// Resolve the target CN's reachable migrate endpoint + SPKI pin
/// + a freshly-minted migrate ticket in the requested `role`
/// ([`tritond_auth::MigrateRole::ZfsSource`] for the dataset legs,
/// [`tritond_auth::MigrateRole::Outbound`] for the live RAM
/// stream). The ticket is bound to
/// (source_cn, target_cn, instance_id, migration_id, role) using
/// the **target** CN's migrate-ticket key; the target's listener
/// verifies against its own key, so the source has to mint with the
/// target's key. Reading that key out of FDB here is safe: the
/// saga process is the tritond process and holds the same trust
/// as the CN approval path that wrote the key.
async fn resolve_migrate_peer(
    store: &std::sync::Arc<dyn tritond_store::Store>,
    target_cn: Uuid,
    source_cn: Uuid,
    instance_id: Uuid,
    migration_id: Uuid,
    role: tritond_auth::MigrateRole,
) -> Result<MigratePeerInfo, ActionError> {
    let cn = store
        .get_cn(target_cn)
        .await
        .map_err(store_err_to_action_err)?;
    let admin_ip = cn.admin_ip.ok_or_else(|| {
        ActionError::action_failed(serde_json::json!({
            "kind": "migration.peer.target_cn_no_admin_ip",
            "target_cn": target_cn.to_string(),
        }))
    })?;
    let migrate_ticket_key_bytes = cn.migrate_ticket_key.ok_or_else(|| {
        ActionError::action_failed(serde_json::json!({
            "kind": "migration.peer.target_cn_no_migrate_ticket_key",
            "reason": "target CN registered before LM-6c — re-approve to mint a key",
            "target_cn": target_cn.to_string(),
        }))
    })?;
    let spki_bytes = cn.console_tls_spki_sha256.ok_or_else(|| {
        ActionError::action_failed(serde_json::json!({
            "kind": "migration.peer.target_cn_no_spki",
            "reason": "target CN never reported a TLS leaf cert SPKI",
            "target_cn": target_cn.to_string(),
        }))
    })?;
    let ticket = tritond_auth::MigrateTicketKey::from_bytes(migrate_ticket_key_bytes)
        .mint(
            source_cn,
            target_cn,
            instance_id,
            migration_id,
            role,
            tritond_auth::DEFAULT_MIGRATE_TICKET_TTL_SECS,
        )
        .map_err(|e| {
            ActionError::action_failed(serde_json::json!({
                "kind": "migration.peer.ticket_mint_failed",
                "error": e.to_string(),
            }))
        })?;
    Ok(MigratePeerInfo {
        endpoint: format!("wss://{admin_ip}:{DEFAULT_AGENT_MIGRATE_PORT}"),
        spki_hex: hex::encode(spki_bytes),
        ticket,
    })
}

/// Enqueue one paired Source/Target `MigrateZfsSend` pass and
/// await both terminals. Shared by `initial_zfs_send`,
/// `sync_convergence` rounds, and `final_zfs_increment` — the
/// three differ only in their snapshot pair. Returns the final
/// job rows so callers can read agent-reported results
/// ([`ZfsSendResult`]).
async fn enqueue_zfs_pair(
    store: &Arc<dyn tritond_store::Store>,
    params: &MigrationSagaParams,
    target_cn: Uuid,
    dataset: &str,
    from_snap: Option<String>,
    to_snap: &str,
    action_name: &'static str,
) -> Result<Vec<ProvisioningJob>, ActionError> {
    // Mint a fresh ZfsSource ticket + read the target's admin IP
    // + SPKI pin so the source agent can dial the target's
    // listener directly. The peer info rides on the Source job's
    // payload.
    let peer = resolve_migrate_peer(
        store,
        target_cn,
        params.source_cn,
        params.instance_id,
        params.migration_id,
        tritond_auth::MigrateRole::ZfsSource,
    )
    .await?;

    // Paired enqueue: Source CN runs `zfs send`, Target CN runs
    // `zfs receive`, both connected via the per-CN migrate
    // WebSocket transport (see `services/tritonagent/src/migrate.rs`).
    // Both jobs are awaited together — the source finishes when
    // its `zfs send` stream completes, the target finishes when
    // `zfs recv` exits 0 against the received stream. A failure
    // on either side fails the saga node + unwinds.
    let src_job = store
        .enqueue_job(tritond_store::NewJob {
            kind: tritond_store::JobKind::MigrateZfsSend {
                migration_id: params.migration_id,
                instance_id: params.instance_id,
                role: tritond_store::MigrationJobRole::Source,
                dataset: dataset.to_string(),
                from_snap: from_snap.clone(),
                to_snap: to_snap.to_string(),
                peer_endpoint: Some(peer.endpoint),
                peer_spki_sha256_hex: Some(peer.spki_hex),
                ticket: Some(peer.ticket),
            },
            target_cn_uuid: Some(params.source_cn),
        })
        .await
        .map_err(store_err_to_action_err)?;
    let dst_job = store
        .enqueue_job(tritond_store::NewJob {
            kind: tritond_store::JobKind::MigrateZfsSend {
                migration_id: params.migration_id,
                instance_id: params.instance_id,
                role: tritond_store::MigrationJobRole::Target,
                dataset: dataset.to_string(),
                from_snap,
                to_snap: to_snap.to_string(),
                peer_endpoint: None,
                peer_spki_sha256_hex: None,
                ticket: None,
            },
            target_cn_uuid: Some(target_cn),
        })
        .await
        .map_err(store_err_to_action_err)?;
    tracing::info!(
        migration_id = %params.migration_id,
        src_job_id = %src_job.id,
        dst_job_id = %dst_job.id,
        %to_snap,
        action = action_name,
        "migration: enqueued source+target ZFS jobs; awaiting both",
    );
    await_jobs_terminal(
        store.clone(),
        Some(params.migration_id),
        &[src_job.id, dst_job.id],
        action_name,
    )
    .await
}

/// Poll a set of `ProvisioningJob` ids until every one reaches a
/// terminal status, returning the final rows (callers read
/// agent-reported `result` payloads off them). Returns `Err` on
/// the first job that lands `Failed`.
///
/// When `migration_id` is `Some`, every tick also re-reads the
/// migration record and converts an operator-requested `Abort`
/// into an `ActionError` so steno unwinds — steno has no external
/// cancel, so this poll is the abort path for the long-running
/// transfer nodes. Pass `None` for post-switch awaits where abort
/// no longer means anything.
/// Poll one job to a terminal state and return the final row even
/// when it Failed; the vmm-stream node applies its own failure
/// policy from the row's `result` instead of the blanket
/// fail-the-action conversion in [`await_jobs_terminal`]. No abort
/// poll on purpose (see the `stream_vmm` comment).
async fn await_job_final_row(
    store: &Arc<dyn tritond_store::Store>,
    job_id: Uuid,
) -> Result<ProvisioningJob, ActionError> {
    use std::time::Duration;
    use tritond_store::JobStatusKind;
    const POLL: Duration = Duration::from_millis(100);
    loop {
        let current = store
            .get_job(job_id)
            .await
            .map_err(store_err_to_action_err)?;
        match current.status.kind() {
            JobStatusKind::Completed | JobStatusKind::Failed => return Ok(current),
            _ => {}
        }
        tokio::time::sleep(POLL).await;
    }
}

async fn await_jobs_terminal(
    store: Arc<dyn tritond_store::Store>,
    migration_id: Option<Uuid>,
    job_ids: &[Uuid],
    action_name: &'static str,
) -> Result<Vec<ProvisioningJob>, ActionError> {
    use std::time::Duration;
    use tritond_store::JobStatusKind;
    const POLL: Duration = Duration::from_millis(100);
    let mut finals: std::collections::HashMap<Uuid, ProvisioningJob> =
        std::collections::HashMap::with_capacity(job_ids.len());
    loop {
        if let Some(mid) = migration_id {
            let record = store
                .get_migration(mid)
                .await
                .map_err(store_err_to_action_err)?;
            if record.action_requested == tritond_store::MigrationAction::Abort {
                return Err(ActionError::action_failed(serde_json::json!({
                    "kind": "migration.aborted",
                    "action": action_name,
                    "reason": "operator requested abort",
                })));
            }
        }
        for id in job_ids {
            if finals.contains_key(id) {
                continue;
            }
            let current = store.get_job(*id).await.map_err(store_err_to_action_err)?;
            match current.status.kind() {
                JobStatusKind::Completed => {
                    finals.insert(*id, current);
                }
                JobStatusKind::Failed => {
                    return Err(ActionError::action_failed(serde_json::json!({
                        "kind": "migration.job_failed",
                        "action": action_name,
                        "job_id": id.to_string(),
                        "reason": match &current.status {
                            tritond_store::JobStatus::Failed { reason } => reason.clone(),
                            _ => "(no reason)".to_string(),
                        },
                    })));
                }
                _ => {}
            }
        }
        if finals.len() == job_ids.len() {
            // Preserve caller order.
            return Ok(job_ids.iter().filter_map(|id| finals.remove(id)).collect());
        }
        tokio::time::sleep(POLL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tritond_store::{JobKind, JobOutcome, MemStore, QuotaDanceOp};

    fn test_params() -> MigrationSagaParams {
        MigrationSagaParams {
            migration_id: Uuid::new_v4(),
            instance_id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            project_id: Uuid::new_v4(),
            source_cn: Uuid::new_v4(),
            target_cn_hint: None,
            automatic: false,
            cold: true,
            was_running: true,
        }
    }

    #[test]
    fn dag_builds_without_error() {
        // Steno's `SagaDag` doesn't expose its internal node
        // graph publicly, so we can't directly count action
        // nodes here. The build call itself succeeding is the
        // contract we care about — a missing dependency between
        // two `append` calls would surface as a `dag.build()`
        // error inside `build_dag`.
        let dag = build_dag(&test_params()).expect("build_dag");
        // Round-trip through serde to confirm the saga record
        // is persistable (recovery needs this).
        let json = serde_json::to_value(dag.as_ref()).expect("serialize dag");
        assert!(json.is_object());
    }

    /// The serialized DAG must contain the full node sequence, in
    /// order. Walking the serde view is the only way to assert
    /// shape without steno exposing its graph.
    #[test]
    fn dag_has_v2_action_sequence() {
        let dag = build_dag(&test_params()).expect("build_dag");
        let json = serde_json::to_value(dag.as_ref()).expect("serialize dag");
        let text = json.to_string();
        let expected_in_order = [
            "migration.associate_record",
            "migration.designate_target",
            "migration.save_source_quota",
            "migration.create_target_zone",
            "migration.reserve_target_nics",
            "migration.initial_zfs_send",
            "migration.sync_convergence",
            "migration.quiesce_source",
            "migration.final_zfs_increment",
            "migration.mount_target",
            "migration.stream_vmm",
            "migration.switch_ownership",
            "migration.activate_target",
            "migration.cleanup_source",
            "migration.finish",
        ];
        let mut cursor = 0usize;
        for action in expected_in_order {
            let found = text[cursor..].find(action).unwrap_or_else(|| {
                panic!("action {action} missing (or out of order) in serialized DAG")
            });
            cursor += found + action.len();
        }
        // v1-only action names must NOT appear in a v2 DAG.
        assert!(!text.contains("migration.snapshot_source_quota"));
        assert!(!text.contains("migration.quiesce_and_stream"));
    }

    #[test]
    fn build_references_includes_instance_and_source_cn() {
        let params = MigrationSagaParams {
            target_cn_hint: Some(Uuid::new_v4()),
            ..test_params()
        };
        let refs = build_references(&params);
        // 5 resources: tenant, project, instance, source_cn,
        // target_cn (because target_cn_hint is Some).
        assert_eq!(refs.len(), 5);
        assert!(
            refs.iter()
                .any(|r| r.scope == ResourceScope::Instance && r.id == params.instance_id)
        );
        assert!(
            refs.iter()
                .any(|r| r.scope == ResourceScope::Cn && r.id == params.source_cn)
        );
    }

    #[test]
    fn build_references_drops_target_cn_when_no_hint() {
        let refs = build_references(&test_params());
        // 4 resources without the target hint: tenant, project,
        // instance, source_cn.
        assert_eq!(refs.len(), 4);
    }

    #[test]
    fn params_round_trip_through_json() {
        let params = MigrationSagaParams {
            target_cn_hint: Some(Uuid::new_v4()),
            automatic: true,
            ..test_params()
        };
        let json = serde_json::to_value(&params).unwrap();
        let back: MigrationSagaParams = serde_json::from_value(json).unwrap();
        assert_eq!(back.migration_id, params.migration_id);
        assert_eq!(back.instance_id, params.instance_id);
        assert_eq!(back.target_cn_hint, params.target_cn_hint);
        assert_eq!(back.automatic, params.automatic);
        assert_eq!(back.cold, params.cold);
        assert_eq!(back.was_running, params.was_running);
    }

    /// v1 params (no `was_running`) must still deserialize — the
    /// field defaults false. Guards saga-recovery across the v1→v2
    /// deploy boundary.
    #[test]
    fn params_v1_shape_decodes_with_default_was_running() {
        let v1 = serde_json::json!({
            "migration_id": Uuid::new_v4(),
            "instance_id": Uuid::new_v4(),
            "tenant_id": Uuid::new_v4(),
            "project_id": Uuid::new_v4(),
            "source_cn": Uuid::new_v4(),
            "cold": true,
        });
        let back: MigrationSagaParams = serde_json::from_value(v1).unwrap();
        assert!(!back.was_running);
        assert!(back.cold);
    }

    /// Smoke test: a fresh MigrationRecord can be created and
    /// read back via the Store. Exercises the contract the saga
    /// relies on (the saga's actions all do `get_migration` →
    /// mutate → `put_migration` cycles).
    #[tokio::test]
    async fn store_supports_migration_record_round_trip() {
        let store: Arc<dyn tritond_store::Store> = Arc::new(MemStore::new());
        let inst = Uuid::new_v4();
        let req = tritond_store::NewMigration {
            instance_id: inst,
            tenant_id: Uuid::new_v4(),
            project_id: Uuid::new_v4(),
            source_cn: Uuid::new_v4(),
            action_requested: tritond_store::MigrationAction::Begin,
            automatic: false,
        };
        let record = store.create_migration(req).await.expect("create_migration");
        assert_eq!(record.instance_id, inst);
        assert_eq!(record.state, MigrationState::Begin);
        let read = store.get_migration(record.id).await.expect("get_migration");
        assert_eq!(read.id, record.id);
    }

    /// Each convergence round appends one phase_transition event
    /// carrying the round's byte count, and a missing job result
    /// still produces a (byte-less) timeline entry.
    #[tokio::test]
    async fn sync_round_progress_appends_timeline_events() {
        let store: Arc<dyn tritond_store::Store> = Arc::new(MemStore::new());
        let record = store
            .create_migration(tritond_store::NewMigration {
                instance_id: Uuid::new_v4(),
                tenant_id: Uuid::new_v4(),
                project_id: Uuid::new_v4(),
                source_cn: Uuid::new_v4(),
                action_requested: tritond_store::MigrationAction::Begin,
                automatic: false,
            })
            .await
            .expect("create_migration");

        append_sync_round_progress(store.as_ref(), record.id, 1, Some(42 << 20)).await;
        append_sync_round_progress(store.as_ref(), record.id, 2, None).await;

        let events = store
            .list_migration_progress(record.id, 0, 10)
            .await
            .expect("list_migration_progress");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].kind, "phase_transition");
        assert_eq!(events[0].phase, Some(MigrationPhase::Sync));
        assert_eq!(events[0].state, Some(MigrationState::Sync));
        assert_eq!(events[0].transferred_bytes, Some(42 << 20));
        assert_eq!(events[0].message.as_deref(), Some("sync round 1 complete"));
        assert_eq!(events[1].transferred_bytes, None);
        assert_eq!(events[1].message.as_deref(), Some("sync round 2 complete"));
        // Seqs are CAS-assigned and strictly increasing.
        assert!(events[0].seq < events[1].seq);

        // An unknown migration id must not panic or error the saga
        // path (it only warns).
        append_sync_round_progress(store.as_ref(), Uuid::new_v4(), 1, Some(1)).await;
    }

    /// Terminal put_migration releases the active guard, so a
    /// follow-up migration of the same instance is accepted.
    #[tokio::test]
    async fn terminal_record_releases_active_guard() {
        let store: Arc<dyn tritond_store::Store> = Arc::new(MemStore::new());
        let inst = Uuid::new_v4();
        let req = tritond_store::NewMigration {
            instance_id: inst,
            tenant_id: Uuid::new_v4(),
            project_id: Uuid::new_v4(),
            source_cn: Uuid::new_v4(),
            action_requested: tritond_store::MigrationAction::Begin,
            automatic: false,
        };
        let mut record = store
            .create_migration(req.clone())
            .await
            .expect("create_migration");
        assert!(
            store
                .get_active_migration(inst)
                .await
                .expect("get_active_migration")
                .is_some()
        );
        // Second migration while active: conflict.
        assert!(matches!(
            store.create_migration(req.clone()).await,
            Err(tritond_store::StoreError::Conflict(_))
        ));
        record.state = MigrationState::Failed;
        store.put_migration(record).await.expect("put_migration");
        assert!(
            store
                .get_active_migration(inst)
                .await
                .expect("get_active_migration")
                .is_none()
        );
        store
            .create_migration(req)
            .await
            .expect("fresh migration after terminal release");
    }

    /// The abort poll: a record whose `action_requested` flips to
    /// Abort fails the await with the structured abort error even
    /// though the watched job never completes.
    #[tokio::test]
    async fn await_jobs_terminal_converts_abort_to_error() {
        let store: Arc<dyn tritond_store::Store> = Arc::new(MemStore::new());
        let inst = Uuid::new_v4();
        let mut record = store
            .create_migration(tritond_store::NewMigration {
                instance_id: inst,
                tenant_id: Uuid::new_v4(),
                project_id: Uuid::new_v4(),
                source_cn: Uuid::new_v4(),
                action_requested: tritond_store::MigrationAction::Begin,
                automatic: false,
            })
            .await
            .unwrap();
        let job = store
            .enqueue_job(tritond_store::NewJob {
                kind: JobKind::Stop { instance_id: inst },
                target_cn_uuid: None,
            })
            .await
            .unwrap();
        record.action_requested = tritond_store::MigrationAction::Abort;
        store.put_migration(record.clone()).await.unwrap();

        let err = await_jobs_terminal(store, Some(record.id), &[job.id], "test.await")
            .await
            .expect_err("abort must fail the await");
        let ActionError::ActionFailed { source_error } = err else {
            panic!("expected ActionFailed, got {err:?}");
        };
        assert_eq!(
            source_error.get("kind").and_then(|k| k.as_str()),
            Some("migration.aborted")
        );
    }

    /// await_jobs_terminal returns the final rows (with agent
    /// results) in caller order.
    #[tokio::test]
    async fn await_jobs_terminal_returns_final_rows_in_order() {
        let store: Arc<dyn tritond_store::Store> = Arc::new(MemStore::new());
        let inst = Uuid::new_v4();
        let a = store
            .enqueue_job(tritond_store::NewJob {
                kind: JobKind::Stop { instance_id: inst },
                target_cn_uuid: None,
            })
            .await
            .unwrap();
        let b = store
            .enqueue_job(tritond_store::NewJob {
                kind: JobKind::Start { instance_id: inst },
                target_cn_uuid: None,
            })
            .await
            .unwrap();
        // Complete out of order, with a result on `b`.
        store
            .complete_job(
                b.id,
                JobOutcome::Completed,
                Some(serde_json::json!({"bytes_streamed": 7})),
            )
            .await
            .unwrap();
        store
            .complete_job(a.id, JobOutcome::Completed, None)
            .await
            .unwrap();
        let finals = await_jobs_terminal(store, None, &[a.id, b.id], "test.await")
            .await
            .expect("both completed");
        assert_eq!(finals.len(), 2);
        assert_eq!(finals[0].id, a.id);
        assert_eq!(finals[1].id, b.id);
        let parsed: ZfsSendResult =
            serde_json::from_value(finals[1].result.clone().unwrap()).unwrap();
        assert_eq!(parsed.bytes_streamed, 7);
    }

    /// The live failure policy's phase classifier: only phases that
    /// provably precede the device-state send are safe to unwind
    /// (resume source + clean target). "device_state" onward, and any
    /// missing/garbled report, is ambiguous and fails closed because
    /// the target may already have imported and resumed.
    /// Labels mirror `tritonagent::migrate_vmm::phase_label`.
    #[test]
    fn live_failure_phase_classification() {
        for safe in ["sync", "pause", "ram_push", "ram_hash", "time_data"] {
            assert!(
                live_failure_is_pre_finish(Some(safe)),
                "{safe} must be safe"
            );
        }
        for ambiguous in [
            Some("device_state"),
            Some("finish"),
            Some("complete"),
            Some("not-a-phase"),
            None,
        ] {
            assert!(
                !live_failure_is_pre_finish(ambiguous),
                "{ambiguous:?} must be ambiguous",
            );
        }
    }

    /// The ambiguous marker round-trips through the record and is
    /// what the undo guards key off.
    #[tokio::test]
    async fn ambiguous_marker_round_trips_through_record() {
        let store: Arc<dyn tritond_store::Store> = Arc::new(MemStore::new());
        let record = store
            .create_migration(tritond_store::NewMigration {
                instance_id: Uuid::new_v4(),
                tenant_id: Uuid::new_v4(),
                project_id: Uuid::new_v4(),
                source_cn: Uuid::new_v4(),
                action_requested: tritond_store::MigrationAction::Begin,
                automatic: false,
            })
            .await
            .unwrap();
        assert!(!live_failure_is_ambiguous(&store, record.id).await);
        let params = MigrationSagaParams {
            migration_id: record.id,
            instance_id: record.instance_id,
            tenant_id: record.tenant_id,
            project_id: record.project_id,
            source_cn: record.source_cn,
            target_cn_hint: None,
            automatic: false,
            cold: false,
            was_running: true,
        };
        mark_live_failure_ambiguous(&store, &params, Uuid::new_v4(), Some("finish"), "boom").await;
        assert!(live_failure_is_ambiguous(&store, record.id).await);
        let error = store
            .get_migration(record.id)
            .await
            .unwrap()
            .error
            .expect("error stamped");
        assert!(error.starts_with(LIVE_AMBIGUOUS_ERROR_PREFIX));
        assert!(error.contains(&record.source_cn.to_string()));
    }

    /// QuotaDanceOp wire shapes the saga enqueues are the ones the
    /// agent will match on.
    #[test]
    fn quota_dance_restore_carries_optional_values() {
        let op = QuotaDanceOp::Restore {
            quota_bytes: Some(1),
            refreservation_bytes: None,
        };
        let json = serde_json::to_value(&op).unwrap();
        assert_eq!(json["kind"].as_str(), Some("restore"));
        assert_eq!(json["quota_bytes"].as_u64(), Some(1));
    }
}
