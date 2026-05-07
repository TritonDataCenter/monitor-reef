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
//! ## Local host execution
//!
//! The agent is the only component that mutates CN-local runtime
//! state. Provision jobs drive image import, Proteus port realization,
//! and `vmadm`; edge jobs persist fhrun manifests under the configured
//! edge root and supervise the local firehyve/fhrun process. Dry-run
//! mode remains available for transport-only smoke testing.
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
pub mod edge;
pub mod images;
pub mod platform;
pub mod proteus;
pub mod registration;
pub mod status;
pub mod vmadm;
pub mod zfs;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use base64::Engine;
use proteus_api::blueprint::PortBlueprint;
use proteus_api::ids::PortId;
use tracing::{error, info, warn};
use tritond_client::Client;
use tritond_client::types::{
    AgentPortBlueprint, ClaimJobRequest, CompleteJobRequest, ImageCompatibility, JobKind,
    JobOutcome, NetworkRealizationRequest, NetworkResourceId, Nic, ProvisioningBlueprint,
    ProvisioningJob, RealizationStatus, RealizerId,
};
use tritond_cn_platform::cn_status::{
    DiskUsageSampler, Heartbeater, StatusCollector, UuidNamedImageFilter, ZoneeventWatcher,
};
use tritond_cn_platform::smartos::{KstatTool, VmadmTool, ZfsTool};

use crate::status::TritondStatusSink;

/// Default Proteus kernel device node on SmartOS.
pub const DEFAULT_PROTEUS_DEVICE: &str = "/dev/proteus";

/// Default root for fhrun/firehyve edge instance runtime state on a CN.
pub const DEFAULT_EDGE_ROOT: &str = "/var/lib/tritonagent/edge";

/// Default fhrun launcher path on SmartOS CNs.
pub const DEFAULT_FHRUN_BIN: &str = "/opt/firehyve/bin/fhrun";

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
    /// Proteus kernel device node. The real backend opens this on
    /// SmartOS; non-illumos builds require `dry_run` for provision work.
    pub proteus_dev: PathBuf,
    /// Root directory for per-edge-instance fhrun manifests, pid
    /// files, logs, and edge-control Unix sockets.
    pub edge_root: PathBuf,
    /// Path to the fhrun launcher used for `JobKind::EdgeApply`.
    pub fhrun_bin: PathBuf,
    /// When `true`, the agent fetches the blueprint and logs it
    /// but does NOT call `vmadm`; every job reports `Completed`
    /// regardless. Used for transport-only smoke testing on hosts
    /// without SmartOS (e.g. the dev laptop). Defaults to `false`
    /// so the production path is the obvious default.
    pub dry_run: bool,
    /// When `true` (the default), the agent spawns the harvested
    /// `cn_status::Heartbeater` alongside the job-claim loop and
    /// posts liveness + status to tritond's `/v2/agent/heartbeat`
    /// and `/v2/agent/status`. Disabled by `--no-heartbeater`
    /// for tritond integration tests that don't want background
    /// chatter at the test server.
    pub spawn_heartbeater: bool,
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
    let client = Arc::new(cfg.build_client()?);
    info!(
        agent_id = %cfg.agent_id,
        endpoint = %cfg.endpoint,
        poll_interval_ms = cfg.poll_interval.as_millis(),
        proteus_dev = %cfg.proteus_dev.display(),
        edge_root = %cfg.edge_root.display(),
        fhrun_bin = %cfg.fhrun_bin.display(),
        dry_run = cfg.dry_run,
        spawn_heartbeater = cfg.spawn_heartbeater,
        "tritonagent starting",
    );

    // Optional background publisher. Spawned only when the operator
    // hasn't asked us to stay quiet (the integration-test path).
    // Both handles must outlive the poll loop so that on shutdown
    // we can drain them gracefully — the heartbeater holds the
    // dirty flag the watcher pokes, and tearing them down out of
    // order risks a missed status sample.
    let mut publisher = if cfg.spawn_heartbeater {
        Some(spawn_publisher(Arc::clone(&client)))
    } else {
        None
    };

    let result = run_poll_loop(client.as_ref(), &cfg).await;

    if let Some(p) = publisher.take() {
        p.shutdown().await;
    }

    result
}

