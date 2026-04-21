// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! On-disk token format + load/store helpers.
//!
//! Token files live at `~/.triton/tokens/<profile>.json`, mode 0600.
//! Writes go through an atomic `.new` + rename swap so a crashed CLI
//! cannot leave a torn / half-written file that a subsequent request
//! would try to parse.
//!
//! Tokens themselves (access + refresh) are never logged or printed —
//! we only ever emit the expiry timestamp and username in user-facing
//! output.
#![allow(dead_code)] // public API consumed by login/logout/whoami commits

use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, anyhow};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt as _;

use crate::config::paths;

/// On-disk representation of a token pair for one profile.
#[derive(Clone, Serialize, Deserialize)]
pub struct StoredTokens {
    /// ES256 JWT presented as `Authorization: Bearer <token>`.
    pub access_token: String,
    /// Single-use refresh token presented to `POST /v1/auth/refresh`.
    pub refresh_token: String,
    /// Derived from the access token's `exp` claim; used to schedule
    /// proactive refresh.
    pub expires_at: DateTime<Utc>,
    /// Gateway URL the tokens were issued by. Stored for sanity-check
    /// purposes so a token file accidentally moved between profiles
    /// pointing at different gateways errors clearly rather than
    /// silently hitting the wrong host.
    pub gateway_url: String,
}

// Deliberately do NOT derive Debug — we don't want access / refresh
// tokens showing up in any format string accidentally.
impl std::fmt::Debug for StoredTokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoredTokens")
            .field("access_token", &"<redacted>")
            .field("refresh_token", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .field("gateway_url", &self.gateway_url)
            .finish()
    }
}

/// Location of the tokens directory: `<config_dir>/tokens`.
///
/// Matches the on-disk layout of `profiles.d/` but namespaced separately
/// so `ls ~/.triton/profiles.d` doesn't surface bearer tokens.
pub fn tokens_dir() -> PathBuf {
    paths::config_dir().join("tokens")
}

/// Absolute path to the token file for a given profile.
///
/// Profile names are validated by [`paths::validate_profile_name`], so
/// they cannot escape the directory via path traversal.
pub fn token_path(profile: &str) -> Result<PathBuf> {
    paths::validate_profile_name(profile)?;
    Ok(tokens_dir().join(format!("{profile}.json")))
}

/// Ensure the tokens directory exists (mode 0700).
async fn ensure_tokens_dir() -> Result<PathBuf> {
    let dir = tokens_dir();
    tokio::fs::create_dir_all(&dir)
        .await
        .with_context(|| format!("creating {}", dir.display()))?;
    // Tighten the directory mode too — tokens should not be world-readable
    // even if someone later adds a file without the 0600 dance.
    let mut perms = tokio::fs::metadata(&dir).await?.permissions();
    if perms.mode() & 0o777 != 0o700 {
        perms.set_mode(0o700);
        tokio::fs::set_permissions(&dir, perms).await.ok();
    }
    Ok(dir)
}

impl StoredTokens {
    /// Load the tokens for the given profile.
    ///
    /// Returns `Ok(None)` if the file does not exist (logged-out state).
    /// Emits a warning to stderr if the file mode is not 0600, but still
    /// loads it — the user may have moved it between hosts and lost
    /// perms; failing hard on every request would be worse UX than a
    /// nudge.
    pub async fn load(profile: &str) -> Result<Option<Self>> {
        let path = token_path(profile)?;
        match tokio::fs::read_to_string(&path).await {
            Ok(contents) => {
                warn_if_wrong_mode(&path).await;
                let tokens: StoredTokens = serde_json::from_str(&contents)
                    .with_context(|| format!("parsing token file {}", path.display()))?;
                Ok(Some(tokens))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(anyhow!("reading {}: {e}", path.display())),
        }
    }

    /// Atomically write the tokens to disk at mode 0600.
    ///
    /// Writes a sibling `<profile>.json.new` with the right perms, fsyncs,
    /// then renames over the final path. Never leaves a torn file.
    pub async fn save(&self, profile: &str) -> Result<()> {
        let dir = ensure_tokens_dir().await?;
        let final_path = dir.join(format!("{profile}.json"));
        let tmp_path = dir.join(format!("{profile}.json.new"));

        let json = serde_json::to_vec_pretty(self)?;

        // Create the tempfile mode 0600 up-front; truncate so a stale
        // previous .new is overwritten cleanly.
        let std_file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp_path)
            .with_context(|| format!("opening {}", tmp_path.display()))?;
        // Convert to tokio's file so subsequent writes are async.
        let mut f = tokio::fs::File::from_std(std_file);
        f.write_all(&json).await?;
        f.sync_all().await?;
        drop(f);

        // Double-check perms survived even if an umask / filesystem did
        // something unexpected.
        let mut perms = tokio::fs::metadata(&tmp_path).await?.permissions();
        if perms.mode() & 0o777 != 0o600 {
            perms.set_mode(0o600);
            tokio::fs::set_permissions(&tmp_path, perms).await?;
        }

        tokio::fs::rename(&tmp_path, &final_path)
            .await
            .with_context(|| {
                format!(
                    "renaming {} -> {}",
                    tmp_path.display(),
                    final_path.display()
                )
            })?;
        Ok(())
    }

