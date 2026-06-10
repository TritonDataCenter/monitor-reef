// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Instance-lifecycle reconciliation: vmadm-state projection, the
//! agent-wins classifier pass, lifecycle CAS drivers for job
//! claim/complete, and the shared start/stop/restart transition body.

use dropshot::{HttpError, HttpResponseOk, Path, RequestContext};
use uuid::Uuid;

use tritond_api::TenantProjectInstancePath;
use tritond_api::types::{
    Instance, JobKind, JobOutcome, LifecycleState, LifecycleStateKind, ProvisioningJob,
};
use tritond_audit::Outcome as AuditOutcome;
use tritond_store::{Cn, CnRole, CnState, InstanceBrand, NewJob, Store, StoreError};

use crate::auth::{self, Action, authenticate_and_authorize_in_tenant};
use crate::context::ApiContext;
use crate::error::{not_found, store_error_to_audit_outcome, store_error_to_http};
use crate::validate::parse_request_id;

/// Drive the classifier over a CN status report and fold each VM's
/// outcome into the store. Called from `agent_status` after the CN's
/// `last_status` blob has been persisted.
///
/// Best-effort: errors here are logged by the caller and do not fail
/// the heartbeat. The data we produce here (LegacyVm rows, drift
/// alarms) is operationally important but not load-bearing for the
/// CN's own ability to claim jobs.
/// Map a `VmState` reported by `vmadm` to the corresponding tritond
/// `LifecycleState`. Returns `None` for states that aren't safe to
/// project onto the tritond lifecycle machine (`Receiving`, `Sending`,
/// in-flight `Configured`/`Incomplete`, `Destroyed`, and `Unknown` --
/// the classifier's deliberate hands-off list).
pub(crate) fn vm_state_to_lifecycle(
    state: Option<tritond_store::VmState>,
) -> Option<LifecycleState> {
    use tritond_store::VmState;
    match state? {
        VmState::Running => Some(LifecycleState::Running),
        // `installed` zones are configured but not booted; treat as
        // Stopped from tritond's perspective.
        VmState::Stopped | VmState::Installed => Some(LifecycleState::Stopped),
        VmState::Provisioning => Some(LifecycleState::Provisioning),
        VmState::Failed => Some(LifecycleState::Failed {
            reason: "agent reports vmadm state=failed".to_string(),
        }),
        VmState::Receiving
        | VmState::Sending
        | VmState::Configured
        | VmState::Incomplete
        | VmState::Destroyed
        | VmState::Unknown => None,
        // VmState is `#[non_exhaustive]`; future agent versions can
        // add states. Treat anything we don't recognize as
        // unmappable so the classifier doesn't write a stale or
        // wrong lifecycle from a state we haven't reasoned about.
        _ => None,
    }
}

/// Compare an existing `LifecycleState` against an observed one.
/// Returns true when they refer to the same logical state; the
/// `Failed.reason` string is intentionally ignored so a re-report
/// of the same Failed state with a slightly different reason
/// doesn't churn the record.
pub(crate) fn lifecycle_eq(a: &LifecycleState, b: &LifecycleState) -> bool {
    a.kind() == b.kind()
}

/// Reconcile the lifecycle field on a managed Instance from a CN
/// status report. Agent-wins: the CN is the source of truth, so we
/// CAS from any current state to the observed state. No-ops when
/// the reported state is unmappable, the instance has vanished, or
/// the lifecycle already matches.
pub(crate) async fn reconcile_managed_lifecycle(
    store: &dyn Store,
    instance_id: Uuid,
    reported_state: Option<tritond_store::VmState>,
) -> Result<(), StoreError> {
    let Some(observed) = vm_state_to_lifecycle(reported_state) else {
        return Ok(());
    };
    let inst = match store.get_instance(instance_id).await {
        Ok(i) => i,
        // Instance vanished between the per-CN list and now (rare).
        // Nothing to update.
        Err(StoreError::NotFound) => return Ok(()),
        Err(e) => return Err(e),
    };
    if lifecycle_eq(&inst.lifecycle, &observed) {
        return Ok(());
    }
    // CAS from any current state to the observed state. Listing
    // every kind here is the "force" path: agent-wins reconciliation
    // doesn't care what tritond thought the state was.
    let any_state = &[
        LifecycleStateKind::Pending,
        LifecycleStateKind::Provisioning,
        LifecycleStateKind::Running,
        LifecycleStateKind::Stopping,
        LifecycleStateKind::Stopped,
        LifecycleStateKind::Failed,
    ];
    store
        .transition_instance_lifecycle(instance_id, any_state, observed)
        .await?;
    Ok(())
}

