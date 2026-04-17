// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Wrapper around the `imgadm` CLI.
//!
//! We have two kinds of imgadm access:
//!
//! * [`ImgadmDb`](crate::smartos::imgadm::ImgadmDb) — reads
//!   `/var/imgadm/images/<zpool>-<uuid>.json` directly. Fast; used by
//!   the heartbeater's disk-usage sampler to decide whether a UUID-named
//!   dataset is really an imgadm-installed image.
//!
//! * [`ImgadmTool`] (this module) — shells out to the real `imgadm`
//!   binary. Used by the `image_get` and `image_ensure_present` tasks
//!   because the legacy implementation depends on imgadm's full install
//!   logic (locking, concurrent-import checks, image source resolution).
//!
//! `ImgadmTool` takes a [`ZfsTool`] injection so
//! `wait_for_concurrent_import` can poll the `<zpool>/<uuid>-partial`
//! dataset without re-running `zfs list` directly.

use std::path::PathBuf;
use std::process::ExitStatus;
use std::sync::Arc;
use std::time::Duration;

use thiserror::Error;
use tokio::time::Instant;

use crate::smartos::zfs::{DatasetType, ListDatasetsOptions, ZfsError, ZfsTool};

pub const DEFAULT_IMGADM_BIN: &str = "/usr/sbin/imgadm";

/// Default zpool imgadm imports into. Every Triton CN uses `zones`.
pub const DEFAULT_ZPOOL: &str = "zones";

/// How long `image_ensure_present` waits for a stale `<pool>/<uuid>-partial`
/// dataset to disappear before giving up. Matches the legacy 1-hour
/// timeout (same as the CNAPI provision workflow).
pub const DEFAULT_IMPORT_LOCK_TIMEOUT: Duration = Duration::from_secs(60 * 60);

/// Polling cadence used while waiting on the partial-dataset lock.
pub const IMPORT_LOCK_POLL_INTERVAL: Duration = Duration::from_secs(10);

#[derive(Debug, Error)]
pub enum ImgadmCliError {
    #[error("failed to spawn {path}: {source}")]
    Spawn {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("imgadm exited with status {status}: {stderr}")]
    NonZeroExit { status: ExitStatus, stderr: String },
    #[error("failed to parse imgadm output: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("timed out after {timeout:?} waiting for {dataset} to be removed")]
    ImportLockTimeout { dataset: String, timeout: Duration },
    #[error("zfs error while polling import lock: {0}")]
    Zfs(#[from] ZfsError),
    #[error("imgadm failed: {0}")]
    ImgadmReported(String),
}

/// Options for [`ImgadmTool::import`]. Maps directly to the imgadm CLI
/// flags our tasks set.
#[derive(Debug, Clone, Default)]
pub struct ImportOptions {
    /// URL of an alternate image source (`-S`). When set, imgadm fetches
    /// from there; when unset, imgadm uses its default configured sources.
    pub source: Option<String>,
    /// If true, pass `--zstream` so the source produces a raw `zfs send`
    /// stream. Implies `source` is an in-cluster peer.
    pub zstream: bool,
    /// Override for the partial-dataset lock timeout.
    pub lock_timeout: Option<Duration>,
}

/// Shell-based imgadm wrapper.
#[derive(Clone)]
pub struct ImgadmTool {
    pub bin: PathBuf,
    zfs: Arc<ZfsTool>,
}

impl ImgadmTool {
    pub fn new(zfs: Arc<ZfsTool>) -> Self {
        Self {
            bin: PathBuf::from(DEFAULT_IMGADM_BIN),
            zfs,
        }
    }

    pub fn with_bin(mut self, bin: impl Into<PathBuf>) -> Self {
        self.bin = bin.into();
        self
    }

