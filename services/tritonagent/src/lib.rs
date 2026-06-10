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

pub mod capacity;
pub mod console;
pub mod console_creds;
pub mod credentials;
pub mod dhcp_events;
pub mod edge;
pub mod fip_link;
pub mod fip_net;
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
pub mod migrate_jobs;
pub mod migrate_probe;
pub mod migrate_progress;
pub mod migrate_vmm;
pub mod nic_tags;
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
    /// Local nic_tags this CN provides, enumerated from `nictagadm` /
    /// sysinfo at startup. Published to tritond once on the
    /// authenticated `/v1/agent/nic-tags` endpoint at the top of
    /// [`run`] (keyed by the authenticated CN). Empty = no-op.
    pub nic_tags: Vec<tritond_client::types::RegisterNicTagProvision>,
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

    // Publish this CN's nic_tag inventory on the authenticated endpoint
    // (keyed server-side by the bound CN, never by request body). Best
    // effort: a failure here means floating-IP placement onto this CN
    // is fail-closed until the next publish, but must not block the
    // job loop. Gated on the heartbeater flag so integration tests that
    // don't want control-plane chatter stay quiet.
    if cfg.spawn_heartbeater {
        let report = tritond_client::types::NicTagInventoryReport {
            nic_tags: cfg.nic_tags.clone(),
        };
        match client.agent_report_nic_tags().body(report).send().await {
            Ok(_) => info!(count = cfg.nic_tags.len(), "published CN nic_tag inventory",),
            Err(e) => warn!(
                error = %e,
                "failed to publish CN nic_tag inventory; floating-IP placement \
                 onto this CN stays fail-closed until the next publish",
            ),
        }
    }

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

    // Capacity ticker: publishes the placement engine's cn-capacity
    // floor (static hardware + live RAM/CPU/zpool-free). Gated on the
    // same flag as the heartbeater so quiet integration tests stay
    // quiet. nic_tags travel on the capacity row too (authoritative for
    // the cn-nic-tags placement filter), so carry the same names the
    // nic-tag inventory publish used above.
    let mut capacity_handle = if cfg.spawn_heartbeater {
        let nic_tag_names: Vec<String> = cfg.nic_tags.iter().map(|t| t.name.clone()).collect();
        Some(capacity::spawn(
            Arc::clone(&client),
            nic_tag_names,
            capacity::DEFAULT_CAPACITY_INTERVAL,
        ))
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

    // One migration data plane per CN (see `migrate_jobs` docs).
    let migration_lane = migrate_jobs::new_lane();

    let result = run_poll_loop(
        &client,
        &cfg,
        &bindings,
        reservoir_runtime.as_ref(),
        &migration_lane,
    )
    .await;

    if let Some(h) = dhcp_events_handle.take() {
        h.shutdown().await;
    }

    if let Some(h) = metrics_handle.take() {
        h.shutdown().await;
    }
    if let Some(h) = capacity_handle.take() {
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
        proteus_dev: cfg.proteus_dev.clone(),
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
    client: &Arc<Client>,
    cfg: &AgentConfig,
    bindings: &ImdsBindingTable,
    reservoir: Option<&reservoir::ReservoirRuntime>,
    migration_lane: &Arc<tokio::sync::Semaphore>,
) -> Result<()> {
    loop {
        match poll_once(client, cfg, bindings, reservoir, migration_lane).await {
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
    client: &Arc<Client>,
    cfg: &AgentConfig,
    bindings: &ImdsBindingTable,
    reservoir: Option<&reservoir::ReservoirRuntime>,
    migration_lane: &Arc<tokio::sync::Semaphore>,
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

    // Migration data-plane streams run for hours; detach them onto
    // the per-CN lane so this loop keeps serving fast jobs. The
    // detached task reports its own completion. Dry-run jobs stay
    // inline so they keep their report-Completed-without-work path.
    if !cfg.dry_run && migrate_jobs::is_data_plane_kind(&job.kind) {
        migrate_jobs::spawn_data_plane_job(
            Arc::clone(client),
            cfg.clone(),
            Arc::clone(migration_lane),
            job,
        );
        return Ok(true);
    }

    let (outcome, result) = match drive_job(client.as_ref(), cfg, bindings, reservoir, &job).await {
        Ok(result) => (JobOutcome::Completed, result),
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
            (JobOutcome::Failed(chain), None)
        }
    };

    let updated = client
        .agent_complete_job()
        .job_id(job.id)
        .body(CompleteJobRequest { outcome, result })
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
/// `Ok(result)` for success (caller reports `Completed` with the
/// optional per-kind result payload), `Err` for agent-side
/// failure (caller reports `Failed { reason }`).
async fn drive_job(
    client: &Client,
    cfg: &AgentConfig,
    bindings: &ImdsBindingTable,
    reservoir: Option<&reservoir::ReservoirRuntime>,
    job: &ProvisioningJob,
) -> Result<Option<serde_json::Value>> {
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
        return Ok(None);
    }

    // Per-kind result payload attached to the completion (only the
    // quota dance produces one on the inline path today).
    let mut result: Option<serde_json::Value> = None;

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
                PortActivation::Started,
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
            // Future migration sources need the bhyve brand's
            // in-zone control socket, which only exists when the
            // zone carries the `migrate_export` attr at boot.
            // Best-effort: failing the provision here would strand
            // a fully created guest over a capability it may never
            // use; the attr can be added by hand before a live
            // migration.
            if vmadm::blueprint_is_bhyve(&blueprint)
                && let Err(err) = vmadm::set_zone_attr(*instance_id, "migrate_export", "true").await
            {
                warn!(
                    %instance_id,
                    error = %format!("{err:#}"),
                    "provision: setting migrate_export zonecfg attr failed; \
                     this guest cannot be a live-migration source until it is set",
                );
            }
        }
        JobKind::MigrationProvisionTarget {
            migration_id,
            instance_id,
        } => {
            provision_migration_target(
                client,
                cfg,
                bindings,
                &blueprint,
                *migration_id,
                *instance_id,
            )
            .await?;
        }
        JobKind::MigrateQuotaDance {
            migration_id,
            instance_id,
            dataset,
            op,
        } => {
            result = run_quota_dance(*migration_id, *instance_id, dataset, op).await?;
        }
        JobKind::MigratePauseSource {
            migration_id,
            instance_id,
        } => {
            result = Some(migrate_vmm::pause_source(*migration_id, *instance_id).await?);
        }
        JobKind::MigrateResumeSource {
            migration_id,
            instance_id,
        } => {
            migrate_vmm::resume_source(*migration_id, *instance_id).await?;
        }
        JobKind::MigrateTargetListen {
            migration_id,
            instance_id,
        } => {
            // Routed to the data-plane lane by `poll_once`; reaching
            // the inline path means the diversion predicate and this
            // match disagree. Fail loudly rather than blocking the
            // poll loop on a zone boot.
            error!(
                %migration_id, %instance_id,
                "migrate-target-listen: reached the inline dispatcher",
            );
            anyhow::bail!("MigrateTargetListen must run on the migration data-plane lane");
        }
        JobKind::Start { instance_id } => {
            // Power on an existing stopped zone. The zone and its
            // Proteus ports already exist and persist across a power
            // cycle (same reason Restart re-realizes nothing), so we
            // only boot it — no port realization, no vmadm create.
            vmadm::start_zone(*instance_id).await?;
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
            // Destroy whatever `@migration-*` snapshots remain.
            // Listed rather than hardcoded: the saga's sync loop
            // creates an unbounded `@migration-sync-N` family on
            // top of base/final, and a half-completed migration
            // may have any subset. A vanished dataset (vmadm
            // delete took the tree) lists as empty.
            let dataset = format!("zones/{instance_id}");
            match zfs::list_migration_snapshots(&dataset).await {
                Ok(snapshots) => {
                    for snap in snapshots {
                        if let Err(e) = zfs::destroy_snapshot(&snap).await {
                            warn!(
                                %migration_id, %instance_id, side, %snap, error = %e,
                                "migration cleanup: destroy_snapshot failed (best-effort, continuing)",
                            );
                        }
                    }
                }
                Err(e) => {
                    warn!(
                        %migration_id, %instance_id, side, %dataset, error = %e,
                        "migration cleanup: listing migration snapshots failed (best-effort, continuing)",
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
            ..
        } => match role {
            tritond_client::types::MigrationJobRole::Source => {
                // Diverted to the data-plane lane by `poll_once`
                // before this dispatcher runs (the stream takes
                // hours and must not hold the poll loop). Reaching
                // here means the diversion predicate and this
                // match disagree; fail loudly rather than running
                // the stream inline.
                anyhow::bail!(
                    "source-role MigrateZfsSend must run on the migration data-plane lane \
                     (migration {migration_id}, instance {instance_id})"
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
            ..
        } => match role {
            tritond_client::types::MigrationJobRole::Source => {
                // Same diversion contract as source-role
                // MigrateZfsSend.
                anyhow::bail!(
                    "source-role MigrateVmmStream must run on the migration data-plane lane \
                     (migration {migration_id}, instance {instance_id})"
                );
            }
            tritond_client::types::MigrationJobRole::Target => {
                // Mirrors the zfs target arm: the listener's
                // `/migrate/{id}` route owns the inbound stream.
                info!(
                    %migration_id, %instance_id,
                    "migrate-vmm-stream/target: dispatcher is a no-op; listener handles transfer",
                );
            }
        },
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
        // FIP dataplane realization (C-4b). The saga (C-4a) enqueues a
        // pinned FipClaim/FipRelease and awaits terminal; these arms do
        // the host-side work, ordered so the inbound classifier is the
        // last thing turned on (claim) and the first thing turned off
        // (release).
        JobKind::FipClaim {
            floating_ip_id,
            nic_id,
            fip_addr,
            external_nic_tag,
            vlan_id,
            ..
        } => {
            let proteus = open_proteus_lifecycle(&cfg.proteus_dev)?;
            realize_fip_claim(
                client,
                proteus.as_ref(),
                &HostFipNet,
                &HostFipLink,
                *floating_ip_id,
                *nic_id,
                fip_addr,
                external_nic_tag.as_deref(),
                *vlan_id,
            )
            .await?;
        }
        JobKind::FipRelease {
            floating_ip_id,
            fip_addr,
            external_nic_tag,
            vlan_id,
            ..
        } => {
            let proteus = open_proteus_lifecycle(&cfg.proteus_dev)?;
            realize_fip_release(
                proteus.as_ref(),
                &HostFipNet,
                &HostFipLink,
                *floating_ip_id,
                fip_addr,
                external_nic_tag.as_deref(),
                *vlan_id,
            )?;
        }
        JobKind::ResizeDisk {
            instance_id,
            size_bytes,
            ..
        } => {
            // Grow the boot zvol + flexible pool to the new size. The
            // durable Disk record is already grown control-plane side;
            // the guest realizes the capacity on its next reboot.
            vmadm::grow_boot_disk(*instance_id, *size_bytes).await?;
        }
    }

    Ok(result)
}

/// `JobKind::MigrationProvisionTarget`: create the target-side
/// zone shell for a migration. Reuses the Provision arm's
/// blueprint resolution and port realization, with the migration
/// deltas: ports come up paused (the source still owns the
/// identity on the wire), the zone is created with
/// `autoboot=false`, and the vmadm-created dataset tree is
/// destroyed so the first `zfs recv` lands clean.
async fn provision_migration_target(
    client: &Client,
    cfg: &AgentConfig,
    bindings: &ImdsBindingTable,
    blueprint: &ProvisioningBlueprint,
    migration_id: Uuid,
    instance_id: Uuid,
) -> Result<()> {
    if blueprint.instance.is_none() {
        anyhow::bail!(
            "instance {instance_id} no longer exists; refusing to provision a migration target \
             for a phantom"
        );
    }
    let is_bhyve = vmadm::blueprint_is_bhyve(blueprint);
    // No image ensure for bhyve: its disks are created blank (see
    // `build_migration_target_payload`) because the recv replaces
    // them, so pulling the image would be pure waste. Native
    // zones can't be created imageless (vmadm clones the zone
    // root from the image), so ensure content for them only.
    if !is_bhyve {
        let image = blueprint
            .image
            .as_ref()
            .ok_or_else(|| anyhow!("MigrationProvisionTarget blueprint has no image"))?;
        if let Some(compat) = image.compatibility.as_ref() {
            check_image_compatibility(compat).await?;
        }
        images::ensure(image)
            .await
            .context("ensure image content for native-zone migration target")?;
    }

    let proteus = open_proteus_lifecycle(&cfg.proteus_dev)?;
    let started_ports = realize_provision_ports(
        client,
        client,
        proteus.as_ref(),
        &cfg.agent_id,
        blueprint,
        PortActivation::Paused,
    )
    .await?;
    // Same binding registration as Provision: harmless while the
    // guest is elsewhere (the table is CN-local and nothing
    // resolves to it until traffic flows here), and required for
    // IMDS to work once the cutover lands the guest on this CN.
    let registered = register_blueprint_bindings(bindings, blueprint);
    if registered > 0 {
        info!(
            %migration_id, %instance_id,
            bindings = registered,
            "imds: registered binding(s) for migration target",
        );
    }
    let nic_tags = started_ports
        .iter()
        .map(|port| (port.nic_id, port.link_name.clone()))
        .collect::<BTreeMap<_, _>>();
    if let Err(err) = vmadm::create_migration_target_zone(blueprint, &nic_tags).await {
        cleanup_started_ports(proteus.as_ref(), &started_ports);
        return Err(err).context("create migration target zone after Proteus port realization");
    }

    // The first recv must land on a clean slate; `vmadm create`
    // made a dataset tree (zone root + disk zvols) that would
    // collide with the incoming replication stream.
    zfs::destroy_forced(&format!("zones/{instance_id}"))
        .await
        .context("destroy vmadm-created dataset tree for migration target")?;

    // Listen-mode is a live-path concern set by `MigrateTargetListen`
    // right before it boots the zone; a cold target boots normally via
    // `activate_target`, so the unbooted shell carries no listen attr.
    info!(
        %migration_id, %instance_id, is_bhyve,
        "migration-provision-target: zone shell created (unbooted, datasets cleared)",
    );
    Ok(())
}

/// `JobKind::MigrateQuotaDance`: the legacy-compat quota dance.
/// Returns the job `result` payload for `SaveAndClear` (the
/// original values, `QuotaDanceSaveResult` shape), `None` for
/// `Restore`.
async fn run_quota_dance(
    migration_id: Uuid,
    instance_id: Uuid,
    dataset: &str,
    op: &tritond_client::types::QuotaDanceOp,
) -> Result<Option<serde_json::Value>> {
    match op {
        tritond_client::types::QuotaDanceOp::SaveAndClear => {
            let live = zfs::save_quotas(dataset).await?;
            let saved = if live.quota_bytes.is_some() || live.refreservation_bytes.is_some() {
                // First run (or the operator restored values since):
                // stash the originals on the dataset BEFORE clearing
                // so a crash between the two steps can't lose them.
                zfs::stash_saved_quotas(dataset, live).await?;
                zfs::clear_quotas(dataset).await?;
                live
            } else {
                // Both already clear: either a re-claimed job whose
                // clear already ran (the stash holds the originals)
                // or nothing was ever set (no stash, report None).
                zfs::read_stashed_quotas(dataset).await?.unwrap_or(live)
            };
            info!(
                %migration_id, %instance_id, %dataset,
                quota_bytes = ?saved.quota_bytes,
                refreservation_bytes = ?saved.refreservation_bytes,
                "migrate-quota-dance: saved and cleared",
            );
            Ok(Some(
                serde_json::to_value(saved).context("encode QuotaDanceSaveResult payload")?,
            ))
        }
        tritond_client::types::QuotaDanceOp::Restore {
            quota_bytes,
            refreservation_bytes,
        } => {
            zfs::restore_quotas(
                dataset,
                zfs::SavedQuotas {
                    quota_bytes: *quota_bytes,
                    refreservation_bytes: *refreservation_bytes,
                },
            )
            .await?;
            // Drop the stash so a later migration's SaveAndClear
            // can't read this one's values. No-op on the target,
            // where no stash was ever written.
            zfs::clear_stashed_quotas(dataset).await?;
            info!(
                %migration_id, %instance_id, %dataset,
                quota_bytes = ?quota_bytes,
                refreservation_bytes = ?refreservation_bytes,
                "migrate-quota-dance: restored",
            );
            Ok(None)
        }
    }
}

/// Parse a FIP address string carried on a job into an `IpAddr`,
/// turning a malformed value into an `anyhow` error so the job fails
/// loudly rather than mis-plumbing the host.
fn parse_fip_addr(fip_addr: &str, floating_ip_id: Uuid) -> Result<std::net::IpAddr> {
    fip_addr
        .parse()
        .with_context(|| format!("parse FIP address {fip_addr:?} for floating ip {floating_ip_id}"))
}

/// Host-OS networking effects for a FIP claim/release: the ipadm
/// `<fip>/32` alias and the gratuitous-ARP burst. Abstracted behind a
/// trait so the claim/release ordering is unit-testable without
/// shelling out to `ipadm` / `arp` (which are illumos-only and need a
/// real external link). The production impl ([`HostFipNet`]) delegates
/// to [`fip_net`] + [`imds_arp`].
trait FipHostNet {
    /// Add the `<fip>/32` alias on `link`. Fail-stop (the saga retries).
    fn create_alias(&self, link: &str, fip: std::net::IpAddr) -> Result<()>;
    /// Gratuitous-ARP burst (best-effort).
    fn announce(&self, fip: std::net::IpAddr);
    /// Drop the static ARP entry (best-effort).
    fn drop_arp(&self, fip: std::net::IpAddr);
    /// Remove the `<fip>/32` alias from `link` (best-effort, idempotent).
    fn delete_alias(&self, link: &str, fip: std::net::IpAddr);
}

/// Production [`FipHostNet`]: the real ipadm / arp shell-outs
/// (illumos-gated, no-ops elsewhere).
struct HostFipNet;

impl FipHostNet for HostFipNet {
    fn create_alias(&self, link: &str, fip: std::net::IpAddr) -> Result<()> {
        fip_net::create_addr(link, fip)
    }
    fn announce(&self, fip: std::net::IpAddr) {
        imds_arp::send_garp(fip);
    }
    fn drop_arp(&self, fip: std::net::IpAddr) {
        imds_arp::del(fip);
    }
    fn delete_alias(&self, link: &str, fip: std::net::IpAddr) {
        fip_net::delete_addr(link, fip);
    }
}

/// Resolve a CN-terminated FIP's external nic_tag + VLAN to the
/// datalink it egresses on, realizing the per-`(link, vlan)` `fipN`
/// vnic over the nic_tag's physical link (legacy SDC model: the nic_tag
/// is the physical-link identity, the VLAN lives on the network).
/// Abstracted behind a trait so the claim/release flow is unit-testable
/// without `nictagadm`/`dladm`. The production impl ([`HostFipLink`])
/// delegates to [`fip_link`].
trait FipExternalLink {
    /// Resolve + create-or-reuse the external datalink for this FIP.
    fn realize_link(&self, nic_tag: &str, vlan_id: Option<u16>) -> Result<String>;
    /// Find the existing external datalink (no create) for release.
    /// `Ok(None)` = link genuinely gone; `Err` = query failed (the
    /// caller fail-stops so a hiccup never strands the alias).
    fn find_link(&self, nic_tag: &str, vlan_id: Option<u16>) -> Result<Option<String>>;
}

/// Production [`FipExternalLink`]: real `nictagadm` / `dladm` resolution
/// + vnic creation (illumos-gated; errors elsewhere).
struct HostFipLink;

impl FipExternalLink for HostFipLink {
    fn realize_link(&self, nic_tag: &str, vlan_id: Option<u16>) -> Result<String> {
        fip_link::realize(nic_tag, vlan_id)
    }
    fn find_link(&self, nic_tag: &str, vlan_id: Option<u16>) -> Result<Option<String>> {
        fip_link::find(nic_tag, vlan_id)
    }
}

/// Realize a `FipClaim`: land the recomputed blueprint (SetSrc/SetDst
/// rules + the `hosted_fips` delta, owned by the kmod ApplyBlueprint
/// handler), ensure the external link, add the `<fip>/32` ipadm alias,
/// then fire the gratuitous-ARP burst. Ordered so the inbound
/// classifier (installed by the blueprint delta) is live before the
/// alias starts answering ARP, and the GARP that re-points upstream is
/// dead last. Each step before the GARP is fail-stop so the saga can
/// retry; the GARP is best-effort.
async fn realize_fip_claim<S, P, H, L>(
    source: &S,
    proteus: &P,
    host_net: &H,
    ext_link: &L,
    floating_ip_id: Uuid,
    nic_id: Uuid,
    fip_addr: &str,
    external_nic_tag: Option<&str>,
    vlan_id: Option<u16>,
) -> Result<()>
where
    S: PortBlueprintSource + Sync,
    P: ProteusLifecycle + ?Sized,
    H: FipHostNet + ?Sized,
    L: FipExternalLink + ?Sized,
{
    let fip = parse_fip_addr(fip_addr, floating_ip_id)?;
    let Some(nic_tag) = external_nic_tag else {
        // A claim with no external link is a control-plane bug: the
        // attach saga only enqueues FipClaim for a CN-terminated FIP,
        // which always carries its resolved external nic_tag name.
        anyhow::bail!(
            "FipClaim for floating ip {floating_ip_id} carries no external_nic_tag; \
             refusing to claim a FIP with no external link"
        );
    };

    // Resolve the nic_tag + VLAN to the realized external datalink
    // (`fipN`), creating the per-(link,vlan) vnic over the nic_tag's
    // physical link if needed. The nic_tag is the physical-link identity
    // and the VLAN (from the external subnet) is stamped on the vnic —
    // exactly like legacy SDC `global-nic` + `vlan-id`.
    let realized_link = ext_link.realize_link(nic_tag, vlan_id).with_context(|| {
        format!("realize external FIP link for nic_tag {nic_tag} vlan {vlan_id:?}")
    })?;
    let link_name = realized_link.as_str();

    // 1. Ensure the external siphon link exists FIRST (idempotent).
    //    The kmod's ApplyBlueprint hosted_fips delta is a NO-OP while
    //    `external_link` is None, so the inbound classifier only
    //    populates if the link is registered before the apply below.
    proteus
        .ensure_external_link(link_name)
        .with_context(|| format!("ensure external link {link_name} for FIP {fip}"))?;

    // 2. Apply the recomputed port blueprint at its bumped generation.
    //    tritond owns the bytes; the agent fetches + applies. This
    //    installs the FIP's NAT SetSrc/SetDst and (via the kmod
    //    ApplyBlueprint delta, P-5) the inbound hosted_fips entry — now
    //    landing because the external link exists.
    let port_blueprint = source
        .fetch_port_blueprint(nic_id)
        .await
        .with_context(|| format!("fetch port blueprint to claim FIP on nic {nic_id}"))?;
    proteus
        .apply_blueprint(&port_blueprint)
        .with_context(|| format!("apply Proteus blueprint to claim FIP on nic {nic_id}"))?;

    // 3. Add the host /32 alias so the stack answers solicited ARP.
    host_net.create_alias(link_name, fip)?;

    // 4. Gratuitous-ARP burst so upstream re-points to this CN
    //    (best-effort; the alias already answers solicited ARP).
    host_net.announce(fip);

    info!(%floating_ip_id, %nic_id, %fip, link_name, "fip-claim: realized");
    Ok(())
}

/// Realize a `FipRelease`: reverse of the claim, and ordered so the
/// inbound classifier stops delivering to the guest FIRST. Invalidate
/// the `hosted_fips` entry, drop the static ARP entry, remove the
/// ipadm alias. The withdraw of the SetSrc/SetDst NAT rules is driven
/// by tritond pushing a recomputed (FIP-less) blueprint on the
/// surviving port via the C-0 ApplyPortBlueprint path; if the port is
/// already gone (instance deleted), the kmod drops the port state with
/// it, so there is nothing more to apply here. All steps are
/// idempotent best-effort except the classifier invalidate, which is
/// fail-stop so a release that cannot reach the kmod is retried rather
/// than silently leaving a stale inbound entry.
fn realize_fip_release<P, H, L>(
    proteus: &P,
    host_net: &H,
    ext_link: &L,
    floating_ip_id: Uuid,
    fip_addr: &str,
    external_nic_tag: Option<&str>,
    vlan_id: Option<u16>,
) -> Result<()>
where
    P: ProteusLifecycle + ?Sized,
    H: FipHostNet + ?Sized,
    L: FipExternalLink + ?Sized,
{
    let fip = parse_fip_addr(fip_addr, floating_ip_id)?;

    // 1. Stop inbound delivery first (fail-stop). Idempotent at the
    //    kmod: a release for a FIP that was never installed no-ops.
    proteus
        .invalidate_fip_entry(fip)
        .with_context(|| format!("invalidate hosted FIP {fip} on release"))?;

    // 2. Drop the static ARP entry (best-effort).
    host_net.drop_arp(fip);

    // 3. Remove the host /32 alias. Find the realized `fipN` link for
    //    this nic_tag + VLAN (no create) so the alias is removed from the
    //    same link the claim added it to. A genuinely-gone link is
    //    nothing to remove (idempotent); a FAILED nictagadm/dladm query
    //    is fail-stop so a transient hiccup re-runs rather than stranding
    //    the `<fip>/32` alias as a stale ARP responder on the live link.
    if let Some(nic_tag) = external_nic_tag {
        match ext_link.find_link(nic_tag, vlan_id) {
            Ok(Some(link_name)) => host_net.delete_alias(&link_name, fip),
            Ok(None) => {}
            Err(e) => {
                return Err(e).with_context(|| {
                    format!("resolve external link to remove FIP {fip} alias on release")
                });
            }
        }
    }

    info!(%floating_ip_id, %fip, "fip-release: realized");
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

    /// Create + apply a port without starting packet processing.
    /// Migration targets use this: the datalink must exist for
    /// `vmadm create`, but the port must not forward while the
    /// source instance still owns the identity on the wire.
    fn ensure_paused(
        &self,
        blueprint: &PortBlueprint,
        link_name: &str,
    ) -> Result<proteus::ProteusPortStatus>;

    /// Re-apply a port's blueprint in place: no create, no start.
    /// Pushes a recomputed blueprint to an already-running port (e.g. a
    /// FIP attach on a running VM). The kmod no-ops a re-apply at the
    /// same generation, so the caller must bump the generation first.
    fn apply_blueprint(&self, blueprint: &PortBlueprint) -> Result<()>;

    /// Start packet processing on an existing (paused) port. The
    /// live-migration cutover fence uses this on the target: the
    /// port was created paused by `MigrationProvisionTarget` and
    /// must begin forwarding the instant the imported guest can run.
    fn start_port(&self, port_id: PortId) -> Result<()>;

    fn cleanup_port(&self, port_id: PortId) -> Result<()>;

    /// Idempotently register the per-CN external datalink the inbound
    /// FIP siphon attaches to (C-4b `FipClaim`). The implementation
    /// resolves `link_name` to a dladm `linkid` before issuing the
    /// ioctl.
    fn ensure_external_link(&self, link_name: &str) -> Result<()>;

    /// Invalidate one hosted-FIP entry by address (C-4b `FipRelease`,
    /// step 1). Idempotent at the kmod.
    fn invalidate_fip_entry(&self, fip_addr: std::net::IpAddr) -> Result<()>;
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

    fn ensure_paused(
        &self,
        blueprint: &PortBlueprint,
        _link_name: &str,
    ) -> Result<proteus::ProteusPortStatus> {
        proteus::ProteusClient::ensure_paused(self, blueprint, None)
    }

    fn apply_blueprint(&self, blueprint: &PortBlueprint) -> Result<()> {
        proteus::ProteusClient::apply_blueprint(self, blueprint)
    }

    fn start_port(&self, port_id: PortId) -> Result<()> {
        proteus::ProteusClient::start_port(self, port_id)
    }

    fn cleanup_port(&self, port_id: PortId) -> Result<()> {
        proteus::ProteusClient::cleanup_port(self, port_id)
    }

    fn ensure_external_link(&self, link_name: &str) -> Result<()> {
        let linkid = resolve_external_linkid(link_name)?;
        proteus::ProteusClient::ensure_external_link_with_id(self, linkid, link_name)
    }

    fn invalidate_fip_entry(&self, fip_addr: std::net::IpAddr) -> Result<()> {
        proteus::ProteusClient::invalidate_hosted_fip(self, fip_addr)
    }
}

/// Resolve an existing external NIC's dladm `linkid` by name. The
/// external link physically pre-exists on the CN (an operator
/// nic_tag), so we look it up — never create it. illumos-only; on any
/// other platform the kernel transport is unavailable anyway, so this
/// errors (matching [`open_proteus_lifecycle`]).
fn resolve_external_linkid(link_name: &str) -> Result<u32> {
    #[cfg(target_os = "illumos")]
    {
        use proteus_ioctl::dladm::DladmHandle;
        let dladm = DladmHandle::open().with_context(
            || "open libdladm to resolve external FIP link; tritonagent must run as root",
        )?;
        dladm
            .name2info(link_name)
            .with_context(|| format!("resolve dladm linkid for external FIP link {link_name}"))
    }
    #[cfg(not(target_os = "illumos"))]
    {
        let _ = link_name;
        bail!("external FIP link resolution is only available on illumos")
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

    /// Shared body of `ensure_started` / `ensure_paused`: allocate
    /// the dladm link, create + apply the port, then start it only
    /// when asked.
    fn ensure_port(
        &self,
        blueprint: &PortBlueprint,
        link_name: &str,
        start: bool,
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
        if start {
            self.inner.start_port(blueprint.port_id)?;
        }
        self.inner.dump_status(blueprint.port_id)
    }
}

#[cfg(target_os = "illumos")]
impl ProteusLifecycle for KernelProteusLifecycle {
    fn ensure_started(
        &self,
        blueprint: &PortBlueprint,
        link_name: &str,
    ) -> Result<proteus::ProteusPortStatus> {
        self.ensure_port(blueprint, link_name, true)
    }

    fn ensure_paused(
        &self,
        blueprint: &PortBlueprint,
        link_name: &str,
    ) -> Result<proteus::ProteusPortStatus> {
        self.ensure_port(blueprint, link_name, false)
    }

    fn apply_blueprint(&self, blueprint: &PortBlueprint) -> Result<()> {
        self.inner.apply_blueprint(blueprint)
    }

    fn start_port(&self, port_id: PortId) -> Result<()> {
        self.inner.start_port(port_id)
    }

    fn cleanup_port(&self, port_id: PortId) -> Result<()> {
        self.inner.cleanup_port(port_id)
    }

    fn ensure_external_link(&self, link_name: &str) -> Result<()> {
        let linkid = resolve_external_linkid(link_name)?;
        self.inner.ensure_external_link_with_id(linkid, link_name)
    }

    fn invalidate_fip_entry(&self, fip_addr: std::net::IpAddr) -> Result<()> {
        self.inner.invalidate_hosted_fip(fip_addr)
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

/// How [`realize_provision_ports`] leaves each Proteus port:
/// forwarding (normal provision) or created-but-paused (migration
/// target, where forwarding before the cutover would put a
/// duplicate identity on the wire).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PortActivation {
    Started,
    Paused,
}

async fn realize_provision_ports<S, R, P>(
    source: &S,
    sink: &R,
    proteus: &P,
    agent_id: &str,
    blueprint: &ProvisioningBlueprint,
    activation: PortActivation,
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

        let ensured = match activation {
            PortActivation::Started => proteus.ensure_started(&port_blueprint, &link_name),
            PortActivation::Paused => proteus.ensure_paused(&port_blueprint, &link_name),
        };
        match ensured {
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

        let started = realize_provision_ports(
            &source,
            &sink,
            &proteus,
            &cn_id.to_string(),
            &provision,
            PortActivation::Started,
        )
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
    async fn realize_provision_ports_paused_applies_without_starting() {
        let cn_id = Uuid::new_v4();
        let nic_id = Uuid::new_v4();
        let vpc_id = Uuid::new_v4();
        let nic = sample_nic(nic_id, vpc_id);
        let provision = sample_provisioning_blueprint(nic);
        let port = sample_port_blueprint(nic_id, 2);
        let source = StaticPortBlueprintSource {
            by_port: HashMap::from([(nic_id, port.clone())]),
        };
        let sink = RecordingRealizationSink::default();
        let proteus = proteus::ProteusClient::new(FakeTransport::new());

        let started = realize_provision_ports(
            &source,
            &sink,
            &proteus,
            &cn_id.to_string(),
            &provision,
            PortActivation::Paused,
        )
        .await
        .unwrap();

        assert_eq!(started.len(), 1);
        let status = proteus.dump_status(port.port_id).unwrap();
        // Applied but NOT forwarding: the migration target's port
        // must stay off the wire until the cutover starts it.
        assert_ne!(status.summary.state, PortState::Running);
        assert_eq!(status.summary.apply_status, BlueprintApplyStatus::Applied);
        assert_eq!(status.generation.applied_generation, Generation::new(2));
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

    // -----------------------------------------------------------------
    // C-4b: FIP claim / release handler ordering + error-stop.
    //
    // The kmod ioctl handlers for EnsureExternalLink / InvalidateFipEntry
    // are P-5 (not built), and the host shell-outs (ipadm / GARP) only
    // run on illumos, so the handler is exercised against a recording
    // mock that captures the call sequence — no live kmod, no SmartOS.
    // -----------------------------------------------------------------

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum ProteusOp {
        Apply { port: Uuid, generation: u64 },
        EnsureExternalLink(String),
        InvalidateFip(std::net::IpAddr),
    }

    /// Records the host-OS networking effects (ipadm alias + GARP)
    /// without shelling out, so the claim/release ordering is testable
    /// off-illumos and without a real external link.
    #[derive(Default)]
    struct RecordingFipHostNet {
        created: Mutex<Vec<(String, std::net::IpAddr)>>,
        announced: Mutex<Vec<std::net::IpAddr>>,
        dropped_arp: Mutex<Vec<std::net::IpAddr>>,
        deleted: Mutex<Vec<(String, std::net::IpAddr)>>,
    }

    impl FipHostNet for RecordingFipHostNet {
        fn create_alias(&self, link: &str, fip: std::net::IpAddr) -> Result<()> {
            self.created.lock().unwrap().push((link.to_string(), fip));
            Ok(())
        }
        fn announce(&self, fip: std::net::IpAddr) {
            self.announced.lock().unwrap().push(fip);
        }
        fn drop_arp(&self, fip: std::net::IpAddr) {
            self.dropped_arp.lock().unwrap().push(fip);
        }
        fn delete_alias(&self, link: &str, fip: std::net::IpAddr) {
            self.deleted.lock().unwrap().push((link.to_string(), fip));
        }
    }

    #[derive(Default)]
    struct RecordingProteusLifecycle {
        ops: Mutex<Vec<ProteusOp>>,
        /// When set, the named op fails so the handler's fail-stop
        /// ordering can be asserted.
        fail_on_apply: bool,
        fail_on_invalidate: bool,
    }

    impl RecordingProteusLifecycle {
        fn ops(&self) -> Vec<ProteusOp> {
            self.ops.lock().unwrap().clone()
        }
    }

    impl ProteusLifecycle for RecordingProteusLifecycle {
        fn ensure_started(
            &self,
            _blueprint: &PortBlueprint,
            _link_name: &str,
        ) -> Result<proteus::ProteusPortStatus> {
            unreachable!("FIP handler never creates a port")
        }

        fn ensure_paused(
            &self,
            _blueprint: &PortBlueprint,
            _link_name: &str,
        ) -> Result<proteus::ProteusPortStatus> {
            unreachable!("FIP handler never creates a port")
        }

        fn apply_blueprint(&self, blueprint: &PortBlueprint) -> Result<()> {
            self.ops.lock().unwrap().push(ProteusOp::Apply {
                port: blueprint.port_id.0,
                generation: blueprint.generation.0,
            });
            if self.fail_on_apply {
                anyhow::bail!("simulated apply failure");
            }
            Ok(())
        }

        fn start_port(&self, _port_id: PortId) -> Result<()> {
            unreachable!("FIP handler never starts a port")
        }

        fn cleanup_port(&self, _port_id: PortId) -> Result<()> {
            unreachable!("FIP handler never deletes a port")
        }

        fn ensure_external_link(&self, link_name: &str) -> Result<()> {
            self.ops
                .lock()
                .unwrap()
                .push(ProteusOp::EnsureExternalLink(link_name.to_string()));
            Ok(())
        }

        fn invalidate_fip_entry(&self, fip_addr: std::net::IpAddr) -> Result<()> {
            self.ops
                .lock()
                .unwrap()
                .push(ProteusOp::InvalidateFip(fip_addr));
            if self.fail_on_invalidate {
                anyhow::bail!("simulated invalidate failure");
            }
            Ok(())
        }
    }

    /// Fake [`FipExternalLink`] that records the `(nic_tag, vlan)` it is
    /// asked to resolve and returns a canned realized link (default
    /// `fip0`), so the claim/release flow can be asserted to use the
    /// RESOLVED link, not the raw nic_tag.
    #[derive(Default)]
    struct RecordingFipExternalLink {
        realized: Mutex<Vec<(String, Option<u16>)>>,
        found: Mutex<Vec<(String, Option<u16>)>>,
        link: Option<String>,
        /// When set, `find_link` returns `Err` (simulates a transient
        /// nictagadm/dladm failure) so the release fail-stop is testable.
        find_errors: bool,
    }

    impl RecordingFipExternalLink {
        fn returns(link: &str) -> Self {
            Self {
                link: Some(link.to_string()),
                ..Default::default()
            }
        }
        fn find_fails() -> Self {
            Self {
                find_errors: true,
                ..Default::default()
            }
        }
    }

    impl FipExternalLink for RecordingFipExternalLink {
        fn realize_link(&self, nic_tag: &str, vlan_id: Option<u16>) -> Result<String> {
            self.realized
                .lock()
                .unwrap()
                .push((nic_tag.to_string(), vlan_id));
            self.link
                .clone()
                .ok_or_else(|| anyhow::anyhow!("no external link configured"))
        }
        fn find_link(&self, nic_tag: &str, vlan_id: Option<u16>) -> Result<Option<String>> {
            self.found
                .lock()
                .unwrap()
                .push((nic_tag.to_string(), vlan_id));
            if self.find_errors {
                anyhow::bail!("simulated nictagadm/dladm query failure");
            }
            Ok(self.link.clone())
        }
    }

    #[tokio::test]
    async fn fip_claim_applies_blueprint_then_ensures_external_link() {
        let nic_id = Uuid::new_v4();
        let fip_id = Uuid::new_v4();
        let port = sample_port_blueprint(nic_id, 7);
        let source = StaticPortBlueprintSource {
            by_port: HashMap::from([(nic_id, port)]),
        };
        let proteus = RecordingProteusLifecycle::default();
        let host_net = RecordingFipHostNet::default();
        // nic_tag `external` + vlan 2003 resolves to the realized `fip0`
        // datalink; the claim must egress on the RESOLVED link.
        let ext_link = RecordingFipExternalLink::returns("fip0");

        realize_fip_claim(
            &source,
            &proteus,
            &host_net,
            &ext_link,
            fip_id,
            nic_id,
            "192.0.2.10",
            Some("external"),
            Some(2003),
        )
        .await
        .expect("claim succeeds");

        // The nic_tag + VLAN were resolved to the realized link.
        assert_eq!(
            *ext_link.realized.lock().unwrap(),
            vec![("external".to_string(), Some(2003))]
        );
        // The external link must be ensured BEFORE the blueprint apply:
        // the kmod's hosted_fips delta no-ops while `external_link` is
        // None, so the inbound classifier only populates if the link is
        // registered first. The link is `fip0`, not the raw nic_tag.
        assert_eq!(
            proteus.ops(),
            vec![
                ProteusOp::EnsureExternalLink("fip0".to_string()),
                ProteusOp::Apply {
                    port: nic_id,
                    generation: 7,
                },
            ]
        );
        // Then the /32 alias is added on `fip0` and the GARP burst fires.
        let fip: std::net::IpAddr = "192.0.2.10".parse().unwrap();
        assert_eq!(
            *host_net.created.lock().unwrap(),
            vec![("fip0".to_string(), fip)]
        );
        assert_eq!(*host_net.announced.lock().unwrap(), vec![fip]);
    }

    #[tokio::test]
    async fn fip_claim_without_external_nic_tag_is_rejected() {
        let nic_id = Uuid::new_v4();
        let source = StaticPortBlueprintSource {
            by_port: HashMap::new(),
        };
        let proteus = RecordingProteusLifecycle::default();
        let host_net = RecordingFipHostNet::default();
        let ext_link = RecordingFipExternalLink::default();
        let err = realize_fip_claim(
            &source,
            &proteus,
            &host_net,
            &ext_link,
            Uuid::new_v4(),
            nic_id,
            "192.0.2.10",
            None,
            None,
        )
        .await
        .expect_err("a CN-terminated claim must carry an external link");
        assert!(err.to_string().contains("no external link"));
        // Nothing touched the dataplane — not even link resolution.
        assert!(proteus.ops().is_empty());
        assert!(host_net.created.lock().unwrap().is_empty());
        assert!(ext_link.realized.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn fip_claim_apply_failure_stops_before_alias() {
        let nic_id = Uuid::new_v4();
        let port = sample_port_blueprint(nic_id, 3);
        let source = StaticPortBlueprintSource {
            by_port: HashMap::from([(nic_id, port)]),
        };
        let proteus = RecordingProteusLifecycle {
            fail_on_apply: true,
            ..Default::default()
        };
        let host_net = RecordingFipHostNet::default();
        let ext_link = RecordingFipExternalLink::returns("fip0");
        realize_fip_claim(
            &source,
            &proteus,
            &host_net,
            &ext_link,
            Uuid::new_v4(),
            nic_id,
            "192.0.2.10",
            Some("external"),
            Some(2003),
        )
        .await
        .expect_err("apply failure fails the claim");
        // The external link is ensured first (idempotent; harmless
        // without a populated classifier), then the apply fails — so a
        // failed apply must NOT reach the host alias / GARP step. The
        // saga retries; ensure_external_link is idempotent on re-run.
        assert_eq!(
            proteus.ops(),
            vec![
                ProteusOp::EnsureExternalLink("fip0".to_string()),
                ProteusOp::Apply {
                    port: nic_id,
                    generation: 3,
                },
            ]
        );
        // And no host alias was created (the failure stopped before it).
        assert!(host_net.created.lock().unwrap().is_empty());
    }

    #[test]
    fn fip_release_invalidates_classifier_first() {
        let proteus = RecordingProteusLifecycle::default();
        let host_net = RecordingFipHostNet::default();
        let ext_link = RecordingFipExternalLink::returns("fip0");
        realize_fip_release(
            &proteus,
            &host_net,
            &ext_link,
            Uuid::new_v4(),
            "192.0.2.10",
            Some("external"),
            Some(2003),
        )
        .expect("release succeeds");
        // The classifier invalidate is step 1 (fail-stop) so inbound
        // delivery stops before the host teardown.
        let fip: std::net::IpAddr = "192.0.2.10".parse().unwrap();
        assert_eq!(proteus.ops(), vec![ProteusOp::InvalidateFip(fip)]);
        // Then the ARP entry and the alias on the resolved `fip0` link
        // are torn down.
        assert_eq!(*host_net.dropped_arp.lock().unwrap(), vec![fip]);
        assert_eq!(
            *host_net.deleted.lock().unwrap(),
            vec![("fip0".to_string(), fip)]
        );
    }

    #[test]
    fn fip_release_invalidate_failure_is_fatal() {
        let proteus = RecordingProteusLifecycle {
            fail_on_invalidate: true,
            ..Default::default()
        };
        let host_net = RecordingFipHostNet::default();
        let ext_link = RecordingFipExternalLink::returns("fip0");
        realize_fip_release(
            &proteus,
            &host_net,
            &ext_link,
            Uuid::new_v4(),
            "192.0.2.10",
            Some("external"),
            Some(2003),
        )
        .expect_err("a classifier invalidate that cannot reach the kmod must fail the release");
        // Fail-stop: the host teardown must not run after a failed
        // invalidate.
        assert!(host_net.deleted.lock().unwrap().is_empty());
    }

    #[test]
    fn fip_release_fail_stops_when_link_query_errors() {
        // A transient nictagadm/dladm failure while locating the alias
        // link must FAIL the release (so the saga retries) rather than
        // silently stranding the <fip>/32 alias as a stale ARP responder.
        let proteus = RecordingProteusLifecycle::default();
        let host_net = RecordingFipHostNet::default();
        let ext_link = RecordingFipExternalLink::find_fails();
        realize_fip_release(
            &proteus,
            &host_net,
            &ext_link,
            Uuid::new_v4(),
            "192.0.2.10",
            Some("external"),
            Some(2003),
        )
        .expect_err("a failed external-link query must fail the release, not leak the alias");
        // The classifier invalidate (step 1) ran, but no alias delete
        // happened — we could not resolve which link to clean.
        assert!(host_net.deleted.lock().unwrap().is_empty());
    }

    #[test]
    fn fip_release_rejects_malformed_address() {
        let proteus = RecordingProteusLifecycle::default();
        let host_net = RecordingFipHostNet::default();
        let ext_link = RecordingFipExternalLink::returns("fip0");
        realize_fip_release(
            &proteus,
            &host_net,
            &ext_link,
            Uuid::new_v4(),
            "not-an-ip",
            Some("external"),
            Some(2003),
        )
        .expect_err("malformed FIP address must fail loudly");
        assert!(proteus.ops().is_empty());
    }
}
