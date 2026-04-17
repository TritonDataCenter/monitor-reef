// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `recovery_config` — stage or activate an EDAR recovery configuration.
//!
//! Only meaningful on CNs with an encrypted zpool (EDAR). The task:
//! 1. Validates the action (`stage` or `activate`).
//! 2. Reads sysinfo to confirm the zpool is encrypted and collect the
//!    current recovery state.
//! 3. For `activate`, asserts the requested recovery_uuid matches the
//!    currently staged config.
//! 4. For `stage`, writes the template to `/var/tmp/.recovery-config-
//!    template-<fid>` and runs `kbmadm recovery add -t <file> -r <token>
//!    <zpool>`.
//! 5. For `activate`, runs `kbmadm recovery activate <zpool>`.
//! 6. Re-reads sysinfo for updated recovery state.
//! 7. Posts the updated state to KBMAPI at `PUT
//!    /pivtokens/<guid>/recovery-tokens`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use cn_agent_api::{TaskError, TaskResult};
use serde::Deserialize;
use sha2::{Digest, Sha512};

use crate::registry::TaskHandler;
use crate::smartos::config::SdcConfig;
use crate::smartos::sysinfo::Sysinfo;

pub const DEFAULT_KBMADM_BIN: &str = "/usr/sbin/kbmadm";

#[derive(Debug, Deserialize)]
struct Params {
    action: Action,
    pivtoken: String,
    recovery_uuid: String,
    #[serde(default)]
    template: Option<String>,
    #[serde(default)]
    token: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Action {
    Stage,
    Activate,
}

pub struct RecoveryConfigTask {
    kbmadm_bin: PathBuf,
    template_dir: PathBuf,
    /// Loader for the current SDC config; used to resolve the KBMAPI URL.
    sdc_config: Arc<dyn SdcConfigLoader>,
    /// Override for the KBMAPI base URL (tests inject a stub server).
    kbmapi_override: Option<String>,
    /// Sysinfo source; tests substitute a fixture.
    sysinfo: Arc<dyn SysinfoSource>,
}

#[async_trait]
pub trait SdcConfigLoader: Send + Sync + 'static {
    async fn load(&self) -> Result<SdcConfig, String>;
}

#[async_trait]
pub trait SysinfoSource: Send + Sync + 'static {
    async fn load(&self) -> Result<Sysinfo, String>;
}

struct LiveSdcConfig;

#[async_trait]
impl SdcConfigLoader for LiveSdcConfig {
    async fn load(&self) -> Result<SdcConfig, String> {
        SdcConfig::load().await.map_err(|e| e.to_string())
    }
}

struct LiveSysinfo;

#[async_trait]
impl SysinfoSource for LiveSysinfo {
    async fn load(&self) -> Result<Sysinfo, String> {
        Sysinfo::collect().await.map_err(|e| e.to_string())
    }
}

impl RecoveryConfigTask {
    pub fn new() -> Self {
        Self {
            kbmadm_bin: PathBuf::from(DEFAULT_KBMADM_BIN),
            template_dir: PathBuf::from("/var/tmp"),
            sdc_config: Arc::new(LiveSdcConfig),
            kbmapi_override: None,
            sysinfo: Arc::new(LiveSysinfo),
        }
    }

    pub fn with_kbmadm(mut self, bin: impl Into<PathBuf>) -> Self {
        self.kbmadm_bin = bin.into();
        self
    }

    pub fn with_template_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.template_dir = dir.into();
        self
    }

    pub fn with_kbmapi_override(mut self, url: impl Into<String>) -> Self {
        self.kbmapi_override = Some(url.into());
        self
    }

    pub fn with_sdc_config(mut self, loader: Arc<dyn SdcConfigLoader>) -> Self {
        self.sdc_config = loader;
        self
    }

    pub fn with_sysinfo(mut self, source: Arc<dyn SysinfoSource>) -> Self {
        self.sysinfo = source;
        self
    }
}

