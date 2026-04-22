// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `triton login` — exchange credentials for a tritonapi JWT.
//!
//! Two flows, sharing the same token-storage tail:
//!
//!   * **SSH-key login** (default): the profile's SSH key signs a
//!     request to `POST /v1/auth/login-ssh`. The server verifies the
//!     signature against the account's keys in mahi and issues a JWT
//!     pair.
//!   * **Password login** (`--user <login>` or a profile with no
//!     keyId): prompts for the LDAP password, then calls
//!     `POST /v1/auth/login` which binds against UFDS directly.
//!
//! Either path produces the same `LoginResponse`, which we stash at
//! `~/.triton/tokens/<profile>.json` (mode 0600, atomic write) so
//! subsequent commands can present the JWT as Bearer. The token file
//! lives outside the profile file intentionally -- older CLIs won't
//! trip over unfamiliar fields, and a future Keychain / libsecret
//! backend can replace the file storage without churning the profile
//! format.

use anyhow::{Context, Result, anyhow};
use clap::Args;
use serde::{Deserialize, Serialize};
use triton_gateway_client::TypedClient;
use triton_gateway_client::types::LoginResponse;

use crate::config::{Profile, paths};

#[derive(Args, Clone, Debug, Default)]
pub struct LoginArgs {
    /// Force password login for this login name. Without this flag,
    /// `triton login` uses the profile's SSH key to authenticate via
    /// `/v1/auth/login-ssh`. With it, prompts for the LDAP password
    /// and authenticates via `/v1/auth/login`. Useful when the profile
    /// has no keyId or the operator wants to exercise the password
    /// path explicitly.
    #[arg(short = 'u', long = "user")]
    pub user: Option<String>,
}

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

pub async fn run(
    args: LoginArgs,
    client: &TypedClient,
    profile: &Profile,
    use_json: bool,
) -> Result<()> {
    let login = match args.user {
        Some(username) => password_login(client, &username).await?,
        None => ssh_login(client).await?,
    };

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

async fn ssh_login(client: &TypedClient) -> Result<LoginResponse> {
    let response = client
        .inner()
        .auth_login_ssh()
        .send()
        .await
        .map_err(|e| anyhow!("SSH login failed: {e}"))?;
    Ok(response.into_inner())
}

async fn password_login(client: &TypedClient, username: &str) -> Result<LoginResponse> {
    // `TRITON_PASSWORD` is an undocumented escape hatch for non-tty
    // flows (integration tests, scripted operator runbooks). An
    // interactive prompt is the primary UX.
    let password = match std::env::var("TRITON_PASSWORD").ok() {
        Some(p) => p,
        None => rpassword::prompt_password(format!("Password for {username}: "))
            .map_err(|e| anyhow!("failed to read password: {e}"))?,
    };
    let body = triton_gateway_client::types::LoginRequest {
        username: username.to_string(),
        password,
    };
    let response = client
        .inner()
        .auth_login()
        .body(body)
        .send()
        .await
        .map_err(|e| anyhow!("password login failed: {e}"))?;
    Ok(response.into_inner())
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
