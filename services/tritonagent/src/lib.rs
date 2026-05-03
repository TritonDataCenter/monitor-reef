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

pub mod credentials;
pub mod images;
pub mod platform;
pub mod registration;
pub mod vmadm;
pub mod zfs;

use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use tracing::{error, info, warn};
use tritond_client::Client;
use tritond_client::types::{
    ClaimJobRequest, CompleteJobRequest, ImageCompatibility, JobKind, JobOutcome, ProvisioningJob,
};

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
    /// When `true`, the agent fetches the blueprint and logs it
    /// but does NOT call `vmadm`; every job reports `Completed`
    /// regardless. Used for transport-only smoke testing on hosts
    /// without SmartOS (e.g. the dev laptop). Defaults to `false`
    /// so the production path is the obvious default.
    pub dry_run: bool,
}

impl AgentConfig {
    /// Build a [`Client`] with a default `Authorization: Bearer …`
    /// header set from the API key. Returns an error if `endpoint`
    /// or `api_key` is malformed.
    ///
    /// We pre-configure rustls with the bundled `webpki_roots`
    /// trust store rather than letting reqwest call the platform
    /// verifier — SmartOS global zones have no system CA bundle,
    /// and the agent is expected to ship as a self-contained
    /// binary regardless of the host's OpenSSL/NSS layout.
    pub fn build_client(&self) -> Result<Client> {
        let mut headers = reqwest::header::HeaderMap::new();
        let value = reqwest::header::HeaderValue::from_str(&format!("Bearer {}", self.api_key))
            .context("api_key contains characters that are invalid in an HTTP header")?;
        headers.insert(reqwest::header::AUTHORIZATION, value);

        let mut root_store = rustls::RootCertStore::empty();
        root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let tls = rustls::ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth();

        let http = reqwest::Client::builder()
            .default_headers(headers)
            .use_preconfigured_tls(tls)
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
        dry_run = cfg.dry_run,
        "tritonagent starting",
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
/// processed (regardless of whether the work succeeded — failures
/// are reported via `JobOutcome::Failed`), `Ok(false)` if the
/// queue was empty.
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

    let outcome = match drive_job(client, cfg, &job).await {
        Ok(()) => JobOutcome::Completed,
        Err(reason) => {
            // Agent-side failures are reported back to tritond so
            // the operator sees the cause in `tcadm jobs get` (a
            // future slice) and the audit chain. The agent does
            // not retry — operators retry by issuing the
            // originating action again.
            //
            // `{:#}` renders the full anyhow chain on one line
            // (top message + each `with_context` cause), which is
            // what the operator and the audit chain need to
            // diagnose without an interactive shell on the agent.
            let chain = format!("{reason:#}");
            error!(job_id = %job.id, error = %chain, "job failed; reporting to tritond");
            JobOutcome::Failed(chain)
        }
    };

    let updated = client
        .agent_complete_job()
        .job_id(job.id)
        .body(CompleteJobRequest { outcome })
        .send()
        .await
        .context("agent_complete_job")?
        .into_inner();
    info!(
        job_id = %updated.id,
        status = ?updated.status,
        "completed job",
    );
    Ok(true)
}

/// Drive a single claimed job to a terminal state. Returns
/// `Ok(())` for success (caller reports `Completed`), `Err` for
/// agent-side failure (caller reports `Failed { reason }`).
async fn drive_job(client: &Client, cfg: &AgentConfig, job: &ProvisioningJob) -> Result<()> {
    info!(
        job_id = %job.id,
        kind = ?job.kind,
        seq = job.seq,
        agent_id = %cfg.agent_id,
        "claimed job",
    );

    let blueprint = client
        .agent_job_blueprint()
        .job_id(job.id)
        .send()
        .await
        .context("agent_job_blueprint")?
        .into_inner();

    if cfg.dry_run {
        info!(
            job_id = %job.id,
            "dry-run mode: skipping vmadm; reporting Completed",
        );
        return Ok(());
    }

    // The match is intentionally exhaustive (no `_` arm). The
    // tritond-store `JobKind` is `#[non_exhaustive]` but
    // Progenitor strips that on the client side, so when a future
    // tritond slice adds a new variant the regenerated client
    // will force this match to grow — which is the right place
    // for the agent author to make the "do I support this yet?"
    // call. A runtime "unsupported" surprise here would be
    // strictly worse.
    match &job.kind {
        JobKind::Provision(instance_id) => {
            // The instance must still exist — a concurrent operator
            // delete races to None.
            if blueprint.instance.is_none() {
                anyhow::bail!(
                    "instance {instance_id} no longer exists; refusing to provision a phantom"
                );
            }
            // Make sure the boot image is on this host before
            // we hand off to vmadm. `ensure` is idempotent — on
            // hosts that already have the dataset it returns
            // immediately. On a fresh host the first instance
            // pays the download + zfs-recv cost; subsequent
            // instances clone the snapshot for ~free.
            let image = blueprint
                .image
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Provision blueprint has no image"))?;
            // Compatibility gate: refuse the provision if the
            // image declares a min_smartos_platform newer than
            // this host. Image records minted via the legacy
            // (non-bundle) image-create path have
            // `compatibility = None` and skip the check —
            // matches the behaviour from before slice B.
            if let Some(compat) = image.compatibility.as_ref() {
                check_image_compatibility(compat).await?;
            }
            images::ensure(image)
                .await
                .context("ensure image content on host")?;
            vmadm::create_zone(&blueprint).await?;
        }
        JobKind::Stop(instance_id) => {
            vmadm::stop_zone(*instance_id).await?;
        }
        JobKind::Restart(instance_id) => {
            vmadm::reboot_zone(*instance_id).await?;
        }
        JobKind::Delete(instance_id) => {
            // The blueprint won't have an `instance` for Delete
            // jobs (tritond's record is already cleared); the
            // agent acts on the kind alone. `delete_zone` is
            // idempotent against zone-not-found.
            vmadm::delete_zone(*instance_id).await?;
        }
    }

    Ok(())
}

/// Refuse a Provision when the host can't satisfy the image's
/// declared compatibility constraints. Returns `Ok(())` when
/// the host meets every constraint; `Err` otherwise — the
/// caller wraps the error into `JobOutcome::Failed` so the
/// operator sees a clear reason in the audit chain.
///
/// Phase 0 enforces:
///
/// * `min_smartos_platform` — host's `uname -v` buildstamp
///   must lex-compare `>=` the image's minimum.
///
/// `compatibility.brand` is *not* enforced here because the
/// agent's vmadm payload always uses the brand the image
/// declares (`joyent-minimal`); a mismatch between the
/// image's brand and what vmadm would accept fails inside
/// `vmadm create` itself. A future slice that lets operators
/// pick the instance brand independently of the image will
/// add the brand check too.
async fn check_image_compatibility(compat: &ImageCompatibility) -> Result<()> {
    let Some(min_required) = compat.min_smartos_platform.as_deref() else {
        return Ok(());
    };
    let host = platform::host_platform_buildstamp()
        .await
        .context("read host platform buildstamp")?;
    if host.as_str() < min_required {
        return Err(anyhow!(
            "host platform {host} is older than image's min_smartos_platform {min_required}",
        ));
    }
    info!(
        host = %host,
        min_required,
        "host platform satisfies image compatibility",
    );
    Ok(())
}