pub(crate) async fn run_classifier_pass(
    ctx: &ApiContext,
    reporting_cn: Uuid,
    payload: &serde_json::Value,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(), StoreError> {
    use crate::legacy_classify::{Classification, ClassifierContext, classify_vm};
    use std::collections::HashMap;
    use tritond_store::{AdoptableState, LegacyNic, LegacyVm, parse_vm_reports};

    let reports = parse_vm_reports(payload);
    if reports.is_empty() {
        return Ok(());
    }
    // Pre-fetch every Instance the store thinks is on this CN so the
    // classifier's instance lookup is a HashMap probe. We classify
    // every report up-front and collect outcomes -- crucially the
    // classifier context is dropped before any subsequent
    // store-mutating awaits, so the trait-object closure can stay
    // non-Send (the per-report awaits below run on the same task and
    // don't need to cross an await boundary while holding it).
    let instances = ctx.store.list_instances_for_cn(reporting_cn).await?;
    let outcomes: Vec<Classification> = {
        let by_id: HashMap<Uuid, &Instance> = instances.iter().map(|i| (i.id, i)).collect();
        let lookup = |id: Uuid| -> Option<&Instance> { by_id.get(&id).copied() };
        let classifier_ctx = ClassifierContext {
            reporting_cn_uuid: reporting_cn,
            instance_lookup: &lookup,
            identity_hmac_key: &ctx.identity_hmac_key,
        };
        reports
            .iter()
            .map(|r| classify_vm(r, &classifier_ctx))
            .collect()
    };

    for (report, outcome) in reports.iter().zip(outcomes.into_iter()) {
        match outcome {
            Classification::Managed { instance_id }
            | Classification::MidProvision { instance_id } => {
                // Clean up any stale `LegacyVm` row for this zone --
                // happens when a zone was previously classified
                // Unmanaged (e.g. tritond didn't yet know about it,
                // OR the agent was on older code that didn't stamp
                // identity, OR the metadata was cleared in-zone).
                // Deletion is idempotent; no-ops if no row exists.
                if let Err(e) = ctx.store.delete_legacy_vm(report.uuid).await {
                    tracing::warn!(
                        smartos_uuid = %report.uuid,
                        error = %e,
                        "failed to clear stale legacy_vm row for managed zone",
                    );
                }

                // Agent-wins reconciliation for the lifecycle field.
                // The CN is the source of truth: when an operator
                // runs `vmadm stop` directly on the GZ, that change
                // must propagate back to the Instance record. We
                // skip MidProvision (the agent's vmadm-create is in
                // flight; tritond should stay Provisioning until
                // the next tick classifies Managed).
                if matches!(outcome, Classification::Managed { .. }) {
                    if let Err(e) =
                        reconcile_managed_lifecycle(ctx.store.as_ref(), instance_id, report.state)
                            .await
                    {
                        tracing::warn!(
                            instance_id = %instance_id,
                            smartos_uuid = %report.uuid,
                            error = %e,
                            "failed to reconcile lifecycle from CN report",
                        );
                    }
                }
            }
            Classification::Orphan {
                instance_id,
                expected_host,
            } => {
                tracing::warn!(
                    smartos_uuid = %report.uuid,
                    %instance_id,
                    ?expected_host,
                    %reporting_cn,
                    "managed instance reported by unexpected CN; possible vmadm send|recv evac",
                );
            }
            Classification::StaleFingerprint { reason } => {
                tracing::warn!(
                    smartos_uuid = %report.uuid,
                    ?reason,
                    %reporting_cn,
                    "tritond identity tag failed verification",
                );
            }
            Classification::Unmanaged => {
                // Preserve the original first_seen_at across upserts.
                let existing = ctx.store.get_legacy_vm(report.uuid).await.ok();
                let first_seen_at = existing.as_ref().map(|v| v.first_seen_at).unwrap_or(now);
                let adoptable = existing
                    .map(|v| v.adoptable)
                    .unwrap_or(AdoptableState::Unevaluated);
                let nics: Vec<LegacyNic> =
                    report.nics.iter().cloned().map(LegacyNic::from).collect();
                let legacy_vm = LegacyVm {
                    smartos_uuid: report.uuid,
                    host_cn_uuid: reporting_cn,
                    legacy_owner_uuid: report.owner_uuid,
                    alias: report.alias.clone(),
                    brand: report.brand.clone(),
                    state: report.state,
                    zone_state: report.zone_state.clone(),
                    // vmadm reports `max_physical_memory` in MiB and
                    // `quota` in GiB. Convert to bytes for the
                    // tritond-side schema; preserve None when the
                    // report omits the field (partial vmadm output).
                    memory_bytes: report.max_physical_memory.map(|mib| mib * 1024 * 1024),
                    quota_bytes: report.quota.map(|gib| gib * 1024 * 1024 * 1024),
                    cpu_cap: report.cpu_cap,
                    last_modified: report.last_modified.clone(),
                    nics,
                    adoptable,
                    first_seen_at,
                    last_seen_at: now,
                };
                ctx.store.upsert_legacy_vm(legacy_vm).await?;
            }
        }
    }
    Ok(())
}

/// Best-effort backfill of [`Instance::brand`] from the agent's
/// periodic status report.
///
/// The agent's status blob carries every zone on the CN with its live
/// brand (e.g. `bhyve`, `joyent-minimal`). Managed instances appear in
/// it too. Instances created before the `brand` field existed (or from
/// an image with no compatibility block) carry
/// [`InstanceBrand::NotApplicable`]; this folds the observed brand in so
/// the console UI's VNC gate becomes precise instead of
/// "offer-unless-known-bad".
///
/// Only writes when the current brand is `NotApplicable`, so it's a
/// one-time write per instance rather than a per-status-post storm.
/// Best-effort: any error is logged and skipped; this never fails the
/// status post.
pub(crate) async fn backfill_instance_brands(ctx: &ApiContext, payload: &serde_json::Value) {
    use tritond_store::parse_vm_reports;

    for report in parse_vm_reports(payload) {
        let Some(brand) = report.brand.as_deref().filter(|b| !b.is_empty()) else {
            continue;
        };
        match ctx.store.get_instance(report.uuid).await {
            Ok(instance) if instance.brand == InstanceBrand::NotApplicable => {
                let resolved = InstanceBrand::from_compat_brand(brand);
                if let Err(e) = ctx.store.set_instance_brand(report.uuid, resolved).await {
                    tracing::warn!(
                        instance_id = %report.uuid,
                        brand,
                        error = %e,
                        "failed to backfill instance brand from CN status report",
                    );
                }
            }
            // Already known, or not a managed instance — nothing to do.
            Ok(_) => {}
            Err(StoreError::NotFound) => {}
            Err(e) => {
                tracing::warn!(
                    smartos_uuid = %report.uuid,
                    error = %e,
                    "failed to look up instance for brand backfill",
                );
            }
        }
    }
}

/// 409 when a migration is in flight for the instance. The
/// start/stop/restart/delete entry points call this so an operator
/// (or tenant) lifecycle mutation cannot race the migrate-instance
/// saga's own quiesce/activate sequencing — a concurrent stop
/// would, for example, make the saga's "skip if already stopped"
/// read lie about who owns the guest's power state. The saga
/// itself enqueues jobs directly against the store and never
/// passes through these handlers, so it is not affected.
pub(crate) async fn reject_if_migration_active(
    store: &dyn Store,
    instance_id: Uuid,
) -> Result<(), HttpError> {
    match store.get_active_migration(instance_id).await {
        Ok(Some(migration)) => Err(HttpError::for_client_error(
            Some("Conflict".to_string()),
            dropshot::ClientErrorStatusCode::CONFLICT,
            format!(
                "instance {instance_id} is being migrated (migration {}); retry after it finishes or abort it",
                migration.id,
            ),
        )),
        Ok(None) => Ok(()),
        Err(e) => Err(store_error_to_http(e)),
    }
}

/// Token-only enum used by `instance_lifecycle_transition` to pick
/// the matching `JobKind` after the CAS lands. We don't pass a
/// `JobKind` directly because that would require the caller to
/// already know the `instance_id`, which only becomes available
/// inside the helper.
#[derive(Debug, Clone, Copy)]
pub(crate) enum JobKindTemplate {
    Provision,
    Stop,
    Restart,
}

impl JobKindTemplate {
    fn for_instance(self, instance_id: Uuid) -> JobKind {
        match self {
            JobKindTemplate::Provision => JobKind::Provision { instance_id },
            JobKindTemplate::Stop => JobKind::Stop { instance_id },
            JobKindTemplate::Restart => JobKind::Restart { instance_id },
        }
    }
}

/// Shared helper for the three lifecycle-transition handlers. Does
/// auth, the path-recheck, the store CAS, the optional job
/// enqueue, and the audit emission.
///
/// `enqueue` is `Some(JobKindTemplate)` for endpoints whose
/// follow-on transitions are agent-driven (start/stop/restart);
/// the CAS to the *transitional* state runs first (so we get the
/// right 409 on a stale state), then the job is enqueued. If the
/// enqueue fails after a successful CAS, the instance is left in
/// the transitional state and the caller gets a 5xx; a future
/// slice can move CAS+enqueue into a single FDB transaction.
pub(crate) async fn instance_lifecycle_transition(
    rqctx: RequestContext<ApiContext>,
    path: Path<TenantProjectInstancePath>,
    action: Action,
    expected_from: &[LifecycleStateKind],
    to: LifecycleState,
    enqueue: Option<JobKindTemplate>,
) -> Result<HttpResponseOk<Instance>, HttpError> {
    let ctx = rqctx.context();
    let TenantProjectInstancePath {
        tenant_id,
        project_id,
        instance_id,
    } = path.into_inner();
    let principal = authenticate_and_authorize_in_tenant(
        &rqctx, &ctx.auth, &ctx.audit, &ctx.store, action, tenant_id,
    )
    .await?;
    let request_id = parse_request_id(&rqctx);

    // Defence-in-depth on tenant+project before we try to transition.
    let instance = ctx
        .store
        .get_instance(instance_id)
        .await
        .map_err(store_error_to_http)?;
    if instance.tenant_id != tenant_id || instance.project_id != project_id {
        return Err(not_found());
    }
    reject_if_migration_active(ctx.store.as_ref(), instance_id).await?;
    let target_cn_uuid = instance.host_cn_uuid;

    let updated = match ctx
        .store
        .transition_instance_lifecycle(instance_id, expected_from, to.clone())
        .await
    {
        Ok(i) => i,
        Err(e) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    action,
                    request_id,
                    Some(format!("Instance::\"{instance_id}\"")),
                    store_error_to_audit_outcome(&e),
                    serde_json::Value::Null,
                )
                .await;
            return Err(store_error_to_http(e));
        }
    };

    if let Some(template) = enqueue
        && let Err(e) = ctx
            .store
            .enqueue_job(NewJob {
                kind: template.for_instance(instance_id),
                target_cn_uuid,
            })
            .await
    {
        ctx.audit
            .record_mutation(
                &principal,
                action,
                request_id,
                Some(format!("Instance::\"{instance_id}\"")),
                store_error_to_audit_outcome(&e),
                serde_json::Value::Null,
            )
            .await;
        return Err(store_error_to_http(e));
    }

    ctx.audit
        .record_mutation(
            &principal,
            action,
            request_id,
            Some(format!("Instance::\"{instance_id}\"")),
            AuditOutcome::Success {
                resource: Some(format!("Instance::\"{instance_id}\"")),
            },
            serde_json::json!({
                "tenant_id": tenant_id,
                "project_id": project_id,
                "to_state": format!("{:?}", to.kind()),
            }),
        )
        .await;
    Ok(HttpResponseOk(updated))
}

