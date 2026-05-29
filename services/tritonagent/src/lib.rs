// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Triton Cloud per-CN provisioning agent. The agent is the only
//! component that mutates CN-local runtime state. The presented API
//! key is `ApiKeyScope::Agent` — even if the owner is root, it can
//! only call `agent_claim`/`agent_complete`. Audit captures both
//! the key owner and the agent's `claimed_by` identifier.

pub mod console;
pub mod console_creds;
pub mod credentials;
pub mod dhcp_events;
pub mod edge;
pub mod images;
pub mod imds;
pub mod imds_arp;
pub mod imds_bindings;
pub mod imds_creds;
pub mod imds_data;
pub mod imds_ratelimit;
pub mod log_tailer;
pub mod metrics;
pub mod migrate;
pub mod platform;
pub mod proteus;
pub mod registration;
pub mod reservoir;
pub mod status;
pub mod vmadm;
pub mod zfs;

use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use base64::Engine;
use proteus_api::blueprint::PortBlueprint;
use proteus_api::ids::PortId;
use tracing::{error, info, warn};
use tritond_auth::CONSOLE_TICKET_KEY_BYTES;
use tritond_client::Client;
use tritond_client::types::{
    AgentPortBlueprint, ClaimJobRequest, CompleteJobRequest, ImageCompatibility, JobKind,
    JobOutcome, NetworkRealizationRequest, NetworkResourceId, Nic, ProvisioningBlueprint,
    ProvisioningJob, RealizationStatus, RealizerId,
};
use tritond_cn_platform::cn_status::{
    DiskUsageSampler, Heartbeater, StatusCollector, UuidNamedImageFilter, ZoneeventWatcher,
};
use tritond_cn_platform::smartos::zoneadm::ZoneadmTool;
use tritond_cn_platform::smartos::{KstatTool, ReservoirTool, VmadmTool, ZfsTool};
use uuid::Uuid;

use crate::console_creds::ConsoleTls;
use crate::imds_bindings::{ImdsBindingTable, register_blueprint_bindings};
use tritond_auth::IMDS_TOKEN_KEY_BYTES;

use crate::status::TritondStatusSink;

/// Default Proteus kernel device node on SmartOS.
pub const DEFAULT_PROTEUS_DEVICE: &str = "/dev/proteus";

/// Default root for fhrun/firehyve edge instance runtime state on a CN.
pub const DEFAULT_EDGE_ROOT: &str = "/var/lib/tritonagent/edge";

/// Default fhrun launcher path on SmartOS CNs.
pub const DEFAULT_FHRUN_BIN: &str = "/opt/firehyve/bin/fhrun";

pub const DEFAULT_CONSOLE_LISTEN_PORT: u16 = 9101;