    /// Delete the token file for this profile. Idempotent — no-ops if
    /// the file is already gone.
    pub async fn delete(profile: &str) -> Result<()> {
        let path = token_path(profile)?;
        match tokio::fs::remove_file(&path).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(anyhow!("deleting {}: {e}", path.display())),
        }
    }

    /// Seconds remaining until the access token expires. Negative if
    /// the token has already expired.
    pub fn seconds_to_expiry(&self) -> i64 {
        (self.expires_at - Utc::now()).num_seconds()
    }
}

async fn warn_if_wrong_mode(path: &Path) {
    let Ok(meta) = tokio::fs::metadata(path).await else {
        return;
    };
    let mode = meta.permissions().mode() & 0o777;
    if mode != 0o600 {
        eprintln!(
            "triton: warning: {} has mode {:o}; expected 0600. \
             Tighten permissions: `chmod 600 {}`",
            path.display(),
            mode,
            path.display()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use std::sync::Mutex;
    use std::sync::OnceLock;

    /// Tests mutate the process-wide `TRITON_CONFIG_DIR` env var, which
    /// would race if cargo ran them in parallel. Serialize them through
    /// this mutex. All tokens tests take the lock for their whole body.
    fn serial_guard() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|p| p.into_inner())
    }

    /// Return a unique config dir for this test and set TRITON_CONFIG_DIR
    /// so `paths::config_dir()` resolves to it. The returned TempDir must
    /// outlive the test body; drop it to clean up.
    fn scoped_config_dir() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().expect("tempdir");
        // SAFETY: callers hold `serial_guard()` so no other test thread is
        // reading/writing TRITON_CONFIG_DIR concurrently.
        unsafe {
            std::env::set_var("TRITON_CONFIG_DIR", tmp.path());
        }
        tmp
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // serial_guard is held intentionally to gate shared env
    async fn save_and_load_round_trip() {
        let _g = serial_guard();
        let _dir = scoped_config_dir();

        let tokens = StoredTokens {
            access_token: "AT".into(),
            refresh_token: "RT".into(),
            expires_at: Utc::now() + Duration::hours(1),
            gateway_url: "https://gw.example.com".into(),
        };
        tokens.save("roundtrip").await.expect("save");

        let loaded = StoredTokens::load("roundtrip")
            .await
            .expect("load ok")
            .expect("file exists");
        assert_eq!(loaded.access_token, "AT");
        assert_eq!(loaded.refresh_token, "RT");
        assert_eq!(loaded.gateway_url, "https://gw.example.com");
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn load_missing_file_returns_none() {
        let _g = serial_guard();
        let _dir = scoped_config_dir();
        assert!(
            StoredTokens::load("nope")
                .await
                .expect("no error")
                .is_none(),
            "missing token file should return None"
        );
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn saved_file_is_mode_0600() {
        let _g = serial_guard();
        let _dir = scoped_config_dir();

        let tokens = StoredTokens {
            access_token: "a".into(),
            refresh_token: "r".into(),
            expires_at: Utc::now(),
            gateway_url: "https://example".into(),
        };
        tokens.save("modechk").await.unwrap();

        let path = token_path("modechk").unwrap();
        let meta = tokio::fs::metadata(&path).await.unwrap();
        let mode = meta.permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "token file should be mode 0600 but was {mode:o}",
        );
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn delete_is_idempotent() {
        let _g = serial_guard();
        let _dir = scoped_config_dir();
        StoredTokens::delete("nosuch")
            .await
            .expect("no error on missing");
    }

    /// Tokens must not appear in `{:?}`.
    #[test]
    fn debug_redacts_tokens() {
        let tokens = StoredTokens {
            access_token: "SECRET-ACCESS".into(),
            refresh_token: "SECRET-REFRESH".into(),
            expires_at: Utc::now(),
            gateway_url: "https://x".into(),
        };
        let dbg = format!("{tokens:?}");
        assert!(!dbg.contains("SECRET-ACCESS"), "{dbg}");
        assert!(!dbg.contains("SECRET-REFRESH"), "{dbg}");
        assert!(dbg.contains("<redacted>"));
    }
}
