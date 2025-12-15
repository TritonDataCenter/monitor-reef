// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Profile management types

use serde::{Deserialize, Serialize};

/// A connection profile
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    /// Profile name
    pub name: String,

    /// CloudAPI URL
    pub url: String,

    /// Account login name
    pub account: String,

    /// SSH key fingerprint (MD5 or SHA256 format)
    #[serde(rename = "keyId")]
    pub key_id: String,

    /// Skip TLS certificate verification
    #[serde(default)]
    pub insecure: bool,

    /// RBAC sub-user login (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,

    /// RBAC roles to assume (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub roles: Option<Vec<String>>,

    /// Impersonate another account (optional)
    #[serde(rename = "actAsAccount", skip_serializing_if = "Option::is_none")]
    pub act_as_account: Option<String>,
}

impl Profile {
    /// Create a new profile
    pub fn new(name: String, url: String, account: String, key_id: String) -> Self {
        Self {
            name,
            url,
            account,
            key_id,
            insecure: false,
            user: None,
            roles: None,
            act_as_account: None,
        }
    }

    /// Load a profile from a file
    pub fn load(name: &str) -> anyhow::Result<Self> {
        let path = super::paths::profile_path(name);
        let content = std::fs::read_to_string(&path)
            .map_err(|e| anyhow::anyhow!("Failed to read profile '{}': {}", name, e))?;
        let profile: Profile = serde_json::from_str(&content)
            .map_err(|e| anyhow::anyhow!("Failed to parse profile '{}': {}", name, e))?;
        Ok(profile)
    }

    /// Save the profile to a file
    pub fn save(&self) -> anyhow::Result<()> {
        super::paths::ensure_config_dirs()?;
        let path = super::paths::profile_path(&self.name);
        let content = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, content)?;
        Ok(())
    }

    /// Delete the profile file
    pub fn delete(name: &str) -> anyhow::Result<()> {
        let path = super::paths::profile_path(name);
        std::fs::remove_file(&path)
            .map_err(|e| anyhow::anyhow!("Failed to delete profile '{}': {}", name, e))?;
        Ok(())
    }

    /// List all available profiles
    pub fn list_all() -> anyhow::Result<Vec<String>> {
        let profiles_dir = super::paths::profiles_dir();
        if !profiles_dir.exists() {
            return Ok(vec![]);
        }

        let mut profiles = vec![];
        for entry in std::fs::read_dir(&profiles_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "json")
                && let Some(stem) = path.file_stem()
            {
                profiles.push(stem.to_string_lossy().to_string());
            }
        }
        profiles.sort();
        Ok(profiles)
    }

    /// Convert to AuthConfig for triton-auth
    #[allow(dead_code)]
    pub fn to_auth_config(&self) -> triton_auth::AuthConfig {
        let mut config = triton_auth::AuthConfig::new(
            self.account.clone(),
            self.key_id.clone(),
            triton_auth::KeySource::auto(&self.key_id),
        );

        if let Some(user) = &self.user {
            config = config.with_user(user.clone());
        }

        if let Some(roles) = &self.roles {
            config = config.with_roles(roles.clone());
        }

        config
    }
}

/// Main configuration file
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    /// Current active profile name
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,

    /// Previous profile (for `triton profile set -`)
    #[serde(rename = "oldProfile", skip_serializing_if = "Option::is_none")]
    pub old_profile: Option<String>,
}

impl Config {
    /// Load the main config file
    pub fn load() -> anyhow::Result<Self> {
        let path = super::paths::config_file();
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = std::fs::read_to_string(&path)?;
        let config: Config = serde_json::from_str(&content)?;
        Ok(config)
    }

    /// Save the main config file
    pub fn save(&self) -> anyhow::Result<()> {
        super::paths::ensure_config_dirs()?;
        let path = super::paths::config_file();
        let content = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, content)?;
        Ok(())
    }

    /// Get the current profile name
    pub fn current_profile(&self) -> Option<&str> {
        self.profile.as_deref()
    }

    /// Set the current profile
    pub fn set_current_profile(&mut self, name: &str) {
        self.old_profile = self.profile.take();
        self.profile = Some(name.to_string());
    }
}
