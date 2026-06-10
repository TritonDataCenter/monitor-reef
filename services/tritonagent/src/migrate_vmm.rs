// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Live-migration RAM/device-state drivers (LM-7).
//!
//! The source side ([`run_source`]) runs on the agent's migration
//! data-plane lane: it reconnects the paused guest's bhyve control
//! socket, exports device state, opens the kernel vmm device, dials
//! the target's `GET /migrate/{id}` listener, and drives
//! [`OutboundMigration`]. The target side ([`run_target`]) is
//! invoked from the migrate listener's WebSocket upgrade and drives
//! [`InboundMigration`], whose `state_received` fence imports the
//! device state, starts the instance's paused Proteus ports, and
//! resumes the guest, all before the source is told the cutover
//! happened.
//!
//! Two small process-local registries glue the job arms to the
//! listener:
//!
//! * the **pause registry** carries the `pause_complete_ts` from the
//!   `MigratePauseSource` job to the later `MigrateVmmStream` job so
//!   the wire's `PauseComplete` message reports the real pause
//!   instant rather than "whenever the stream started";
//! * the **inbound registry** carries the Proteus port ids the
//!   `MigrateTargetListen` job resolved (via the job blueprint) to
//!   the WebSocket session, which otherwise has no way to learn
//!   them; the listener only sees the ticket. A WS upgrade with no
//!   registered entry is refused: the control plane never told this
//!   CN to expect that migration.
//!
//! Both registries are best-effort across an agent restart: a
//! missing pause timestamp degrades to `0` (audit-only data), a
//! missing inbound entry fails the stream and the saga unwinds.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use proteus_api::ids::PortId;
use tracing::{info, warn};
use tritond_client::Client;
use tritond_client::types::ProvisioningJob;
use tritond_vmm_migrate::bhyve_ctl::BhyveCtl;
use tritond_vmm_migrate::{
    InboundMigration, MemLayout, MigrateError, OutboundMigration, Phase, SharedVmm, SourceHooks,
    StateBlobs, TargetCaptured, TargetHooks, Transport,
};
use uuid::Uuid;

use crate::{AgentConfig, migrate, migrate_progress, vmadm};

/// bhyve's in-zone control-socket listener is single-threaded; the
/// pause job's connection must fully close before ours can be
/// accepted, so connects retry briefly instead of failing on the
/// first refusal.
const CTL_CONNECT_ATTEMPTS: u32 = 10;
const CTL_CONNECT_DELAY: Duration = Duration::from_millis(500);

/// How long `MigrateTargetListen` waits for the freshly booted
/// listen-mode zone's bhyve.sock to answer `status`.
const LISTEN_READY_TIMEOUT: Duration = Duration::from_secs(60);

/// Inbound-registry entries older than this are pruned at the next
/// register; they belong to migrations whose stream never arrived
/// (the saga has long since unwound).
const INBOUND_ENTRY_TTL: Duration = Duration::from_secs(60 * 60);

/// In-zonepath bhyve control socket, by SmartOS convention (the
/// same `zones/<uuid>` convention the saga's dataset names rely on).
pub(crate) fn bhyve_sock_path(instance_id: Uuid) -> PathBuf {
    PathBuf::from(format!("/zones/{instance_id}/root/tmp/bhyve.sock"))
}

fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

/// Poison-tolerant lock (the imds_ratelimit pattern): a panic
/// elsewhere in the agent must not wedge migration bookkeeping.
fn lock_or_recover<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match m.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    }
}

/// Wire-phase label shared with the saga's failure policy: the
/// `MigrateVmmStream` job result reports the phase the source had
/// entered when the stream ended, and
/// `tritond::sagas::migration::stream_vmm` classifies "finish" /
/// "complete" as the ambiguous window (the target holds complete
/// device state and may have activated). Keep the strings in sync
/// with that classifier.
pub(crate) fn phase_label(phase: Phase) -> &'static str {
    match phase {
        Phase::Sync => "sync",
        Phase::Pause => "pause",
        Phase::RamPush => "ram_push",
        Phase::RamHash => "ram_hash",
        Phase::TimeData => "time_data",
        Phase::DeviceState => "device_state",
        Phase::Finish => "finish",
        Phase::Complete => "complete",
    }
}

// ──────────────────────────────────────────────────────────────────
// Process-local registries.
// ──────────────────────────────────────────────────────────────────

fn pause_registry() -> &'static Mutex<HashMap<Uuid, u64>> {
    static REG: OnceLock<Mutex<HashMap<Uuid, u64>>> = OnceLock::new();
    REG.get_or_init(|| Mutex::new(HashMap::new()))
}

