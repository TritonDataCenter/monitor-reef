// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! The migration data-plane lane.
//!
//! ZFS replication streams and RAM streams run for
//! hours; driving them inline in the agent's strictly-serial poll
//! loop would starve every other job on the CN. Kinds matched by
//! [`is_data_plane_kind`] are instead detached onto a tokio task
//! behind a `Semaphore(1)` owned by the agent; one migration data
//! plane per CN, because concurrent streams just split the admin
//! NIC's bandwidth and double every migration's wall-clock. The
//! detached task reports its own job completion; the poll loop
//! keeps claiming other jobs meanwhile. Control-plane migration
//! jobs (quota dance, provision-target, cleanup, pause/resume)
//! stay inline; they finish in seconds.

use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};
use tokio::sync::Semaphore;
use tracing::{error, info, warn};
use tritond_client::Client;
use tritond_client::types::{
    CompleteJobRequest, JobKind, JobOutcome, MigrationJobRole, ProvisioningJob,
};
use uuid::Uuid;

use crate::AgentConfig;
use crate::{migrate, migrate_progress, migrate_vmm, zfs};

/// Build the per-agent data-plane lane (see module docs for why
/// the permit count is 1).
pub(crate) fn new_lane() -> Arc<Semaphore> {
    Arc::new(Semaphore::new(1))
}

/// Whether a claimed job is a migration data-plane stream that
/// must run on the detached lane instead of the poll loop.
pub(crate) fn is_data_plane_kind(kind: &JobKind) -> bool {
    matches!(
        kind,
        JobKind::MigrateZfsSend {
            role: MigrationJobRole::Source,
            ..
        } | JobKind::MigrateVmmStream {
            role: MigrationJobRole::Source,
            ..
        } | JobKind::MigrateTargetListen { .. }
    )
}

/// Detach a claimed data-plane job onto the lane. The task itself
/// reports the terminal outcome to tritond; the caller must not.
pub(crate) fn spawn_data_plane_job(
    client: Arc<Client>,
    cfg: AgentConfig,
    lane: Arc<Semaphore>,
    job: ProvisioningJob,
) {
    tokio::spawn(async move {
        // The claim is already held; the permit only serializes
        // the heavy stream itself. A job queued behind an
        // in-flight stream simply stays Claimed until its turn.
        let Ok(_permit) = lane.acquire_owned().await else {
            // Closed only at process teardown; tritond's stale-claim
            // sweeper re-queues the job.
            return;
        };
        info!(job_id = %job.id, kind = ?job.kind, "migration data plane: starting");
        let (outcome, result) = match run_data_plane_job(&client, &cfg, &job).await {
            Ok(result) => (JobOutcome::Completed, result),
            Err(reason) => {
                // A failed vmm stream still ships its protocol-phase
                // report as the job result; the saga's failure policy
                // (resume the source vs leave it paused) keys off it.
                let result = failure_result(&reason);
                let chain = format!("{reason:#}");
                error!(
                    job_id = %job.id,
                    error = %chain,
                    "migration data-plane job failed; reporting to tritond",
                );
                (JobOutcome::Failed(chain), result)
            }
        };
        report_completion(&client, job.id, outcome, result).await;
    });
}

/// Extract the [`migrate_vmm::StreamFailed`] report from an error
/// chain so a failed stream's completion still carries the
/// last-phase payload.
fn failure_result(err: &anyhow::Error) -> Option<serde_json::Value> {
    err.downcast_ref::<migrate_vmm::StreamFailed>()
        .map(|f| f.report.clone())
}

/// Dispatch one data-plane job kind. Returns the optional job
/// `result` payload to attach to the completion.
async fn run_data_plane_job(
    client: &Arc<Client>,
    cfg: &AgentConfig,
    job: &ProvisioningJob,
) -> Result<Option<serde_json::Value>> {
    match &job.kind {
        JobKind::MigrateZfsSend {
            migration_id,
            instance_id,
            role: MigrationJobRole::Source,
            dataset,
            from_snap,
            to_snap,
            peer_endpoint,
            peer_spki_sha256_hex,
            ticket,
        } => {
            let bytes = run_zfs_send_source(
                client,
                cfg,
                *migration_id,
                *instance_id,
                dataset,
                from_snap.as_deref(),
                to_snap,
                peer_endpoint.as_deref(),
                peer_spki_sha256_hex.as_deref(),
                ticket.as_deref(),
            )
            .await?;
            // The saga's sync-convergence loop reads this
            // (`ZfsSendResult`) to decide whether another
            // incremental round is worth it.
            Ok(Some(serde_json::json!({ "bytes_streamed": bytes })))
        }
        JobKind::MigrateVmmStream {
            migration_id,
            instance_id,
            role: MigrationJobRole::Source,
            peer_endpoint,
            peer_spki_sha256_hex,
            ticket,
        } => {
            let peer_endpoint = peer_endpoint
                .as_deref()
                .ok_or_else(|| anyhow!("Source-role MigrateVmmStream missing peer_endpoint"))?;
            let peer_spki = peer_spki_sha256_hex.as_deref().ok_or_else(|| {
                anyhow!("Source-role MigrateVmmStream missing peer_spki_sha256_hex")
            })?;
            let ticket = ticket
                .as_deref()
                .ok_or_else(|| anyhow!("Source-role MigrateVmmStream missing ticket"))?;
            let report = migrate_vmm::run_source(
                client,
                cfg,
                *migration_id,
                *instance_id,
                peer_endpoint,
                peer_spki,
                ticket,
            )
            .await?;
            Ok(Some(report))
        }
        JobKind::MigrateTargetListen {
            migration_id,
            instance_id,
        } => {
            let result =
                migrate_vmm::target_listen(client, job, *migration_id, *instance_id).await?;
            Ok(Some(result))
        }
        other => bail!("job kind {other:?} is not a migration data-plane kind"),
    }
}