/// Not `Debug`: carries API key, console-ticket key, and a TLS
/// private key — a stray `{:?}` would be a credential leak.
#[derive(Clone)]
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
    /// Root directory for per-edge-instance manifests and
    /// edge-control Unix sockets. The legacy host-process edge shim
    /// also writes pid files and logs here.
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
    /// posts liveness + status to tritond's `/v1/agent/heartbeat`
    /// and `/v1/agent/status`. Disabled by `--no-heartbeater`
    /// for tritond integration tests that don't want background
    /// chatter at the test server. Also gates the console listener
    /// (so integration tests don't open a port).
    pub spawn_heartbeater: bool,
    /// When `true` (the default), the agent manages the bhyve memory
    /// reservoir (RFD 0185): at startup it sizes the reservoir to
    /// [`reservoir_percent`] of physical RAM and reports its state on
    /// each heartbeat. `false` leaves the reservoir untouched (guests use
    /// transient memory, as before). RV-2 will source this per-CN from
    /// tritond; for now it is an agent-local default.
    ///
    /// [`reservoir_percent`]: AgentConfig::reservoir_percent
    pub reservoir_enabled: bool,
    /// Fraction of physical RAM to target for the reservoir floor
    /// (`0.0..=1.0`). Clamped to the kernel's reservoir limit at apply
    /// time. Ignored when [`reservoir_enabled`] is `false`.
    ///
    /// [`reservoir_enabled`]: AgentConfig::reservoir_enabled
    pub reservoir_percent: f32,
    /// Admin-network IPv4 the console listener binds on. `None` when
    /// sysinfo didn't report one — the console listener is skipped.
    pub admin_ip: Option<Ipv4Addr>,
    /// TCP port the console listener binds (on `admin_ip`).
    pub console_listen_port: u16,
    /// Per-CN HS256 console-ticket key. `None` for an agent that
    /// registered before consoles were supported — the listener is
    /// skipped.
    pub console_ticket_key: Option<[u8; CONSOLE_TICKET_KEY_BYTES]>,
    /// Self-signed TLS material for the console listener. `None` only if
    /// `load_or_init_tls` couldn't be run (it always can in `main`).
    pub console_tls: Option<ConsoleTls>,
    /// Bind address for the in-VM IMDS HTTP listener
    /// (`IMDS_DESIGN.md` §3). `None` skips the listener entirely --
    /// the production path expects the proteus `RouteTarget::LocalImds`
    /// redirect to land on a dedicated proteus-owned internal datalink,
    /// not the CN admin IP; until the proteus apply path wires that
    /// datalink up, leaving this `None` is correct.
    pub imds_listen_addr: Option<SocketAddr>,
    /// Per-CN HS256 token key for IMDSv2 session tokens. `None`
    /// disables the IMDS listener (same as `imds_listen_addr` being
    /// `None`). tritond delivers the bytes at CN approval alongside
    /// the console-ticket key; the registration-side wire is a
    /// follow-up commit.
    pub imds_token_key_bytes: Option<[u8; IMDS_TOKEN_KEY_BYTES]>,
    /// File the IMDS binding table mirrors to. `Some` opens the
    /// table via [`ImdsBindingTable::open`] so a tritonagent restart
    /// recovers existing VMs' bindings before any new provision job
    /// arrives. `None` keeps the table in-memory only (tests).
    pub imds_bindings_path: Option<PathBuf>,
    /// `false` is the rollback path: miss events are dropped and
    /// intra-VPC forwarding falls back to the pre-shipped per-port
    /// `peer_table`. No VM/kmod restart needed to flip.
    pub peer_resolver_enabled: bool,
    /// `None` skips the migrate listener (older registrations lack
    /// the key). tritond delivers it alongside the console-ticket
    /// key at CN approval.
    pub migrate_ticket_key: Option<[u8; tritond_auth::MIGRATE_TICKET_KEY_BYTES]>,
    /// TCP port the live-migration WebSocket listener binds (on
    /// `admin_ip`). Source-side agents dial
    /// `wss://<target_admin_ip>:<this_port>/migrate/{id}`.
    pub migrate_listen_port: u16,
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
    // Per-CN IMDS reverse-lookup table. Disk-backed when
    // `cfg.imds_bindings_path` is set so a tritonagent restart picks
    // up every existing VM's binding before traffic flows. Falls
    // back to an empty in-memory table when unconfigured (tests).
    let bindings = match cfg.imds_bindings_path.clone() {
        Some(path) => {
            let table = ImdsBindingTable::open(path.clone());
            let loaded = table.len();
            info!(
                path = %path.display(),
                loaded,
                "imds: bindings restored from disk",
            );
            // Re-install the static ARP entries on `proteusimds0`
            // for every restored binding so listener replies route
            // back through the kmod. Idempotent — `arp -s` of an
            // existing entry is a no-op.
            for ip in table.pseudo_srcs() {
                crate::imds_arp::add(ip);
            }
            table
        }
        None => ImdsBindingTable::new(),
    };

    // In-VM IMDSv2 listener (`IMDS_DESIGN.md` §3). Skipped when
    // either the bind address or the token key isn't configured --
    // the production path needs both, plus the proteus apply path
    // populating `blueprint.imds_bindings` (today: empty), so an
    // unwired agent stays silent rather than serving 401s out of an
    // empty table.
    if let (Some(bind), Some(token_key_bytes)) = (cfg.imds_listen_addr, cfg.imds_token_key_bytes) {
        use crate::imds::{ImdsListenerConfig, start as imds_start};
        use crate::imds_data::TritondRealizedDataSource;
        let realized_source =
            std::sync::Arc::new(TritondRealizedDataSource::new((*client).clone()));
        let cfg_imds = ImdsListenerConfig {
            bind,
            token_key_bytes,
            bindings: bindings.clone(),
            realized_source,
            tritond_client: client.clone(),
        };
        if let Err(e) = imds_start(cfg_imds).await {
            warn!(error = %e, "imds: listener failed to start; skipping");
        }
    } else {
        info!("imds: listener disabled (no bind addr or token key)");
    }
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

    // Shared reservoir handle: one [`ReservoirTool`] serializes all
    // `/dev/vmmctl` access (it's opened `O_EXCL`) across the startup
    // floor-apply below and the status collector inside the publisher.
    let reservoir = Arc::new(ReservoirTool::new());

    // Establish the reservoir floor before serving jobs. The effective
    // policy (per-CN override else cluster default) is pulled from
    // tritond; the agent-local `--reservoir-percent` is the fallback if
    // the control plane is unreachable, and `--no-reservoir` is a hard
    // local kill switch. Best-effort: a missing `rsrvrctl` (non-SmartOS
    // dev host) or an under-provisioned box logs and continues rather
    // than blocking startup. Gated on the heartbeater flag so integration
    // tests don't touch the host.
    let reservoir_runtime = if cfg.spawn_heartbeater && cfg.reservoir_enabled {
        let (enabled, percent) = match client.agent_get_config().send().await {
            Ok(resp) => {
                let r = resp.into_inner();
                (r.reservoir_enabled, r.reservoir_percent)
            }
            Err(e) => {
                warn!(error = %e, "agent config pull failed; using local reservoir defaults");
                (true, cfg.reservoir_percent)
            }
        };
        let mgr =
            reservoir::ReservoirManager::new(Arc::clone(&reservoir), Arc::new(KstatTool::new()));
        if enabled {
            match mgr.apply_floor(percent).await {
                Ok(st) => info!(
                    current_mib = st.current_mib(),
                    limit_mib = st.limit_mib,
                    "reservoir floor applied",
                ),
                Err(e) => warn!(
                    error = %format!("{e:#}"),
                    "reservoir floor apply failed; continuing without a managed reservoir",
                ),
            }
        } else {
            info!("reservoir disabled by control-plane policy for this CN");
        }
        Some(reservoir::ReservoirRuntime::new(enabled, percent, mgr))
    } else {
        None
    };

    // Optional background publisher. Spawned only when the operator
    // hasn't asked us to stay quiet (the integration-test path).
    // Both handles must outlive the poll loop so that on shutdown
    // we can drain them gracefully — the heartbeater holds the
    // dirty flag the watcher pokes, and tearing them down out of
    // order risks a missed status sample.
    let mut publisher = if cfg.spawn_heartbeater {
        Some(spawn_publisher(Arc::clone(&client), Arc::clone(&reservoir)))
    } else {
        None
    };

    // Metrics ticker rides on the same `spawn_heartbeater` flag so
    // integration tests that disable the heartbeater don't get
    // metrics chatter either. The CN UUID is parsed from
    // `agent_id`; main.rs builds AgentConfig with the SmartOS
    // server_uuid as the agent_id, so this round-trips cleanly.
    let (mut metrics_handle, mut log_handle) = if cfg.spawn_heartbeater {
        match Uuid::parse_str(&cfg.agent_id) {
            Ok(cn_uuid) => {
                let m = metrics::spawn(
                    Arc::clone(&client),
                    cn_uuid,
                    Arc::new(KstatTool::new()),
                    metrics::DEFAULT_METRICS_INTERVAL,
                );
                let l = log_tailer::spawn(Arc::clone(&client), log_tailer::Config::new(cn_uuid));
                (Some(m), Some(l))
            }
            Err(_) => {
                warn!(
                    agent_id = %cfg.agent_id,
                    "agent_id is not a UUID; metrics + log emission disabled"
                );
                (None, None)
            }
        }
    } else {
        (None, None)
    };

    // Console listener: gated on the same `spawn_heartbeater` flag as
    // the metrics/log tickers (so tritond integration tests with
    // `--no-heartbeater` don't open a port), and only if we have the
    // three things it needs: an admin IP to bind, a per-CN
    // console-ticket key to verify against, and TLS material. Spawn it
    // detached — its lifetime is the process; a serve() error is logged
    // (it would mean the bind failed) but is not fatal to the agent.
    maybe_spawn_console_listener(&cfg);
    maybe_spawn_migrate_listener(&cfg);

    // DHCP-event ticker: drains the Proteus event ring and forwards
    // observed DHCP requests to tritond so lease records' renewal
    // clocks stay fresh. Gated on the same `spawn_heartbeater` flag as
    // the metrics/log tickers so `--no-heartbeater` integration tests
    // don't touch /dev/proteus; best-effort if the device is absent.
    let mut dhcp_events_handle = if cfg.spawn_heartbeater {
        Some(dhcp_events::spawn(
            Arc::clone(&client),
            cfg.proteus_dev.clone(),
            dhcp_events::DEFAULT_DHCP_EVENT_INTERVAL,
            cfg.peer_resolver_enabled,
        ))
    } else {
        None
    };

    let result =
        run_poll_loop(client.as_ref(), &cfg, &bindings, reservoir_runtime.as_ref()).await;

    if let Some(h) = dhcp_events_handle.take() {
        h.shutdown().await;
    }

    if let Some(h) = metrics_handle.take() {
        h.shutdown().await;
    }
    if let Some(h) = log_handle.take() {
        h.shutdown().await;
    }
    if let Some(p) = publisher.take() {
        p.shutdown().await;
    }

    result
}

