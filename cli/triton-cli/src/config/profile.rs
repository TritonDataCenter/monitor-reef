// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Profile management types
//!
//! A [`Profile`] is one of two kinds:
//!
//! * [`SshKeyProfile`] — authenticates directly to CloudAPI using HTTP
//!   Signature and an SSH key. This is the pre-existing shape every
//!   `~/.triton/profiles/*.json` file uses today.
//! * [`TritonApiProfile`] — authenticates against a `triton-gateway` via
//!   Bearer JWT (obtained with `triton login`). This is new in Phase 3 of the
//!   tritonapi rollout.
//!
//! On-disk format: the enum serializes with a `"auth"` tag
//! (`"ssh"` vs `"tritonapi"`). Existing SSH profile files predate the tag and
//! are loaded without it — [`Profile::deserialize`] falls back to the SSH
//! shape when the tag is missing, so no migration is required.

use serde::{Deserialize, Serialize};

/// A connection profile.
///
/// Tagged-enum on disk: `{"auth": "ssh", ...}` or
/// `{"auth": "tritonapi", ...}`. Legacy SSH profiles (no `auth` field) are
/// accepted transparently via the custom [`Deserialize`] impl below.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "auth", rename_all = "kebab-case")]
pub enum Profile {
    /// SSH-key / HTTP Signature profile (talks to cloudapi directly).
    #[serde(rename = "ssh")]
    SshKey(SshKeyProfile),
    /// Triton gateway profile (talks to triton-gateway with a Bearer JWT).
    #[serde(rename = "tritonapi")]
    TritonApi(TritonApiProfile),
}

/// SSH-key-backed profile talking directly to CloudAPI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SshKeyProfile {
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
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
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

/// Gateway-backed profile authenticating with a Bearer JWT.
///
/// `TritonApiProfile` intentionally has no SSH key, RBAC sub-user, roles, or
/// impersonation fields — those are CloudAPI / HTTP-Signature concepts that
/// do not apply to the JWT path. The actual access + refresh tokens live
/// outside the profile (in `~/.triton/tokens/<name>.json`, see the `auth`
/// module) so that rotating credentials does not rewrite the profile file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TritonApiProfile {
    /// Profile name (derived from filename, not present in JSON)
    #[serde(default)]
    pub name: String,

    /// Gateway base URL (e.g. `https://triton-api.example.com`)
    pub url: String,

    /// LDAP username used for `POST /v1/auth/login`.
    ///
    /// This is the login name, not an RBAC sub-user.
    pub account: String,

    /// Skip TLS certificate verification (self-signed gateways, dev DCs).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub insecure: bool,
}

// --- Custom Deserialize for backward-compat with untagged SSH profiles. ---
//
// Strategy: peek at the raw JSON value. If there is an `auth` field we use
// the tagged deserialization; otherwise we assume the legacy SSH shape.
// Using `serde_json::Value` lets us avoid the footgun of
// `#[serde(untagged)]` + `tag = "auth"` interactions (mutually exclusive).

impl<'de> Deserialize<'de> for Profile {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error as _;

        let value = serde_json::Value::deserialize(deserializer)?;

        let Some(obj) = value.as_object() else {
            return Err(D::Error::custom("profile must be a JSON object"));
        };

        let auth = obj.get("auth").and_then(|v| v.as_str()).map(str::to_string);
        match auth.as_deref() {
            None | Some("ssh") => {
                let inner: SshKeyProfile =
                    serde_json::from_value(value).map_err(D::Error::custom)?;
                Ok(Profile::SshKey(inner))
            }
            Some("tritonapi") => {
                let inner: TritonApiProfile =
                    serde_json::from_value(value).map_err(D::Error::custom)?;
                Ok(Profile::TritonApi(inner))
            }
            Some(other) => Err(D::Error::custom(format!(
                "unknown profile auth kind '{other}' (expected 'ssh' or 'tritonapi')"
            ))),
        }
    }
}

