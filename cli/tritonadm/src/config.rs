// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! On-disk configuration for `tritonadm`.
//!
//! Lives at `$XDG_CONFIG_HOME/triton/tritonadm/config.json` (typically
//! `~/.config/triton/tritonadm/config.json` on Linux, `~/Library/Application
//! Support/triton/tritonadm/config.json` on macOS). Carries the cluster
//! endpoint and the most recent access/refresh token pair from
//! `tritonadm login` or `tritonadm configure`. The refresh middleware in
//! [`crate::session`] rewrites the file in place after a successful
//! `/v1/auth/refresh`.

use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Cluster endpoint plus, when logged in, the cached token pair.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub endpoint: String,
    #[serde(default)]
    pub tokens: Option<Tokens>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: String,
    pub access_expires_at: DateTime<Utc>,
    pub refresh_expires_at: DateTime<Utc>,
}

impl Config {
    pub fn path() -> Result<PathBuf> {
        // `TRITONADM_CONFIG_DIR` overrides the system default. Useful for
        // tests, for sandboxed deployments, and for operators who want
        // to keep the config alongside other secret material on disk.
        if let Ok(custom) = std::env::var("TRITONADM_CONFIG_DIR") {
            return Ok(PathBuf::from(custom).join("config.json"));
        }
        let base = dirs::config_dir().context("could not determine user config dir")?;
        Ok(base.join("triton").join("tritonadm").join("config.json"))
    }

    /// Load the config file, returning `Ok(None)` if it doesn't exist
    /// (so the caller can decide whether absence is fatal).
    pub fn load() -> Result<Option<Self>> {
        let path = Self::path()?;
        if !path.exists() {
            return Ok(None);
        }
        let bytes = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
        let config: Config =
            serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))?;
        Ok(Some(config))
    }

    /// Atomically write the config file with mode 0600 (on Unix).
    pub fn save(&self) -> Result<()> {
        let path = Self::path()?;
        let parent = path
            .parent()
            .context("config path has no parent directory")?;
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
        let bytes = serde_json::to_vec_pretty(self).context("serialize config")?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, &bytes).with_context(|| format!("write {}", tmp.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o600);
            std::fs::set_permissions(&tmp, perms)
                .with_context(|| format!("chmod {}", tmp.display()))?;
        }
        std::fs::rename(&tmp, &path)
            .with_context(|| format!("rename {} → {}", tmp.display(), path.display()))?;
        Ok(())
    }

    /// Remove the config file (used by `tritonadm logout`).
    pub fn delete() -> Result<()> {
        let path = Self::path()?;
        if path.exists() {
            std::fs::remove_file(&path).with_context(|| format!("delete {}", path.display()))?;
        }
        Ok(())
    }
}