/// Spawn the on-CN console listener if and only if the config has all
/// the pieces it needs and the heartbeater/metrics tickers are also
/// enabled. Logs a warning and returns without doing anything otherwise
/// (a CN with no console is degraded, not broken).
fn maybe_spawn_console_listener(cfg: &AgentConfig) {
    if !cfg.spawn_heartbeater {
        return;
    }
    let Some(admin_ip) = cfg.admin_ip else {
        warn!("no admin IP known; serial / VNC console listener not started");
        return;
    };
    let Some(console_ticket_key) = cfg.console_ticket_key else {
        warn!(
            "no per-CN console-ticket key; serial / VNC console listener not started \
             (re-register this CN to obtain one)",
        );
        return;
    };
    let Some(tls) = cfg.console_tls.clone() else {
        warn!("no console TLS material; serial / VNC console listener not started");
        return;
    };
    let server_uuid = match Uuid::parse_str(&cfg.agent_id) {
        Ok(u) => u,
        Err(_) => {
            warn!(
                agent_id = %cfg.agent_id,
                "agent_id is not a UUID; console listener not started",
            );
            return;
        }
    };
    let bind = SocketAddr::new(IpAddr::V4(admin_ip), cfg.console_listen_port);
    let listener_cfg = console::ConsoleListenerConfig {
        bind,
        tls,
        console_ticket_key,
        server_uuid,
        zoneadm: ZoneadmTool::new(),
        edge_root: cfg.edge_root.clone(),
    };
    tokio::spawn(async move {
        if let Err(e) = console::serve(listener_cfg).await {
            error!(error = %format!("{e:#}"), "console listener exited");
        }
    });
}