fn record_pause_ts(migration_id: Uuid, ts_ns: u64) {
    lock_or_recover(pause_registry()).insert(migration_id, ts_ns);
}

fn take_pause_ts(migration_id: Uuid) -> Option<u64> {
    lock_or_recover(pause_registry()).remove(&migration_id)
}

/// What the target-listen job hands the later WebSocket session.
pub(crate) struct InboundContext {
    pub instance_id: Uuid,
    /// Proteus ports created paused by `MigrationProvisionTarget`;
    /// the cutover fence starts them.
    pub port_ids: Vec<PortId>,
    registered_at: Instant,
}

fn inbound_registry() -> &'static Mutex<HashMap<Uuid, InboundContext>> {
    static REG: OnceLock<Mutex<HashMap<Uuid, InboundContext>>> = OnceLock::new();
    REG.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(crate) fn register_inbound(migration_id: Uuid, instance_id: Uuid, port_ids: Vec<PortId>) {
    let mut reg = lock_or_recover(inbound_registry());
    reg.retain(|_, ctx| ctx.registered_at.elapsed() < INBOUND_ENTRY_TTL);
    reg.insert(
        migration_id,
        InboundContext {
            instance_id,
            port_ids,
            registered_at: Instant::now(),
        },
    );
}

pub(crate) fn take_inbound(migration_id: Uuid) -> Option<InboundContext> {
    lock_or_recover(inbound_registry()).remove(&migration_id)
}

/// Listener fast-path check: refuse the WS upgrade with a real
/// status code when no inbound session is registered, instead of an
/// opaque post-upgrade close.
pub(crate) fn peek_inbound_instance(migration_id: Uuid) -> Option<Uuid> {
    lock_or_recover(inbound_registry())
        .get(&migration_id)
        .map(|ctx| ctx.instance_id)
}

// ──────────────────────────────────────────────────────────────────
// Control-socket helpers.
// ──────────────────────────────────────────────────────────────────

async fn connect_ctl_with_retry(path: &Path) -> Result<BhyveCtl> {
    let mut last: Option<std::io::Error> = None;
    for attempt in 0..CTL_CONNECT_ATTEMPTS {
        match BhyveCtl::connect(path).await {
            Ok(ctl) => return Ok(ctl),
            Err(e) => last = Some(e),
        }
        if attempt + 1 < CTL_CONNECT_ATTEMPTS {
            tokio::time::sleep(CTL_CONNECT_DELAY).await;
        }
    }
    Err(anyhow!(
        "connect bhyve control socket {} failed after {CTL_CONNECT_ATTEMPTS} attempts: {}",
        path.display(),
        last.map_or_else(|| "no error recorded".to_string(), |e| e.to_string()),
    ))
}