/// Audit + return a 400 in one shot. Used by `create_project_instance`
/// for cpu/memory size validation; can't easily live as a free
/// function because it borrows `ctx` and `principal`.
pub(crate) async fn reject_audit(
    ctx: &ApiContext,
    principal: &auth::Principal,
    action: Action,
    request_id: Option<Uuid>,
    message: &str,
    context: serde_json::Value,
) -> HttpError {
    ctx.audit
        .record_mutation(
            principal,
            action,
            request_id,
            None,
            AuditOutcome::ClientError {
                code: 400,
                message: message.to_string(),
            },
            context,
        )
        .await;
    HttpError::for_bad_request(Some("BadRequest".to_string()), message.to_string())
}

/// Drive the instance lifecycle forward in response to an agent
/// claiming a job. For Provision: Pending → Provisioning. Start
/// stays Pending (there is no Provisioning step when booting an
/// existing zone); it advances straight to Running on complete.
/// Stop / Restart already entered Stopping in the operator-facing
/// `instance_*` handler before the job was enqueued, so claim
/// has nothing to advance. CAS failures are logged but do not
/// propagate — the job is already in InProgress regardless.
pub(crate) async fn drive_lifecycle_for_claim(store: &dyn Store, job: &ProvisioningJob) {
    if let JobKind::Provision { instance_id } = job.kind {
        if let Err(e) = store
            .transition_instance_lifecycle(
                instance_id,
                &[LifecycleStateKind::Pending],
                LifecycleState::Provisioning,
            )
            .await
        {
            tracing::warn!(
                %instance_id,
                error = %e,
                "Pending → Provisioning lifecycle CAS failed at claim",
            );
        }
    }
}

