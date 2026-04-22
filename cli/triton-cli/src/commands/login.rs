// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `triton login` — exchange an SSH key for a tritonapi JWT.
//!
//! The profile's SSH key signs a request to `POST /v1/auth/login-ssh`
//! on the configured URL (gateway or triton-api-server direct). The
//! server verifies the signature against the account's keys in mahi
//! and returns an access token + refresh token. We stash both at
//! `~/.triton/tokens/<profile>.json` (mode 0600, atomic write) so
//! subsequent commands can present the JWT as Bearer. The token file
//! lives outside the profile file intentionally -- older CLIs won't
//! trip over unfamiliar fields, and a future Keychain / libsecret
//! backend can replace the file storage without churning the profile
//! format.

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use triton_gateway_client::TypedClient;

use crate::config::{Profile, paths};

/// Persisted form of a successful `/v1/auth/login-ssh` exchange.
/// Written to `~/.triton/tokens/<profile>.json`. Deliberately flat
/// and stable -- any future backend (Keychain, etc.) can read/write
/// the same shape.
#[derive(Debug, Serialize, Deserialize)]
pub struct StoredTokens {
    pub token: String,
    pub refresh_token: String,
    pub username: String,
    pub user_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    pub is_admin: bool,
    /// Unix epoch seconds at which these tokens were issued. Used by
    /// future proactive-refresh logic; today just informational.
    pub issued_at: i64,
}

pub async fn run(client: &TypedClient, profile: &Profile, use_json: bool) -> Result<()> {
    let response = client
        .inner()
        .auth_login_ssh()
        .send()
        .await
        .map_err(|e| anyhow!("login failed: {e}"))?;
    let login = response.into_inner();

    let stored = StoredTokens {
        token: login.token.clone(),
        refresh_token: login.refresh_token.clone(),
        username: login.user.username.clone(),
        user_id: login.user.id.to_string(),
        email: login.user.email.clone(),
        is_admin: login.user.is_admin,
        issued_at: chrono::Utc::now().timestamp(),
    };

    write_tokens(&profile.name, &stored)
        .await
        .with_context(|| format!("failed to persist tokens for profile '{}'", profile.name))?;

    if use_json {
        // Emit the stored shape (minus the raw token value, which the
        // user can fetch from the file if they need it) so scripts can
        // confirm success without parsing prose.
        let summary = serde_json::json!({
            "profile": profile.name,
            "username": stored.username,
            "user_id": stored.user_id,
            "email": stored.email,
            "is_admin": stored.is_admin,
            "issued_at": stored.issued_at,
        });
        println!("{}", serde_json::to_string_pretty(&summary)?);
    } else {
        println!("Logged in to {} as {}.", profile.url, stored.username);
        if stored.is_admin {
            println!("  (operator / admin)");
        }
        let path = paths::token_path(&profile.name)?;
        println!("Token saved to {}", path.display());
    }

    Ok(())
}

/// Write tokens atomically with mode 0600. Create-parent-dir on
/// demand (mode 0700 on the directory). Uses a temp file + rename so
/// a concurrent login or crash can never produce a half-written
/// token file that subsequent Bearer calls would choke on.
async fn write_tokens(profile_name: &str, tokens: &StoredTokens) -> Result<()> {
    let path = paths::token_path(profile_name)?;
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("create tokens directory {}", parent.display()))?;
        set_mode_if_unix(parent, 0o700).await?;
    }
    let tmp = path.with_extension("json.new");
    let json = serde_json::to_vec_pretty(tokens)?;
    tokio::fs::write(&tmp, &json)
        .await
        .with_context(|| format!("write {}", tmp.display()))?;
    set_mode_if_unix(&tmp, 0o600).await?;
    tokio::fs::rename(&tmp, &path)
        .await
        .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

#[cfg(unix)]
async fn set_mode_if_unix(path: &std::path::Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let perms = std::fs::Permissions::from_mode(mode);
    tokio::fs::set_permissions(path, perms)
        .await
        .with_context(|| format!("chmod {:o} {}", mode, path.display()))
}

#[cfg(not(unix))]
async fn set_mode_if_unix(_: &std::path::Path, _: u32) -> Result<()> {
    // Non-unix (Windows dev builds): rely on the default ACL. Triton
    // deployments are illumos/Linux; this is a dev-ergonomics fallback.
    Ok(())
}