/// Poll a freshly booted listen-mode zone until its bhyve.sock
/// answers `status`. Each probe drops its connection so the
/// single-threaded listener is free for the real session.
async fn wait_for_bhyve_listener(path: &Path, deadline: Duration) -> Result<()> {
    let start = Instant::now();
    let mut last = String::from("no probe attempted");
    loop {
        match BhyveCtl::connect(path).await {
            Ok(mut ctl) => match ctl.status().await {
                Ok(_) => return Ok(()),
                Err(e) => last = format!("status: {e}"),
            },
            Err(e) => last = format!("connect: {e}"),
        }
        if start.elapsed() >= deadline {
            bail!(
                "bhyve listener {} not ready after {:?}: {last}",
                path.display(),
                deadline,
            );
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

/// Open the kernel vmm device. Factored so the platform gate sits
/// in one place; everything else in this module compiles (and is
/// testable) on dev hosts.
fn open_smartos_vmm(vmm_name: &str, layout: MemLayout) -> Result<SharedVmm> {
    #[cfg(target_os = "illumos")]
    {
        let vmm = tritond_vmm_migrate::SmartOsVmm::open(vmm_name, layout)
            .with_context(|| format!("open /dev/vmm/{vmm_name}"))?;
        Ok(Arc::new(vmm))
    }
    #[cfg(not(target_os = "illumos"))]
    {
        let _ = (vmm_name, layout);
        bail!("/dev/vmm is illumos-only; live migration requires a SmartOS CN")
    }
}

// ──────────────────────────────────────────────────────────────────
// Pause / resume job arms (inline lane; seconds-scale).
// ──────────────────────────────────────────────────────────────────

/// `JobKind::MigratePauseSource`: freeze the source guest for the
/// final ZFS increment + RAM stream. Returns the job `result`
/// payload (`{"pause_complete_ts": ns}`).
pub(crate) async fn pause_source(
    migration_id: Uuid,
    instance_id: Uuid,
) -> Result<serde_json::Value> {
    pause_source_at(&bhyve_sock_path(instance_id), migration_id).await
}

/// Pause order is load-bearing (donor-proven): viona rings first so
/// the kernel stops draining avail rings, then vCPUs, then the
/// block-device drain so every in-flight request's completion is
/// committed before state capture.
pub(crate) async fn pause_source_at(path: &Path, migration_id: Uuid) -> Result<serde_json::Value> {
    let mut ctl = connect_ctl_with_retry(path).await?;
    ctl.pause_devices().await.context("bhyve pause-devices")?;
    ctl.pause_vm().await.context("bhyve pause-vm")?;
    ctl.drain_devices().await.context("bhyve drain-devices")?;
    let ts = now_ns();
    record_pause_ts(migration_id, ts);
    info!(%migration_id, pause_complete_ts = ts, "migrate-pause-source: guest paused + drained");
    Ok(serde_json::json!({ "pause_complete_ts": ts }))
}

/// `JobKind::MigrateResumeSource`: pre-cutover failure undo. The saga
/// only enqueues this when the source failed in a phase that provably
/// precedes the device-state send (see `live_failure_is_pre_finish`);
/// once the target may have imported, the failure is ambiguous and the
/// source is left paused for the operator rather than resumed here.
pub(crate) async fn resume_source(migration_id: Uuid, instance_id: Uuid) -> Result<()> {
    resume_source_at(&bhyve_sock_path(instance_id), migration_id).await
}

/// Resume brings the rings back before the vCPUs so the guest never
/// runs against paused viona queues.
pub(crate) async fn resume_source_at(path: &Path, migration_id: Uuid) -> Result<()> {
    let mut ctl = connect_ctl_with_retry(path).await?;
    ctl.resume_devices().await.context("bhyve resume-devices")?;
    ctl.resume_vm().await.context("bhyve resume-vm")?;
    let _ = take_pause_ts(migration_id);
    info!(%migration_id, "migrate-resume-source: guest resumed");
    Ok(())
}

// ──────────────────────────────────────────────────────────────────
// Target listen job (data-plane lane).
// ──────────────────────────────────────────────────────────────────

/// `JobKind::MigrateTargetListen`: resolve the instance's Proteus
/// port ids from the job blueprint, boot the listen-mode zone, wait
/// for its control socket, and register the inbound session for the
/// listener. Runs strictly before the saga enqueues the source's
/// `MigrateVmmStream`, so the registry entry is always present when
/// the dial arrives.
pub(crate) async fn target_listen(
    client: &Client,
    job: &ProvisioningJob,
    migration_id: Uuid,
    instance_id: Uuid,
) -> Result<serde_json::Value> {
    use crate::PortBlueprintSource as _;
    let blueprint = client
        .agent_job_blueprint()
        .job_id(job.id)
        .send()
        .await
        .context("agent_job_blueprint for MigrateTargetListen")?
        .into_inner();
    let mut port_ids = Vec::with_capacity(blueprint.nics.len());
    for nic in &blueprint.nics {
        let pb = client.fetch_port_blueprint(nic.id).await.with_context(|| {
            format!(
                "fetch Proteus port blueprint for NIC {} on migration target",
                nic.id
            )
        })?;
        port_ids.push(pb.port_id);
    }

    vmadm::start_zone(instance_id)
        .await
        .context("vmadm start listen-mode target zone")?;
    wait_for_bhyve_listener(&bhyve_sock_path(instance_id), LISTEN_READY_TIMEOUT).await?;
    register_inbound(migration_id, instance_id, port_ids);
    info!(
        %migration_id, %instance_id,
        "migrate-target-listen: zone booted in listen mode; inbound session registered",
    );
    Ok(serde_json::json!({ "listen_ready": true }))
}

// ──────────────────────────────────────────────────────────────────
// Source driver.
// ──────────────────────────────────────────────────────────────────

/// Failure carrier for [`run_source`]: the saga's failure policy
/// needs the protocol-phase report even when the stream failed, and
/// the data-plane lane's completion path only ships a `result`
/// payload it can extract from the error chain.
#[derive(Debug)]
pub(crate) struct StreamFailed {
    pub reason: String,
    pub report: serde_json::Value,
}

impl fmt::Display for StreamFailed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.reason)
    }
}

impl std::error::Error for StreamFailed {}

#[derive(Default)]
struct SourceShared {
    last_phase: Option<&'static str>,
    pages_pushed: u64,
    bytes_pushed: u64,
    switch_complete_ts_ns: Option<u64>,
}

