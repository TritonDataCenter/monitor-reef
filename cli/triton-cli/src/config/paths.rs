// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Configuration path resolution
//!
//! Matches node-triton's config directory conventions (lib/constants.js).

use anyhow::{Result, bail};
use std::path::PathBuf;

/// Get the triton configuration directory
///
/// Matches node-triton lib/constants.js behavior:
/// 1. TRITON_CONFIG_DIR environment variable (also used by tests)
/// 2. $XDG_CONFIG_HOME/triton (if XDG_CONFIG_HOME is set)
/// 3. ~/.triton (default)
pub fn config_dir() -> PathBuf {
    // Check environment variable first
    if let Ok(dir) = std::env::var("TRITON_CONFIG_DIR") {
        return PathBuf::from(dir);
    }

    // Honor XDG_CONFIG_HOME if explicitly set (matches node-triton)
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        return PathBuf::from(xdg).join("triton");
    }

    // Default: ~/.triton (matches node-triton)
    dirs::home_dir()
        .unwrap_or_else(|| {
            tracing::warn!(
                "could not determine home directory, \
                 using current directory for config"
            );
            PathBuf::from(".")
        })
        .join(".triton")
}

/// Check for profiles in alternative config directories and warn on stderr.
///
/// Call once during startup to alert users who may have profiles in a
/// directory that isn't being used.
pub async fn warn_alternative_config_dirs() {
    let active = config_dir();

    // Only check alternatives when TRITON_CONFIG_DIR isn't set (explicit override)
    if std::env::var("TRITON_CONFIG_DIR").is_ok() {
        return;
    }

    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return,
    };

    let mut alternatives: Vec<PathBuf> = Vec::new();

    // Always check ~/.triton as a potential alternative
    let dot_triton = home.join(".triton");
    if dot_triton != active {
        alternatives.push(dot_triton);
    }

    // Check $XDG_CONFIG_HOME/triton if XDG is set
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        let xdg_triton = PathBuf::from(xdg).join("triton");
        if xdg_triton != active {
            alternatives.push(xdg_triton);
        }
    }

    for alt in alternatives {
        let alt_profiles = alt.join("profiles.d");
        if let Ok(mut entries) = tokio::fs::read_dir(&alt_profiles).await {
            let mut has_profiles = false;
            while let Ok(Some(entry)) = entries.next_entry().await {
                if entry.path().extension().is_some_and(|ext| ext == "json") {
                    has_profiles = true;
                    break;
                }
            }
            if has_profiles {
                tracing::warn!(
                    "profiles also found in {}, but using {}",
                    alt.display(),
                    active.display()
                );
            }
        }
    }
}

/// Validate that a profile name is safe for use in filesystem paths.
///
/// Rejects names that could escape the config directory via path traversal
/// or cause other filesystem issues. Allowed characters: alphanumeric,
/// hyphens, underscores, and dots (but not a leading dot).
pub fn validate_profile_name(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("Profile name must not be empty");
    }

    if name.starts_with('.') {
        bail!("Profile name must not start with a dot: '{}'", name);
    }

    if name.contains("..") {
        bail!("Profile name must not contain '..': '{}'", name);
    }

    for ch in name.chars() {
        match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' | '@' => {}
            '/' | '\\' | '\0' => {
                bail!(
                    "Profile name contains forbidden character {:?}: '{}'",
                    ch,
                    name
                );
            }
            _ => {
                bail!(
                    "Profile name contains invalid character {:?}: '{}'. \
                     Only alphanumeric, hyphens, underscores, dots, and @ are allowed.",
                    ch,
                    name
                );
            }
        }
    }

    Ok(())
}

/// Get the cache directory for a specific profile
pub fn cache_dir(profile_slug: &str) -> Result<PathBuf> {
    validate_profile_name(profile_slug)?;
    Ok(config_dir().join("cache").join(profile_slug))
}

/// Get the profiles directory
pub fn profiles_dir() -> PathBuf {
    config_dir().join("profiles.d")
}

/// Get the path to the main config file
pub fn config_file() -> PathBuf {
    config_dir().join("config.json")
}

/// Get the path to a specific profile
pub fn profile_path(name: &str) -> Result<PathBuf> {
    validate_profile_name(name)?;
    Ok(profiles_dir().join(format!("{}.json", name)))
}

/// Get the tokens directory. Used by `triton login` to stash the
/// JWT issued by `/v1/auth/login-ssh` outside the profile file so
/// older CLIs don't trip over unknown profile fields and so future
/// storage backends (Keychain, libsecret) can slot in without
/// churning the profile format.
pub fn tokens_dir() -> PathBuf {
    config_dir().join("tokens")
}

/// Get the path to the cached token for a specific profile.
pub fn token_path(profile_name: &str) -> Result<PathBuf> {
    validate_profile_name(profile_name)?;
    Ok(tokens_dir().join(format!("{}.json", profile_name)))
}

/// Ensure config directories exist
pub async fn ensure_config_dirs() -> std::io::Result<()> {
    let config = config_dir();
    let profiles = profiles_dir();

    tokio::fs::create_dir_all(&config).await?;
    tokio::fs::create_dir_all(&profiles).await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_profile_path() {
        let path = profile_path("default").unwrap();
        assert!(path.ends_with("profiles.d/default.json"));
    }

    #[test]
    fn test_profile_path_rejects_traversal() {
        assert!(profile_path("../etc/passwd").is_err());
        assert!(profile_path("..").is_err());
        assert!(profile_path("foo/../bar").is_err());
    }

    #[test]
    fn test_profile_path_rejects_path_separators() {
        assert!(profile_path("foo/bar").is_err());
        assert!(profile_path("foo\\bar").is_err());
    }

    #[test]
    fn test_profile_path_rejects_hidden_files() {
        assert!(profile_path(".hidden").is_err());
    }

    #[test]
    fn test_profile_path_rejects_null_bytes() {
        assert!(profile_path("foo\0bar").is_err());
    }

    #[test]
    fn test_profile_path_rejects_empty() {
        assert!(profile_path("").is_err());
    }

    #[test]
    fn test_profile_path_allows_valid_names() {
        assert!(profile_path("default").is_ok());
        assert!(profile_path("my-profile").is_ok());
        assert!(profile_path("test_profile").is_ok());
        assert!(profile_path("profile.v2").is_ok());
        assert!(profile_path("MyProfile123").is_ok());
    }

    #[test]
    fn test_cache_dir_allows_at_in_slug() {
        assert!(cache_dir("myaccount@us-central-1.api.example.com").is_ok());
    }
}