/// Source side of a `MigrateZfsSend`: take the (recursive)
/// migration snapshot, dial the target listener's
/// `/migrate/{id}/zfs` route, and pump `zfs send` stdout through
/// the WebSocket. Returns the bytes streamed.
#[allow(clippy::too_many_arguments)]
async fn run_zfs_send_source(
    client: &Arc<Client>,
    cfg: &AgentConfig,
    migration_id: Uuid,
    instance_id: Uuid,
    dataset: &str,
    from_snap: Option<&str>,
    to_snap: &str,
    peer_endpoint: Option<&str>,
    peer_spki_sha256_hex: Option<&str>,
    ticket: Option<&str>,
) -> Result<u64> {
    // Snapshot the source dataset (idempotent; `zfs snapshot`
    // errors if the snapshot already exists, which a re-claimed
    // job after a crash hits; we ignore that specific error).
    if let Err(e) = zfs::snapshot_for_migration(
        dataset,
        to_snap.trim_start_matches(&format!("{dataset}@migration-")),
    )
    .await
    {
        let msg = e.to_string();
        if !msg.contains("dataset already exists") {
            bail!("snapshot {to_snap} failed: {msg}");
        }
        info!(
            %migration_id, %to_snap,
            "migrate-zfs-send: snapshot already exists; reusing",
        );
    }

    let peer_endpoint =
        peer_endpoint.ok_or_else(|| anyhow!("Source-role MigrateZfsSend missing peer_endpoint"))?;
    let peer_spki = peer_spki_sha256_hex
        .ok_or_else(|| anyhow!("Source-role MigrateZfsSend missing peer_spki_sha256_hex"))?;
    let ticket = ticket.ok_or_else(|| anyhow!("Source-role MigrateZfsSend missing ticket"))?;

    let server_uuid = Uuid::parse_str(&cfg.agent_id)
        .context("agent_id is not a UUID; cannot present source_cn in dial")?;

    // Each dataset in the tree is sent on its own connection as a
    // flattened (non-`-R`) stream, parent first so the target's receive
    // parent always exists. `dataset`/`to_snap`/`from_snap` name the
    // tree root; per dataset we keep the snapshot suffix and swap the
    // dataset name.
    let datasets = zfs::list_migration_tree(dataset)
        .await
        .context("enumerate source dataset tree")?;
    let to_suffix = to_snap
        .split_once('@')
        .map(|(_, s)| s.to_string())
        .ok_or_else(|| anyhow!("to_snap {to_snap} has no @snapshot component"))?;
    let from_suffix = match from_snap {
        Some(f) => Some(
            f.split_once('@')
                .map(|(_, s)| s.to_string())
                .ok_or_else(|| anyhow!("from_snap {f} has no @snapshot component"))?,
        ),
        None => None,
    };

    // Advisory: per-dataset dry runs summed; a failure degrades the
    // progress feed to byte counts without a percentage, never the
    // transfer.
    let mut total_estimate = 0u64;
    let mut have_estimate = true;
    let mut plan: Vec<(String, String, Option<String>)> = Vec::with_capacity(datasets.len());
    for ds in &datasets {
        let ds_to = format!("{ds}@{to_suffix}");
        let ds_from = from_suffix.as_ref().map(|s| format!("{ds}@{s}"));
        if have_estimate {
            match zfs::estimate_send_bytes(ds_from.as_deref(), &ds_to).await {
                Ok(bytes) => total_estimate += bytes,
                Err(e) => {
                    warn!(
                        %migration_id, %ds_to, error = %format!("{e:#}"),
                        "migrate-zfs-send: size estimate failed; progress without a total",
                    );
                    have_estimate = false;
                }
            }
        }
        plan.push((ds.clone(), ds_to, ds_from));
    }

    let reporter = migrate_progress::ProgressReporter::start(
        Arc::clone(client),
        migration_id,
        have_estimate.then_some(total_estimate),
        format!("zfs send {dataset} (flattened tree)"),
    );
    let counter = reporter.observer();

    let mut total_bytes = 0u64;
    for (ds, ds_to, ds_from) in &plan {
        let transport = migrate::dial_zfs(migrate::DialZfsParams {
            base_url: peer_endpoint.to_string(),
            migration_id,
            source_cn: server_uuid,
            vm_uuid: instance_id,
            target_dataset: ds.clone(),
            ticket: ticket.to_string(),
            target_spki_sha256_hex: peer_spki.to_string(),
        })
        .await
        .with_context(|| format!("dial target /migrate/{{id}}/zfs for {ds}"))?;

        let mut child = match ds_from {
            Some(from) => zfs::spawn_send_incremental(from, ds_to)
                .context("spawn zfs send -i for incremental")?,
            None => zfs::spawn_send_full(ds_to).context("spawn zfs send for full")?,
        };
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("zfs send child has no piped stdout"))?;
        let base = total_bytes;
        let counter = Arc::clone(&counter);
        let sender = tritond_vmm_migrate::zfs_stream::ZfsSender::new(transport, stdout)
            .with_progress(move |n| {
                counter.store(base + n, std::sync::atomic::Ordering::Relaxed);
            });
        let bytes = sender
            .run()
            .await
            .with_context(|| format!("ZfsSender::run for {ds}"))?;
        let status = child.wait().await.context("await zfs send exit")?;
        if !status.success() {
            bail!("zfs send {ds_to} exited non-zero: {status}");
        }
        total_bytes += bytes;
    }
    reporter.finish().await;
    info!(
        %migration_id, %instance_id, %dataset, datasets = plan.len(), bytes = total_bytes,
        "migrate-zfs-send/source: flattened tree streamed",
    );
    Ok(total_bytes)
}