struct SourceRunHooks {
    migration_id: Uuid,
    pause_ts_ns: u64,
    shared: Arc<Mutex<SourceShared>>,
    /// Cumulative bytes-pushed cell the throttled progress
    /// reporter samples. `Arc::default()` (never read) in tests.
    progress: Arc<AtomicU64>,
}

impl SourceHooks for SourceRunHooks {
    fn phase(&mut self, phase: Phase) {
        let label = phase_label(phase);
        lock_or_recover(&self.shared).last_phase = Some(label);
        info!(migration_id = %self.migration_id, phase = label, "migrate-vmm-stream/source: phase");
    }

    fn pause_complete_ts_ns(&mut self) -> u64 {
        self.pause_ts_ns
    }

    fn switch_complete(&mut self, target_activated_at_ns: u64) {
        lock_or_recover(&self.shared).switch_complete_ts_ns = Some(target_activated_at_ns);
        info!(
            migration_id = %self.migration_id,
            target_activated_at_ns,
            "migrate-vmm-stream/source: target reported SwitchComplete",
        );
    }

    fn pages_pushed(&mut self, pages: u64, bytes: u64) {
        let mut g = lock_or_recover(&self.shared);
        g.pages_pushed += pages;
        g.bytes_pushed += bytes;
        self.progress.store(g.bytes_pushed, Ordering::Relaxed);
    }
}

fn source_report(shared: &Arc<Mutex<SourceShared>>, pause_ts_ns: u64) -> serde_json::Value {
    let g = lock_or_recover(shared);
    serde_json::json!({
        "last_phase": g.last_phase,
        "pause_complete_ts_ns": pause_ts_ns,
        "switch_complete_ts_ns": g.switch_complete_ts_ns,
        "pages_pushed": g.pages_pushed,
        "bytes_pushed": g.bytes_pushed,
    })
}

/// `JobKind::MigrateVmmStream { role: Source }` driver. The guest is
/// ALREADY paused by the earlier `MigratePauseSource` job; bhyve's
/// control protocol has no pause-state query, so this trusts the
/// saga's ordering and never re-pauses. On success the returned
/// JSON is the job `result`; on failure the same report rides the
/// [`StreamFailed`] error so the saga still sees the last phase.
pub(crate) async fn run_source(
    client: &Arc<Client>,
    cfg: &AgentConfig,
    migration_id: Uuid,
    instance_id: Uuid,
    peer_endpoint: &str,
    peer_spki_sha256_hex: &str,
    ticket: &str,
) -> Result<serde_json::Value> {
    let source_cn = Uuid::parse_str(&cfg.agent_id)
        .context("agent_id is not a UUID; cannot present source_cn in dial")?;
    let vmm_name = vmadm::vmm_device_name(instance_id).await?;

    let mut ctl = connect_ctl_with_retry(&bhyve_sock_path(instance_id)).await?;
    let status = ctl
        .status()
        .await
        .context("bhyve status on paused source")?;
    let layout = MemLayout {
        num_cpus: status.num_cpus,
        lowmem_size: status.lowmem_size,
        highmem_size: status.highmem_size,
    };
    // Export requires the pause+drain the MigratePauseSource job
    // already performed.
    let (kern_state, dev_state) = ctl
        .export_state()
        .await
        .context("bhyve export-state on paused source")?;
    // Free the single-threaded listener; nothing further to ask it.
    drop(ctl);

    let vmm = open_smartos_vmm(&vmm_name, layout)?;
    let pause_ts_ns = take_pause_ts(migration_id).unwrap_or_else(|| {
        // Agent restarted between the pause job and this stream;
        // audit-only data, the guest itself is still paused.
        warn!(%migration_id, "migrate-vmm-stream/source: pause timestamp lost; reporting 0");
        0
    });

    let transport = migrate::dial(migrate::DialParams {
        base_url: peer_endpoint.to_string(),
        migration_id,
        source_cn,
        vm_uuid: instance_id,
        ticket: ticket.to_string(),
        target_spki_sha256_hex: peer_spki_sha256_hex.to_string(),
    })
    .await
    .context("dial target /migrate/{id}")?;

    // The RAM push dominates the stream; the state blobs are noise
    // next to it, so guest memory is the progress total.
    let reporter = migrate_progress::ProgressReporter::start(
        Arc::clone(client),
        migration_id,
        Some(layout.total_bytes() as u64),
        "vmm ram stream".to_string(),
    );
    let shared = Arc::new(Mutex::new(SourceShared::default()));
    let hooks = SourceRunHooks {
        migration_id,
        pause_ts_ns,
        shared: Arc::clone(&shared),
        progress: reporter.observer(),
    };
    let blobs = StateBlobs {
        time_data: Vec::new(),
        kern_state,
        dev_state,
    };
    let outcome = OutboundMigration::new(transport, vmm, blobs, hooks)
        .run()
        .await;
    let report = source_report(&shared, pause_ts_ns);
    match outcome {
        Ok(()) => {
            reporter.finish().await;
            info!(
                %migration_id, %instance_id, report = %report,
                "migrate-vmm-stream/source: stream complete",
            );
            Ok(report)
        }
        Err(e) => Err(anyhow::Error::new(StreamFailed {
            reason: format!("vmm stream failed: {e}"),
            report,
        })),
    }
}

