// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Triton Cloud per-CN provisioning agent.
//!
//! Polls tritond's `/v2/agent/jobs/claim` endpoint, drives each
//! claimed [`ProvisioningJob`] to a terminal state, and reports
//! the outcome via `/v2/agent/jobs/{id}/complete`.
//!
//! ## Phase 0 stub mode
//!
//! This v0 *does not* touch SmartOS. It logs each claimed job and
//! reports `JobOutcome::Completed` immediately. The point of v0 is
//! to validate the agent transport seam end-to-end (auth, claim,
//! complete, audit emission) before adding `vmadm`/`imgadm`/OPTE
//! integration risk in a follow-on slice.
//!
//! ## Authentication
//!
//! The agent presents an API key (`tcadm_…` wire-form) minted with
//! [`ApiKeyScope::Agent`] from the operator-CLI. The scope check on
//! tritond's side gates the key to *only* `agent_claim` and
//! `agent_complete` — even if the underlying user is root, this
//! key cannot read tenant resources or audit events. The audit
//! chain captures both the key's owner *and* the agent's
//! self-reported `claimed_by` identifier.
//!
//! [`ApiKeyScope::Agent`]: tritond_client::types::ApiKeyScope::Agent
//! [`ProvisioningJob`]: tritond_client::types::ProvisioningJob

use std::time::Duration;

use anyhow::{Context, Result};
use tracing::{info, warn};
use tritond_client::Client;
use tritond_client::types::{ClaimJobRequest, CompleteJobRequest, JobOutcome, ProvisioningJob};

/// Configuration for an [`Agent`] run.
#[derive(Debug, Clone)]
pub struct AgentConfig {
    /// Tritond endpoint, e.g. `http://10.199.199.10:8080`.
    pub endpoint: String,
    /// `tcadm_…` API key minted with `ApiKeyScope::Agent`.
    pub api_key: String,
    /// Self-reported agent identity. Recorded as `claimed_by` on
    /// each job and rolled into the tritond-side audit event so
    /// concurrent agents can be told apart.
    pub agent_id: String,
    /// Sleep between empty-queue polls.
    pub poll_interval: Duration,
    /// Phase 0 stub: simulate work for this duration before
    /// reporting `Completed`. Set near zero for tests; default
    /// 50ms keeps the agent visible in logs without slowing the
    /// queue.
    pub stub_work_duration: Duration,
}

impl AgentConfig {
    /// Build a [`Client`] with a default `Authorization: Bearer …`
    /// header set from the API key. Returns an error if `endpoint`
    /// or `api_key` is malformed.
    pub fn build_client(&self) -> Result<Client> {
        let mut headers = reqwest::header::HeaderMap::new();
        let value = reqwest::header::HeaderValue::from_str(&format!("Bearer {}", self.api_key))
            .context("api_key contains characters that are invalid in an HTTP header")?;
        headers.insert(reqwest::header::AUTHORIZATION, value);
        let http = reqwest::Client::builder()
            .default_headers(headers)
            .build()
            .context("build reqwest client")?;
        Ok(Client::new_with_client(&self.endpoint, http))
    }
}

/// Run the agent loop forever. Returns only on a fatal error.
pub async fn run(cfg: AgentConfig) -> Result<()> {
    let client = cfg.build_client()?;
    info!(
        agent_id = %cfg.agent_id,
        endpoint = %cfg.endpoint,
        poll_interval_ms = cfg.poll_interval.as_millis(),
        "tritonagent stub starting; will mark every claimed job Completed",
    );

    loop {
        match poll_once(&client, &cfg).await {
            Ok(true) => {
                // Worked a job; immediately try the next one — the
                // queue may have more.
            }
            Ok(false) => {
                tokio::time::sleep(cfg.poll_interval).await;
            }
            Err(e) => {
                // Transient error against tritond. Back off so a
                // dead control plane doesn't spin the agent.
                warn!(error = %e, "claim/complete cycle failed; backing off");
                tokio::time::sleep(cfg.poll_interval * 2).await;
            }
        }
    }
}

/// Run one claim+complete cycle. Returns `Ok(true)` if a job was
/// processed, `Ok(false)` if the queue was empty.
async fn poll_once(client: &Client, cfg: &AgentConfig) -> Result<bool> {
    let claimed = client
        .agent_claim_job()
        .body(ClaimJobRequest {
            claimed_by: cfg.agent_id.clone(),
        })
        .send()
        .await
        .context("agent_claim_job")?
        .into_inner();

    let Some(job) = claimed.job else {
        return Ok(false);
    };

    handle_job(client, cfg, &job).await?;
    Ok(true)
}

/// Phase 0 stub: log the claim, simulate brief work, report
/// success. The signature matches the eventual real handler so a
/// future slice that adds `vmadm`/`imgadm` integration only
/// substitutes the body.
async fn handle_job(client: &Client, cfg: &AgentConfig, job: &ProvisioningJob) -> Result<()> {
    info!(
        job_id = %job.id,
        kind = ?job.kind,
        seq = job.seq,
        agent_id = %cfg.agent_id,
        "claimed job (stub: marking Completed without acting)",
    );
    if !cfg.stub_work_duration.is_zero() {
        tokio::time::sleep(cfg.stub_work_duration).await;
    }
    let updated = client
        .agent_complete_job()
        .job_id(job.id)
        .body(CompleteJobRequest {
            outcome: JobOutcome::Completed,
        })
        .send()
        .await
        .context("agent_complete_job")?
        .into_inner();
    info!(
        job_id = %updated.id,
        status = ?updated.status,
        "completed job",
    );
    Ok(())
}