/// Drive the instance lifecycle to its terminal state in
/// response to an agent reporting a job's outcome. Mapping:
///
/// | JobKind / Outcome      | Lifecycle target                 |
/// |------------------------|----------------------------------|
/// | Provision / Completed  | Provisioning → Running           |
/// | Start / Completed      | Pending → Running                |
/// | Stop / Completed       | Stopping → Stopped               |
/// | Restart / Completed    | Stopping → Running               |
/// | (any) / Failed{reason} | (current) → Failed{reason}       |
///
/// For Failed outcomes the CAS accepts any of the in-flight
/// states (Pending, Provisioning, Stopping) so a job that
/// failed before its claim-time advance still lands in Failed
/// rather than getting stuck. CAS failures (instance deleted
/// out from under the job, lifecycle drift) are logged.
pub(crate) async fn drive_lifecycle_for_complete(
    store: &dyn Store,
    job: &ProvisioningJob,
    outcome: &JobOutcome,
) {
    // Delete jobs run *after* the tritond record is gone, so
    // there is no lifecycle to transition. Skip cleanly to
    // avoid a noisy "instance not found" warning that would
    // fire on every successful zone teardown.
    if matches!(job.kind, JobKind::Delete { .. }) {
        return;
    }
    // Dataplane jobs (FIP realize/withdraw, running-VM blueprint
    // re-apply) run *against* a VM that is already in its steady
    // lifecycle state — they must never mutate it. A FipClaim /
    // FipRelease / ApplyPortBlueprint failure (kmod unreachable,
    // EnsureExternalLink/ipadm error, flaky external link) is a
    // dataplane problem, not a VM-health problem; driving the
    // healthy hosting VM to Failed on such a failure would let
    // anyone who can make a victim's FIP/blueprint job fail force
    // that running VM into Failed. The catch-all Failed arm below
    // would otherwise match these kinds (Running is in its
    // accepted-from set), so guard them out explicitly here. Listed
    // by kind (rather than allow-listing the lifecycle kinds) so a
    // future new instance-lifecycle JobKind still defaults to
    // driving and migration/edge handling is unchanged.
    if matches!(
        job.kind,
        JobKind::FipClaim { .. } | JobKind::FipRelease { .. } | JobKind::ApplyPortBlueprint { .. }
    ) {
        return;
    }
    let (expected_from, target): (&[LifecycleStateKind], LifecycleState) =
        match (&job.kind, outcome) {
            (JobKind::Provision { .. }, JobOutcome::Completed) => {
                (&[LifecycleStateKind::Provisioning], LifecycleState::Running)
            }
            // Start powers on an existing zone; it never enters
            // Provisioning (claim leaves it Pending), so it lands
            // Running directly from Pending.
            (JobKind::Start { .. }, JobOutcome::Completed) => {
                (&[LifecycleStateKind::Pending], LifecycleState::Running)
            }
            (JobKind::Stop { .. }, JobOutcome::Completed) => {
                (&[LifecycleStateKind::Stopping], LifecycleState::Stopped)
            }
            (JobKind::Restart { .. }, JobOutcome::Completed) => {
                (&[LifecycleStateKind::Stopping], LifecycleState::Running)
            }
            (_, JobOutcome::Failed { reason }) => (
                &[
                    LifecycleStateKind::Pending,
                    LifecycleStateKind::Provisioning,
                    LifecycleStateKind::Stopping,
                    LifecycleStateKind::Running,
                ],
                LifecycleState::Failed {
                    reason: reason.clone(),
                },
            ),
            _ => return,
        };
    let Some(instance_id) = job.kind.instance_id() else {
        return;
    };
    if let Err(e) = store
        .transition_instance_lifecycle(instance_id, expected_from, target.clone())
        .await
    {
        tracing::warn!(
            %instance_id,
            kind = ?job.kind,
            ?target,
            error = %e,
            "lifecycle CAS failed at job complete",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use tritond_store::{
        JobStatus, MemStore, NewImage, NewInstance, NewProject, NewSilo, NewSshKey, NewSubnet,
        NewVpc, ProvisioningJob,
    };

    /// Create a single instance and drive it to `Running`. Returns
    /// `(store, instance_id, nic_id)`.
    async fn running_instance() -> (MemStore, Uuid, Uuid) {
        let store = MemStore::new();
        let silo = store
            .create_silo(NewSilo {
                name: "s".into(),
                description: None,
            })
            .await
            .unwrap();
        let tenant_id = silo.default_tenant_id;
        let project = store
            .create_project(
                tenant_id,
                NewProject {
                    name: "p".into(),
                    description: None,
                },
            )
            .await
            .unwrap();
        let vpc = store
            .create_vpc(
                tenant_id,
                project.id,
                NewVpc {
                    name: "v".into(),
                    description: None,
                    ipv4_block: Some("10.0.0.0/16".parse().unwrap()),
                    ipv6_block: None,
                },
            )
            .await
            .unwrap();
        let subnet = store
            .create_subnet(
                tenant_id,
                project.id,
                vpc.id,
                NewSubnet {
                    name: "primary".into(),
                    description: None,
                    ipv4_block: Some("10.0.1.0/24".parse().unwrap()),
                    ipv6_block: None,
                },
            )
            .await
            .unwrap();
        let image = store
            .create_image_silo(
                silo.id,
                NewImage {
                    name: "img".into(),
                    description: None,
                    os: "linux".into(),
                    version: "1".into(),
                    size_bytes: 1_000_000,
                    sha256: "a".repeat(64),
                    source_url: Some("mantafs://i".into()),
                    id: None,
                    compatibility: None,
                },
            )
            .await
            .unwrap();
        let ssh = store
            .create_ssh_key_silo(
                silo.id,
                NewSshKey {
                    name: "k".into(),
                    description: None,
                    public_key: "ssh-ed25519 AAAA".into(),
                },
                "SHA256:fixture".into(),
            )
            .await
            .unwrap();
        let created = store
            .create_instance(
                tenant_id,
                project.id,
                NewInstance {
                    name: "web".into(),
                    description: None,
                    image_id: image.id,
                    primary_subnet_id: subnet.id,
                    ssh_key_ids: vec![ssh.id],
                    cpu: 1,
                    memory_bytes: 1024 * 1024 * 1024,
                    mac: None,
                    disk_bytes: None,
                    extra_nics: Vec::new(),
                },
            )
            .await
            .unwrap();
        let instance_id = created.instance.id;
        store
            .transition_instance_lifecycle(
                instance_id,
                &[LifecycleStateKind::Pending],
                LifecycleState::Running,
            )
            .await
            .unwrap();
        (store, instance_id, created.nics[0].id)
    }

    fn failed_job(kind: JobKind) -> ProvisioningJob {
        ProvisioningJob {
            id: Uuid::new_v4(),
            kind,
            status: JobStatus::Failed {
                reason: "kmod unreachable".into(),
            },
            seq: 1,
            created_at: Utc::now(),
            claimed_at: Some(Utc::now()),
            claimed_by: Some("agent".into()),
            completed_at: Some(Utc::now()),
            target_cn_uuid: None,
            result: None,
        }
    }

    async fn lifecycle_kind(store: &MemStore, instance_id: Uuid) -> LifecycleStateKind {
        store
            .get_instance(instance_id)
            .await
            .unwrap()
            .lifecycle
            .kind()
    }

    /// CTF regression: a victim's FipClaim failing (kmod unreachable,
    /// flaky external link) must NOT drive the healthy hosting VM to
    /// Failed. The dataplane guard must short-circuit before any CAS.
    #[tokio::test]
    async fn failed_fip_claim_leaves_running_instance_unchanged() {
        let (store, instance_id, nic_id) = running_instance().await;
        let job = failed_job(JobKind::FipClaim {
            floating_ip_id: Uuid::new_v4(),
            nic_id,
            instance_id,
            fip_addr: "192.0.2.10".into(),
            external_nic_tag: Some("external".into()),
            vlan_id: Some(2003),
            generation: 2,
        });
        let outcome = JobOutcome::Failed {
            reason: "kmod unreachable".into(),
        };
        drive_lifecycle_for_complete(&store, &job, &outcome).await;
        assert_eq!(
            lifecycle_kind(&store, instance_id).await,
            LifecycleStateKind::Running,
            "a failed FipClaim must not fail the hosting VM",
        );
    }

    /// Same guard for the running-VM blueprint re-apply job: an
    /// ApplyPortBlueprint failure is a dataplane problem, not VM health.
    #[tokio::test]
    async fn failed_apply_port_blueprint_leaves_running_instance_unchanged() {
        let (store, instance_id, nic_id) = running_instance().await;
        let job = failed_job(JobKind::ApplyPortBlueprint {
            instance_id,
            nic_id,
        });
        let outcome = JobOutcome::Failed {
            reason: "ipadm error".into(),
        };
        drive_lifecycle_for_complete(&store, &job, &outcome).await;
        assert_eq!(
            lifecycle_kind(&store, instance_id).await,
            LifecycleStateKind::Running,
            "a failed ApplyPortBlueprint must not fail the hosting VM",
        );
    }

    /// Instance-lifecycle failures still transition to Failed: the
    /// guard must not have broadened to swallow Provision/Start/Stop.
    #[tokio::test]
    async fn failed_instance_lifecycle_jobs_still_transition_to_failed() {
        for kind in [
            JobKind::Provision {
                instance_id: Uuid::nil(),
            },
            JobKind::Start {
                instance_id: Uuid::nil(),
            },
            JobKind::Stop {
                instance_id: Uuid::nil(),
            },
        ] {
            let (store, instance_id, _nic_id) = running_instance().await;
            // Rebind the kind onto this fixture's instance id.
            let kind = match kind {
                JobKind::Provision { .. } => JobKind::Provision { instance_id },
                JobKind::Start { .. } => JobKind::Start { instance_id },
                JobKind::Stop { .. } => JobKind::Stop { instance_id },
                other => other,
            };
            let job = failed_job(kind);
            let outcome = JobOutcome::Failed {
                reason: "agent failure".into(),
            };
            drive_lifecycle_for_complete(&store, &job, &outcome).await;
            assert_eq!(
                lifecycle_kind(&store, instance_id).await,
                LifecycleStateKind::Failed,
                "a failed instance-lifecycle job must still fail the VM",
            );
        }
    }
}