// ──────────────────────────────────────────────────────────────────
// Target driver.
// ──────────────────────────────────────────────────────────────────

/// Narrow port-start surface for the cutover fence, separate from
/// [`crate::ProteusLifecycle`] so the fence ordering is testable
/// without an illumos kernel transport.
pub(crate) trait CutoverPorts: Send {
    fn start_all(&mut self) -> Result<()>;
}

/// Production [`CutoverPorts`]: opens the Proteus device and starts
/// every paused port the target-listen job registered.
struct ProteusPortStarter {
    proteus_dev: PathBuf,
    port_ids: Vec<PortId>,
}

impl CutoverPorts for ProteusPortStarter {
    fn start_all(&mut self) -> Result<()> {
        if self.port_ids.is_empty() {
            return Ok(());
        }
        let proteus = crate::open_proteus_lifecycle(&self.proteus_dev)?;
        for port_id in &self.port_ids {
            proteus.start_port(*port_id)?;
        }
        Ok(())
    }
}

#[derive(Default)]
struct TargetShared {
    source_paused_at_ns: u64,
    pages_received: u64,
    bytes_received: u64,
    activated_ts_ns: u64,
}

struct TargetRunHooks {
    migration_id: Uuid,
    ctl: BhyveCtl,
    ports: Box<dyn CutoverPorts>,
    shared: Arc<Mutex<TargetShared>>,
}

impl TargetRunHooks {
    /// The import fence body: bhyve import-state, then dataplane up,
    /// then resume (rings before vCPUs). Runs after RAM is verified
    /// and before the source is told the cutover happened.
    async fn cutover(&mut self, blobs: &TargetCaptured) -> Result<u64, MigrateError> {
        self.ctl
            .import_state(&blobs.kern_state, &blobs.dev_state)
            .await
            .map_err(|e| MigrateError::Hook(format!("bhyve import-state: {e}")))?;
        self.ports
            .start_all()
            .map_err(|e| MigrateError::Hook(format!("proteus port start: {e:#}")))?;
        self.ctl
            .resume_devices()
            .await
            .map_err(|e| MigrateError::Hook(format!("bhyve resume-devices: {e}")))?;
        self.ctl
            .resume_vm()
            .await
            .map_err(|e| MigrateError::Hook(format!("bhyve resume-vm: {e}")))?;
        let ts = now_ns();
        lock_or_recover(&self.shared).activated_ts_ns = ts;
        info!(
            migration_id = %self.migration_id,
            activated_ts_ns = ts,
            "migrate-vmm-stream/target: state imported, ports started, guest resumed",
        );
        Ok(ts)
    }
}

impl TargetHooks for TargetRunHooks {
    fn phase(&mut self, phase: Phase) {
        info!(
            migration_id = %self.migration_id,
            phase = phase_label(phase),
            "migrate-vmm-stream/target: phase",
        );
    }

    fn pause_complete(&mut self, source_paused_at_ns: u64) {
        lock_or_recover(&self.shared).source_paused_at_ns = source_paused_at_ns;
    }

    fn state_received(&mut self, blobs: &TargetCaptured) -> Result<u64, MigrateError> {
        // The hook trait is sync so MockVmm tests stay runtime-free;
        // the agent always runs a multi-thread tokio runtime, which
        // makes block_in_place sound here (the cutover is the only
        // thing this task is doing anyway).
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(self.cutover(blobs))
        })
    }

    fn pages_received(&mut self, pages: u64, bytes: u64) {
        let mut g = lock_or_recover(&self.shared);
        g.pages_received += pages;
        g.bytes_received += bytes;
    }
}

/// Everything [`run_target`] needs beyond the upgraded socket.
pub(crate) struct RunTargetParams {
    pub migration_id: Uuid,
    /// Instance uuid the verified ticket binds.
    pub vm_uuid: Uuid,
    pub proteus_dev: PathBuf,
}

