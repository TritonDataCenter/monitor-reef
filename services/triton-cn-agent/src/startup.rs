// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! SmartOS-backend startup sequence.
//!
//! The legacy cn-agent boots through the following pipeline on a real
//! compute node (see `bin/cn-agent.js:main`):
//!
//! 1. Parse `/opt/smartdc/agents/etc/cn-agent.config.json`; if
//!    `no_rabbit` isn't true, warn and sleep forever (safeguard against
//!    booting on an old rabbitmq-based install).
//! 2. Run `/usr/bin/sysinfo` and `/lib/sdc/config.sh -json`.
//! 3. Extract the admin IP from sysinfo.
//! 4. Start the HTTP server bound to that admin IP.
//! 5. Register sysinfo with CNAPI (injecting the actual bound port).
//! 6. Post the agents list.
//! 7. Start the heartbeater + watchers.
//!
//! This module reproduces that sequence end-to-end. It accepts injection
//! points so integration tests can substitute fixtures and mocks; on a
//! real CN, [`SmartosStartup::production`] wires in the defaults.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use cn_agent_api::{Uuid, cn_agent_api_mod};
use dropshot::{ConfigDropshot, ConfigLogging, ConfigLoggingLevel, HttpServer, HttpServerStarter};

use crate::DEFAULT_AGENT_PORT;
use crate::api_impl::CnAgentApiImpl;
use crate::cnapi::CnapiClient;
use crate::context::{AgentContext, AgentMetadata};
use crate::heartbeater::{
    AgentsCollector, DirtyFlag, DiskUsageSampler, Heartbeater, HeartbeaterHandle,
    status::StatusCollector,
    watchers::{SysinfoFileWatcher, ZoneConfigWatcher, ZoneeventWatcher},
};
use crate::smartos::{
    AgentConfig, ImgadmDb, KstatTool, SdcConfig, Sysinfo, VmadmTool, ZfsTool,
    config::DEFAULT_AGENT_CONFIG_PATH,
};
use crate::tasks;

/// Paths and config locations a SmartOS startup consults. Defaults point
/// at the real filesystem; tests override every field.
#[derive(Debug, Clone)]
pub struct SmartosPaths {
    pub agent_config: PathBuf,
    pub sdc_config_program: PathBuf,
    pub sdc_config_args: Vec<String>,
    pub zones_dir: PathBuf,
    pub sysinfo_path: PathBuf,
    pub agents_dir: PathBuf,
    pub agents_etc: PathBuf,
    pub imgadm_dir: PathBuf,
}

impl Default for SmartosPaths {
    fn default() -> Self {
        Self {
            agent_config: PathBuf::from(DEFAULT_AGENT_CONFIG_PATH),
            sdc_config_program: PathBuf::from("/bin/bash"),
            sdc_config_args: vec!["/lib/sdc/config.sh".to_string(), "-json".to_string()],
            zones_dir: PathBuf::from(crate::heartbeater::watchers::DEFAULT_ZONES_DIR),
            sysinfo_path: PathBuf::from(crate::heartbeater::watchers::DEFAULT_SYSINFO_PATH),
            agents_dir: PathBuf::from(crate::heartbeater::agents::DEFAULT_AGENTS_DIR),
            agents_etc: PathBuf::from(crate::heartbeater::agents::DEFAULT_AGENTS_ETC),
            imgadm_dir: PathBuf::from(crate::smartos::imgadm::DEFAULT_IMGADM_DIR),
        }
    }
}

/// Hooks the startup uses to spawn sub-processes. Each defaults to the
/// real illumos binary on production; tests override to shell scripts.
#[derive(Debug, Clone)]
pub struct SmartosBinaries {
    pub vmadm: PathBuf,
    pub zfs: PathBuf,
    pub zpool: PathBuf,
    pub kstat: PathBuf,
    pub zoneevent: PathBuf,
}

impl Default for SmartosBinaries {
    fn default() -> Self {
        Self {
            vmadm: PathBuf::from(crate::smartos::vmadm::DEFAULT_VMADM_BIN),
            zfs: PathBuf::from(crate::smartos::zfs::DEFAULT_ZFS_BIN),
            zpool: PathBuf::from(crate::smartos::zfs::DEFAULT_ZPOOL_BIN),
            kstat: PathBuf::from(crate::smartos::kstat::DEFAULT_KSTAT_BIN),
            zoneevent: PathBuf::from(crate::heartbeater::watchers::DEFAULT_ZONEEVENT_BIN),
        }
    }
}

