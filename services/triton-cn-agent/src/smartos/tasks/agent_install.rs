// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `agent_install` — download an agent image from IMGAPI and install it
//! via APM.
//!
//! The legacy task implements a careful self-update dance when the
//! target agent is cn-agent itself. APM replacement unlinks the
//! running binary; to avoid the task killing itself mid-run, cn-agent
//! forwards the install request to an auxiliary `cn-agent-update`
//! service (port 5310, same binary under a different SMF FMRI), which
//! performs the install, and then asks cn-agent to disable
//! cn-agent-update via the `shutdown_cn_agent_update` task. We
//! reproduce all of that — the `self_update` flag is on when the
//! receiving agent is running with bind_port == 5310.
//!
//! Backup-and-restore: if the install fails partway, the legacy task
//! rolls the old install back from a `<pkg>.updating-to.<image>` sibling
//! directory. We do the same.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use cn_agent_api::{TaskError, TaskResult};
use serde::Deserialize;

use crate::cnapi::CnapiClient;
use crate::heartbeater::AgentsCollector;
use crate::registry::TaskHandler;
use crate::smartos::apm::Apm;
use crate::smartos::config::SdcConfig;
use crate::smartos::imgapi::ImgapiClient;
use crate::smartos::sysinfo::Sysinfo;

/// TCP port used by cn-agent-update (auxiliary service). Running on
/// this port means we're the update-helper variant, so it's safe to
/// replace cn-agent itself.
pub const CN_AGENT_UPDATE_PORT: u16 = 5310;

/// TCP port cn-agent itself listens on — receiver of the "please shut
/// me down" task after a self-update.
pub const CN_AGENT_PORT: u16 = 5309;

#[derive(Debug, Deserialize)]
struct Params {
    image_uuid: String,
    /// Optional: pre-computed by cn-agent when it forwards to
    /// cn-agent-update. When set, the receiver skips the IMGAPI fetch.
    #[serde(default)]
    package_file: Option<PathBuf>,
    #[serde(default)]
    package_name: Option<String>,
}

/// Loader for the SDC config. Production uses `/lib/sdc/config.sh`;
/// tests inject a fixture.
#[async_trait]
pub trait SdcConfigSource: Send + Sync + 'static {
    async fn load(&self) -> Result<SdcConfig, String>;
}

struct LiveSdcConfig;

#[async_trait]
impl SdcConfigSource for LiveSdcConfig {
    async fn load(&self) -> Result<SdcConfig, String> {
        SdcConfig::load().await.map_err(|e| e.to_string())
    }
}

pub struct AgentInstallTask {
    apm: Arc<Apm>,
    cnapi: Arc<CnapiClient>,
    collector: AgentsCollector,
    sdc_config: Arc<dyn SdcConfigSource>,
    /// Port this agent is currently bound on. Determines whether we're
    /// the cn-agent-update helper or plain cn-agent.
    bind_port: u16,
    /// Override for IMGAPI base URL (tests).
    imgapi_override: Option<String>,
    /// Override for the cn-agent update-helper base URL (tests).
    update_helper_override: Option<String>,
    /// Override for the cn-agent base URL when sending
    /// `shutdown_cn_agent_update` (tests).
    cn_agent_override: Option<String>,
    /// Path to svcadm. Injectable for tests.
    svcadm_bin: PathBuf,
    /// Download dir for agent tarballs. Matches the legacy `/var/tmp`.
    download_dir: PathBuf,
}

impl AgentInstallTask {
    pub fn new(
        apm: Arc<Apm>,
        cnapi: Arc<CnapiClient>,
        collector: AgentsCollector,
        bind_port: u16,
    ) -> Self {
        Self {
            apm,
            cnapi,
            collector,
            sdc_config: Arc::new(LiveSdcConfig),
            bind_port,
            imgapi_override: None,
            update_helper_override: None,
            cn_agent_override: None,
            svcadm_bin: PathBuf::from("/usr/sbin/svcadm"),
            download_dir: PathBuf::from("/var/tmp"),
        }
    }

    pub fn with_sdc_config(mut self, src: Arc<dyn SdcConfigSource>) -> Self {
        self.sdc_config = src;
        self
    }

    pub fn with_imgapi_override(mut self, url: impl Into<String>) -> Self {
        self.imgapi_override = Some(url.into());
        self
    }

    pub fn with_update_helper_override(mut self, url: impl Into<String>) -> Self {
        self.update_helper_override = Some(url.into());
        self
    }