impl Default for RecoveryConfigTask {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl TaskHandler for RecoveryConfigTask {
    async fn run(&self, params: serde_json::Value) -> Result<TaskResult, TaskError> {
        let p: Params = serde_json::from_value(params)
            .map_err(|e| TaskError::new(format!("invalid params: {e}")))?;

        if matches!(p.action, Action::Stage) && p.template.is_none() {
            return Err(TaskError::new(
                "Recovery Configuration error: Missing template request parameter".to_string(),
            ));
        }

        let sdc_config = self
            .sdc_config
            .load()
            .await
            .map_err(|e| TaskError::new(format!("Recovery Configuration error: {e}")))?;
        let kbmapi_url = self.kbmapi_override.clone().unwrap_or_else(|| {
            format!(
                "http://kbmapi.{}.{}",
                sdc_config.datacenter_name, sdc_config.dns_domain
            )
        });

        let sysinfo = self
            .sysinfo
            .load()
            .await
            .map_err(|e| TaskError::new(format!("Recovery Configuration error: {e}")))?;
        let encrypted = sysinfo
            .raw
            .get("Zpool Encrypted")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if !encrypted {
            return Err(TaskError::new(
                "Recovery Configuration error: Recovery configuration can be \
                 staged or activated only on servers with encrypted zpools"
                    .to_string(),
            ));
        }
        let mut zpool_recovery = sysinfo
            .raw
            .get("Zpool Recovery")
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();
        let zpool = sysinfo
            .raw
            .get("Zpool")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                TaskError::new("Recovery Configuration error: sysinfo missing Zpool".to_string())
            })?
            .to_string();
        let server_uuid = sysinfo
            .uuid()
            .ok_or_else(|| {
                TaskError::new("Recovery Configuration error: sysinfo missing UUID".to_string())
            })?
            .to_string();

        // Activate only works for the currently staged config.
        if p.action == Action::Activate {
            let staged_hex = zpool_recovery
                .get("staged")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let staged_uuid = repeatable_uuid_from_hex(staged_hex);
            if staged_uuid.as_deref() != Some(p.recovery_uuid.as_str()) {
                return Err(TaskError::new(
                    "Recovery Configuration error: Only the staged recovery \
                     configuration can be activated"
                        .to_string(),
                ));
            }
        }

        // For stage, write the template to a tempfile; then invoke kbmadm.
        let template_file = if p.action == Action::Stage {
            let tmpl = p.template.as_ref().ok_or_else(|| {
                TaskError::new(
                    "Recovery Configuration error: template required for stage".to_string(),
                )
            })?;
            let path = self.make_template_path();
            tokio::fs::write(&path, tmpl).await.map_err(|e| {
                TaskError::new(format!(
                    "Recovery Configuration error: failed to write template: {e}"
                ))
            })?;
            Some(path)
        } else {
            None
        };

        let kbmadm_result = self
            .run_kbmadm(
                p.action,
                template_file.as_deref(),
                p.token.as_deref(),
                &zpool,
            )
            .await;

        if let Some(path) = template_file
            && let Err(e) = tokio::fs::remove_file(&path).await
        {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "failed to remove recovery template tempfile"
            );
        }

        kbmadm_result?;

        // Re-read sysinfo after kbmadm runs so we pick up the new state.
        if let Ok(updated) = self.sysinfo.load().await
            && let Some(rec) = updated
                .raw
                .get("Zpool Recovery")
                .and_then(|v| v.as_object())
        {
            zpool_recovery = rec.clone();
        }

        // Push the updated state to KBMAPI. Failure here is logged but
        // not fatal — the sysinfo reregister path will catch up.
        if let Err(e) = post_to_kbmapi(
            &kbmapi_url,
            &p.pivtoken,
            &server_uuid,
            &zpool_recovery,
            p.token.as_deref(),
        )
        .await
        {
            tracing::warn!(error = %e, "failed to post recovery info to KBMAPI");
        }

        Ok(serde_json::json!({}))
    }
}

impl RecoveryConfigTask {
    fn make_template_path(&self) -> PathBuf {
        let fid = make_random_hex();
        self.template_dir
            .join(format!(".recovery-config-template-{fid}"))
    }

