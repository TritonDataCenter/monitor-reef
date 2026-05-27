// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `migrate-instance` saga. Forward actions are no-op state-machine
//! transitions on the `MigrationRecord`; the unwind tail is wired
//! correctly so it survives the day real send/recv bodies land.
//! `switch_ownership` is the atomic FDB CAS — the point of no return.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tritond_saga::{
    ActionContext, ActionError, ActionFunc, ActionRegistry, DagBuilder, Node, ResourceRef,
    ResourceScope, SagaDag, SagaError, SagaName, SagaResult, TritondSagaType,
};
use tritond_store::{MigrationPhase, MigrationRecord, MigrationState};
use uuid::Uuid;

use crate::sagas::common::{
    ACTION_TIMEOUT_AWAIT, ACTION_TIMEOUT_STORE, fence_check, no_op_undo, store_err_to_action_err,
};

pub const SAGA_NAME: &str = "migrate-instance";

/// Steno saga version. Bump on any change to the action sequence,
/// action ids, or [`MigrationSagaParams`] shape. Registry keeps
/// the previous N=2 versions registered so a rolling deploy + a
/// crash recovery against the prior version both still work.
pub const SAGA_VERSION: u32 = 1;

/// Per-action timeout for the LM-5 skeleton. Every body is a
/// short store mutation; 30 s catches a wedged FDB write and
/// nothing else. LM-6 / LM-7 will introduce longer-timeout
/// constants for the actions that wait on real ZFS / RAM
/// transfers.
const ACTION_TIMEOUT: std::time::Duration = ACTION_TIMEOUT_STORE;

type Ctx = ActionContext<TritondSagaType>;

