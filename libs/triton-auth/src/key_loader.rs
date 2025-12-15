// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! SSH key loading from files
//!
//! Supports loading SSH private keys from files in multiple formats:
//! - OpenSSH format (`-----BEGIN OPENSSH PRIVATE KEY-----`)
//! - PKCS#1 RSA (`-----BEGIN RSA PRIVATE KEY-----`)
//! - SEC1 ECDSA (`-----BEGIN EC PRIVATE KEY-----`)
//! - DSA (`-----BEGIN DSA PRIVATE KEY-----`)
//! - PKCS#8 (`-----BEGIN PRIVATE KEY-----`)

use crate::error::AuthError;
use crate::legacy_pem::{LegacyPrivateKey, PemKeyFormat};
use ssh_key::PrivateKey;
use std::path::{Path, PathBuf};

/// Source for loading SSH keys
#[derive(Clone, Debug)]
pub enum KeySource {
    /// Load key from SSH agent using fingerprint
    Agent {
        /// MD5 fingerprint in colon-separated hex format
        fingerprint: String,
    },
    /// Load key from file path
    File {
        /// Path to the private key file
        path: PathBuf,
        /// Passphrase for encrypted keys (None for unencrypted)
        passphrase: Option<String>,
    },
    /// Auto-detect: try agent first, then common file locations
    Auto {
        /// MD5 fingerprint in colon-separated hex format
        fingerprint: String,
    },
}

impl KeySource {
    /// Create a KeySource for loading from SSH agent
    pub fn agent(fingerprint: impl Into<String>) -> Self {
        Self::Agent {
            fingerprint: fingerprint.into(),
        }
    }

    /// Create a KeySource for loading from a file
    pub fn file(path: impl Into<PathBuf>) -> Self {
        Self::File {
            path: path.into(),
            passphrase: None,
        }
    }

    /// Create a KeySource for loading from a file with passphrase
    pub fn file_with_passphrase(path: impl Into<PathBuf>, passphrase: impl Into<String>) -> Self {
        Self::File {
            path: path.into(),
            passphrase: Some(passphrase.into()),
        }
    }

    /// Create a KeySource for auto-detection
    pub fn auto(fingerprint: impl Into<String>) -> Self {
        Self::Auto {
            fingerprint: fingerprint.into(),
        }
    }
}

/// Key loader for various sources
pub struct KeyLoader;

impl KeyLoader {
    /// Load a private key from the specified source
    ///
    /// For `KeySource::Agent`, this returns an error indicating that
    /// agent-based signing should be used instead of loading the key.
    /// Use `crate::agent::sign_with_agent` for signing operations.
    pub async fn load_private_key(source: &KeySource) -> Result<PrivateKey, AuthError> {
        match source {
            KeySource::File { path, passphrase } => Self::load_from_file(path, passphrase.as_ref()),
            KeySource::Agent { fingerprint } => {
                // For agent-based keys, we can't extract the private key
                // Return an error indicating agent signing is required
                Err(AuthError::ConfigError(format!(
                    "Key {} is in SSH agent; use agent signing instead of loading",
                    fingerprint
                )))
            }
            KeySource::Auto { fingerprint } => {
                // Try agent first via the agent module
                match crate::agent::find_key_in_agent(fingerprint).await {
                    Ok(_) => {
                        // Key found in agent, but we can't extract it
                        Err(AuthError::ConfigError(format!(
                            "Key {} found in SSH agent; use agent signing instead of loading",
                            fingerprint
                        )))
                    }
                    Err(_) => {
                        // Fall back to common file locations
                        Self::load_from_common_paths(fingerprint)
                    }
                }
            }
        }
    }