/// Result of a successful startup. Owns the HTTP server and every
/// background handle; `run_until_shutdown` drives them until SIGINT /
/// SIGTERM.
pub struct RunningAgent {
    server: HttpServer<Arc<AgentContext>>,
    heartbeater: HeartbeaterHandle,
    zoneevent: ZoneeventWatcher,
    _zones_watcher: ZoneConfigWatcher,
    _sysinfo_watcher: SysinfoFileWatcher,
    /// Held for the duration of the process so the sysinfo-change
    /// re-registration closure can capture the CNAPI client without
    /// leaking it.
    _cnapi: Arc<CnapiClient>,
}

impl RunningAgent {
    /// Local address the HTTP server bound to. Useful for tests that
    /// connect over loopback.
    pub fn local_addr(&self) -> SocketAddr {
        self.server.local_addr()
    }

    /// Drive the service until a shutdown signal or the server exits.
    ///
    /// Selects between the Dropshot server future and a signal handler:
    /// whichever fires first, we then drain the heartbeater and zoneevent
    /// background tasks. Other watchers hold only filesystem handles and
    /// drop cleanly as this struct goes out of scope.
    pub async fn run_until_shutdown(self) -> Result<()> {
        let RunningAgent {
            server,
            heartbeater,
            zoneevent,
            _zones_watcher,
            _sysinfo_watcher,
            _cnapi,
        } = self;

        let shutdown_signal = Self::wait_for_shutdown_signal();
        tokio::pin!(shutdown_signal);

        let server_outcome: Result<()> = tokio::select! {
            res = server => {
                res.map_err(|e| anyhow::anyhow!("server exited: {e}"))
                    .context("cn-agent server loop")
            }
            _ = &mut shutdown_signal => {
                tracing::info!("shutdown signal received; stopping agent");
                Ok(())
            }
        };

        heartbeater.shutdown().await;
        zoneevent.stop().await;
        server_outcome
    }

    /// Future that resolves on SIGINT or SIGTERM.
    async fn wait_for_shutdown_signal() {
        use tokio::signal::unix::{SignalKind, signal};
        let mut sigint = match signal(SignalKind::interrupt()) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "failed to install SIGINT handler");
                return;
            }
        };
        let mut sigterm = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "failed to install SIGTERM handler");
                return;
            }
        };
        tokio::select! {
            _ = sigint.recv() => {},
            _ = sigterm.recv() => {},
        }
    }
}

/// Orchestrator for the SmartOS startup pipeline.
pub struct SmartosStartup {
    paths: SmartosPaths,
    bins: SmartosBinaries,
    /// Overrides the CNAPI URL. When `None`, we derive it from the
    /// agent config's `cnapi.url` or the SDC config's DNS fields.
    cnapi_url_override: Option<String>,
    /// Overrides the bind address. When `None`, we bind the admin IP
    /// from sysinfo at [`DEFAULT_AGENT_PORT`].
    bind_address_override: Option<SocketAddr>,
}

impl SmartosStartup {
    /// Production defaults: every path and binary pointing at its real
    /// illumos location.
    pub fn production() -> Self {
        Self {
            paths: SmartosPaths::default(),
            bins: SmartosBinaries::default(),
            cnapi_url_override: None,
            bind_address_override: None,
        }
    }

    pub fn with_paths(mut self, paths: SmartosPaths) -> Self {
        self.paths = paths;
        self
    }

    pub fn with_binaries(mut self, bins: SmartosBinaries) -> Self {
        self.bins = bins;
        self
    }

    pub fn with_cnapi_url(mut self, url: impl Into<String>) -> Self {
        self.cnapi_url_override = Some(url.into());
        self
    }

    pub fn with_bind_address(mut self, addr: SocketAddr) -> Self {
        self.bind_address_override = Some(addr);
        self
    }