    pub fn with_cn_agent_override(mut self, url: impl Into<String>) -> Self {
        self.cn_agent_override = Some(url.into());
        self
    }

    pub fn with_svcadm(mut self, bin: impl Into<PathBuf>) -> Self {
        self.svcadm_bin = bin.into();
        self
    }

    pub fn with_download_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.download_dir = dir.into();
        self
    }

    fn is_cn_agent_update_helper(&self) -> bool {
        self.bind_port == CN_AGENT_UPDATE_PORT
    }
}

#[async_trait]
impl TaskHandler for AgentInstallTask {
    async fn run(&self, params: serde_json::Value) -> Result<TaskResult, TaskError> {
        let p: Params = serde_json::from_value(params)
            .map_err(|e| TaskError::new(format!("invalid params: {e}")))?;

        let sdc_config = self
            .sdc_config
            .load()
            .await
            .map_err(|e| TaskError::new(format!("AgentInstall error: load SDC config: {e}")))?;

        // Fetch the tarball unless the caller already supplied it.
        let (package_file, package_name) =
            if let (Some(file), Some(name)) = (p.package_file.clone(), p.package_name.clone()) {
                (file, name)
            } else {
                let imgapi_url = self
                    .imgapi_override
                    .clone()
                    .unwrap_or_else(|| format!("http://{}", imgapi_domain(&sdc_config)));
                let imgapi = ImgapiClient::new(&imgapi_url).map_err(|e| {
                    TaskError::new(format!("AgentInstall error: build IMGAPI client: {e}"))
                })?;
                let image = imgapi
                    .fetch_agent_image(&p.image_uuid, &self.download_dir, &p.image_uuid)
                    .await
                    .map_err(|e| TaskError::new(format!("AgentInstall error: {e}")))?;
                (image.file, image.name)
            };

        let do_update = package_name != "cn-agent" || self.is_cn_agent_update_helper();

        // Self-update indirection: if we're plain cn-agent and the target
        // IS cn-agent, forward to the update helper instead of installing
        // in-process.
        if !do_update {
            self.forward_to_update_helper(&p.image_uuid, &package_name, &package_file)
                .await?;
            return Ok(serde_json::json!({}));
        }

        // Real install path. Back up, install, restore on failure.
        let is_update = self.apm.paths.package_path(&package_name).exists();
        let backup_dir = self
            .apm
            .paths
            .package_path(&format!("{package_name}.updating-to.{}", p.image_uuid));

        // Clean up any stale backup from a previous partial run.
        if backup_dir.exists()
            && let Err(e) = tokio::fs::remove_dir_all(&backup_dir).await
        {
            return Err(TaskError::new(format!(
                "AgentInstall error: clean previous backup {}: {e}",
                backup_dir.display()
            )));
        }

        if is_update {
            copy_recursively(&self.apm.paths.package_path(&package_name), &backup_dir)
                .await
                .map_err(|e| TaskError::new(format!("AgentInstall error: backup: {e}")))?;
        }

        let install_result = self.apm.install_tarball(&package_file).await;

        if let Err(install_err) = install_result {
            tracing::error!(error = %install_err, "agent install failed; rolling back");
            // Remove partial install
            let _ = tokio::fs::remove_dir_all(self.apm.paths.package_path(&package_name)).await;
            // Restore backup if we have one
            if is_update
                && let Err(restore_err) =
                    copy_recursively(&backup_dir, &self.apm.paths.package_path(&package_name)).await
            {
                tracing::warn!(error = %restore_err, "failed to restore agent backup");
            }
            return Err(TaskError::new(format!("AgentInstall error: {install_err}")));
        }

        // Successful install: clean up the backup.
        if is_update
            && backup_dir.exists()
            && let Err(e) = tokio::fs::remove_dir_all(&backup_dir).await
        {
            tracing::warn!(
                path = %backup_dir.display(),
                error = %e,
                "failed to remove agent backup (non-fatal)"
            );
        }

        // Re-post agents list. Failure here is logged but non-fatal —
        // heartbeater will catch up.
        match Sysinfo::collect().await {
            Ok(sysinfo) => match self.collector.collect(&sysinfo.raw).await {
                Ok(agents) => {
                    if let Err(e) = self.cnapi.post_agents(&agents).await {
                        tracing::error!(error = %e, "Error posting agents to CNAPI");
                    } else {
                        tracing::info!("Agents updated into CNAPI");
                    }
                }
                Err(e) => tracing::error!(error = %e, "Error collecting agents"),
            },
            Err(e) => tracing::error!(error = %e, "Error reading sysinfo"),
        }

        // If this WAS a self-update (we're cn-agent-update installing
        // a new cn-agent), tell cn-agent to disable the update helper.
        if self.is_cn_agent_update_helper() && package_name == "cn-agent" {
            let _ = self.send_shutdown_helper_task().await;
        }

        Ok(serde_json::json!({}))
    }
}

