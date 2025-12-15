// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Configuration management

pub mod paths;
pub mod profile;

pub use paths::{config_dir, config_file, ensure_config_dirs, profile_path, profiles_dir};
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

/// Resolve which profile to use
///
/// Priority:
/// 1. CLI --profile argument
/// 2. TRITON_PROFILE environment variable
/// 3. "env" if TRITON_URL/SDC_URL is set (use env vars directly)
/// 4. Current profile from config.json
pub fn resolve_profile(cli_profile: Option<&str>) -> Result<Profile> {
    // 1. CLI argument
    if let Some(name) = cli_profile {
        if name == "env" {
            return env_profile();
        }
        return Profile::load(name);
    }

    // 2. TRITON_PROFILE env var
    if let Ok(name) = std::env::var("TRITON_PROFILE") {
        if name == "env" {
            return env_profile();
        }
        return Profile::load(&name);
    }

    // 3. Check if env vars are set (implicit "env" profile)
    if std::env::var("TRITON_URL").is_ok() || std::env::var("SDC_URL").is_ok() {
        return env_profile();
    }

    // 4. Current profile from config
    let config = Config::load()?;
    if let Some(name) = config.current_profile() {
        return Profile::load(name);
    }

    Err(anyhow::anyhow!(
        "No profile configured. Use 'triton profile create' or set TRITON_* environment variables."
    ))
}