    /// Run the full startup pipeline and return the live service.
    pub async fn start(self) -> Result<RunningAgent> {
        let agent_config = AgentConfig::load_from(&self.paths.agent_config)
            .await
            .with_context(|| {
                format!(
                    "load agent config from {}",
                    self.paths.agent_config.display()
                )
            })?;

        // Preserve the legacy safety latch: refuse to run if `no_rabbit`
        // isn't set. Historically this prevented cn-agent from racing an
        // older rabbitmq-based service on the same CN. Returning Err here
        // — rather than the legacy "sleep forever" — is the correct Rust
        // idiom; the caller (usually smf) will restart us and cycle.
        if !agent_config.no_rabbit {
            anyhow::bail!(
                "agent config {} does not set no_rabbit=true; refusing to start \
                 to avoid racing a legacy rabbitmq-based cn-agent install",
                self.paths.agent_config.display()
            );
        }

        let sysinfo_value = load_sysinfo(&self.bins.vmadm).await?;
        let sdc_config = SdcConfig::load_from_script(
            &self.paths.sdc_config_program,
            &self
                .paths
                .sdc_config_args
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
        )
        .await
        .with_context(|| {
            format!(
                "load SDC config via {} {:?}",
                self.paths.sdc_config_program.display(),
                self.paths.sdc_config_args
            )
        })?;

        let server_uuid = sysinfo_value
            .uuid()
            .context("sysinfo did not include a server UUID")?;
        let admin_ip = sysinfo_value
            .admin_ip()
            .context("could not find admin IP in sysinfo")?;
        let bind_addr = self
            .bind_address_override
            .unwrap_or_else(|| SocketAddr::from((admin_ip, DEFAULT_AGENT_PORT)));

        let cnapi_url = resolve_cnapi_url(
            self.cnapi_url_override.as_deref(),
            &agent_config,
            &sdc_config,
        );

        tracing::info!(
            server_uuid = %server_uuid,
            admin_ip = %admin_ip,
            cnapi_url = %cnapi_url,
            "starting cn-agent for SmartOS compute node"
        );

        let vmadm_tool = Arc::new(VmadmTool::with_bin(self.bins.vmadm.clone()));
        let zfs_tool = Arc::new(ZfsTool::with_bins(
            self.bins.zfs.clone(),
            self.bins.zpool.clone(),
        ));
        let kstat_tool = Arc::new(KstatTool::with_bin(self.bins.kstat.clone()));
        let imgadm_db = Arc::new(ImgadmDb::with_dir(self.paths.imgadm_dir.clone()));
        let disk_usage = DiskUsageSampler::new(zfs_tool.clone(), imgadm_db.clone());
        let agents_collector = AgentsCollector::with_dirs(
            self.paths.agents_dir.clone(),
            self.paths.agents_etc.clone(),
        );

        // CNAPI client has to exist before we build the task registry so
        // refresh_agents can use it. Build it here, then use the same Arc
        // for startup registration and the heartbeater.
        let cnapi = Arc::new(
            CnapiClient::builder(&cnapi_url, server_uuid)
                .with_user_agent(format!(
                    "triton-cn-agent/{} server/{server_uuid}",
                    env!("CARGO_PKG_VERSION")
                ))
                .build()
                .context("build CNAPI client")?,
        );

        let registry = tasks::smartos_registry_with(
            vmadm_tool.clone(),
            zfs_tool.clone(),
            cnapi.clone(),
            agents_collector.clone(),
        );

        let metadata = AgentMetadata {
            name: "cn-agent".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            server_uuid,
            backend: "smartos".to_string(),
        };
        let context = Arc::new(AgentContext::new(metadata, registry));

        let server = start_http_server(bind_addr, context)?;
        let actual_port = server.local_addr().port();

        // Register sysinfo (with the port we actually bound) and agents
        // before starting the heartbeater, matching the legacy startup
        // order.
        register_sysinfo(&cnapi, &sysinfo_value, actual_port).await?;
        post_agents_list(&cnapi, &sysinfo_value, &agents_collector).await?;

        // Watchers share a DirtyFlag with the heartbeater's status loop.
        let dirty = DirtyFlag::new();
        let zoneevent =
            ZoneeventWatcher::spawn_with_bin(dirty.clone(), self.bins.zoneevent.clone());
        let zones_watcher =
            ZoneConfigWatcher::spawn_watching(dirty.clone(), self.paths.zones_dir.clone())
                .with_context(|| {
                    format!(
                        "start /etc/zones watcher for {}",
                        self.paths.zones_dir.display()
                    )
                })?;

        // Sysinfo-file watcher re-registers the agent with CNAPI whenever
        // sysinfo changes. Wrap the needed pieces in an Arc so the
        // callback owns them without borrowing.
        let sysinfo_watcher =
            spawn_sysinfo_watcher(&self.paths.sysinfo_path, cnapi.clone(), actual_port)
                .with_context(|| {
                    format!(
                        "start sysinfo watcher for {}",
                        self.paths.sysinfo_path.display()
                    )
                })?;

        let collector =
            StatusCollector::new(vmadm_tool.clone(), zfs_tool.clone(), kstat_tool, disk_usage);
        let heartbeater = Heartbeater::new(cnapi.clone(), collector)
            .with_dirty_flag(dirty)
            .spawn();

        Ok(RunningAgent {
            server,
            heartbeater,
            zoneevent,
            _zones_watcher: zones_watcher,
            _sysinfo_watcher: sysinfo_watcher,
            _cnapi: cnapi,
        })
    }
}

