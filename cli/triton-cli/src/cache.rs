// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Image cache for avoiding redundant API calls
//!
//! Matches node-triton's caching behavior (lib/tritonapi.js):
//! - Single `images.json` file per profile
//! - Validated by file mtime + TTL
//! - Cache failures are always silent (return None, never error)

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use triton_gateway_client::types::Image;

use crate::config::profile::Profile;

/// TTL for image list lookups (name resolution, instance list)
const LIST_TTL: Duration = Duration::from_secs(5 * 60);

/// TTL for single image lookups (ssh default_user)
const GET_TTL: Duration = Duration::from_secs(60 * 60);

pub struct ImageCache {
    cache_path: PathBuf,
}

impl ImageCache {
    /// Create a new ImageCache for the given profile.
    ///
    /// Creates the cache directory if needed. Returns None if the directory
    /// cannot be created or if emit-payload mode is active (so every
    /// request is visible in the captured output).
    pub async fn new(profile: &Profile) -> Option<Self> {
        #[cfg(debug_assertions)]
        if triton_gateway_client::is_emit_payload_mode() {
            return None;
        }
        let slug = profile_slug(profile);
        let dir = crate::config::paths::cache_dir(&slug).ok()?;
        tokio::fs::create_dir_all(&dir).await.ok()?;
        Some(Self {
            cache_path: dir.join("images.json"),
        })
    }

    /// Load the cached image list if it exists and is fresh enough.
    pub async fn load_list(&self) -> Option<Vec<Image>> {
        self.load_if_fresh(LIST_TTL).await
    }

    /// Save the image list to the cache file (best-effort).
    pub async fn save_list(&self, images: &[Image]) {
        let Some(json) = serde_json::to_string(images)
            .inspect_err(|e| tracing::debug!("Failed to serialize image cache: {}", e))
            .ok()
        else {
            return;
        };
        tokio::fs::write(&self.cache_path, json)
            .await
            .inspect_err(|e| tracing::debug!("Failed to write image cache: {}", e))
            .ok();
    }

    /// Look up a single image by UUID from the cache (uses longer TTL).
    pub async fn get_image(&self, id: uuid::Uuid) -> Option<Image> {
        let images = self.load_if_fresh(GET_TTL).await?;
        images.into_iter().find(|img| img.id == id)
    }

    async fn load_if_fresh(&self, ttl: Duration) -> Option<Vec<Image>> {
        let meta = tokio::fs::metadata(&self.cache_path).await.ok()?;
        let modified = meta.modified().ok()?;
        if SystemTime::now().duration_since(modified).unwrap_or(ttl) >= ttl {
            return None;
        }
        let data = tokio::fs::read_to_string(&self.cache_path).await.ok()?;
        match serde_json::from_str(&data) {
            Ok(images) => Some(images),
            Err(e) => {
                tracing::debug!("Corrupt image cache, removing: {}", e);
                tokio::fs::remove_file(&self.cache_path)
                    .await
                    .inspect_err(|e| tracing::debug!("Failed to remove corrupt image cache: {}", e))
                    .ok();
                None
            }
        }
    }
}

/// Build a profile slug matching node-triton's `lib/common.js:profileSlug`.
///
/// Format: `{account}@{url_without_protocol}` with special chars replaced by `_`.
fn profile_slug(profile: &Profile) -> String {
    let url_part = profile
        .url
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    let raw = format!("{}@{}", profile.account, url_part);
    raw.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '@' || c == '.' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_profile_slug() {
        let profile = Profile::new(
            "test".into(),
            "https://us-central-1.api.example.com".into(),
            "myaccount".into(),
            "ff:ff:ff:ff:ff:ff:ff:ff:ff:ff:ff:ff:ff:ff:ff:ff".into(),
        );
        assert_eq!(
            profile_slug(&profile),
            "myaccount@us-central-1.api.example.com"
        );
    }

    #[test]
    fn test_profile_slug_with_port() {
        let profile = Profile::new(
            "test".into(),
            "https://localhost:8443".into(),
            "admin".into(),
            "ff:ff:ff:ff:ff:ff:ff:ff:ff:ff:ff:ff:ff:ff:ff:ff".into(),
        );
        // The colon in the port becomes underscore
        assert_eq!(profile_slug(&profile), "admin@localhost_8443");
    }

    #[tokio::test]
    async fn test_cache_load_missing_file() {
        let cache = ImageCache {
            cache_path: PathBuf::from("/nonexistent/path/images.json"),
        };
        assert!(cache.load_list().await.is_none());
    }

    #[tokio::test]
    async fn test_cache_save_and_load() {
        let dir = std::env::temp_dir().join(format!("triton-cache-test-{}", std::process::id()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let cache = ImageCache {
            cache_path: dir.join("images.json"),
        };

        // Save empty list
        cache.save_list(&[]).await;
        let loaded = cache.load_list().await;
        assert!(loaded.is_some());
        assert!(loaded.unwrap().is_empty());

        // Cleanup
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn test_cache_corrupt_json_deleted() {
        let dir =
            std::env::temp_dir().join(format!("triton-cache-corrupt-test-{}", std::process::id()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let path = dir.join("images.json");
        tokio::fs::write(&path, "not valid json{{{").await.unwrap();

        let cache = ImageCache {
            cache_path: path.clone(),
        };
        assert!(cache.load_list().await.is_none());
        // File should have been deleted
        assert!(!path.exists());

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
