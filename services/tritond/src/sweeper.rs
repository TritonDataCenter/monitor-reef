// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Stale-claim sweeper.
//!
//! A tokio task that periodically calls
//! [`tritond_store::Store::list_stale_claims`] and transitions
//! every returned `InProgress` job to terminal `Failed`. It
//! catches the failure mode where an agent claimed a job and
//! crashed (host reboot, network partition, OOM, …) before
//! reporting an outcome. Without the sweeper, the affected
//! instance lifecycle stays in `Provisioning` / `Stopping`
//! indefinitely and the job sits stuck in the queue.
//!
//! ## Why complete-with-Failed instead of resetting to Pending
//!
//! Resetting the job to Pending and re-queuing assumes the
//! agent's prior partial work is recoverable. For Provision
//! that's usually false — vmadm may have created a partial
//! zone, or the image fetch may be half-done. Marking the job
//! `Failed { reason }` and the instance lifecycle `Failed`
//! makes the situation visible to the operator, who can then
//! `tcadm instance delete --force` and re-issue the originating
//! action. Auto-retry without resync would risk silently
//! producing duplicate or half-broken zones.
//!
//! ## Configuration
//!
//! * `TRITOND_SWEEPER_INTERVAL_SECS` — how often the sweeper
//!   wakes (default 60).
//! * `TRITOND_STALE_CLAIM_THRESHOLD_SECS` — how old a claim must
//!   be before it's considered stale (default 600 = 10 min).
//! * `TRITOND_SAGA_RETENTION_SECS` — how long a terminal saga is
//!   kept in FDB before deletion (default 30 days). Stuck sagas are
//!   exempt; `stuck_reason` is operator-actionable.
//!
//! All env-driven so deployments tighten or loosen without a
//! rebuild.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use tracing::{debug, info, warn};
use tritond_audit::Outcome as AuditOutcome;
use tritond_saga::SagaExecutor;
use tritond_store::{JobOutcome, Store};

use crate::audit::AuditService;
use crate::auth::{Action, Principal};

/// Reason recorded on Failed outcomes when the sweeper
/// reaps a stale claim. Stable across versions because it
/// shows up in the audit chain and operator UX.
const SWEEPER_FAIL_REASON: &str = "agent claimed but never completed; reaped by sweeper";

/// Identifier the sweeper uses as the actor on audit events.
/// Visible in the audit chain so operators can tell sweeper-
/// driven completions apart from agent-driven ones.
const SWEEPER_ACTOR: &str = "tritond-sweeper";

/// Spawn the sweeper task. Returns a [`tokio::task::JoinHandle`]
/// for typical detached use; the task exits when the tokio
/// runtime shuts down.
pub fn spawn(
    store: Arc<dyn Store>,
    audit: Arc<AuditService>,
    saga: Arc<SagaExecutor>,
    interval: Duration,
    threshold: Duration,
    saga_retention: Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(run(store, audit, saga, interval, threshold, saga_retention))
}

async fn run(
    store: Arc<dyn Store>,
    audit: Arc<AuditService>,
    saga: Arc<SagaExecutor>,
    interval: Duration,
    threshold: Duration,
    saga_retention: Duration,
) {
    info!(
        interval_secs = interval.as_secs(),
        threshold_secs = threshold.as_secs(),
        saga_retention_secs = saga_retention.as_secs(),
        "stale-claim sweeper starting",
    );
    loop {
        tokio::time::sleep(interval).await;
        let cutoff = match chrono::Duration::from_std(threshold) {
            Ok(d) => Utc::now() - d,
            Err(e) => {
                warn!(error = %e, "stale-claim threshold doesn't fit chrono::Duration; skipping sweep");
                continue;
            }
        };
        match store.list_stale_claims(cutoff).await {
            Ok(stale) if stale.is_empty() => {
                debug!("no stale claims this sweep");
            }
            Ok(stale) => {
                info!(count = stale.len(), "sweeping stale claims");
                for job in stale {
                    sweep_one(&store, &audit, &job).await;
                }
            }
            Err(e) => {
                warn!(error = %e, "list_stale_claims failed; will retry next interval");
            }
        }
        // / SG-1: the same sweeper now picks up
        // sagas whose owning SEC's heartbeat is older than `cutoff`.
        // Reassignment CASes `current_sec` over and bumps
        // `current_epoch` (D-Sg-8), then resumes through Steno.
        // We reuse `cutoff` rather than introduce a separate
        // threshold (README open question, settled in code for SG-1
        // to match the existing tunable; SG-1b may split it).
        match saga.reassign_stale_sec_sagas(cutoff).await {
            Ok(0) => debug!("no stale-SEC sagas this sweep"),
            Ok(n) => info!(adopted = n, "tritond-saga: adopted stale-SEC sagas"),
            Err(e) => {
                warn!(error = %e, "tritond-saga: reassign_stale_sec_sagas failed; will retry next interval")
            }
        }
        // retention pass: drop every terminal saga
        // whose `time_done` is older than `saga_retention`. Stuck
        // sagas are exempt — those carry an operator-actionable
        // `stuck_reason` and stay until human cleanup.
        let saga_retention_cutoff = match chrono::Duration::from_std(saga_retention) {
            Ok(d) => Utc::now() - d,
            Err(e) => {
                warn!(error = %e, "saga retention doesn't fit chrono::Duration; skipping retention sweep");
                continue;
            }
        };
        match saga
            .prune_terminal_sagas_older_than(saga_retention_cutoff)
            .await
        {
            Ok(0) => debug!("no aged-out terminal sagas this sweep"),
            Ok(n) => info!(pruned = n, "tritond-saga: pruned aged-out terminal sagas"),
            Err(e) => {
                warn!(error = %e, "tritond-saga: prune_terminal_sagas_older_than failed; will retry next interval")
            }
        }
    }
}

async fn sweep_one(
    store: &Arc<dyn Store>,
    audit: &Arc<AuditService>,
    job: &tritond_store::ProvisioningJob,
) {
    let job_id = job.id;
    let outcome = JobOutcome::Failed {
        reason: SWEEPER_FAIL_REASON.to_string(),
    };
    let updated = match store.complete_job(job_id, outcome.clone(), None).await {
        Ok(j) => j,
        Err(e) => {
            warn!(%job_id, error = %e, "complete_job failed during sweep");
            return;
        }
    };
    info!(
        %job_id,
        kind = ?updated.kind,
        prior_claimed_by = ?updated.claimed_by,
        "stale claim reaped",
    );
    // Drive instance lifecycle the same way the HTTP handler
    // does on a Failed outcome — keeps the operator-visible
    // state consistent.
    crate::lifecycle::drive_lifecycle_for_complete(store.as_ref(), &updated, &outcome).await;
    // Emit an audit event so an operator reading the chain sees
    // the sweeper's action (distinct from a real agent's
    // Failed report).
    audit
        .record_mutation(
            &Principal::Anonymous,
            Action::AgentComplete,
            None,
            Some(format!("ProvisioningJob::\"{job_id}\"")),
            AuditOutcome::Success {
                resource: Some(format!("ProvisioningJob::\"{job_id}\"")),
            },
            serde_json::json!({
                "job_id": job_id,
                "outcome": "failed_swept",
                "reason": SWEEPER_FAIL_REASON,
                "actor": SWEEPER_ACTOR,
                "prior_claimed_by": updated.claimed_by,
            }),
        )
        .await;
}
