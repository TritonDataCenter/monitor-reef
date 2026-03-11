// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Configuration management

pub mod paths;
pub mod profile;

pub use profile::{Config, Profile};

use anyhow::Result;

/// Build an "env" profile from environment variables
///
/// Reference: node-triton lib/config.js:275-317
pub fn env_profile() -> Result<Profile> {
    let url = std::env::var("TRITON_URL")
        .or_else(|_| std::env::var("SDC_URL"))
        .map_err(|_| anyhow::anyhow!("TRITON_URL or SDC_URL must be set"))?;

    let account = std::env::var("TRITON_ACCOUNT")
        .or_else(|_| std::env::var("SDC_ACCOUNT"))
        .map_err(|_| anyhow::anyhow!("TRITON_ACCOUNT or SDC_ACCOUNT must be set"))?;

    let key_id = std::env::var("TRITON_KEY_ID")
        .or_else(|_| std::env::var("SDC_KEY_ID"))
        .map_err(|_| anyhow::anyhow!("TRITON_KEY_ID or SDC_KEY_ID must be set"))?;

    let mut profile = Profile::new("env".to_string(), url, account, key_id);

    // Optional settings
    if let Ok(user) = std::env::var("TRITON_USER").or_else(|_| std::env::var("SDC_USER")) {
        profile.user = Some(user);
    }

    if let Ok(insecure) =
        std::env::var("TRITON_TLS_INSECURE").or_else(|_| std::env::var("SDC_TLS_INSECURE"))
    {
        profile.insecure = insecure == "1" || insecure.to_lowercase() == "true";
    }

    Ok(profile)
}

/// Check if all required environment variables for an env profile are available.
///
/// Returns true only when all three required variables (URL, account, key ID)
/// are present. This prevents partial env vars from triggering the env profile
/// path, which would produce confusing errors about missing variables. Partial
/// env vars instead fall through to the saved-profile path where they can act
/// as overrides.
fn env_profile_available() -> bool {
    let has_url = std::env::var("TRITON_URL").is_ok() || std::env::var("SDC_URL").is_ok();
    let has_account =
        std::env::var("TRITON_ACCOUNT").is_ok() || std::env::var("SDC_ACCOUNT").is_ok();
    let has_key_id = std::env::var("TRITON_KEY_ID").is_ok() || std::env::var("SDC_KEY_ID").is_ok();
    has_url && has_account && has_key_id
}

/// Resolve which profile to use
///
/// This is the single source of truth for profile resolution. Both the
/// `Cli::build_client()` method (for API commands) and standalone commands
/// (`env`, `profile docker-setup`, etc.) use this function.
///
/// Priority:
/// 1. CLI --profile argument
/// 2. TRITON_PROFILE environment variable
/// 3. Current profile from config.json
/// 4. "env" if all required env vars are set (TRITON_URL/SDC_URL,
///    TRITON_ACCOUNT/SDC_ACCOUNT, TRITON_KEY_ID/SDC_KEY_ID)
pub async fn resolve_profile(cli_profile: Option<&str>) -> Result<Profile> {
    // 1. CLI argument
    if let Some(name) = cli_profile {
        if name == "env" {
            return env_profile();
        }
        return Profile::load(name).await;
    }

    // 2. TRITON_PROFILE env var
    if let Ok(name) = std::env::var("TRITON_PROFILE") {
        if name == "env" {
            return env_profile();
        }
        return Profile::load(&name).await;
    }

    // 3. Current profile from config
    let config = Config::load().await?;
    if let Some(name) = config.current_profile() {
        if name == "env" {
            return env_profile();
        }
        return Profile::load(name).await;
    }

    // 4. Check if all required env vars are set (implicit "env" profile)
    if env_profile_available() {
        return env_profile();
    }

    Err(anyhow::anyhow!(
        "No profile configured. Use 'triton profile create' or set TRITON_URL, \
         TRITON_ACCOUNT, and TRITON_KEY_ID environment variables."
    ))
}
