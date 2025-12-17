// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Test configuration management
//!
//! Loads test configuration from:
//! 1. `TRITON_TEST_CONFIG` environment variable (path to config file)
//! 2. `tests/config.json` file
//!
//! Configuration supports:
//! - `profile_name`: Name of profile to use from ~/.triton/profiles.d/
//! - `profile`: Inline profile with url, account, keyId
//! - `allow_write_actions`: Allow tests that create/modify resources
//! - `allow_image_create`: Allow tests that create images
//! - `allow_volumes_tests`: Allow volume-related tests
//! - `skip_affinity_tests`: Skip affinity-related tests
//! - `skip_kvm_tests`: Skip KVM-specific tests
//! - `skip_flex_disk_tests`: Skip flexible disk tests

use serde::Deserialize;
use std::path::PathBuf;
use std::sync::OnceLock;

/// Test configuration
#[derive(Debug, Clone, Deserialize)]
pub struct TestConfig {
    /// Profile name to load from ~/.triton/profiles.d/
    #[serde(rename = "profileName")]
    pub profile_name: Option<String>,

    /// Inline profile configuration
    pub profile: Option<ProfileConfig>,

    /// Allow tests that create/modify resources (default: false)
    #[serde(rename = "allowWriteActions", default)]
    pub allow_write_actions: bool,

    /// Allow tests that create images (default: false)
    #[serde(rename = "allowImageCreate", default)]
    pub allow_image_create: bool,

    /// Allow volume-related tests (default: true)
    #[serde(rename = "allowVolumesTests", default = "default_true")]
    pub allow_volumes_tests: bool,

    /// Skip affinity-related tests (default: false)
    #[serde(rename = "skipAffinityTests", default)]
    pub skip_affinity_tests: bool,

    /// Skip KVM-specific tests (default: false)
    #[serde(rename = "skipKvmTests", default)]
    pub skip_kvm_tests: bool,

    /// Skip flexible disk tests (default: false)
    #[serde(rename = "skipFlexDiskTests", default)]
    pub skip_flex_disk_tests: bool,

    /// Specific image to use for tests (optional)
    pub image: Option<String>,

    /// Specific package to use for tests (optional)
    pub package: Option<String>,

    /// KVM image for KVM tests (optional)
    #[serde(rename = "kvmImage")]
    pub kvm_image: Option<String>,

    /// KVM package for KVM tests (optional)
    #[serde(rename = "kvmPackage")]
    pub kvm_package: Option<String>,

    /// Bhyve image for bhyve tests (optional)
    #[serde(rename = "bhyveImage")]
    pub bhyve_image: Option<String>,

    /// Bhyve package for bhyve tests (optional)
    #[serde(rename = "bhyvePackage")]
    pub bhyve_package: Option<String>,

    /// Flexible disk package (optional)
    #[serde(rename = "flexPackage")]
    pub flex_package: Option<String>,

    /// Resize package for resize tests (optional)
    #[serde(rename = "resizePackage")]
    pub resize_package: Option<String>,
}

fn default_true() -> bool {
    true
}

/// Inline profile configuration
#[derive(Debug, Clone, Deserialize)]
pub struct ProfileConfig {
    /// CloudAPI URL
    pub url: String,

    /// Account name
    pub account: String,

    /// SSH key ID (fingerprint)
    #[serde(rename = "keyId")]
    pub key_id: String,

    /// Allow insecure TLS (default: false)
    #[serde(default)]
    pub insecure: bool,
}

impl Default for TestConfig {
    fn default() -> Self {
        Self {
            profile_name: None,
            profile: None,
            allow_write_actions: false,
            allow_image_create: false,
            allow_volumes_tests: true,
            skip_affinity_tests: false,
            skip_kvm_tests: false,
            skip_flex_disk_tests: false,
            image: None,
            package: None,
            kvm_image: None,
            kvm_package: None,
            bhyve_image: None,
            bhyve_package: None,
            flex_package: None,
            resize_package: None,
        }
    }
}

/// Global test configuration (lazily initialized)
static TEST_CONFIG: OnceLock<Option<TestConfig>> = OnceLock::new();

/// Load test configuration
///
/// Returns None if no configuration is available (for unit tests that don't need API access)
pub fn load_config() -> Option<&'static TestConfig> {
    TEST_CONFIG
        .get_or_init(|| {
            // Check TRITON_TEST_CONFIG env var first
            let config_path = std::env::var("TRITON_TEST_CONFIG")
                .or_else(|_| std::env::var("TEST_CONFIG"))
                .map(PathBuf::from)
                .ok()
                .or_else(|| {
                    // Look for tests/config.json relative to manifest dir
                    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                        .join("tests")
                        .join("config.json");
                    if path.exists() { Some(path) } else { None }
                });

            config_path.and_then(|path| {
                let content = std::fs::read_to_string(&path).ok()?;
                let config: TestConfig = serde_json::from_str(&content).ok()?;
                Some(config)
            })
        })
        .as_ref()
}

/// Check if integration tests should run
///
/// Returns true if a valid test configuration exists
pub fn has_integration_config() -> bool {
    load_config().is_some()
}

/// Get environment variables for running triton with test profile
pub fn get_profile_env() -> Vec<(&'static str, String)> {
    let config = match load_config() {
        Some(c) => c,
        None => return vec![],
    };

    if let Some(profile) = &config.profile {
        vec![
            ("TRITON_PROFILE", "env".to_string()),
            ("TRITON_URL", profile.url.clone()),
            ("TRITON_ACCOUNT", profile.account.clone()),
            ("TRITON_KEY_ID", profile.key_id.clone()),
            ("TRITON_TLS_INSECURE", profile.insecure.to_string()),
        ]
    } else if let Some(profile_name) = &config.profile_name {
        vec![("TRITON_PROFILE", profile_name.clone())]
    } else {
        vec![]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = TestConfig::default();
        assert!(!config.allow_write_actions);
        assert!(!config.allow_image_create);
        assert!(config.allow_volumes_tests);
    }

    #[test]
    fn test_config_deserialization() {
        let json = r#"{
            "profileName": "test-profile",
            "allowWriteActions": true,
            "skipKvmTests": true
        }"#;

        let config: TestConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.profile_name, Some("test-profile".to_string()));
        assert!(config.allow_write_actions);
        assert!(config.skip_kvm_tests);
        assert!(config.allow_volumes_tests); // default true
    }

    #[test]
    fn test_inline_profile_deserialization() {
        let json = r#"{
            "profile": {
                "url": "https://cloudapi.example.com",
                "account": "testuser",
                "keyId": "SHA256:abc123",
                "insecure": true
            }
        }"#;

        let config: TestConfig = serde_json::from_str(json).unwrap();
        let profile = config.profile.unwrap();
        assert_eq!(profile.url, "https://cloudapi.example.com");
        assert_eq!(profile.account, "testuser");
        assert!(profile.insecure);
    }
}
