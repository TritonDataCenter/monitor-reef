// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Profile management types

use serde::{Deserialize, Serialize};

/// A connection profile
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    /// Profile name (derived from filename, not present in JSON)
    #[serde(default)]
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
    pub async fn load(name: &str) -> anyhow::Result<Self> {
        let path = super::paths::profile_path(name);
        let content = tokio::fs::read_to_string(&path)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to read profile '{}': {}", name, e))?;
        let mut profile: Profile = serde_json::from_str(&content)
            .map_err(|e| anyhow::anyhow!("Failed to parse profile '{}': {}", name, e))?;
        profile.name = name.to_string();
        Ok(profile)
    }

    /// Save the profile to a file
    ///
    /// The `name` field is excluded from the JSON file because it is derived
    /// from the filename (e.g. `local.json` → `"local"`). node-triton forbids
    /// `name` in profile JSON files (see node-triton lib/config.js:331-336).
    pub async fn save(&self) -> anyhow::Result<()> {
        super::paths::ensure_config_dirs().await?;
        let path = super::paths::profile_path(&self.name);
        let mut value = serde_json::to_value(self)?;
        if let Some(obj) = value.as_object_mut() {
            obj.remove("name");
        }
        let content = serde_json::to_string_pretty(&value)?;
        tokio::fs::write(&path, content).await?;
        Ok(())
    }

    /// Delete the profile file
    pub async fn delete(name: &str) -> anyhow::Result<()> {
        let path = super::paths::profile_path(name);
        tokio::fs::remove_file(&path)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to delete profile '{}': {}", name, e))?;
        Ok(())
    }

    /// List all available profiles
    pub async fn list_all() -> anyhow::Result<Vec<String>> {
        let profiles_dir = super::paths::profiles_dir();
        if !tokio::fs::try_exists(&profiles_dir).await.unwrap_or(false) {
            return Ok(vec![]);
        }

        let mut profiles = vec![];
        let mut entries = tokio::fs::read_dir(&profiles_dir).await?;
        while let Some(entry) = entries.next_entry().await? {
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
    pub async fn load() -> anyhow::Result<Self> {
        let path = super::paths::config_file();
        if !tokio::fs::try_exists(&path).await.unwrap_or(false) {
            return Ok(Self::default());
        }
        let content = tokio::fs::read_to_string(&path).await?;
        let config: Config = serde_json::from_str(&content)?;
        Ok(config)
    }

    /// Save the main config file
    pub async fn save(&self) -> anyhow::Result<()> {
        super::paths::ensure_config_dirs().await?;
        let path = super::paths::config_file();
        let content = serde_json::to_string_pretty(self)?;
        tokio::fs::write(&path, content).await?;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Test that a node-triton-compatible profile file (without a "name"
    /// field) can be deserialized.
    ///
    /// node-triton forbids "name" in profile JSON files (see node-triton
    /// lib/config.js:331-336) — the name is derived from the filename
    /// (e.g. local.json -> "local").
    #[test]
    fn test_deserialize_profile_without_name_field() {
        let json = r#"{
            "url": "https://127.0.0.1",
            "account": "user",
            "keyId": "00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00",
            "insecure": true
        }"#;

        let profile: Profile =
            serde_json::from_str(json).expect("Should parse profile without 'name' field");

        assert_eq!(profile.url, "https://127.0.0.1");
        assert_eq!(profile.account, "user");
        assert_eq!(
            profile.key_id,
            "00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00"
        );
        assert!(profile.insecure);
    }

    /// Test that serialized profile output includes `name` (needed for CLI
    /// `-j` JSON output) but that the save-to-disk path would strip it.
    #[test]
    fn test_serialize_profile_includes_name_for_cli_output() {
        let profile = Profile::new(
            "test".to_string(),
            "https://127.0.0.1".to_string(),
            "user".to_string(),
            "00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00".to_string(),
        );

        // Normal serialization includes name (used by `print_json`)
        let value = serde_json::to_value(&profile).expect("should serialize");
        assert_eq!(value["name"], "test");

        // The save() path strips name for node-triton compatibility
        let mut disk_value = value;
        disk_value.as_object_mut().unwrap().remove("name");
        assert!(disk_value.get("name").is_none());
        // Other fields are preserved
        assert_eq!(disk_value["url"], "https://127.0.0.1");
        assert_eq!(disk_value["account"], "user");
    }
}