/// Parameters the handler hands to `SagaExecutor::saga_execute`.
///
/// The migration record is **pre-created** by the handler before
/// the saga starts (so the operator's `POST` immediately surfaces
/// it on `/v2/migrations`). The saga's first action associates
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
    /// `true` for the cold-migrate path: the VM is stopped before
    /// the migration begins (or the saga stops it as part of
    /// `quiesce_and_stream`), so node 8's live-memory transfer is
    /// skipped — the dataset on the target is canonical after the
    /// incremental ZFS send. `false` runs the live path (lands
    /// with LM-7). LM-6c only ships the cold path; passing
    /// `cold: false` falls through to a placeholder no-op until
    /// then.
    #[serde(default)]
    pub cold: bool,
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
        "migration.snapshot_source_quota",
        snapshot_source_quota,
        snapshot_source_quota_undo,
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
        "migration.final_zfs_increment",
        final_zfs_increment,
        no_op_undo,
    ));
    reg.register(ActionFunc::new_action(
        "migration.quiesce_and_stream",
        quiesce_and_stream,
        quiesce_and_stream_undo,
    ));
    reg.register(ActionFunc::new_action(
        "migration.switch_ownership",
        switch_ownership,
        switch_ownership_undo,
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
}

/// Build the saga DAG. Linear 11-action chain.
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
        "snapshot_source_quota",
        &*ActionFunc::new_action(
            "migration.snapshot_source_quota",
            snapshot_source_quota,
            snapshot_source_quota_undo,
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
        "final_zfs",
        "final_zfs_increment",
        &*ActionFunc::new_action(
            "migration.final_zfs_increment",
            final_zfs_increment,
            no_op_undo,
        ),
    ));
    b.append(Node::action(
        "post_quiesce",
        "quiesce_and_stream",
        &*ActionFunc::new_action(
            "migration.quiesce_and_stream",
            quiesce_and_stream,
            quiesce_and_stream_undo,
        ),
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
        // /v2/migrations surface can link to the operation.
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
    // Clear the saga_id pointer + transition to Failed terminal
    // so the active-migration guard releases. Tolerant of a
    // missing record (race with an out-of-band cleanup).
    if let Ok(mut r) = store.get_migration(params.migration_id).await {
        r.saga_id = None;
        r.state = MigrationState::Failed;
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

        // Build the placement request. The migration filters
        // (cn-not-in-avoid-list + the four migration compat
        // filters) engage because `avoid_cn` is non-empty and
        // `migration` is `Some(_)`. The compat fingerprint is
        // empty for LM-6 — the agent capability probe (LM-0
        // task #13b) will populate the source CN's
        // `CapacityView::{vmm_protocol_version, cpu_features,
        // tsc_offset_ns, zpool_props}`, at which point the
        // migration filters start gating real candidates. Until
        // then they `Skip` on missing fields.
        let migration_compat = tritond_placement::types::MigrationCompat {
            // PROTOCOL_V0 from tritond-vmm-migrate. tritond
            // doesn't link the data-plane crate (the agents
            // do), so we hard-code the wire string here. A
            // mismatch with tritond_vmm_migrate::PROTOCOL_V0
            // would surface as the `cn-bhyve-compatible` filter
            // rejecting every CN, which a smoke test would
            // catch immediately.
            vmm_protocol_version: "vmm-migrate-ron/0".to_string(),
            cpu_features: Vec::new(),
            tsc_offset_ns: 0,
            zpool_props: std::collections::BTreeMap::new(),
            source_dataset_encrypted: false,
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
    crate::placement::release_reservation(&store, cn, saga_id_uuid, params.instance_id)
        .await
        .map_err(|e| anyhow::anyhow!("migration.designate undo: release_reservation: {e}"))?;
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

async fn snapshot_source_quota(ctx: Ctx) -> Result<(), ActionError> {
    crate::sagas::with_action_timeout(
        "migration.snapshot_source_quota",
        ACTION_TIMEOUT,
        async move {
            let user_ctx = ctx.user_data();
            fence_check(user_ctx).await?;
            let store = user_ctx.store().clone();
            let params: MigrationSagaParams = ctx.saga_params()?;

            // Derive the dataset name from the SmartOS
            // convention: `zones/<instance_uuid>`. A future
            // slice replaces this with the actual dataset path
            // read off the source agent (the agent knows from
            // its blueprint cache); the convention is
            // load-bearing on every current SmartOS deploy so
            // hard-coding it here is fine for LM-6b.
            let dataset = format!("zones/{}", params.instance_id);

            // LM-6b: persist a `SourceFilesystemDetails` with
            // the dataset name + empty quota/refres values. The
            // agent-side query that returns actual saved quotas
            // (via `zfs::save_quotas` from LM-4) lands when
            // LM-3b wires the agent's migrate-job dispatcher;
            // until then we record the dataset so the saga's
            // abort + finalize paths know which dataset to
            // address.
            let details = tritond_store::SourceFilesystemDetails {
                dataset: dataset.clone(),
                original_quota_bytes: None,
                original_refreservation_bytes: None,
                snapshots: Vec::new(),
                encrypted: false,
            };
            let mut record = store
                .get_migration(params.migration_id)
                .await
                .map_err(store_err_to_action_err)?;
            record.source_filesystem_details = Some(details);
            store
                .put_migration(record)
                .await
                .map_err(store_err_to_action_err)?;
            tracing::info!(
                migration_id = %params.migration_id,
                %dataset,
                "migration.snapshot_source_quota: recorded dataset; quota probe pending LM-3b",
            );
            Ok(())
        },
    )
    .await
}

async fn snapshot_source_quota_undo(_ctx: Ctx) -> Result<(), anyhow::Error> {
    Ok(())
}

async fn create_target_zone(ctx: Ctx) -> Result<tritond_store::ProvisioningJob, ActionError> {
    crate::sagas::with_action_timeout(
        "migration.create_target_zone",
        ACTION_TIMEOUT_AWAIT,
        async move {
            let user_ctx = ctx.user_data();
            fence_check(user_ctx).await?;
            let store = user_ctx.store().clone();
            let params: MigrationSagaParams = ctx.saga_params()?;
            let target_cn: Uuid = ctx.lookup("designated_target_cn")?;

            // Enqueue a Provision job on the target CN. The
            // agent's existing Provision handler vmadm-creates
            // the zone shell. LM-7 will swap in a migration
            // blueprint variant that suppresses guest boot (the
            // target zone waits for the memory stream before it
            // runs vCPUs); LM-6c cold-migrate gets a stopped
            // zone the cutover then "activates" via the
            // Instance.host_cn_uuid flip + a follow-up start.
            let job = store
                .enqueue_job(tritond_store::NewJob {
                    kind: tritond_store::JobKind::Provision {
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
                "migration.create_target_zone: enqueued Provision job; awaiting terminal",
            );
            await_jobs_terminal(store, &[job.id], "migration.create_target_zone").await?;
            tracing::info!(
                migration_id = %params.migration_id,
                target_cn = %target_cn,
                job_id = %job.id,
                "migration.create_target_zone: Provision job completed",
            );
            Ok(job)
        },
    )
    .await
}

async fn create_target_zone_undo(ctx: Ctx) -> Result<(), anyhow::Error> {
    let store = ctx.user_data().store().clone();
    let params: MigrationSagaParams = ctx.saga_params()?;
    let target_cn: Uuid = ctx.lookup("designated_target_cn").unwrap_or(Uuid::nil());
    if target_cn == Uuid::nil() {
        return Ok(());
    }
    // Best-effort: enqueue a MigrationCleanupTarget job so the
    // target agent vmadm-deletes the half-started zone. If the
    // target CN is unreachable, the job sits in the queue until
    // the agent comes back (operator-visible via /v2/migrations
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
            // with the VM in vnext (per-VM in FDB, not per-CN),
            // so reservation here is bookkeeping for the
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
        ACTION_TIMEOUT_AWAIT,
        async move {
            let user_ctx = ctx.user_data();
            fence_check(user_ctx).await?;
            let store = user_ctx.store().clone();
            let params: MigrationSagaParams = ctx.saga_params()?;
            let target_cn: Uuid = ctx.lookup("designated_target_cn")?;

            let dataset = format!("zones/{}", params.instance_id);
            let base_snap = format!("{dataset}@migration-base");

            // Mint a fresh ZfsSource ticket + read the target's
            // admin IP + SPKI pin so the source agent can dial
            // the target's listener directly. The peer info
            // rides on the Source job's payload.
            let peer = resolve_zfs_peer(
                &store,
                target_cn,
                params.source_cn,
                params.instance_id,
                params.migration_id,
            )
            .await?;

            // Paired enqueue: Source CN runs `zfs send`, Target
            // CN runs `zfs receive`, both connected via the
            // per-CN migrate WebSocket transport (see
            // `services/tritonagent/src/migrate.rs`). Both jobs
            // are awaited together — the source finishes when
            // its `zfs send` stream completes, the target
            // finishes when `zfs recv` exits 0 against the
            // received stream. A failure on either side fails
            // the saga node + unwinds.
            let src_job = store
                .enqueue_job(tritond_store::NewJob {
                    kind: tritond_store::JobKind::MigrateZfsSend {
                        migration_id: params.migration_id,
                        instance_id: params.instance_id,
                        role: tritond_store::MigrationJobRole::Source,
                        dataset: dataset.clone(),
                        from_snap: None,
                        to_snap: base_snap.clone(),
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
                        dataset,
                        from_snap: None,
                        to_snap: base_snap,
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
                "migration.initial_zfs_send: enqueued source+target ZFS jobs; awaiting both",
            );
            await_jobs_terminal(
                store,
                &[src_job.id, dst_job.id],
                "migration.initial_zfs_send",
            )
            .await?;
            tracing::info!(
                migration_id = %params.migration_id,
                "migration.initial_zfs_send: both ZFS jobs completed",
            );
            Ok(())
        },
    )
    .await
}

async fn final_zfs_increment(ctx: Ctx) -> Result<(), ActionError> {
    crate::sagas::with_action_timeout(
        "migration.final_zfs_increment",
        ACTION_TIMEOUT_AWAIT,
        async move {
            let user_ctx = ctx.user_data();
            fence_check(user_ctx).await?;
            let store = user_ctx.store().clone();
            let params: MigrationSagaParams = ctx.saga_params()?;
            let target_cn: Uuid = ctx.lookup("designated_target_cn")?;

            let dataset = format!("zones/{}", params.instance_id);
            let base_snap = format!("{dataset}@migration-base");
            let final_snap = format!("{dataset}@migration-final");

            // Mint a fresh source ticket for the incremental
            // pass (separate TTL window from the base pass).
            let peer = resolve_zfs_peer(
                &store,
                target_cn,
                params.source_cn,
                params.instance_id,
                params.migration_id,
            )
            .await?;

            let src_job = store
                .enqueue_job(tritond_store::NewJob {
                    kind: tritond_store::JobKind::MigrateZfsSend {
                        migration_id: params.migration_id,
                        instance_id: params.instance_id,
                        role: tritond_store::MigrationJobRole::Source,
                        dataset: dataset.clone(),
                        from_snap: Some(base_snap.clone()),
                        to_snap: final_snap.clone(),
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
                        dataset,
                        from_snap: Some(base_snap),
                        to_snap: final_snap,
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
                "migration.final_zfs_increment: enqueued incremental ZFS jobs; awaiting both",
            );
            await_jobs_terminal(
                store,
                &[src_job.id, dst_job.id],
                "migration.final_zfs_increment",
            )
            .await?;
            tracing::info!(
                migration_id = %params.migration_id,
                "migration.final_zfs_increment: both ZFS jobs completed",
            );
            Ok(())
        },
    )
    .await
}

async fn quiesce_and_stream(ctx: Ctx) -> Result<(), ActionError> {
    crate::sagas::with_action_timeout("migration.quiesce_and_stream", ACTION_TIMEOUT, async move {
        let user_ctx = ctx.user_data();
        fence_check(user_ctx).await?;
        let store = user_ctx.store().clone();
        let params: MigrationSagaParams = ctx.saga_params()?;

        // Transition record phase to Switch — the cutover
        // window is open.
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
            // Cold-migrate: source VM is already stopped (or
            // the operator handled it pre-migration); the
            // dataset transferred in nodes 6+7 is canonical,
            // so the only thing left for this node is the
            // phase transition above. switch_ownership
            // commits the FDB CAS next; on the target CN,
            // the existing Provision job (node 4) brings the
            // newly-received dataset up under the migrated
            // identity.
            tracing::info!(
                migration_id = %params.migration_id,
                "migration.quiesce_and_stream: cold path, phase Switch recorded; nothing to stream",
            );
            return Ok(());
        }

        // Live path: LM-7 enqueues the MigrateVmmStream
        // jobs that pause the guest, stream RAM, and run the
        // PauseComplete/SwitchComplete cutover fence. Until
        // LM-7 the live path is a no-op (which would race
        // forward into a switch with no quiesce — the
        // migration would corrupt the guest's open
        // connections). Surface that here as an explicit
        // failure so an operator who passes `cold: false`
        // before LM-7 gets a clear error rather than a
        // silently-broken cutover.
        Err(ActionError::action_failed(serde_json::json!({
            "kind": "migration.quiesce_and_stream.live_path_pending_lm7",
            "reason": "live-memory transfer not implemented until LM-7; pass `cold: true` for now",
        })))
    })
    .await
}

async fn quiesce_and_stream_undo(_ctx: Ctx) -> Result<(), anyhow::Error> {
    // LM-7 best-effort `bhyve_ctl resume-vm` on the source
    // here; LM-5 skeleton has nothing paused.
    Ok(())
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
        // target_cn. LM-7 will wrap this in the
        // PauseComplete / SwitchComplete WebSocket fence; for
        // LM-6 cold-migrate the agent stops the source guest
        // before this runs (so there's nothing to fence) and the
        // FDB write is the cutover.
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

/// Source-side ZFS-job peer info bundle: the wss base URL, the
/// pinned TLS SPKI, and the freshly-minted migrate ticket the
/// source agent needs to dial the target's
/// `/migrate/{id}/zfs` listener route. Built once per saga node
/// (initial + incremental each mint a fresh ticket so the 10-min
/// TTL covers each transfer independently) and embedded into the
/// Source job's [`tritond_store::JobKind::MigrateZfsSend`]
/// variant.
struct ZfsPeerInfo {
    endpoint: String,
    spki_hex: String,
    ticket: String,
}

/// Default agent migrate-listener port. Mirrors
/// `tritonagent::migrate::DEFAULT_MIGRATE_LISTEN_PORT = 4568`
/// (plan §D.3). A future slice persists the per-CN port on the
/// `Cn` record (analogous to `console_listen_port`) for
/// dynamically-bound deployments; for LM-6c we rely on the
/// invariant that every agent runs with the documented default.
const DEFAULT_AGENT_MIGRATE_PORT: u16 = 4568;

/// Resolve the target CN's reachable migrate endpoint + SPKI pin
/// + a freshly-minted Source-role migrate ticket for the saga's
/// ZFS handoff steps. The ticket is bound to
/// (source_cn, target_cn, instance_id, migration_id,
/// [`MigrateRole::ZfsSource`]) using the **target** CN's
/// migrate-ticket key — the target's listener verifies against
/// its own key, so the source has to mint with the target's
/// key. Reading that key out of FDB here is safe: the saga
/// process is the tritond process and holds the same trust as
/// the CN approval path that wrote the key.
async fn resolve_zfs_peer(
    store: &std::sync::Arc<dyn tritond_store::Store>,
    target_cn: Uuid,
    source_cn: Uuid,
    instance_id: Uuid,
    migration_id: Uuid,
) -> Result<ZfsPeerInfo, ActionError> {
    let cn = store
        .get_cn(target_cn)
        .await
        .map_err(store_err_to_action_err)?;
    let admin_ip = cn.admin_ip.ok_or_else(|| {
        ActionError::action_failed(serde_json::json!({
            "kind": "migration.zfs.target_cn_no_admin_ip",
            "target_cn": target_cn.to_string(),
        }))
    })?;
    let migrate_ticket_key_bytes = cn.migrate_ticket_key.ok_or_else(|| {
        ActionError::action_failed(serde_json::json!({
            "kind": "migration.zfs.target_cn_no_migrate_ticket_key",
            "reason": "target CN registered before LM-6c — re-approve to mint a key",
            "target_cn": target_cn.to_string(),
        }))
    })?;
    let spki_bytes = cn.console_tls_spki_sha256.ok_or_else(|| {
        ActionError::action_failed(serde_json::json!({
            "kind": "migration.zfs.target_cn_no_spki",
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
            tritond_auth::MigrateRole::ZfsSource,
            tritond_auth::DEFAULT_MIGRATE_TICKET_TTL_SECS,
        )
        .map_err(|e| {
            ActionError::action_failed(serde_json::json!({
                "kind": "migration.zfs.ticket_mint_failed",
                "error": e.to_string(),
            }))
        })?;
    Ok(ZfsPeerInfo {
        endpoint: format!("wss://{admin_ip}:{DEFAULT_AGENT_MIGRATE_PORT}"),
        spki_hex: hex::encode(spki_bytes),
        ticket,
    })
}

/// Poll a set of `ProvisioningJob` ids until every one reaches a
/// terminal status. Returns `Err` on the first one that lands in
/// `Failed`. Used by the migration saga's enqueue-then-await
/// pattern; the standard
/// [`crate::sagas::common::await_provisioning_job_terminal`] helper
/// expects a single job looked up by node name, while the migrate
/// flow enqueues paired Source + Target jobs that need to be
/// awaited together.
async fn await_jobs_terminal(
    store: Arc<dyn tritond_store::Store>,
    job_ids: &[Uuid],
    action_name: &'static str,
) -> Result<(), ActionError> {
    use std::time::Duration;
    use tritond_store::JobStatusKind;
    const POLL: Duration = Duration::from_millis(100);
    let mut remaining: Vec<Uuid> = job_ids.to_vec();
    while !remaining.is_empty() {
        let mut still_pending = Vec::with_capacity(remaining.len());
        for id in &remaining {
            let current = store.get_job(*id).await.map_err(store_err_to_action_err)?;
            match current.status.kind() {
                JobStatusKind::Completed => {}
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
                _ => still_pending.push(*id),
            }
        }
        if still_pending.is_empty() {
            return Ok(());
        }
        remaining = still_pending;
        tokio::time::sleep(POLL).await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tritond_store::MemStore;

    #[test]
    fn dag_builds_without_error() {
        // Steno's `SagaDag` doesn't expose its internal node
        // graph publicly, so we can't directly count action
        // nodes here. The build call itself succeeding is the
        // contract we care about — a missing dependency between
        // two `append` calls would surface as a `dag.build()`
        // error inside `build_dag`. A future improvement: serde
        // a SagaDag to JSON and walk that.
        let params = MigrationSagaParams {
            migration_id: Uuid::new_v4(),
            instance_id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            project_id: Uuid::new_v4(),
            source_cn: Uuid::new_v4(),
            target_cn_hint: None,
            automatic: false,
            cold: true,
        };
        let dag = build_dag(&params).expect("build_dag");
        // Round-trip through serde to confirm the saga record
        // is persistable (recovery needs this).
        let json = serde_json::to_value(dag.as_ref()).expect("serialize dag");
        assert!(json.is_object());
    }

    #[test]
    fn build_references_includes_instance_and_source_cn() {
        let params = MigrationSagaParams {
            migration_id: Uuid::new_v4(),
            instance_id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            project_id: Uuid::new_v4(),
            source_cn: Uuid::new_v4(),
            target_cn_hint: Some(Uuid::new_v4()),
            automatic: false,
            cold: true,
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
        let params = MigrationSagaParams {
            migration_id: Uuid::new_v4(),
            instance_id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            project_id: Uuid::new_v4(),
            source_cn: Uuid::new_v4(),
            target_cn_hint: None,
            automatic: false,
            cold: true,
        };
        let refs = build_references(&params);
        // 4 resources without the target hint: tenant, project,
        // instance, source_cn.
        assert_eq!(refs.len(), 4);
    }

    #[test]
    fn params_round_trip_through_json() {
        let params = MigrationSagaParams {
            migration_id: Uuid::new_v4(),
            instance_id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            project_id: Uuid::new_v4(),
            source_cn: Uuid::new_v4(),
            target_cn_hint: Some(Uuid::new_v4()),
            automatic: true,
            cold: true,
        };
        let json = serde_json::to_value(&params).unwrap();
        let back: MigrationSagaParams = serde_json::from_value(json).unwrap();
        assert_eq!(back.migration_id, params.migration_id);
        assert_eq!(back.instance_id, params.instance_id);
        assert_eq!(back.target_cn_hint, params.target_cn_hint);
        assert_eq!(back.automatic, params.automatic);
        assert_eq!(back.cold, params.cold);
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

    // The actual saga end-to-end test (drive the SagaExecutor +
    // assert state transitions) lands with LM-5 task #41 wiring
    // the saga into `register_all_actions`, since the executor's
    // setup needs the global registry. Until then the unit tests
    // here cover the params + DAG shape only.
    #[allow(dead_code)]
    fn _smoke_compile_check(
        _store: Arc<dyn tritond_store::Store>,
    ) -> Result<(), tritond_store::StoreError> {
        Ok(())
    }
}
