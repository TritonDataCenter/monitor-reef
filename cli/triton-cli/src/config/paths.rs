// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Configuration path resolution
//!
//! Matches node-triton's config directory conventions (lib/constants.js).

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
        .unwrap_or_else(|| PathBuf::from("."))
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
                eprintln!(
                    "Warning: profiles also found in {}, but using {}",
                    alt.display(),
                    active.display()
                );
            }
        }
    }
}

/// Get the cache directory for a specific profile
pub fn cache_dir(profile_slug: &str) -> PathBuf {
    config_dir().join("cache").join(profile_slug)
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
pub fn profile_path(name: &str) -> PathBuf {
    profiles_dir().join(format!("{}.json", name))
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
        let path = profile_path("default");
        assert!(path.ends_with("profiles.d/default.json"));
    }
}