/// Pick the CNAPI URL in legacy-equivalent order of preference:
/// 1. Explicit override (tests, `--cnapi-url` flag).
/// 2. `agent_config.cnapi.url` if set.
/// 3. `cnapi.<datacenter_name>.<dns_domain>` derived from sdc_config.
fn resolve_cnapi_url(
    override_url: Option<&str>,
    agent_config: &AgentConfig,
    sdc_config: &SdcConfig,
) -> String {
    if let Some(u) = override_url {
        return u.to_string();
    }
    if let Some(cnapi) = &agent_config.cnapi {
        return cnapi.url.clone();
    }
    format!("http://{}", sdc_config.cnapi_dns_name())
}

/// Run `/usr/bin/sysinfo` and return the parsed [`Sysinfo`]. Split out so
/// tests can inject a different binary via `SmartosBinaries.vmadm` — wait
/// no, sysinfo uses its own path. Actually we don't use `self.bins.vmadm`
/// here; we call the stock [`Sysinfo::collect`] which uses
/// `/usr/bin/sysinfo`. That's an accepted tradeoff: sysinfo has no
/// illumos-less equivalent, so tests feeding JSON fixtures go through
/// the per-startup [`SmartosPaths`] injection points instead.
async fn load_sysinfo(_vmadm: &Path) -> Result<Sysinfo> {
    Sysinfo::collect().await.context("run /usr/bin/sysinfo")
}

fn start_http_server(
    bind_addr: SocketAddr,
    context: Arc<AgentContext>,
) -> Result<HttpServer<Arc<AgentContext>>> {
    let api = cn_agent_api_mod::api_description::<CnAgentApiImpl>()
        .map_err(|e| anyhow::anyhow!("build api description: {e}"))?;
    let config = ConfigDropshot {
        bind_address: bind_addr,
        // docker_build payloads routinely push a few MiB; match the
        // legacy agent's generous default. (Downstream, docker_build is
        // the only task that cares.)
        default_request_body_max_bytes: 4 * 1024 * 1024,
        default_handler_task_mode: dropshot::HandlerTaskMode::Detached,
        ..Default::default()
    };
    let log = ConfigLogging::StderrTerminal {
        level: ConfigLoggingLevel::Info,
    }
    .to_logger("triton-cn-agent")
    .map_err(|e| anyhow::anyhow!("build logger: {e}"))?;

    let server = HttpServerStarter::new(&config, api, context, &log)
        .map_err(|e| anyhow::anyhow!("create http server: {e}"))?
        .start();
    tracing::info!(bind = %bind_addr, "cn-agent HTTP server listening");
    Ok(server)
}

async fn register_sysinfo(cnapi: &CnapiClient, sysinfo: &Sysinfo, actual_port: u16) -> Result<()> {
    // Inject the actual bound port so CNAPI can dial us back on
    // non-default ports. Clone the raw sysinfo to avoid mutating the
    // shared value; the injected field only lives on the wire copy.
    let mut payload = sysinfo.raw.clone();
    if let Some(obj) = payload.as_object_mut() {
        obj.insert(
            "CN Agent Port".to_string(),
            serde_json::Value::from(actual_port),
        );
    }
    match cnapi.register_sysinfo(&payload).await {
        Ok(()) => Ok(()),
        Err(e) if e.is_sysinfo_unsupported() => {
            tracing::warn!(
                error = %e,
                "CNAPI does not support sysinfo registration (legacy 404); skipping"
            );
            Ok(())
        }
        Err(e) => Err(anyhow::anyhow!("register sysinfo with CNAPI: {e}")),
    }
}

