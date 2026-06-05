// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! On-disk CLI configuration: the cluster endpoint plus, when logged
//! in, the cached access/refresh token pair.
//!
//! Lives at `~/.config/triton/<app>/config.json` (mode 0600 on Unix),
//! or under `<PREFIX>_CONFIG_DIR/config.json` when that env var is set
//! (used by tests and sandboxed deployments).

use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::App;

/// Cluster endpoint plus, when logged in, the cached token pair.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub endpoint: String,
    #[serde(default)]
    pub tokens: Option<Tokens>,
}

/// Access/refresh JWT pair from `/v1/auth/login` or `/v1/auth/refresh`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: String,
    pub access_expires_at: DateTime<Utc>,
    pub refresh_expires_at: DateTime<Utc>,
}

impl Config {
    /// Resolve the config file path for `app`.
    pub fn path(app: &App) -> Result<PathBuf> {
        if let Ok(custom) = std::env::var(app.env("CONFIG_DIR")) {
            return Ok(PathBuf::from(custom).join("config.json"));
        }
        let base = dirs::config_dir().context("could not determine user config dir")?;
        Ok(base.join("triton").join(app.name).join("config.json"))
    }

    /// Load the config for `app`, or `None` if it does not exist yet.
    pub fn load(app: &App) -> Result<Option<Self>> {
        let path = Self::path(app)?;
        match std::fs::read_to_string(&path) {
            Ok(body) => {
                let cfg = serde_json::from_str(&body)
                    .with_context(|| format!("parse config at {}", path.display()))?;
                Ok(Some(cfg))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("read config at {}", path.display())),
        }
    }

    /// Persist the config for `app`, atomically and 0600.
    pub fn save(&self, app: &App) -> Result<()> {
        let path = Self::path(app)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create config dir {}", parent.display()))?;
        }
        let body = serde_json::to_string_pretty(self).context("serialize config")?;

        // Write to a temp file created 0600 *from the start* so the
        // token bytes never touch disk world-readable, then atomically
        // rename into place. Clear any stale temp from a crashed run
        // first, since create_new requires the path to be absent.
        let tmp = path.with_extension("json.tmp");
        let _ = std::fs::remove_file(&tmp);
        {
            use std::io::Write;
            #[cfg(unix)]
            let mut f = {
                use std::os::unix::fs::OpenOptionsExt;
                std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(&tmp)
                    .with_context(|| format!("create {}", tmp.display()))?
            };
            #[cfg(not(unix))]
            let mut f =
                std::fs::File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?;
            f.write_all(body.as_bytes())
                .with_context(|| format!("write {}", tmp.display()))?;
            f.sync_all()
                .with_context(|| format!("sync {}", tmp.display()))?;
        }
        std::fs::rename(&tmp, &path)
            .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
        Ok(())
    }

    /// Remove the config file for `app` (used by `<app> logout`).
    /// Succeeds if the file is already absent.
    pub fn remove(app: &App) -> Result<()> {
        let path = Self::path(app)?;
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e).with_context(|| format!("remove {}", path.display())),
        }
    }
}