    async fn run_kbmadm(
        &self,
        action: Action,
        template: Option<&Path>,
        token: Option<&str>,
        zpool: &str,
    ) -> Result<(), TaskError> {
        let mut args: Vec<String> = vec!["recovery".to_string()];
        match action {
            Action::Activate => {
                args.push("activate".to_string());
                args.push(zpool.to_string());
            }
            Action::Stage => {
                let tmpl = template.ok_or_else(|| {
                    TaskError::new(
                        "Recovery Configuration error: internal: missing staged template"
                            .to_string(),
                    )
                })?;
                let tok = token.ok_or_else(|| {
                    TaskError::new(
                        "Recovery Configuration error: stage requires a token".to_string(),
                    )
                })?;
                args.push("add".to_string());
                args.push("-t".to_string());
                args.push(tmpl.display().to_string());
                args.push("-r".to_string());
                args.push(tok.to_string());
                args.push(zpool.to_string());
            }
        }
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let output = tokio::process::Command::new(&self.kbmadm_bin)
            .args(&arg_refs)
            .output()
            .await
            .map_err(|e| {
                TaskError::new(format!(
                    "Recovery Configuration error: failed to spawn kbmadm: {e}"
                ))
            })?;
        if !output.status.success() {
            return Err(TaskError::new(format!(
                "Recovery Configuration error: kbmadm exited with {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            )));
        }
        Ok(())
    }
}

async fn post_to_kbmapi(
    base_url: &str,
    guid: &str,
    server_uuid: &str,
    zpool_recovery: &serde_json::Map<String, serde_json::Value>,
    token: Option<&str>,
) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| e.to_string())?;

    // Convert hex-encoded recovery ids to repeatable UUIDs. The legacy
    // task does this for every value in zpool_recovery.
    let mut recovery: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();
    for (k, v) in zpool_recovery {
        if let Some(hex) = v.as_str()
            && let Some(uuid) = repeatable_uuid_from_hex(hex)
        {
            recovery.insert(k.clone(), serde_json::Value::String(uuid));
        }
    }

    let mut params = serde_json::Map::new();
    params.insert(
        "zpool_recovery".to_string(),
        serde_json::Value::Object(recovery),
    );
    params.insert(
        "cn_uuid".to_string(),
        serde_json::Value::String(server_uuid.to_string()),
    );
    if let Some(tok) = token {
        let hash = Sha512::digest(tok.as_bytes());
        let uuid = repeatable_uuid_from_bytes(&hash);
        if let Some(u) = uuid {
            params.insert("recovery_token".to_string(), serde_json::Value::String(u));
        }
    }

    let url = format!("{base_url}/pivtokens/{guid}/recovery-tokens");
    let resp = client
        .put(&url)
        .json(&serde_json::Value::Object(params))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("status {status}: {body}"));
    }
    Ok(())
}

/// Convert a hex string to a version-5/variant-RFC-4122 UUID.
/// Reproduces the legacy `repeatableUUIDFromHexString` helper so the
/// UUIDs we post to KBMAPI match those emitted by the Node.js agent
/// byte-for-byte.
pub fn repeatable_uuid_from_hex(hex: &str) -> Option<String> {
    if hex.len() < 32 {
        return None;
    }
    let bytes = hex_decode(&hex[..32])?;
    repeatable_uuid_from_bytes(&bytes)
}

/// Same as [`repeatable_uuid_from_hex`] but starts from raw bytes.
pub fn repeatable_uuid_from_bytes(bytes: &[u8]) -> Option<String> {
    if bytes.len() < 16 {
        return None;
    }
    let mut b = [0u8; 16];
    b.copy_from_slice(&bytes[..16]);
    // Set variant (RFC-4122)
    b[8] = (b[8] & 0x3f) | 0xa0;
    // Set version 5
    b[6] = (b[6] & 0x0f) | 0x50;
    Some(format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0],
        b[1],
        b[2],
        b[3],
        b[4],
        b[5],
        b[6],
        b[7],
        b[8],
        b[9],
        b[10],
        b[11],
        b[12],
        b[13],
        b[14],
        b[15]
    ))
}

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    let bytes = s.as_bytes();
    for i in (0..s.len()).step_by(2) {
        let hi = hex_nibble(bytes[i])?;
        let lo = hex_nibble(bytes[i + 1])?;
        out.push((hi << 4) | lo);
    }
    Some(out)
}

fn hex_nibble(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

fn make_random_hex() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let pid = std::process::id();
    format!("{:08x}", now ^ pid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeatable_uuid_from_hex_matches_node_output() {
        // Fixture computed via the legacy Node helper:
        //   Buffer.from('00112233445566778899aabbccddeeff','hex'),
        //   variant byte at [8]: 0xa9, version byte at [6]: 0x55.
        let uuid = repeatable_uuid_from_hex("00112233445566778899aabbccddeeff").expect("uuid");
        assert_eq!(uuid, "00112233-4455-5677-a899-aabbccddeeff");
    }

    #[test]
    fn repeatable_uuid_rejects_short_input() {
        assert_eq!(repeatable_uuid_from_hex("00"), None);
    }

    #[test]
    fn hex_decode_roundtrips() {
        let bytes = hex_decode("ff00aa").expect("decode");
        assert_eq!(bytes, vec![0xff, 0x00, 0xaa]);
        assert_eq!(hex_decode("nothex"), None);
    }
}