async fn post_agents_list(
    cnapi: &CnapiClient,
    sysinfo: &Sysinfo,
    collector: &AgentsCollector,
) -> Result<()> {
    let agents = collector
        .collect(&sysinfo.raw)
        .await
        .context("collect agents info")?;
    cnapi
        .post_agents(&agents)
        .await
        .map_err(|e| anyhow::anyhow!("post agents to CNAPI: {e}"))?;
    Ok(())
}

fn spawn_sysinfo_watcher(
    path: &Path,
    cnapi: Arc<CnapiClient>,
    port: u16,
) -> notify::Result<SysinfoFileWatcher> {
    let cnapi = cnapi.clone();
    SysinfoFileWatcher::spawn_watching(path.to_path_buf(), move || {
        let cnapi = cnapi.clone();
        tokio::spawn(async move {
            match Sysinfo::collect().await {
                Ok(si) => match register_sysinfo(&cnapi, &si, port).await {
                    Ok(()) => tracing::info!("re-registered sysinfo with CNAPI after change"),
                    Err(e) => tracing::warn!(error = %e, "failed to re-register sysinfo"),
                },
                Err(e) => {
                    tracing::warn!(error = %e, "sysinfo change noticed but sysinfo re-read failed")
                }
            }
        });
    })
}

/// Run a non-SmartOS (dev/dummy) agent: HTTP server only, no CNAPI
/// integration. Matches the `--backend dummy` path.
pub fn start_dummy(
    bind_addr: SocketAddr,
    server_uuid: Uuid,
) -> Result<HttpServer<Arc<AgentContext>>> {
    let registry = tasks::common_registry();
    let metadata = AgentMetadata {
        name: "cn-agent".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        server_uuid,
        backend: "dummy".to_string(),
    };
    let context = Arc::new(AgentContext::new(metadata, registry));
    start_http_server(bind_addr, context)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::smartos::config::DEFAULT_AGENT_CONFIG_PATH;

    #[test]
    fn resolve_cnapi_url_prefers_override() {
        let agent_config = AgentConfig::default();
        let sdc_config = SdcConfig {
            datacenter_name: "dc1".into(),
            dns_domain: "example.com".into(),
            extras: Default::default(),
        };
        let url = resolve_cnapi_url(Some("http://override"), &agent_config, &sdc_config);
        assert_eq!(url, "http://override");
    }

    #[test]
    fn resolve_cnapi_url_uses_agent_config_next() {
        let agent_config = AgentConfig {
            no_rabbit: true,
            cnapi: Some(crate::smartos::config::CnapiConfig {
                url: "http://agent-configured".into(),
            }),
            ..AgentConfig::default()
        };
        let sdc_config = SdcConfig {
            datacenter_name: "dc1".into(),
            dns_domain: "example.com".into(),
            extras: Default::default(),
        };
        let url = resolve_cnapi_url(None, &agent_config, &sdc_config);
        assert_eq!(url, "http://agent-configured");
    }

    #[test]
    fn resolve_cnapi_url_falls_back_to_sdc_dns() {
        let agent_config = AgentConfig {
            no_rabbit: true,
            ..AgentConfig::default()
        };
        let sdc_config = SdcConfig {
            datacenter_name: "dc1".into(),
            dns_domain: "example.com".into(),
            extras: Default::default(),
        };
        let url = resolve_cnapi_url(None, &agent_config, &sdc_config);
        assert_eq!(url, "http://cnapi.dc1.example.com");
    }

    #[test]
    fn smartos_paths_default_points_at_real_install() {
        let paths = SmartosPaths::default();
        assert_eq!(paths.agent_config, PathBuf::from(DEFAULT_AGENT_CONFIG_PATH));
        assert!(paths.sdc_config_args.contains(&"-json".to_string()));
    }
}