/// Inbound driver, called from the migrate listener's
/// `GET /migrate/{id}` upgrade. Returns the target-activation
/// timestamp (ns) on success.
pub(crate) async fn run_target(
    transport: Box<dyn Transport>,
    params: RunTargetParams,
) -> Result<u64> {
    let ctx = take_inbound(params.migration_id).ok_or_else(|| {
        anyhow!(
            "no pending inbound migration {} on this CN; MigrateTargetListen has not completed \
             here (or the agent restarted since)",
            params.migration_id,
        )
    })?;
    if ctx.instance_id != params.vm_uuid {
        bail!(
            "inbound migration {} is registered for instance {} but the ticket binds {}",
            params.migration_id,
            ctx.instance_id,
            params.vm_uuid,
        );
    }
    let vmm_name = vmadm::vmm_device_name(params.vm_uuid).await?;
    let mut ctl = connect_ctl_with_retry(&bhyve_sock_path(params.vm_uuid)).await?;
    let status = ctl
        .status()
        .await
        .context("bhyve status on listen-mode target")?;
    let layout = MemLayout {
        num_cpus: status.num_cpus,
        lowmem_size: status.lowmem_size,
        highmem_size: status.highmem_size,
    };
    let vmm = open_smartos_vmm(&vmm_name, layout)?;
    let ports = Box::new(ProteusPortStarter {
        proteus_dev: params.proteus_dev,
        port_ids: ctx.port_ids,
    });
    run_target_with(transport, vmm, ctl, ports, params.migration_id).await
}

