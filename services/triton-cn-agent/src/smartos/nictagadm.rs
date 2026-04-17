// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `/usr/bin/nictagadm` wrapper.
//!
//! cn-agent needs two operations:
//!
//! * `list` — to read the current tag → MAC assignment.
//! * `add` / `update` / `delete` — to reconcile that assignment against
//!   the one CNAPI wants.
//!
//! The reconciliation itself lives in [`server_update_nics`]; this
//! module just owns the process spawning.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::ExitStatus;

use thiserror::Error;

pub const DEFAULT_NICTAGADM_BIN: &str = "/usr/bin/nictagadm";

#[derive(Debug, Error)]
pub enum NictagadmError {
    #[error("failed to spawn {path}: {source}")]
    Spawn {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("nictagadm exited with status {status}: {stderr}")]
    NonZeroExit { status: ExitStatus, stderr: String },
}

/// Thin nictagadm wrapper.
#[derive(Debug, Clone)]
pub struct NictagadmTool {
    pub bin: PathBuf,
}

impl Default for NictagadmTool {
    fn default() -> Self {
        Self {
            bin: PathBuf::from(DEFAULT_NICTAGADM_BIN),
        }
    }
}

impl NictagadmTool {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_bin(bin: impl Into<PathBuf>) -> Self {
        Self { bin: bin.into() }
    }

    /// `nictagadm list -p -d '|'` parsed into a map of tag → MAC.
    ///
    /// Lines whose MAC column is `-` are skipped (matches the legacy
    /// "this is an etherstub" filter).
    pub async fn list(&self) -> Result<BTreeMap<String, String>, NictagadmError> {
        let output = tokio::process::Command::new(&self.bin)
            .args(["list", "-p", "-d", "|"])
            .output()
            .await
            .map_err(|source| NictagadmError::Spawn {
                path: self.bin.clone(),
                source,
            })?;
        if !output.status.success() {
            return Err(NictagadmError::NonZeroExit {
                status: output.status,
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }
        let mut tags = BTreeMap::new();
        let text = String::from_utf8_lossy(&output.stdout);
        for line in text.lines() {
            let mut parts = line.splitn(2, '|');
            let tag = parts.next().unwrap_or("").trim();
            let mac = parts.next().unwrap_or("").trim();
            if tag.is_empty() || mac == "-" {
                continue;
            }
            tags.insert(tag.to_string(), mac.to_string());
        }
        Ok(tags)
    }

    /// `nictagadm add <tag> <mac>`.
    pub async fn add(&self, tag: &str, mac: &str) -> Result<(), NictagadmError> {
        self.run(&["add", tag, mac]).await
    }

    /// `nictagadm update <tag> <mac>`.
    pub async fn update(&self, tag: &str, mac: &str) -> Result<(), NictagadmError> {
        self.run(&["update", tag, mac]).await
    }

    /// `nictagadm delete <tag>`.
    pub async fn delete(&self, tag: &str) -> Result<(), NictagadmError> {
        self.run(&["delete", tag]).await
    }

    async fn run(&self, args: &[&str]) -> Result<(), NictagadmError> {
        let output = tokio::process::Command::new(&self.bin)
            .args(args)
            .output()
            .await
            .map_err(|source| NictagadmError::Spawn {
                path: self.bin.clone(),
                source,
            })?;
        if !output.status.success() {
            return Err(NictagadmError::NonZeroExit {
                status: output.status,
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }
        Ok(())
    }
}