/// Spawn the per-CN live-migration WebSocket listener if and only if
/// the config has all the pieces. Mirrors
/// [`maybe_spawn_console_listener`]'s contract: a missing piece is a
/// warn-and-skip (a CN with no migrate listener is degraded — it can
/// be a migration *source* but not a *target* — not broken).
fn maybe_spawn_migrate_listener(cfg: &AgentConfig) {
    if !cfg.spawn_heartbeater {
        return;
    }
    let Some(admin_ip) = cfg.admin_ip else {
        warn!("no admin IP known; live-migration listener not started");
        return;
    };
    let Some(migrate_ticket_key) = cfg.migrate_ticket_key else {
        warn!(
            "no per-CN migrate-ticket key; live-migration listener not started \
             (re-register this CN to obtain one)",
        );
        return;
    };
    // The migrate listener reuses the console listener's TLS material
    // (one cert per CN serves both ports); a missing TLS bag means the
    // CN couldn't generate one at startup, which is a strictly worse
    // failure than the missing-key case above and we already logged it
    // from the console-listener path. Just skip.
    let Some(tls) = cfg.console_tls.clone() else {
        warn!("no TLS material; live-migration listener not started");
        return;
    };
    let server_uuid = match Uuid::parse_str(&cfg.agent_id) {
        Ok(u) => u,
        Err(_) => {
            warn!(
                agent_id = %cfg.agent_id,
                "agent_id is not a UUID; live-migration listener not started",
            );
            return;
        }
    };
    let bind = SocketAddr::new(IpAddr::V4(admin_ip), cfg.migrate_listen_port);
    let listener_cfg = migrate::MigrateListenerConfig {
        bind,
        tls,
        migrate_ticket_key,
        server_uuid,
    };
    tokio::spawn(async move {
        if let Err(e) = migrate::serve(listener_cfg).await {
            error!(error = %format!("{e:#}"), "live-migration listener exited");
        }
    });
}