/// Dependency-injected core of [`run_target`] so tests can drive a
/// full mock migration (MockVmm + scripted control socket +
/// recording ports) over the in-memory transport pair.
pub(crate) async fn run_target_with(
    transport: Box<dyn Transport>,
    vmm: SharedVmm,
    ctl: BhyveCtl,
    ports: Box<dyn CutoverPorts>,
    migration_id: Uuid,
) -> Result<u64> {
    let shared = Arc::new(Mutex::new(TargetShared::default()));
    let hooks = TargetRunHooks {
        migration_id,
        ctl,
        ports,
        shared: Arc::clone(&shared),
    };
    InboundMigration::new(transport, vmm, hooks)
        .run()
        .await
        .map_err(|e| anyhow!("inbound vmm stream failed: {e}"))?;
    let g = lock_or_recover(&shared);
    info!(
        %migration_id,
        pages_received = g.pages_received,
        bytes_received = g.bytes_received,
        source_paused_at_ns = g.source_paused_at_ns,
        activated_ts_ns = g.activated_ts_ns,
        "migrate-vmm-stream/target: inbound migration complete",
    );
    Ok(g.activated_ts_ns)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;
    use tritond_vmm_migrate::vmm_dev::mock::MockVmm;
    use tritond_vmm_migrate::{MemRegion, NoopSourceHooks, inmem};

    /// Scripted bhyve control socket: accepts one connection,
    /// records each command name in order, replies success, and
    /// consumes import-state's binary payload. `status` replies
    /// with the supplied layout.
    fn spawn_scripted_bhyve(
        dir: &std::path::Path,
        layout: MemLayout,
    ) -> (PathBuf, Arc<StdMutex<Vec<String>>>) {
        let path = dir.join("bhyve.sock");
        let listener = UnixListener::bind(&path).expect("bind scripted bhyve sock");
        let log: Arc<StdMutex<Vec<String>>> = Arc::default();
        let log_clone = Arc::clone(&log);
        tokio::spawn(async move {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let (read_half, mut write_half) = stream.into_split();
            let mut reader = BufReader::new(read_half);
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line).await {
                    Ok(0) | Err(_) => return,
                    Ok(_) => {}
                }
                let cmd: serde_json::Value = match serde_json::from_str(line.trim_end()) {
                    Ok(v) => v,
                    Err(_) => return,
                };
                let name = cmd
                    .get("command")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?")
                    .to_string();
                log_clone.lock().unwrap().push(name.clone());
                let reply = match name.as_str() {
                    "status" => serde_json::json!({
                        "success": true,
                        "ncpus": layout.num_cpus,
                        "lowmem": layout.lowmem_size,
                        "highmem": layout.highmem_size,
                    }),
                    "import-state" => {
                        let kern = cmd["kern_len"].as_u64().unwrap_or(0) as usize;
                        let dev = cmd["dev_len"].as_u64().unwrap_or(0) as usize;
                        let mut buf = vec![0u8; kern + dev];
                        if reader.read_exact(&mut buf).await.is_err() {
                            return;
                        }
                        serde_json::json!({ "success": true })
                    }
                    _ => serde_json::json!({ "success": true }),
                };
                let mut bytes = reply.to_string().into_bytes();
                bytes.push(b'\n');
                if write_half.write_all(&bytes).await.is_err() {
                    return;
                }
            }
        });
        (path, log)
    }

    fn small_layout() -> MemLayout {
        MemLayout {
            num_cpus: 1,
            lowmem_size: 16 * tritond_vmm_migrate::PAGE_SIZE,
            highmem_size: 0,
        }
    }

    #[derive(Default)]
    struct RecordingPorts {
        started: Arc<StdMutex<Vec<&'static str>>>,
    }

    impl CutoverPorts for RecordingPorts {
        fn start_all(&mut self) -> Result<()> {
            self.started.lock().unwrap().push("ports-start");
            Ok(())
        }
    }

    #[tokio::test]
    async fn pause_source_issues_commands_in_donor_order() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (path, log) = spawn_scripted_bhyve(dir.path(), small_layout());
        let migration_id = Uuid::new_v4();
        let result = pause_source_at(&path, migration_id)
            .await
            .expect("pause succeeds");
        assert_eq!(
            log.lock().unwrap().as_slice(),
            ["pause-devices", "pause-vm", "drain-devices"],
        );
        let ts = result["pause_complete_ts"].as_u64().expect("ts present");
        assert!(ts > 0);
        // The stream job reads the stash; pause must have recorded it.
        assert_eq!(take_pause_ts(migration_id), Some(ts));
    }

    #[tokio::test]
    async fn resume_source_brings_rings_up_before_vcpus() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (path, log) = spawn_scripted_bhyve(dir.path(), small_layout());
        let migration_id = Uuid::new_v4();
        record_pause_ts(migration_id, 42);
        resume_source_at(&path, migration_id)
            .await
            .expect("resume succeeds");
        assert_eq!(
            log.lock().unwrap().as_slice(),
            ["resume-devices", "resume-vm"],
        );
        // Resume clears the stash so a later migration can't read
        // this one's timestamp.
        assert_eq!(take_pause_ts(migration_id), None);
    }

    /// Full mock live migration: MockVmm on both ends, the in-memory
    /// transport pair as the wire, a scripted control socket as
    /// bhyve, and recording ports as Proteus. Asserts the cutover
    /// fence ordering (import-state → port start → resume-devices →
    /// resume-vm), that guest RAM arrived intact, and that the
    /// target's activation timestamp reached the source's
    /// SwitchComplete hook.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn mock_live_migration_end_to_end() {
        let layout = small_layout();
        let dir = tempfile::tempdir().expect("tempdir");
        let (sock, ctl_log) = spawn_scripted_bhyve(dir.path(), layout);
        let ctl = BhyveCtl::connect(&sock)
            .await
            .expect("connect scripted bhyve");

        let (src_t, dst_t) = inmem::channel_pair(64);
        let migration_id = Uuid::new_v4();

        let src_vmm: SharedVmm = Arc::new(MockVmm::with_pattern(layout));
        let target_mock = Arc::new(MockVmm::filled(layout, 0));
        let dst_vmm: SharedVmm = target_mock.clone();

        let port_log: Arc<StdMutex<Vec<&'static str>>> = Arc::default();
        let ports = Box::new(RecordingPorts {
            started: Arc::clone(&port_log),
        });

        let source_shared = Arc::new(Mutex::new(SourceShared::default()));
        let progress = Arc::new(AtomicU64::new(0));
        let source_hooks = SourceRunHooks {
            migration_id,
            pause_ts_ns: 7_777,
            shared: Arc::clone(&source_shared),
            progress: Arc::clone(&progress),
        };
        let blobs = StateBlobs {
            time_data: b"TIME".to_vec(),
            kern_state: b"KERN-STATE".to_vec(),
            dev_state: b"DEV-STATE".to_vec(),
        };
        let source = tokio::spawn(async move {
            OutboundMigration::new(src_t, src_vmm, blobs, source_hooks)
                .run()
                .await
        });
        let activated = run_target_with(Box::new(dst_t), dst_vmm, ctl, ports, migration_id)
            .await
            .expect("inbound migration succeeds");
        source
            .await
            .expect("source task")
            .expect("outbound migration succeeds");

        assert!(activated > 0);
        // SwitchComplete carried the target's activation instant
        // back to the source.
        assert_eq!(
            source_shared.lock().unwrap().switch_complete_ts_ns,
            Some(activated),
        );
        assert_eq!(source_shared.lock().unwrap().last_phase, Some("complete"),);

        // RAM landed intact (source pattern, byte for byte).
        let pattern: Vec<u8> = (0..layout.lowmem_size).map(|i| (i & 0xff) as u8).collect();
        assert_eq!(target_mock.region_snapshot(MemRegion::Lowmem), pattern);

        // The progress cell mirrored the cumulative RAM push, so the
        // throttled reporter would have had real numbers to sample.
        let pushed = source_shared.lock().unwrap().bytes_pushed;
        assert!(pushed > 0);
        assert_eq!(progress.load(Ordering::Relaxed), pushed);

        // Cutover fence ordering: the connection-scoped script log
        // shows import-state strictly before the resume pair, and
        // the recording ports fired exactly once in between.
        let cmds = ctl_log.lock().unwrap().clone();
        assert_eq!(cmds, ["import-state", "resume-devices", "resume-vm"]);
        assert_eq!(port_log.lock().unwrap().as_slice(), ["ports-start"]);
    }

    /// A failing cutover (scripted bhyve closes on import-state)
    /// must abort the inbound run BEFORE any resume command and
    /// surface as an error, never a half-activated target.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn failed_import_aborts_without_resume() {
        let layout = small_layout();
        let dir = tempfile::tempdir().expect("tempdir");
        // Script that fails import-state.
        let path = dir.path().join("bhyve.sock");
        let listener = UnixListener::bind(&path).expect("bind");
        let log: Arc<StdMutex<Vec<String>>> = Arc::default();
        let log_clone = Arc::clone(&log);
        tokio::spawn(async move {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let (read_half, mut write_half) = stream.into_split();
            let mut reader = BufReader::new(read_half);
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line).await {
                    Ok(0) | Err(_) => return,
                    Ok(_) => {}
                }
                let cmd: serde_json::Value =
                    serde_json::from_str(line.trim_end()).unwrap_or_default();
                let name = cmd
                    .get("command")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?")
                    .to_string();
                log_clone.lock().unwrap().push(name.clone());
                let reply = if name == "import-state" {
                    let kern = cmd["kern_len"].as_u64().unwrap_or(0) as usize;
                    let dev = cmd["dev_len"].as_u64().unwrap_or(0) as usize;
                    let mut buf = vec![0u8; kern + dev];
                    let _ = reader.read_exact(&mut buf).await;
                    serde_json::json!({ "success": false, "error": "scripted import failure" })
                } else {
                    serde_json::json!({ "success": true })
                };
                let mut bytes = reply.to_string().into_bytes();
                bytes.push(b'\n');
                if write_half.write_all(&bytes).await.is_err() {
                    return;
                }
            }
        });
        let ctl = BhyveCtl::connect(&path).await.expect("connect");

        let (src_t, dst_t) = inmem::channel_pair(64);
        let migration_id = Uuid::new_v4();
        let src_vmm: SharedVmm = Arc::new(MockVmm::with_pattern(layout));
        let dst_vmm: SharedVmm = Arc::new(MockVmm::filled(layout, 0));
        let source = tokio::spawn(async move {
            OutboundMigration::new(src_t, src_vmm, StateBlobs::default(), NoopSourceHooks)
                .run()
                .await
        });
        let err = run_target_with(
            Box::new(dst_t),
            dst_vmm,
            ctl,
            Box::new(RecordingPorts::default()),
            migration_id,
        )
        .await
        .expect_err("import failure must fail the inbound run");
        assert!(err.to_string().contains("import-state"), "{err}");
        // The source must NOT have been told the switch happened.
        let src_result = source.await.expect("source task");
        assert!(src_result.is_err(), "source must see the abort");
        // No resume command ever reached the scripted socket.
        assert!(
            !log.lock().unwrap().iter().any(|c| c.starts_with("resume")),
            "no resume after failed import: {:?}",
            log.lock().unwrap(),
        );
    }

    #[test]
    fn inbound_registry_round_trip() {
        let id = Uuid::new_v4();
        let inst = Uuid::new_v4();
        register_inbound(id, inst, Vec::new());
        assert_eq!(peek_inbound_instance(id), Some(inst));
        let ctx = take_inbound(id).expect("registered");
        assert_eq!(ctx.instance_id, inst);
        // take removes; a second stream for the same migration must
        // be refused.
        assert!(take_inbound(id).is_none());
    }

    /// The phase labels are a wire contract with the saga's failure
    /// classifier; pin them.
    #[test]
    fn phase_labels_are_stable() {
        assert_eq!(phase_label(Phase::Sync), "sync");
        assert_eq!(phase_label(Phase::Pause), "pause");
        assert_eq!(phase_label(Phase::RamPush), "ram_push");
        assert_eq!(phase_label(Phase::RamHash), "ram_hash");
        assert_eq!(phase_label(Phase::TimeData), "time_data");
        assert_eq!(phase_label(Phase::DeviceState), "device_state");
        assert_eq!(phase_label(Phase::Finish), "finish");
        assert_eq!(phase_label(Phase::Complete), "complete");
    }
}