impl AgentInstallTask {
    /// Forward the install request to cn-agent-update on port 5310.
    /// Enables the SMF service first, waits 5s for it to start, then
    /// POSTs the task.
    async fn forward_to_update_helper(
        &self,
        image_uuid: &str,
        package_name: &str,
        package_file: &Path,
    ) -> Result<(), TaskError> {
        // Enable cn-agent-update.
        let status = tokio::process::Command::new(&self.svcadm_bin)
            .args(["enable", "cn-agent-update"])
            .status()
            .await
            .map_err(|e| TaskError::new(format!("AgentInstall error: spawn svcadm: {e}")))?;
        if !status.success() {
            return Err(TaskError::new(format!(
                "AgentInstall error: svcadm enable cn-agent-update failed with {status}"
            )));
        }

        // Give cn-agent-update time to come online.
        tokio::time::sleep(Duration::from_secs(5)).await;

        let url = self
            .update_helper_override
            .clone()
            .unwrap_or_else(|| format!("http://127.0.0.1:{CN_AGENT_UPDATE_PORT}"));
        post_task(
            &url,
            "agent_install",
            serde_json::json!({
                "image_uuid": image_uuid,
                "package_file": package_file,
                "package_name": package_name,
            }),
        )
        .await
    }

    async fn send_shutdown_helper_task(&self) -> Result<(), TaskError> {
        // Wait briefly so the main cn-agent is ready to receive.
        tokio::time::sleep(Duration::from_secs(5)).await;

        let url = self
            .cn_agent_override
            .clone()
            .unwrap_or_else(|| format!("http://127.0.0.1:{CN_AGENT_PORT}"));
        post_task(&url, "shutdown_cn_agent_update", serde_json::json!({})).await
    }
}

/// Extract `imgapi_domain` from the SDC config. Triton's config ships a
/// dedicated key for this (`imgapi_domain`) separate from the
/// CNAPI/VMAPI DNS names.
fn imgapi_domain(cfg: &SdcConfig) -> String {
    cfg.extras
        .get("imgapi_domain")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| format!("imgapi.{}.{}", cfg.datacenter_name, cfg.dns_domain))
}

async fn post_task(base_url: &str, task: &str, params: serde_json::Value) -> Result<(), TaskError> {
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(60))
        .build()
        .map_err(|e| TaskError::new(format!("build reqwest client: {e}")))?;
    let url = format!("{base_url}/tasks");
    let resp = client
        .post(&url)
        .json(&serde_json::json!({ "task": task, "params": params }))
        .send()
        .await
        .map_err(|e| TaskError::new(format!("AgentInstall error: POST {url}: {e}")))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(TaskError::new(format!(
            "AgentInstall error: {task} POST returned {status}: {body}"
        )));
    }
    Ok(())
}

/// `cp -rP <src> <dst>` — recursive copy preserving symlinks. Used for
/// the pre-install backup because a tokio-based walk + re-create would
/// be far more code than the legacy shell-out.
async fn copy_recursively(src: &Path, dst: &Path) -> std::io::Result<()> {
    let status = tokio::process::Command::new("/usr/bin/cp")
        .arg("-rP")
        .arg(src)
        .arg(dst)
        .status()
        .await?;
    if !status.success() {
        return Err(std::io::Error::other(format!(
            "cp -rP {} {} exited with {status}",
            src.display(),
            dst.display()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imgapi_domain_prefers_explicit_key() {
        let mut cfg = SdcConfig {
            datacenter_name: "dc".into(),
            dns_domain: "example.com".into(),
            extras: Default::default(),
        };
        cfg.extras.insert(
            "imgapi_domain".into(),
            serde_json::Value::String("imgapi.manual.example".into()),
        );
        assert_eq!(imgapi_domain(&cfg), "imgapi.manual.example");
    }

    #[test]
    fn imgapi_domain_falls_back_to_dns_template() {
        let cfg = SdcConfig {
            datacenter_name: "dc".into(),
            dns_domain: "example.com".into(),
            extras: Default::default(),
        };
        assert_eq!(imgapi_domain(&cfg), "imgapi.dc.example.com");
    }
}