impl Profile {
    /// Construct a new SSH-key profile (convenience for the common case).
    #[allow(dead_code)] // used by cache tests; re-enabled fully once login wires it up
    pub fn new(name: String, url: String, account: String, key_id: String) -> Self {
        Profile::SshKey(SshKeyProfile {
            name,
            url,
            account,
            key_id,
            insecure: false,
            user: None,
            roles: None,
            act_as_account: None,
        })
    }

    /// Construct a new tritonapi profile.
    #[allow(dead_code)] // used by login wiring in upcoming commits
    pub fn new_tritonapi(name: String, url: String, account: String, insecure: bool) -> Self {
        Profile::TritonApi(TritonApiProfile {
            name,
            url,
            account,
            insecure,
        })
    }

    /// Common: profile name (filename stem).
    pub fn name(&self) -> &str {
        match self {
            Profile::SshKey(p) => &p.name,
            Profile::TritonApi(p) => &p.name,
        }
    }

    /// Overwrite the profile name (post-load, after filename derivation).
    pub fn set_name(&mut self, name: String) {
        match self {
            Profile::SshKey(p) => p.name = name,
            Profile::TritonApi(p) => p.name = name,
        }
    }

    /// Common: service base URL.
    pub fn url(&self) -> &str {
        match self {
            Profile::SshKey(p) => &p.url,
            Profile::TritonApi(p) => &p.url,
        }
    }

    /// Common: account/login name.
    pub fn account(&self) -> &str {
        match self {
            Profile::SshKey(p) => &p.account,
            Profile::TritonApi(p) => &p.account,
        }
    }

    /// Common: insecure-TLS flag.
    pub fn insecure(&self) -> bool {
        match self {
            Profile::SshKey(p) => p.insecure,
            Profile::TritonApi(p) => p.insecure,
        }
    }