    /// Load a private key from a specific file path
    ///
    /// This method only returns `ssh_key::PrivateKey` for OpenSSH format keys.
    /// For traditional PEM formats, use `load_legacy_from_file` instead.
    pub fn load_from_file(
        path: &Path,
        passphrase: Option<&String>,
    ) -> Result<PrivateKey, AuthError> {
        let key_data = std::fs::read_to_string(path).map_err(|e| {
            AuthError::KeyLoadError(format!("Failed to read {}: {}", path.display(), e))
        })?;

        // Check if this is OpenSSH format
        let format = PemKeyFormat::detect(&key_data);
        if format != PemKeyFormat::OpenSsh {
            // For non-OpenSSH formats, try to load via legacy_pem and convert
            let legacy_key =
                LegacyPrivateKey::from_pem(&key_data, passphrase.map(|s| s.as_str()))?;

            // If it's already an OpenSSH key wrapped in LegacyPrivateKey, unwrap it
            if let LegacyPrivateKey::OpenSsh(key) = legacy_key {
                return Ok(key);
            }

            // For other formats, we can't convert to ssh_key::PrivateKey directly
            // Return an error suggesting to use the new API
            return Err(AuthError::KeyLoadError(format!(
                "Key {} is in {:?} format. Use load_legacy_from_file() for full format support.",
                path.display(),
                format
            )));
        }

        // OpenSSH format - use ssh-key crate directly
        if let Some(pass) = passphrase {
            PrivateKey::from_openssh(key_data.as_bytes())
                .map_err(|e| {
                    AuthError::KeyLoadError(format!("Failed to parse encrypted key: {}", e))
                })
                .and_then(|key| {
                    if key.is_encrypted() {
                        key.decrypt(pass.as_bytes()).map_err(|e| {
                            AuthError::KeyLoadError(format!("Failed to decrypt key: {}", e))
                        })
                    } else {
                        Ok(key)
                    }
                })
        } else {
            let key = PrivateKey::from_openssh(key_data.as_bytes())
                .map_err(|e| AuthError::KeyLoadError(format!("Failed to parse key: {}", e)))?;

            if key.is_encrypted() {
                Err(AuthError::KeyLoadError(format!(
                    "Key {} is encrypted but no passphrase provided",
                    path.display()
                )))
            } else {
                Ok(key)
            }
        }
    }

    /// Load a private key from a file, supporting all PEM formats
    ///
    /// This method supports:
    /// - OpenSSH format
    /// - PKCS#1 RSA
    /// - SEC1 ECDSA (P-256, P-384)
    /// - DSA
    /// - PKCS#8
    pub fn load_legacy_from_file(
        path: &Path,
        passphrase: Option<&str>,
    ) -> Result<LegacyPrivateKey, AuthError> {
        let key_data = std::fs::read_to_string(path).map_err(|e| {
            AuthError::KeyLoadError(format!("Failed to read {}: {}", path.display(), e))
        })?;

        LegacyPrivateKey::from_pem(&key_data, passphrase)
    }

    /// Try loading from common SSH key locations (~/.ssh/)
    fn load_from_common_paths(fingerprint: &str) -> Result<PrivateKey, AuthError> {
        let home = dirs::home_dir()
            .ok_or_else(|| AuthError::KeyLoadError("Could not determine home directory".into()))?;

        let ssh_dir = home.join(".ssh");
        let key_files = ["id_ed25519", "id_ecdsa", "id_rsa", "id_dsa"];

        for key_file in &key_files {
            let path = ssh_dir.join(key_file);
            if path.exists() {
                // Try to load without passphrase first
                if let Ok(key) = Self::load_from_file(&path, None) {
                    // Check if fingerprint matches
                    let key_fp = crate::fingerprint::md5_fingerprint(key.public_key());
                    if key_fp == fingerprint {
                        tracing::debug!(
                            "Found matching key at {} for fingerprint {}",
                            path.display(),
                            fingerprint
                        );
                        return Ok(key);
                    }
                }
            }
        }

        Err(AuthError::KeyNotFound(format!(
            "No key with fingerprint {} found in ~/.ssh/",
            fingerprint
        )))
    }

    /// List all available key files in ~/.ssh/
    pub fn list_key_files() -> Result<Vec<PathBuf>, AuthError> {
        let home = dirs::home_dir()
            .ok_or_else(|| AuthError::KeyLoadError("Could not determine home directory".into()))?;

        let ssh_dir = home.join(".ssh");
        if !ssh_dir.exists() {
            return Ok(Vec::new());
        }

        let mut keys = Vec::new();
        let key_patterns = ["id_ed25519", "id_ecdsa", "id_rsa", "id_dsa"];

        for pattern in &key_patterns {
            let path = ssh_dir.join(pattern);
            if path.exists() {
                keys.push(path);
            }
        }

        Ok(keys)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_key_source_constructors() {
        let agent = KeySource::agent("aa:bb:cc:dd");
        match agent {
            KeySource::Agent { fingerprint } => assert_eq!(fingerprint, "aa:bb:cc:dd"),
            _ => panic!("Wrong variant"),
        }

        let file = KeySource::file("/path/to/key");
        match file {
            KeySource::File { path, passphrase } => {
                assert_eq!(path, PathBuf::from("/path/to/key"));
                assert!(passphrase.is_none());
            }
            _ => panic!("Wrong variant"),
        }

        let file_pass = KeySource::file_with_passphrase("/path/to/key", "secret");
        match file_pass {
            KeySource::File { path, passphrase } => {
                assert_eq!(path, PathBuf::from("/path/to/key"));
                assert_eq!(passphrase, Some("secret".to_string()));
            }
            _ => panic!("Wrong variant"),
        }
    }
}