/// The job-claim loop, factored out so [`run`] can wrap it with the
/// publisher's lifetime without duplicating the poll/backoff logic.
///
/// Returns `Ok(())` only on a clean caller-initiated stop; today
/// nothing inside the loop can return a clean `Ok(())`, but the
/// signature matches `run` so future SIGTERM handling drops in
/// without a refactor.
async fn run_poll_loop(
    client: &Client,
    cfg: &AgentConfig,
    bindings: &ImdsBindingTable,
    reservoir: Option<&reservoir::ReservoirRuntime>,
) -> Result<()> {
    loop {
        match poll_once(client, cfg, bindings, reservoir).await {
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
fn spawn_publisher(client: Arc<Client>, reservoir: Arc<ReservoirTool>) -> PublisherHandles {
    let sink = TritondStatusSink::new(client);
    let vmadm = Arc::new(VmadmTool::new());
    let zfs = Arc::new(ZfsTool::new());
    let kstat = Arc::new(KstatTool::new());
    let disk_usage = DiskUsageSampler::new(Arc::clone(&zfs), Arc::new(UuidNamedImageFilter));
    let collector = StatusCollector::new(vmadm, zfs, kstat, reservoir, disk_usage);

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
async fn poll_once(
    client: &Client,
    cfg: &AgentConfig,
    bindings: &ImdsBindingTable,
    reservoir: Option<&reservoir::ReservoirRuntime>,
) -> Result<bool> {
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

    let outcome = match drive_job(client, cfg, bindings, reservoir, &job).await {
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
async fn drive_job(
    client: &Client,
    cfg: &AgentConfig,
    bindings: &ImdsBindingTable,
    reservoir: Option<&reservoir::ReservoirRuntime>,
    job: &ProvisioningJob,
) -> Result<()> {
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
        JobKind::ApplyPortBlueprint { nic_id, .. } => {
            // Re-apply a single running port's blueprint at its current
            // (bumped) generation. The port already exists and is
            // started from provision, so apply only -- no zone or port
            // re-create. tritond owns the blueprint and the generation;
            // the agent fetches the recomputed bytes and applies them.
            let proteus = open_proteus_lifecycle(&cfg.proteus_dev)?;
            let port_blueprint = client
                .fetch_port_blueprint(*nic_id)
                .await
                .with_context(|| format!("fetch port blueprint to re-apply for nic {nic_id}"))?;
            proteus
                .apply_blueprint(&port_blueprint)
                .with_context(|| format!("re-apply Proteus blueprint for nic {nic_id}"))?;
        }
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
            let registered = register_blueprint_bindings(bindings, &blueprint);
            if registered > 0 {
                info!(
                    instance_id = %instance_id,
                    bindings = registered,
                    "imds: registered binding(s) for provision",
                );
            }
            let nic_tags = started_ports
                .iter()
                .map(|port| (port.nic_id, port.link_name.clone()))
                .collect::<BTreeMap<_, _>>();
            // For a reservoir-managed CN, grow the bhyve memory reservoir
            // to fit this guest before creating it (the kernel won't fall
            // back to transient memory). If the host is at reservoir
            // capacity, fail the provision so placement steers elsewhere.
            let use_reservoir = match reservoir {
                Some(rt) if rt.enabled && vmadm::blueprint_is_bhyve(&blueprint) => {
                    let requested_mib = blueprint
                        .instance
                        .as_ref()
                        .map(|i| i.memory_bytes / (1024 * 1024))
                        .unwrap_or(0);
                    if let Err(err) = rt.ensure_capacity(requested_mib).await {
                        cleanup_started_ports(proteus.as_ref(), &started_ports);
                        return Err(err).with_context(|| {
                            format!("reserve {requested_mib} MiB of bhyve reservoir for instance {instance_id}")
                        });
                    }
                    true
                }
                _ => false,
            };
            if let Err(err) =
                vmadm::create_zone_with_nic_tags(&blueprint, &nic_tags, use_reservoir).await
            {
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
            let removed = bindings.remove_by_instance(*instance_id);
            for ip in &removed {
                crate::imds_arp::del(*ip);
            }
            if !removed.is_empty() {
                info!(
                    instance_id = %instance_id,
                    removed = removed.len(),
                    "imds: evicted bindings + ARP for deleted instance",
                );
            }
        }
        JobKind::EdgeApply {
            edge_cluster_id,
            edge_instance_id,
            desired_generation,
            manifest_bytes,
        } => {
            let status = match edge::apply(
                &cfg.edge_root,
                &cfg.fhrun_bin,
                *edge_instance_id,
                manifest_bytes,
            ) {
                Ok(status) => status,
                Err(err) => {
                    let chain = format!("{err:#}");
                    report_failed_realization(
                        client,
                        RealizerId::EdgeCluster(*edge_cluster_id),
                        NetworkResourceId::EdgeCluster(*edge_cluster_id),
                        *desired_generation,
                        format!("edge instance {edge_instance_id} failed: {chain}"),
                    )
                    .await;
                    return Err(err)
                        .with_context(|| format!("apply edge instance {edge_instance_id}"));
                }
            };
            report_applied_edge_realization(
                client,
                *edge_cluster_id,
                *edge_instance_id,
                *desired_generation,
                &status,
            )
            .await
            .with_context(|| format!("report edge cluster {edge_cluster_id} realization"))?;
        }
        JobKind::EdgeReap { edge_instance_id } => {
            edge::reap(&cfg.edge_root, *edge_instance_id)
                .with_context(|| format!("reap edge instance {edge_instance_id}"))?;
        }
        // Live-migration arms. Cleanup paths do real work
        // (vmadm-delete + zfs-destroy of the migration snapshots);
        // ZFS-send / VMM-stream / Proteus-flip are stubs that
        // `Ok(())` instead of bailing so a saga on a half-deployed
        // fleet still drains the queue and surfaces the gap on the
        // migration record.
        JobKind::MigrationCleanupSource {
            instance_id,
            migration_id,
        }
        | JobKind::MigrationCleanupTarget {
            instance_id,
            migration_id,
        } => {
            let side = match &job.kind {
                JobKind::MigrationCleanupSource { .. } => "source",
                _ => "target",
            };
            // Best-effort `vmadm delete` first so the zone
            // releases its hold on the dataset before we try
            // to destroy snapshots. Idempotent against zone-
            // not-found (mirrors the `Delete` arm above).
            if let Err(e) = vmadm::delete_zone(*instance_id).await {
                warn!(
                    %migration_id, %instance_id, side, error = %e,
                    "migration cleanup: vmadm delete failed (best-effort, continuing)",
                );
            }
            // Destroy the two migration snapshots the saga
            // creates. `destroy_snapshot` errors on missing
            // snapshots, so we warn-and-continue rather than
            // failing the job — a half-completed migration
            // may not have created both snapshots.
            let dataset = format!("zones/{instance_id}");
            for snap_label in &["migration-base", "migration-final"] {
                let snap = format!("{dataset}@{snap_label}");
                if let Err(e) = zfs::destroy_snapshot(&snap).await {
                    warn!(
                        %migration_id, %instance_id, side, %snap, error = %e,
                        "migration cleanup: destroy_snapshot failed (best-effort, continuing)",
                    );
                }
            }
            info!(
                %migration_id, %instance_id, side,
                "migration cleanup: vmadm-delete + zfs-destroy completed (best-effort)",
            );
        }
        JobKind::MigrateZfsSend {
            migration_id,
            instance_id,
            role,
            dataset,
            from_snap,
            to_snap,
            peer_endpoint,
            peer_spki_sha256_hex,
            ticket,
        } => match role {
            tritond_client::types::MigrationJobRole::Source => {
                // Snapshot the source dataset (idempotent — `zfs
                // snapshot` errors if the snapshot already exists,
                // which a re-claimed job after a crash hits; we
                // ignore that specific error).
                if let Err(e) = zfs::snapshot_for_migration(
                    dataset,
                    to_snap.trim_start_matches(&format!("{dataset}@migration-")),
                )
                .await
                {
                    let msg = e.to_string();
                    if !msg.contains("dataset already exists") {
                        anyhow::bail!("snapshot {to_snap} failed: {msg}");
                    }
                    info!(
                        %migration_id, %to_snap,
                        "migrate-zfs-send: snapshot already exists; reusing",
                    );
                }

                let peer_endpoint = peer_endpoint.as_ref().ok_or_else(|| {
                    anyhow::anyhow!("Source-role MigrateZfsSend missing peer_endpoint")
                })?;
                let peer_spki = peer_spki_sha256_hex.as_ref().ok_or_else(|| {
                    anyhow::anyhow!("Source-role MigrateZfsSend missing peer_spki_sha256_hex")
                })?;
                let ticket = ticket
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("Source-role MigrateZfsSend missing ticket"))?;

                let server_uuid = Uuid::parse_str(&cfg.agent_id)
                    .context("agent_id is not a UUID; cannot present source_cn in dial")?;
                let transport = migrate::dial_zfs(migrate::DialZfsParams {
                    base_url: peer_endpoint.clone(),
                    migration_id: *migration_id,
                    source_cn: server_uuid,
                    vm_uuid: *instance_id,
                    target_dataset: dataset.clone(),
                    ticket: ticket.clone(),
                    target_spki_sha256_hex: peer_spki.clone(),
                })
                .await
                .context("dial target /migrate/{id}/zfs")?;

                let mut child = match from_snap {
                    Some(from) => zfs::spawn_send_incremental(from, to_snap)
                        .context("spawn zfs send -i for incremental")?,
                    None => zfs::spawn_send_full(to_snap).context("spawn zfs send for full")?,
                };
                let stdout = child
                    .stdout
                    .take()
                    .ok_or_else(|| anyhow::anyhow!("zfs send child has no piped stdout"))?;
                let sender = tritond_vmm_migrate::zfs_stream::ZfsSender::new(transport, stdout);
                let bytes = sender.run().await.context("ZfsSender::run")?;
                let status = child.wait().await.context("await zfs send exit")?;
                if !status.success() {
                    anyhow::bail!("zfs send exited non-zero: {status}");
                }
                info!(
                    %migration_id, %instance_id, %dataset, %to_snap, bytes,
                    "migrate-zfs-send/source: stream completed",
                );
            }
            tritond_client::types::MigrationJobRole::Target => {
                // The migrate listener's `/migrate/{id}/zfs` route
                // already spawns `zfs recv` and pumps the WS
                // frames into it (`ZfsReceiver`); the source side
                // dials that route. From the target *job's*
                // perspective there's nothing to do here — the
                // dispatcher reports completed immediately so the
                // saga's await pair can resolve once the source
                // side reports its own completion. The listener
                // running on this CN is the actual workload.
                info!(
                    %migration_id, %instance_id, %dataset,
                    "migrate-zfs-send/target: dispatcher is a no-op; listener handles transfer",
                );
            }
        },
        JobKind::MigrateVmmStream {
            migration_id,
            instance_id,
            role,
        } => {
            // Stub so an accidental enqueue doesn't wedge the agent
            // before the dispatcher lands.
            info!(
                %migration_id, %instance_id, ?role,
                "migrate-vmm-stream: dispatcher pending — completing stub",
            );
        }
        JobKind::ProteusActivate {
            migration_id,
            instance_id,
            nic_ids,
        } => {
            info!(
                %migration_id, %instance_id, nic_count = nic_ids.len(),
                "proteus-activate: dispatcher pending — completing stub",
            );
        }
        JobKind::ProteusDeactivate {
            migration_id,
            instance_id,
            nic_ids,
        } => {
            info!(
                %migration_id, %instance_id, nic_count = nic_ids.len(),
                "proteus-deactivate: dispatcher pending — completing stub",
            );
        }
    }

    Ok(())
}

async fn report_applied_edge_realization<R>(
    sink: &R,
    edge_cluster_id: Uuid,
    edge_instance_id: Uuid,
    desired_generation: u64,
    status: &edge::EdgeApplyStatus,
) -> Result<()>
where
    R: NetworkRealizationSink + Sync,
{
    let request = NetworkRealizationRequest {
        resource: NetworkResourceId::EdgeCluster(edge_cluster_id),
        realizer: RealizerId::EdgeCluster(edge_cluster_id),
        generation: desired_generation,
        status: RealizationStatus::Applied,
        message: Some(format!(
            "edge instance {edge_instance_id} healthy via {} backend; ruleset bytes {}",
            status.backend, status.last_ruleset_bytes
        )),
    };
    sink.report_network_realization(request).await
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
        link_name: &str,
    ) -> Result<proteus::ProteusPortStatus>;

    /// Re-apply a port's blueprint in place: no create, no start.
    /// Pushes a recomputed blueprint to an already-running port (e.g. a
    /// FIP attach on a running VM). The kmod no-ops a re-apply at the
    /// same generation, so the caller must bump the generation first.
    fn apply_blueprint(&self, blueprint: &PortBlueprint) -> Result<()>;

    fn cleanup_port(&self, port_id: PortId) -> Result<()>;
}

impl<T> ProteusLifecycle for proteus::ProteusClient<T>
where
    T: proteus_ioctl::Transport,
{
    fn ensure_started(
        &self,
        blueprint: &PortBlueprint,
        _link_name: &str,
    ) -> Result<proteus::ProteusPortStatus> {
        proteus::ProteusClient::ensure_started(self, blueprint, None)
    }

    fn apply_blueprint(&self, blueprint: &PortBlueprint) -> Result<()> {
        proteus::ProteusClient::apply_blueprint(self, blueprint)
    }

    fn cleanup_port(&self, port_id: PortId) -> Result<()> {
        proteus::ProteusClient::cleanup_port(self, port_id)
    }
}

#[cfg(target_os = "illumos")]
struct KernelProteusLifecycle {
    inner: proteus::ProteusClient<proteus_ioctl::KernelTransport>,
}

#[cfg(target_os = "illumos")]
impl KernelProteusLifecycle {
    fn new(transport: proteus_ioctl::KernelTransport) -> Self {
        Self {
            inner: proteus::ProteusClient::new(transport),
        }
    }
}

#[cfg(target_os = "illumos")]
impl ProteusLifecycle for KernelProteusLifecycle {
    fn ensure_started(
        &self,
        blueprint: &PortBlueprint,
        link_name: &str,
    ) -> Result<proteus::ProteusPortStatus> {
        use proteus_ioctl::dladm::{DATALINK_CLASS_MISC, DL_ETHER, DLADM_OPT_ACTIVE, DladmHandle};

        let dladm = DladmHandle::open().with_context(
            || "open libdladm for Proteus link allocation; tritonagent must run as root on SmartOS",
        )?;
        let linkid = dladm
            .create_datalink_id(link_name, DATALINK_CLASS_MISC, DL_ETHER, DLADM_OPT_ACTIVE)
            .with_context(|| {
                format!(
                    "allocate dladm link {link_name} for Proteus port {}",
                    blueprint.port_id,
                )
            })?;

        if let Err(err) = self.inner.create_port(blueprint, Some(linkid)) {
            let _ = dladm.destroy_datalink_id(linkid, DLADM_OPT_ACTIVE);
            return Err(err);
        }

        self.inner.apply_blueprint(blueprint)?;
        self.inner.assert_generation_applied(blueprint)?;
        self.inner.start_port(blueprint.port_id)?;
        self.inner.dump_status(blueprint.port_id)
    }

    fn apply_blueprint(&self, blueprint: &PortBlueprint) -> Result<()> {
        self.inner.apply_blueprint(blueprint)
    }

    fn cleanup_port(&self, port_id: PortId) -> Result<()> {
        self.inner.cleanup_port(port_id)
    }
}

fn open_proteus_lifecycle(path: &Path) -> Result<Box<dyn ProteusLifecycle>> {
    #[cfg(target_os = "illumos")]
    {
        let transport = proteus_ioctl::KernelTransport::open_path(path)
            .with_context(|| format!("open Proteus device {}", path.display()))?;
        Ok(Box::new(KernelProteusLifecycle::new(transport)))
    }
    #[cfg(not(target_os = "illumos"))]
    {
        let _ = path;
        bail!(
            "Proteus kernel transport is only available on illumos; use --dry-run on non-SmartOS hosts"
        );
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RealizedProvisionPort {
    nic_id: Uuid,
    port_id: PortId,
    link_name: String,
}

async fn realize_provision_ports<S, R, P>(
    source: &S,
    sink: &R,
    proteus: &P,
    agent_id: &str,
    blueprint: &ProvisioningBlueprint,
) -> Result<Vec<RealizedProvisionPort>>
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
        let link_name = proteus::link_name_for_port(port_id);
        let realized = RealizedProvisionPort {
            nic_id: nic.id,
            port_id,
            link_name: link_name.clone(),
        };
        let desired_generation = port_blueprint.generation.0;
        let resource = port_realization_resource(nic);

        match proteus.ensure_started(&port_blueprint, &link_name) {
            Ok(status) => {
                let applied_generation = status.generation.applied_generation.0;
                // Register the proteus link as a SmartOS nic_tag so
                // `vmadm create` accepts it. Without this step the
                // kmod link exists but vmadm rejects with "Invalid
                // nic tag". Idempotent (`nictagadm exists` short-
                // circuits before `add`) and best-effort so a
                // stale tag from a previous realize doesn't block
                // provisioning. Skipped under `dry_run`.
                if let Err(err) = ensure_proteus_nic_tag(&link_name) {
                    tracing::warn!(
                        nic_id = %nic.id,
                        link_name,
                        error = %err,
                        "nictagadm registration failed; vmadm create may reject the tag"
                    );
                }
                let request = NetworkRealizationRequest {
                    resource,
                    realizer: realizer.clone(),
                    generation: applied_generation,
                    status: RealizationStatus::Applied,
                    message: Some(format!(
                        "Proteus port {port_id} ({link_name}) applied generation {applied_generation}"
                    )),
                };
                if let Err(err) = sink.report_network_realization(request).await {
                    let mut cleanup = started_ports.clone();
                    cleanup.push(realized);
                    cleanup_started_ports(proteus, &cleanup);
                    return Err(err).with_context(|| {
                        format!("report applied Proteus realization for NIC {}", nic.id)
                    });
                }
                info!(
                    nic_id = %nic.id,
                    port_id = %port_id,
                    link_name,
                    desired_generation,
                    applied_generation,
                    "Proteus port realized",
                );
                started_ports.push(realized);
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
                let mut cleanup = started_ports.clone();
                cleanup.push(realized);
                cleanup_started_ports(proteus, &cleanup);
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
        warn!(error = %err, "failed to report network realization failure");
    }
}

fn cleanup_started_ports<P>(proteus: &P, ports: &[RealizedProvisionPort])
where
    P: ProteusLifecycle + ?Sized,
{
    for port in ports.iter().rev() {
        if let Err(err) = proteus.cleanup_port(port.port_id) {
            warn!(port_id = %port.port_id, error = %err, "failed to clean up Proteus port");
        }
        if let Err(err) = drop_proteus_nic_tag(&port.link_name) {
            warn!(
                link_name = %port.link_name,
                error = %err,
                "failed to delete proteus nic_tag; will leave stale entry"
            );
        }
    }
}

/// Register a proteus pseudo-link as a SmartOS nic_tag so
/// `vmadm create` accepts it. proteus links aren't in the boot-time
/// `/usbkey/config` nic_tag list, so without this step vmadm rejects
/// the per-NIC `nic_tag=proteus<linkid>` with "Invalid nic tag".
///
/// Idempotent: `nictagadm exists -l` short-circuits the add when the
/// tag is already present (e.g. a previous Provision job for the same
/// port that completed past the agent's local cleanup). We use the
/// `-l` "local" flag because proteus is a pseudo-link, not a physical
/// NIC -- the same flag etherstubs use.
fn ensure_proteus_nic_tag(link_name: &str) -> anyhow::Result<()> {
    use std::process::Command;
    // `nictagadm exists` returns 0 if the tag is registered, 1
    // otherwise. We treat exit 0 as "already done, nothing to do".
    let exists = Command::new("nictagadm")
        .args(["exists", "-l", link_name])
        .status()
        .with_context(|| format!("invoke nictagadm exists for {link_name}"))?;
    if exists.success() {
        return Ok(());
    }
    let out = Command::new("nictagadm")
        .args(["add", "-l", link_name])
        .output()
        .with_context(|| format!("invoke nictagadm add for {link_name}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!(
            "nictagadm add -l {link_name} failed (exit {}): {}",
            out.status,
            stderr.trim(),
        );
    }
    Ok(())
}

/// Tear down the nic_tag created by `ensure_proteus_nic_tag` when the
/// port is cleaned up. Best-effort: failures are logged by the caller
/// but never propagated; a stale tag at worst trips the
/// `nictagadm exists` short-circuit on the next realize.
fn drop_proteus_nic_tag(link_name: &str) -> anyhow::Result<()> {
    use std::process::Command;
    let exists = Command::new("nictagadm")
        .args(["exists", "-l", link_name])
        .status()
        .with_context(|| format!("invoke nictagadm exists for {link_name}"))?;
    if !exists.success() {
        return Ok(());
    }
    let out = Command::new("nictagadm")
        .args(["delete", "-f", link_name])
        .output()
        .with_context(|| format!("invoke nictagadm delete for {link_name}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!(
            "nictagadm delete {link_name} failed (exit {}): {}",
            out.status,
            stderr.trim(),
        );
    }
    Ok(())
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

/// Refuses a Provision when the host can't satisfy the image's
/// `min_smartos_platform` (lex-compared against `uname -v`). Caller
/// wraps the error into `JobOutcome::Failed`. `compatibility.brand`
/// is enforced by `vmadm create` itself.
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
            subnets: Vec::new(),
            disks: Vec::new(),
            ssh_public_keys: Vec::new(),
            managed_identity: None,
            imds_bindings: Vec::new(),
            provision_metadata: Vec::new(),
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

        assert_eq!(started.len(), 1);
        assert_eq!(started[0].nic_id, nic_id);
        assert_eq!(started[0].port_id, port.port_id);
        assert_eq!(
            started[0].link_name,
            proteus::link_name_for_port(port.port_id)
        );
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
        assert!(
            reports[0]
                .message
                .as_deref()
                .is_some_and(|message| message.contains(&started[0].link_name))
        );
    }

    #[tokio::test]
    async fn report_applied_edge_realization_uses_edge_cluster_generation() {
        let sink = RecordingRealizationSink::default();
        let edge_cluster_id = Uuid::new_v4();
        let edge_instance_id = Uuid::new_v4();
        let status = edge::EdgeApplyStatus {
            backend: "nftables".to_string(),
            healthy: true,
            last_ruleset_bytes: 42,
            error: None,
        };

        report_applied_edge_realization(&sink, edge_cluster_id, edge_instance_id, 9, &status)
            .await
            .unwrap();

        let reports = sink.reports.lock().unwrap();
        assert_eq!(reports.len(), 1);
        match &reports[0].resource {
            NetworkResourceId::EdgeCluster(id) => assert_eq!(*id, edge_cluster_id),
            other => panic!("unexpected reported resource: {other:?}"),
        }
        match &reports[0].realizer {
            RealizerId::EdgeCluster(id) => assert_eq!(*id, edge_cluster_id),
            other => panic!("unexpected reported realizer: {other:?}"),
        }
        assert_eq!(reports[0].generation, 9);
        assert_eq!(reports[0].status, RealizationStatus::Applied);
        assert!(
            reports[0]
                .message
                .as_deref()
                .is_some_and(|message| message.contains(&edge_instance_id.to_string()))
        );
    }
}