    /// Short string identifying the auth kind ("ssh" or "tritonapi").
    pub fn auth_kind(&self) -> &'static str {
        match self {
            Profile::SshKey(_) => "ssh",
            Profile::TritonApi(_) => "tritonapi",
        }
    }

    /// Borrow the SSH variant, if this is an SSH profile.
    pub fn as_ssh_key(&self) -> Option<&SshKeyProfile> {
        match self {
            Profile::SshKey(p) => Some(p),
            _ => None,
        }
    }

    /// Mutably borrow the SSH variant, if this is an SSH profile.
    pub fn as_ssh_key_mut(&mut self) -> Option<&mut SshKeyProfile> {
        match self {
            Profile::SshKey(p) => Some(p),
            _ => None,
        }
    }

    /// Borrow the SSH variant or return a user-facing error.
    pub fn require_ssh_key(&self) -> anyhow::Result<&SshKeyProfile> {
        self.as_ssh_key().ok_or_else(|| {
            anyhow::anyhow!(
                "this command requires an SSH-key profile; current profile '{}' \
                 uses '{}' auth",
                self.name(),
                self.auth_kind(),
            )
        })
    }

    /// Borrow the tritonapi variant, if this is a tritonapi profile.
    pub fn as_triton_api(&self) -> Option<&TritonApiProfile> {
        match self {
            Profile::TritonApi(p) => Some(p),
            _ => None,
        }
    }

    /// Borrow the tritonapi variant or return a user-facing error.
    #[allow(dead_code)] // callers land in the login/logout/whoami commits
    pub fn require_triton_api(&self) -> anyhow::Result<&TritonApiProfile> {
        self.as_triton_api().ok_or_else(|| {
            anyhow::anyhow!(
                "this command requires a tritonapi profile; current profile '{}' \
                 uses '{}' auth",
                self.name(),
                self.auth_kind(),
            )
        })
    }

    /// Load a profile from disk.
    pub async fn load(name: &str) -> anyhow::Result<Self> {
        let path = super::paths::profile_path(name)?;
        let content = tokio::fs::read_to_string(&path)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to read profile '{}': {}", name, e))?;
        let mut profile: Profile = serde_json::from_str(&content)
            .map_err(|e| anyhow::anyhow!("Failed to parse profile '{}': {}", name, e))?;
        profile.set_name(name.to_string());
        Ok(profile)
    }

    /// Save the profile to disk.
    ///
    /// The `name` field is excluded from the JSON file because it is derived
    /// from the filename (e.g. `local.json` → `"local"`). node-triton forbids
    /// `name` in profile JSON files (see node-triton lib/config.js:331-336).
    pub async fn save(&self) -> anyhow::Result<()> {
        super::paths::ensure_config_dirs().await?;
        let path = super::paths::profile_path(self.name())?;
        let mut value = serde_json::to_value(self)?;
        if let Some(obj) = value.as_object_mut() {
            obj.remove("name");
        }
        let content = serde_json::to_string_pretty(&value)?;
        tokio::fs::write(&path, content).await?;
        Ok(())
    }

    /// Delete a profile file by name.
    pub async fn delete(name: &str) -> anyhow::Result<()> {
        let path = super::paths::profile_path(name)?;
        tokio::fs::remove_file(&path)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to delete profile '{}': {}", name, e))?;
        Ok(())
    }

    /// List all available profiles by name (sorted).
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

    /// Convert an SSH-key profile into a `triton_auth::AuthConfig`.
    ///
    /// Returns `None` for non-SSH variants (tritonapi profiles don't use
    /// HTTP Signature at all — callers should branch on [`Self::as_ssh_key`]).
    #[allow(dead_code)]
    pub fn to_auth_config(&self) -> Option<triton_auth::AuthConfig> {
        let ssh = self.as_ssh_key()?;
        let mut config = triton_auth::AuthConfig::new(
            ssh.account.clone(),
            triton_auth::KeySource::auto(&ssh.key_id),
        );

        if let Some(user) = &ssh.user {
            config = config.with_user(user.clone());
        }

        if let Some(roles) = &ssh.roles {
            config = config.with_roles(roles.clone());
        }

        Some(config)
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
    /// field and without an "auth" tag) can be deserialized as SSH-kind.
    ///
    /// node-triton forbids "name" in profile JSON files (see node-triton
    /// lib/config.js:331-336) — the name is derived from the filename
    /// (e.g. local.json -> "local").
    ///
    /// This is the round-trip guardrail called out in the Phase 3 plan:
    /// every existing `~/.triton/profiles/*.json` on disk predates the
    /// `auth` tag and must continue to load unchanged.
    #[test]
    fn test_deserialize_legacy_ssh_profile_without_auth_tag() {
        let json = r#"{
            "url": "https://127.0.0.1",
            "account": "user",
            "keyId": "00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00",
            "insecure": true
        }"#;

        let profile: Profile =
            serde_json::from_str(json).expect("Should parse legacy profile without 'auth' tag");

        let ssh = profile
            .as_ssh_key()
            .expect("Legacy profile must land in SshKey variant");
        assert_eq!(ssh.url, "https://127.0.0.1");
        assert_eq!(ssh.account, "user");
        assert_eq!(
            ssh.key_id,
            "00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00"
        );
        assert!(ssh.insecure);
    }

    /// Round-trip: deserialize → serialize → deserialize, for a legacy
    /// (no `auth` tag) SSH profile. The re-serialized form now carries
    /// `"auth": "ssh"` (that's fine and is what's written going forward),
    /// but all original fields must survive untouched.
    #[test]
    fn test_legacy_ssh_profile_round_trips_without_field_loss() {
        let original_json = r#"{
            "url": "https://cloudapi.example.com",
            "account": "alice",
            "keyId": "aa:bb:cc:dd:ee:ff:00:11:22:33:44:55:66:77:88:99",
            "insecure": false,
            "user": "alice-dev",
            "roles": ["ops", "dev"],
            "actAsAccount": "someone-else"
        }"#;

        // First load: no auth tag present, must land in SshKey.
        let profile: Profile = serde_json::from_str(original_json).expect("legacy SSH JSON parses");
        let ssh = profile.as_ssh_key().expect("legacy lands in SshKey");
        assert_eq!(ssh.account, "alice");
        assert_eq!(ssh.user.as_deref(), Some("alice-dev"));
        assert_eq!(
            ssh.roles.as_deref(),
            Some(&["ops".to_string(), "dev".to_string()][..])
        );
        assert_eq!(ssh.act_as_account.as_deref(), Some("someone-else"));

        // Re-serialize. The tag is present now; every other field must
        // round-trip byte-identically.
        let re_serialized = serde_json::to_string(&profile).expect("serialize");
        let value: serde_json::Value = serde_json::from_str(&re_serialized).unwrap();
        assert_eq!(value["auth"], "ssh");
        assert_eq!(value["account"], "alice");
        assert_eq!(
            value["keyId"],
            "aa:bb:cc:dd:ee:ff:00:11:22:33:44:55:66:77:88:99"
        );
        assert_eq!(value["user"], "alice-dev");
        assert_eq!(value["roles"], serde_json::json!(["ops", "dev"]));
        assert_eq!(value["actAsAccount"], "someone-else");

        // Second load: tagged form must still land in SshKey and preserve
        // all fields.
        let reloaded: Profile = serde_json::from_str(&re_serialized).expect("reloads");
        let reloaded_ssh = reloaded.as_ssh_key().expect("stays SshKey");
        assert_eq!(reloaded_ssh.url, ssh.url);
        assert_eq!(reloaded_ssh.account, ssh.account);
        assert_eq!(reloaded_ssh.key_id, ssh.key_id);
        assert_eq!(reloaded_ssh.insecure, ssh.insecure);
        assert_eq!(reloaded_ssh.user, ssh.user);
        assert_eq!(reloaded_ssh.roles, ssh.roles);
        assert_eq!(reloaded_ssh.act_as_account, ssh.act_as_account);
    }

    /// A profile tagged `"auth": "tritonapi"` deserializes into the
    /// TritonApi variant and has no SSH-key fields.
    #[test]
    fn test_tritonapi_profile_deserializes_from_tagged_json() {
        let json = r#"{
            "auth": "tritonapi",
            "url": "https://triton-api.example.com",
            "account": "admin",
            "insecure": true
        }"#;

        let profile: Profile = serde_json::from_str(json).expect("tritonapi parses");
        let api = profile
            .as_triton_api()
            .expect("tagged tritonapi lands in TritonApi variant");
        assert_eq!(api.url, "https://triton-api.example.com");
        assert_eq!(api.account, "admin");
        assert!(api.insecure);

        // And vice-versa: SSH-only accessor returns None + errors cleanly.
        assert!(profile.as_ssh_key().is_none());
        let err = profile.require_ssh_key().unwrap_err().to_string();
        assert!(
            err.contains("SSH-key profile") && err.contains("tritonapi"),
            "error should name the mismatch: {err}",
        );
    }

    /// Unknown `auth` values produce a clear error instead of silently
    /// falling back to SSH.
    #[test]
    fn test_unknown_auth_kind_rejected() {
        let json = r#"{"auth": "magic", "url": "u", "account": "a"}"#;
        let err = serde_json::from_str::<Profile>(json)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("unknown profile auth kind 'magic'"),
            "wanted explicit unknown-kind message, got: {err}",
        );
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
        assert_eq!(value["auth"], "ssh");

        // The save() path strips name for node-triton compatibility
        let mut disk_value = value;
        disk_value.as_object_mut().unwrap().remove("name");
        assert!(disk_value.get("name").is_none());
        // Other fields are preserved
        assert_eq!(disk_value["url"], "https://127.0.0.1");
        assert_eq!(disk_value["account"], "user");
    }
}