    /// `imgadm get <uuid>` → manifest JSON object.
    ///
    /// imgadm wraps the manifest as `{manifest: {...}, zpool: ..., ...}`;
    /// the legacy task returns `.manifest` only, so we do the same.
    pub async fn get(&self, uuid: &str) -> Result<serde_json::Value, ImgadmCliError> {
        let output = tokio::process::Command::new(&self.bin)
            .args(["get", uuid])
            .output()
            .await
            .map_err(|source| ImgadmCliError::Spawn {
                path: self.bin.clone(),
                source,
            })?;
        if !output.status.success() {
            return Err(ImgadmCliError::NonZeroExit {
                status: output.status,
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }
        let parsed: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        // Legacy: `JSON.parse(stdout.trim()).manifest`.
        match parsed.get("manifest") {
            Some(m) => Ok(m.clone()),
            None => Ok(parsed),
        }
    }

    /// `imgadm import -q -P <zpool> <uuid> [-S <source>] [--zstream]`.
    ///
    /// Waits for any concurrent `<zpool>/<uuid>-partial` dataset to be
    /// removed first (imgadm's own "lock" under OS-2203).
    pub async fn import(
        &self,
        zpool: &str,
        uuid: &str,
        opts: &ImportOptions,
    ) -> Result<(), ImgadmCliError> {
        let timeout = opts.lock_timeout.unwrap_or(DEFAULT_IMPORT_LOCK_TIMEOUT);
        self.wait_for_concurrent_import(zpool, uuid, timeout)
            .await?;

        let mut args: Vec<String> = vec![
            "import".to_string(),
            "-q".to_string(),
            "-P".to_string(),
            zpool.to_string(),
            uuid.to_string(),
        ];
        if let Some(source) = &opts.source {
            args.push("-S".to_string());
            args.push(source.clone());
        }
        if opts.zstream {
            args.push("--zstream".to_string());
        }
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();

        let output = tokio::process::Command::new(&self.bin)
            .args(&arg_refs)
            // imgadm >=2.6.0 emits structured debug logs with this env.
            .env("IMGADM_LOG_LEVEL", "debug")
            .output()
            .await
            .map_err(|source| ImgadmCliError::Spawn {
                path: self.bin.clone(),
                source,
            })?;
        if output.status.success() {
            return Ok(());
        }

        // With imgadm >=2.6.0 (IMGADM_LOG_LEVEL=debug) the *last* stderr
        // line is a structured bunyan entry. Pull `.err.message` out if
        // parseable; otherwise surface the raw tail.
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        if let Some(msg) = parse_imgadm_error(&stderr) {
            return Err(ImgadmCliError::ImgadmReported(msg));
        }
        Err(ImgadmCliError::NonZeroExit {
            status: output.status,
            stderr,
        })
    }

    /// Poll for `<zpool>/<uuid>-partial` to disappear. Returns success
    /// when the dataset is gone; errors on timeout.
    pub async fn wait_for_concurrent_import(
        &self,
        zpool: &str,
        uuid: &str,
        timeout: Duration,
    ) -> Result<(), ImgadmCliError> {
        let dataset = format!("{zpool}/{uuid}-partial");
        let deadline = Instant::now() + timeout;

        loop {
            let opts = ListDatasetsOptions {
                dataset: Some(dataset.clone()),
                kind: DatasetType::All,
                recursive: false,
            };
            match self.zfs.list_datasets(&opts).await {
                Ok(rows) if rows.is_empty() => return Ok(()),
                Ok(_) => {
                    tracing::info!(
                        dataset = %dataset,
                        "partial dataset exists, waiting for concurrent import to complete"
                    );
                }
                Err(ZfsError::NonZeroExit { stderr, .. })
                    if stderr.contains("dataset does not exist") =>
                {
                    return Ok(());
                }
                Err(e) => return Err(e.into()),
            }
            if Instant::now() >= deadline {
                return Err(ImgadmCliError::ImportLockTimeout { dataset, timeout });
            }
            tokio::time::sleep(IMPORT_LOCK_POLL_INTERVAL).await;
        }
    }
}

/// Try to extract a structured error message from the last line of
/// imgadm's stderr (IMGADM_LOG_LEVEL=debug produces bunyan JSON lines).
fn parse_imgadm_error(stderr: &str) -> Option<String> {
    let last = stderr.trim().lines().last()?;
    let parsed: serde_json::Value = serde_json::from_str(last).ok()?;
    parsed
        .pointer("/err/message")
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

/// Convenience accessor for the default zfs binary path — matches the
/// module-level defaults so callers can be explicit about pool name.
pub fn default_zpool() -> &'static str {
    DEFAULT_ZPOOL
}

/// Convenience accessor for the default zfs dataset path template used
/// by `image_ensure_present`.
pub fn default_install_dataset(zpool: &str, uuid: &str) -> String {
    format!("{zpool}/{uuid}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bunyan_style_imgadm_error() {
        let stderr = "[debug] starting import\n\
                      {\"level\":50,\"msg\":\"failed\",\"err\":{\"message\":\"image 123 not found\"}}";
        assert_eq!(
            parse_imgadm_error(stderr).as_deref(),
            Some("image 123 not found")
        );
    }

    #[test]
    fn returns_none_when_last_line_isnt_json() {
        assert!(parse_imgadm_error("just a plain error message").is_none());
    }

    #[test]
    fn default_install_dataset_combines_pool_and_uuid() {
        assert_eq!(default_install_dataset("zones", "aaa-bbb"), "zones/aaa-bbb");
    }
}