/// Report a detached job's terminal outcome. Bounded retry: the
/// poll loop's completions ride its claim/complete error path,
/// but a detached task has no caller to retry for it, and losing
/// the completion of a multi-hour stream means the stale-claim
/// sweeper re-queues the entire transfer. A short retry rides out
/// a tritond restart.
async fn report_completion(
    client: &Client,
    job_id: Uuid,
    outcome: JobOutcome,
    result: Option<serde_json::Value>,
) {
    const ATTEMPTS: u32 = 5;
    const BACKOFF: std::time::Duration = std::time::Duration::from_secs(10);
    for attempt in 1..=ATTEMPTS {
        match client
            .agent_complete_job()
            .job_id(job_id)
            .body(CompleteJobRequest {
                outcome: outcome.clone(),
                result: result.clone(),
            })
            .send()
            .await
        {
            Ok(updated) => {
                let updated = updated.into_inner();
                info!(
                    job_id = %updated.id,
                    status = ?updated.status,
                    "migration data plane: completed job",
                );
                return;
            }
            Err(e) if attempt < ATTEMPTS => {
                warn!(
                    %job_id, attempt, error = %e,
                    "migration data plane: completion report failed; retrying",
                );
                tokio::time::sleep(BACKOFF).await;
            }
            Err(e) => {
                error!(
                    %job_id, error = %e,
                    "migration data plane: completion report failed; giving up \
                     (the stale-claim sweeper will re-queue the job)",
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zfs_send(role: MigrationJobRole) -> JobKind {
        JobKind::MigrateZfsSend {
            migration_id: Uuid::new_v4(),
            instance_id: Uuid::new_v4(),
            role,
            dataset: "zones/x".to_string(),
            from_snap: None,
            to_snap: "zones/x@migration-base".to_string(),
            peer_endpoint: None,
            peer_spki_sha256_hex: None,
            ticket: None,
        }
    }

    #[test]
    fn failure_result_extracts_stream_report_through_context() {
        let report = serde_json::json!({ "last_phase": "ram_push" });
        let err = anyhow::Error::new(migrate_vmm::StreamFailed {
            reason: "vmm stream failed: boom".to_string(),
            report: report.clone(),
        })
        .context("outer context must not hide the report");
        assert_eq!(failure_result(&err), Some(report));
        assert_eq!(failure_result(&anyhow::anyhow!("plain failure")), None);
    }

    #[test]
    fn data_plane_kinds_are_source_streams_and_target_listen() {
        assert!(is_data_plane_kind(&zfs_send(MigrationJobRole::Source)));
        // The target side of a ZFS pair is a no-op (the listener
        // does the work) and must stay on the fast inline path.
        assert!(!is_data_plane_kind(&zfs_send(MigrationJobRole::Target)));
        assert!(is_data_plane_kind(&JobKind::MigrateTargetListen {
            migration_id: Uuid::new_v4(),
            instance_id: Uuid::new_v4(),
        }));
        assert!(!is_data_plane_kind(&JobKind::Stop {
            instance_id: Uuid::new_v4(),
        }));
        assert!(!is_data_plane_kind(&JobKind::MigratePauseSource {
            migration_id: Uuid::new_v4(),
            instance_id: Uuid::new_v4(),
        }));
    }
}
