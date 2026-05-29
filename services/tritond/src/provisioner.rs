// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Phase 0 in-process stub provisioner.
//!
//! `tritond` enqueues a [`tritond_store::ProvisioningJob`] every time
//! an operator action requires the kind of work a per-CN agent
//! (`tritonagent`, future) would do — instance create, start, stop,
//! restart. Phase 0 has no real agent, so this module spawns a
//! tokio task that consumes the queue and drives the lifecycle
//! state machine forward by calling
//! [`Store::transition_instance_lifecycle`] directly.
//!
//! When the real `tritonagent` lands, the protocol stays the same:
//! tritond writes jobs, the agent claims and acks them. The
//! swap-out point is exactly this module — replace the in-process
//! task with the agent's HTTP/RPC poll loop.
//!
//! ## Design notes
//!
//! * The task is spawned at server startup and runs until the
//!   tokio runtime is dropped (typical test fixture lifecycle).
//!   No graceful shutdown protocol; tests rely on the per-test
//!   runtime drop.
//! * The poll interval is short (50ms) so integration tests don't
//!   spend most of their wall clock waiting for the queue to
//!   drain. Production deploys with a real agent will not run
//!   this stub.
//! * Each job runs the canonical agent flow: claim → drive
//!   transitions → complete. Failures are recorded as
//!   `JobOutcome::Failed { reason }` and surface in the audit /
//!   `tcadm jobs get` paths (the latter is a future slice).
//! * The lifecycle CAS surfaces conflicts (e.g. operator
//!   transitioned the instance out from under us) as job failures
//!   rather than panics; the operator can retry the originating
//!   action.

use std::sync::Arc;
use std::time::Duration;

use tracing::{debug, warn};
use tritond_store::{
    JobKind, JobOutcome, LifecycleState, LifecycleStateKind, ProvisioningJob, Store, StoreError,
};
use uuid::Uuid;

/// Identifier the stub uses when claiming jobs. Visible in
/// `ProvisioningJob.claimed_by`; useful for telemetry once the
/// real `tritonagent` lands and we want to distinguish stub
/// completions from agent completions.
pub const STUB_AGENT_ID: &str = "stub-provisioner";

/// Poll interval when the queue is empty. Tuned for integration
/// tests; production with a real agent will not run the stub.
const EMPTY_QUEUE_POLL: Duration = Duration::from_millis(50);

/// Backoff when the store itself errors (FDB unavailable, etc.).
/// Longer than [`EMPTY_QUEUE_POLL`] so we don't hot-loop on
/// transient failures.
const ERROR_BACKOFF: Duration = Duration::from_millis(500);

/// Spawn the stub provisioner. The returned [`tokio::task::JoinHandle`]
/// is detached for typical use; the task exits when the tokio
/// runtime shuts down.
pub fn spawn(store: Arc<dyn Store>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(run(store))
}

async fn run(store: Arc<dyn Store>) {
    loop {
        // The in-process stub is unbound, so it only sees
        // unrouted (target_cn_uuid = None) jobs. Routed jobs
        // wait for their target CN's bound agent.
        match store.claim_next_job(STUB_AGENT_ID, None).await {
            Ok(job) => {
                let job_id = job.id;
                let outcome = process(&job, &store).await;
                if let Err(e) = store.complete_job(job_id, outcome).await {
                    warn!(%job_id, error = %e, "complete_job failed");
                }
            }
            Err(StoreError::NotFound) => {
                tokio::time::sleep(EMPTY_QUEUE_POLL).await;
            }
            Err(e) => {
                warn!(error = %e, "claim_next_job failed");
                tokio::time::sleep(ERROR_BACKOFF).await;
            }
        }
    }
}

