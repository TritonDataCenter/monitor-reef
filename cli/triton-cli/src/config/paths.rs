// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Configuration path resolution
//!
//! Supports both legacy ~/.triton/ and XDG ~/.config/triton/ paths.

use std::path::PathBuf;

/// Get the triton configuration directory
///
/// Priority:
/// 1. TRITON_CONFIG_DIR environment variable
/// 2. ~/.triton/ if it exists (migration support)
/// 3. XDG config dir (~/.config/triton/ on Linux/Mac)
pub fn config_dir() -> PathBuf {
    // Check environment variable first
    if let Ok(dir) = std::env::var("TRITON_CONFIG_DIR") {
        return PathBuf::from(dir);
    }

    // Check for existing ~/.triton directory (migration support)
    if let Some(home) = dirs::home_dir() {
        let legacy_dir = home.join(".triton");
        if legacy_dir.exists() {
            return legacy_dir;
        }
    }

    // Default to XDG for new installations
    directories::ProjectDirs::from("com", "tritondatacenter", "triton")
        .map(|dirs| dirs.config_dir().to_path_buf())
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".triton")
        })
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
pub fn ensure_config_dirs() -> std::io::Result<()> {
    let config = config_dir();
    let profiles = profiles_dir();

    std::fs::create_dir_all(&config)?;
    std::fs::create_dir_all(&profiles)?;

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