/// The job-claim loop, factored out so [`run`] can wrap it with the
/// publisher's lifetime without duplicating the poll/backoff logic.
///
/// Returns `Ok(())` only on a clean caller-initiated stop; today
/// nothing inside the loop can return a clean `Ok(())`, but the
/// signature matches `run` so future SIGTERM handling drops in
/// without a refactor.
async fn run_poll_loop(client: &Client, cfg: &AgentConfig) -> Result<()> {
    loop {
        match poll_once(client, cfg).await {
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

/// Owns the heartbeater + zoneevent watcher handles together so
/// `run` can shut them down in lock-step on exit.
struct PublisherHandles {
    heartbeater: tritond_cn_platform::cn_status::HeartbeaterHandle,
    zoneevent: ZoneeventWatcher,
}

impl PublisherHandles {
    /// Stop the watcher first (it can no longer poke a flag the
    /// heartbeater will consume), then wait for the heartbeater's
    /// in-flight tick to finish.
    async fn shutdown(self) {
        self.zoneevent.stop().await;
        self.heartbeater.shutdown().await;
    }
}

/// Build and spawn the heartbeater + zoneevent watcher pair.
///
/// The heartbeater owns the [`DirtyFlag`]; the watcher pokes it on
/// every zone state change so a status sample lands within the
/// 500ms `STATUS_CHECK_INTERVAL` rather than waiting up to 60s for
/// the next periodic max-tick.
///
/// On non-SmartOS hosts the zoneevent binary is missing — the
/// watcher's spawn loop logs a warning and retries every 30s,
/// which is the same behaviour the legacy cn-agent had on dev
/// laptops. We don't gate the watcher on platform detection
/// because the agent's only supported deployment target is
/// SmartOS; a missing binary is operator misconfiguration, not a
/// supported runtime mode.
fn spawn_publisher(client: Arc<Client>) -> PublisherHandles {
    let sink = TritondStatusSink::new(client);
    let vmadm = Arc::new(VmadmTool::new());
    let zfs = Arc::new(ZfsTool::new());
    let kstat = Arc::new(KstatTool::new());
    let disk_usage = DiskUsageSampler::new(Arc::clone(&zfs), Arc::new(UuidNamedImageFilter));
    let collector = StatusCollector::new(vmadm, zfs, kstat, disk_usage);

    let heartbeater = Heartbeater::new(Arc::new(sink), collector);
    // Capture the dirty flag BEFORE spawning — once `spawn()`
    // consumes the heartbeater there's no path back to its
    // internal flag. The watcher needs the same instance the
    // heartbeater is reading, so this ordering matters.
    let dirty = heartbeater.dirty_flag();
    let hb_handle = heartbeater.spawn();
    let zoneevent = ZoneeventWatcher::spawn(dirty);

    PublisherHandles {
        heartbeater: hb_handle,
        zoneevent,
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
        JobKind::Provision { instance_id } => {
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
            let proteus = open_proteus_lifecycle(&cfg.proteus_dev)?;
            let started_ports = realize_provision_ports(
                client,
                client,
                proteus.as_ref(),
                &cfg.agent_id,
                &blueprint,
            )
            .await?;
            if let Err(err) = vmadm::create_zone(&blueprint).await {
                cleanup_started_ports(proteus.as_ref(), &started_ports);
                return Err(err).context("create VM after Proteus port realization");
            }
        }
        JobKind::Stop { instance_id } => {
            vmadm::stop_zone(*instance_id).await?;
        }
        JobKind::Restart { instance_id } => {
            vmadm::reboot_zone(*instance_id).await?;
        }
        JobKind::Delete { instance_id } => {
            // The blueprint won't have an `instance` for Delete
            // jobs (tritond's record is already cleared); the
            // agent acts on the kind alone. `delete_zone` is
            // idempotent against zone-not-found.
            vmadm::delete_zone(*instance_id).await?;
        }
        JobKind::EdgeApply {
            edge_instance_id,
            manifest_bytes,
        } => {
            edge::apply(
                &cfg.edge_root,
                &cfg.fhrun_bin,
                *edge_instance_id,
                manifest_bytes,
            )
            .with_context(|| format!("apply edge instance {edge_instance_id}"))?;
        }
        JobKind::EdgeReap { edge_instance_id } => {
            edge::reap(&cfg.edge_root, *edge_instance_id)
                .with_context(|| format!("reap edge instance {edge_instance_id}"))?;
        }
    }

    Ok(())
}

/// Source of compiled Proteus port blueprints. Abstracted so the
/// provision ordering is testable without an HTTP server.
#[async_trait]
trait PortBlueprintSource {
    async fn fetch_port_blueprint(&self, port_id: uuid::Uuid) -> Result<PortBlueprint>;
}

#[async_trait]
impl PortBlueprintSource for Client {
    async fn fetch_port_blueprint(&self, port_id: uuid::Uuid) -> Result<PortBlueprint> {
        let response = self
            .agent_port_blueprint()
            .port_id(port_id)
            .send()
            .await
            .with_context(|| format!("fetch Proteus blueprint for port {port_id}"))?
            .into_inner();
        decode_agent_port_blueprint(response)
    }
}

/// Sink for realized network state. Kept separate from the source to
/// make failure-path tests simple and to keep HTTP mechanics out of
/// the lifecycle ordering.
#[async_trait]
trait NetworkRealizationSink {
    async fn report_network_realization(&self, request: NetworkRealizationRequest) -> Result<()>;
}

#[async_trait]
impl NetworkRealizationSink for Client {
    async fn report_network_realization(&self, request: NetworkRealizationRequest) -> Result<()> {
        self.agent_report_network_realization()
            .body(request)
            .send()
            .await
            .context("report network realization")?;
        Ok(())
    }
}

/// Minimal lifecycle surface the job driver needs from Proteus.
trait ProteusLifecycle: Send + Sync {
    fn ensure_started(
        &self,
        blueprint: &PortBlueprint,
        linkid: Option<u32>,
    ) -> Result<proteus::ProteusPortStatus>;

    fn cleanup_port(&self, port_id: PortId) -> Result<()>;
}

impl<T> ProteusLifecycle for proteus::ProteusClient<T>
where
    T: proteus_ioctl::Transport,
{
    fn ensure_started(
        &self,
        blueprint: &PortBlueprint,
        linkid: Option<u32>,
    ) -> Result<proteus::ProteusPortStatus> {
        proteus::ProteusClient::ensure_started(self, blueprint, linkid)
    }

    fn cleanup_port(&self, port_id: PortId) -> Result<()> {
        proteus::ProteusClient::cleanup_port(self, port_id)
    }
}

fn open_proteus_lifecycle(path: &Path) -> Result<Box<dyn ProteusLifecycle>> {
    #[cfg(target_os = "illumos")]
    {
        let transport = proteus_ioctl::KernelTransport::open_path(path)
            .with_context(|| format!("open Proteus device {}", path.display()))?;
        Ok(Box::new(proteus::ProteusClient::new(transport)))
    }
    #[cfg(not(target_os = "illumos"))]
    {
        let _ = path;
        bail!(
            "Proteus kernel transport is only available on illumos; use --dry-run on non-SmartOS hosts"
        );
    }
}

async fn realize_provision_ports<S, R, P>(
    source: &S,
    sink: &R,
    proteus: &P,
    agent_id: &str,
    blueprint: &ProvisioningBlueprint,
) -> Result<Vec<PortId>>
where
    S: PortBlueprintSource + Sync,
    R: NetworkRealizationSink + Sync,
    P: ProteusLifecycle + ?Sized,
{
    let realizer = cn_realizer(agent_id)?;
    let mut started_ports = Vec::with_capacity(blueprint.nics.len());

    for nic in &blueprint.nics {
        let port_blueprint = match source.fetch_port_blueprint(nic.id).await {
            Ok(blueprint) => blueprint,
            Err(err) => {
                cleanup_started_ports(proteus, &started_ports);
                return Err(err)
                    .with_context(|| format!("fetch Proteus port blueprint for NIC {}", nic.id));
            }
        };
        let port_id = port_blueprint.port_id;
        let desired_generation = port_blueprint.generation.0;
        let resource = port_realization_resource(nic);

        match proteus.ensure_started(&port_blueprint, None) {
            Ok(status) => {
                let applied_generation = status.generation.applied_generation.0;
                let request = NetworkRealizationRequest {
                    resource,
                    realizer: realizer.clone(),
                    generation: applied_generation,
                    status: RealizationStatus::Applied,
                    message: Some(format!(
                        "Proteus port {port_id} applied generation {applied_generation}"
                    )),
                };
                if let Err(err) = sink.report_network_realization(request).await {
                    let mut cleanup = started_ports.clone();
                    cleanup.push(port_id);
                    cleanup_started_ports(proteus, &cleanup);
                    return Err(err).with_context(|| {
                        format!("report applied Proteus realization for NIC {}", nic.id)
                    });
                }
                info!(
                    nic_id = %nic.id,
                    port_id = %port_id,
                    desired_generation,
                    applied_generation,
                    "Proteus port realized",
                );
                started_ports.push(port_id);
            }
            Err(err) => {
                report_failed_realization(
                    sink,
                    realizer.clone(),
                    resource,
                    desired_generation,
                    format!("Proteus port {port_id} failed: {err:#}"),
                )
                .await;
                cleanup_started_ports(proteus, &started_ports);
                return Err(err)
                    .with_context(|| format!("realize Proteus port {port_id} for NIC {}", nic.id));
            }
        }
    }

    Ok(started_ports)
}

async fn report_failed_realization<R>(
    sink: &R,
    realizer: RealizerId,
    resource: NetworkResourceId,
    generation: u64,
    message: String,
) where
    R: NetworkRealizationSink + Sync,
{
    let request = NetworkRealizationRequest {
        resource,
        realizer,
        generation,
        status: RealizationStatus::Failed,
        message: Some(message),
    };
    if let Err(err) = sink.report_network_realization(request).await {
        warn!(error = %err, "failed to report Proteus realization failure");
    }
}

fn cleanup_started_ports<P>(proteus: &P, ports: &[PortId])
where
    P: ProteusLifecycle + ?Sized,
{
    for port_id in ports.iter().rev() {
        if let Err(err) = proteus.cleanup_port(*port_id) {
            warn!(port_id = %port_id, error = %err, "failed to clean up Proteus port");
        }
    }
}

fn decode_agent_port_blueprint(response: AgentPortBlueprint) -> Result<PortBlueprint> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&response.blueprint_postcard_base64)
        .with_context(|| format!("decode port {} Proteus blueprint base64", response.port_id))?;
    let blueprint: PortBlueprint = postcard::from_bytes(&bytes).with_context(|| {
        format!(
            "decode port {} Proteus blueprint postcard",
            response.port_id
        )
    })?;
    if blueprint.port_id.0 != response.port_id {
        bail!(
            "tritond returned port blueprint {} for requested port {}",
            blueprint.port_id,
            response.port_id,
        );
    }
    if blueprint.generation.0 != response.generation {
        bail!(
            "tritond returned generation {} for port {}, but encoded blueprint has generation {}",
            response.generation,
            response.port_id,
            blueprint.generation.0,
        );
    }
    Ok(blueprint)
}

fn cn_realizer(agent_id: &str) -> Result<RealizerId> {
    let id = agent_id
        .parse()
        .with_context(|| format!("agent_id {agent_id:?} is not a CN UUID"))?;
    Ok(RealizerId::Cn(id))
}

fn port_realization_resource(nic: &Nic) -> NetworkResourceId {
    // The precise affected-resource list belongs to the compiler
    // contract. Until that lands, the agent reports the per-port
    // generation against the enclosing VPC so tritond has a durable
    // CN realization row for M1 debugging.
    NetworkResourceId::Vpc(nic.vpc_id)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    use chrono::Utc;
    use proteus_api::blueprint::{
        BlueprintApplyStatus, ClientLinkConfig, PORT_BLUEPRINT_SCHEMA_V0, PluginConfigBytes,
        PortLimits, PortState,
    };
    use proteus_api::ids::{Generation, NetworkId};
    use proteus_ioctl::FakeTransport;
    use uuid::Uuid;

    struct StaticPortBlueprintSource {
        by_port: HashMap<Uuid, PortBlueprint>,
    }

    #[async_trait]
    impl PortBlueprintSource for StaticPortBlueprintSource {
        async fn fetch_port_blueprint(&self, port_id: Uuid) -> Result<PortBlueprint> {
            self.by_port
                .get(&port_id)
                .cloned()
                .ok_or_else(|| anyhow!("missing test blueprint for port {port_id}"))
        }
    }

    #[derive(Default)]
    struct RecordingRealizationSink {
        reports: Mutex<Vec<NetworkRealizationRequest>>,
    }

    #[async_trait]
    impl NetworkRealizationSink for RecordingRealizationSink {
        async fn report_network_realization(
            &self,
            request: NetworkRealizationRequest,
        ) -> Result<()> {
            self.reports.lock().unwrap().push(request);
            Ok(())
        }
    }

    fn sample_port_blueprint(port_id: Uuid, generation: u64) -> PortBlueprint {
        PortBlueprint {
            port_id: PortId(port_id),
            network_id: NetworkId::TRITON_VPC,
            schema_version: PORT_BLUEPRINT_SCHEMA_V0,
            generation: Generation::new(generation),
            limits: PortLimits::DEFAULT,
            link: ClientLinkConfig {
                mtu: 1500,
                mac_address: Some([0x02, 0x00, 0x00, 0xde, 0xad, 0x01]),
                vlan_id: None,
            },
            plugin_config: PluginConfigBytes::new(NetworkId::TRITON_VPC, 1, Vec::new()),
        }
    }

    fn sample_nic(nic_id: Uuid, vpc_id: Uuid) -> Nic {
        Nic {
            id: nic_id,
            tenant_id: Uuid::new_v4(),
            project_id: Uuid::new_v4(),
            instance_id: Uuid::new_v4(),
            vpc_id,
            subnet_id: Uuid::new_v4(),
            name: "primary".to_string(),
            mac: "02:00:00:de:ad:01".to_string(),
            primary_ipv4: None,
            primary_ipv6: None,
            created_at: Utc::now(),
        }
    }

    fn sample_provisioning_blueprint(nic: Nic) -> ProvisioningBlueprint {
        ProvisioningBlueprint {
            job_id: Uuid::new_v4(),
            kind: JobKind::Provision {
                instance_id: nic.instance_id,
            },
            instance: None,
            image: None,
            nics: vec![nic],
            disks: Vec::new(),
            ssh_public_keys: Vec::new(),
        }
    }

    #[test]
    fn decode_agent_port_blueprint_rejects_mismatched_encoded_port() {
        let encoded_port_id = Uuid::new_v4();
        let response_port_id = Uuid::new_v4();
        let encoded = sample_port_blueprint(encoded_port_id, 7);
        let bytes = postcard::to_allocvec(&encoded).unwrap();
        let response = AgentPortBlueprint {
            port_id: response_port_id,
            generation: 7,
            blueprint_postcard_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
        };

        let err = decode_agent_port_blueprint(response).unwrap_err();

        assert!(err.to_string().contains("tritond returned port blueprint"));
    }

    #[tokio::test]
    async fn realize_provision_ports_applies_port_and_reports_vpc_generation() {
        let cn_id = Uuid::new_v4();
        let nic_id = Uuid::new_v4();
        let vpc_id = Uuid::new_v4();
        let nic = sample_nic(nic_id, vpc_id);
        let provision = sample_provisioning_blueprint(nic);
        let port = sample_port_blueprint(nic_id, 4);
        let source = StaticPortBlueprintSource {
            by_port: HashMap::from([(nic_id, port.clone())]),
        };
        let sink = RecordingRealizationSink::default();
        let proteus = proteus::ProteusClient::new(FakeTransport::new());

        let started =
            realize_provision_ports(&source, &sink, &proteus, &cn_id.to_string(), &provision)
                .await
                .unwrap();

        assert_eq!(started, vec![port.port_id]);
        let status = proteus.dump_status(port.port_id).unwrap();
        assert_eq!(status.summary.state, PortState::Running);
        assert_eq!(status.summary.apply_status, BlueprintApplyStatus::Applied);
        assert_eq!(status.generation.applied_generation, Generation::new(4));

        let reports = sink.reports.lock().unwrap();
        assert_eq!(reports.len(), 1);
        match &reports[0].resource {
            NetworkResourceId::Vpc(id) => assert_eq!(*id, vpc_id),
            other => panic!("unexpected reported resource: {other:?}"),
        }
        match &reports[0].realizer {
            RealizerId::Cn(id) => assert_eq!(*id, cn_id),
            other => panic!("unexpected reported realizer: {other:?}"),
        }
        assert_eq!(reports[0].generation, 4);
        assert_eq!(reports[0].status, RealizationStatus::Applied);
    }
}