async fn process(job: &ProvisioningJob, store: &Arc<dyn Store>) -> JobOutcome {
    let job_id = job.id;
    debug!(%job_id, kind = ?job.kind, "stub-provisioner claimed job");
    let result = match &job.kind {
        JobKind::Provision { instance_id } => provision(*instance_id, store).await,
        JobKind::Start { instance_id } => start(*instance_id, store).await,
        JobKind::Stop { instance_id } => stop(*instance_id, store).await,
        JobKind::Restart { instance_id } => restart(*instance_id, store).await,
        // The stub has no SmartOS to talk to, so a Delete job is
        // a no-op success — tritond's record is already gone by
        // the time the job is enqueued, and there is no zone to
        // destroy under the stub.
        JobKind::Delete { .. } => Ok(()),
        JobKind::EdgeApply {
            edge_instance_id, ..
        } => Err(format!(
            "edge apply job {edge_instance_id} requires a bound tritonagent"
        )),
        JobKind::EdgeReap { edge_instance_id } => Err(format!(
            "edge reap job {edge_instance_id} requires a bound tritonagent"
        )),
        // `JobKind` is `#[non_exhaustive]`; future variants will need
        // their own arms before the queue can usefully process them.
        _ => Err(format!("unsupported job kind: {:?}", job.kind)),
    };
    match result {
        Ok(()) => {
            debug!(%job_id, "stub-provisioner job completed");
            JobOutcome::Completed
        }
        Err(reason) => {
            warn!(%job_id, %reason, "stub-provisioner job failed");
            JobOutcome::Failed { reason }
        }
    }
}

/// Drive Pending → Provisioning → Running. First-time create only;
/// powering on an existing stopped instance uses [`start`].
async fn provision(instance_id: Uuid, store: &Arc<dyn Store>) -> Result<(), String> {
    cas(
        store,
        instance_id,
        &[LifecycleStateKind::Pending],
        LifecycleState::Provisioning,
        "Pending->Provisioning",
    )
    .await?;
    cas(
        store,
        instance_id,
        &[LifecycleStateKind::Provisioning],
        LifecycleState::Running,
        "Provisioning->Running",
    )
    .await?;
    Ok(())
}

/// Drive Pending → Running for a `start`. The start handler has
/// already transitioned Stopped → Pending; booting an existing zone
/// never enters Provisioning, so the stub mirrors the agent and
/// lands Running directly.
async fn start(instance_id: Uuid, store: &Arc<dyn Store>) -> Result<(), String> {
    cas(
        store,
        instance_id,
        &[LifecycleStateKind::Pending],
        LifecycleState::Running,
        "Pending->Running (start)",
    )
    .await
}

/// Drive Stopping → Stopped. Caller (the stop handler) has already
/// transitioned Running → Stopping.
async fn stop(instance_id: Uuid, store: &Arc<dyn Store>) -> Result<(), String> {
    cas(
        store,
        instance_id,
        &[LifecycleStateKind::Stopping],
        LifecycleState::Stopped,
        "Stopping->Stopped",
    )
    .await
}

/// Drive a full restart cycle: Stopping → Pending → Provisioning →
/// Running. The restart handler has already transitioned Running →
/// Stopping; the agent owns the rest of the cycle.
async fn restart(instance_id: Uuid, store: &Arc<dyn Store>) -> Result<(), String> {
    cas(
        store,
        instance_id,
        &[LifecycleStateKind::Stopping],
        LifecycleState::Pending,
        "Stopping->Pending (restart)",
    )
    .await?;
    cas(
        store,
        instance_id,
        &[LifecycleStateKind::Pending],
        LifecycleState::Provisioning,
        "Pending->Provisioning (restart)",
    )
    .await?;
    cas(
        store,
        instance_id,
        &[LifecycleStateKind::Provisioning],
        LifecycleState::Running,
        "Provisioning->Running (restart)",
    )
    .await?;
    Ok(())
}

async fn cas(
    store: &Arc<dyn Store>,
    instance_id: Uuid,
    expected_from: &[LifecycleStateKind],
    to: LifecycleState,
    label: &str,
) -> Result<(), String> {
    store
        .transition_instance_lifecycle(instance_id, expected_from, to)
        .await
        .map(|_| ())
        .map_err(|e| format!("{label}: {e}"))
}
