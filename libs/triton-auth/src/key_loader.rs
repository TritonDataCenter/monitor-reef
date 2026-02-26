// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! SSH key loading from files
//!
//! Supports loading SSH private keys from files in multiple formats:
//! - OpenSSH format (`-----BEGIN OPENSSH PRIVATE KEY-----`)
//! - PKCS#1 RSA (`-----BEGIN RSA PRIVATE KEY-----`)
//! - SEC1 ECDSA (`-----BEGIN EC PRIVATE KEY-----`)
//! - DSA (`-----BEGIN DSA PRIVATE KEY-----`)
//! - PKCS#8 (`-----BEGIN PRIVATE KEY-----`)

use crate::error::AuthError;
use crate::fingerprint::Fingerprint;
use crate::legacy_pem::{LegacyPrivateKey, PemKeyFormat};
use ssh_key::PrivateKey;
use std::fmt;
use std::path::{Path, PathBuf};

/// Source for loading SSH keys
#[derive(Clone)]
pub enum KeySource {
    /// Load key from SSH agent using fingerprint
    Agent {
        /// Fingerprint in MD5 or SHA256 format
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
        /// Fingerprint in MD5 or SHA256 format
        fingerprint: String,
    },
}

impl fmt::Debug for KeySource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KeySource::Agent { fingerprint } => f
                .debug_struct("Agent")
                .field("fingerprint", fingerprint)
                .finish(),
            KeySource::File { path, passphrase } => {
                let mut s = f.debug_struct("File");
                s.field("path", path);
                if passphrase.is_some() {
                    s.field("passphrase", &"[REDACTED]");
                } else {
                    s.field("passphrase", &None::<String>);
                }
                s.finish()
            }
            KeySource::Auto { fingerprint } => f
                .debug_struct("Auto")
                .field("fingerprint", fingerprint)
                .finish(),
        }
    }
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
            KeySource::File { path, passphrase } => {
                Self::load_from_file(path, passphrase.as_ref()).await
            }
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
                        // Fall back to common file locations (supports all key formats)
                        let legacy_key = Self::load_legacy_from_common_paths(fingerprint).await?;
                        match legacy_key {
                            LegacyPrivateKey::OpenSsh(key) => Ok(key),
                            _ => Err(AuthError::ConfigError(
                                "Key found but is in legacy PEM format; \
                                 use sign_request() for full format support"
                                    .into(),
                            )),
                        }
                    }
                }
            }
        }
    }

    /// Load a private key from a specific file path
    ///
    /// This method only returns `ssh_key::PrivateKey` for OpenSSH format keys.
    /// For traditional PEM formats, use `load_legacy_from_file` instead.
    pub async fn load_from_file(
        path: &Path,
        passphrase: Option<&String>,
    ) -> Result<PrivateKey, AuthError> {
        let key_data = tokio::fs::read_to_string(path).await.map_err(|e| {
            AuthError::KeyLoadError(format!("Failed to read {}: {}", path.display(), e))
        })?;

        // Check if this is OpenSSH format
        let format = PemKeyFormat::detect(&key_data);
        if format != PemKeyFormat::OpenSsh {
            // For non-OpenSSH formats, try to load via legacy_pem and convert
            let legacy_key = LegacyPrivateKey::from_pem(&key_data, passphrase.map(|s| s.as_str()))?;

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
    pub async fn load_legacy_from_file(
        path: &Path,
        passphrase: Option<&str>,
    ) -> Result<LegacyPrivateKey, AuthError> {
        let key_data = tokio::fs::read_to_string(path).await.map_err(|e| {
            AuthError::KeyLoadError(format!("Failed to read {}: {}", path.display(), e))
        })?;

        LegacyPrivateKey::from_pem(&key_data, passphrase)
    }

    /// Try loading from common SSH key locations (~/.ssh/)
    ///
    /// Supports all key formats (OpenSSH, PKCS#1, SEC1, DSA, PKCS#8) by using
    /// `load_legacy_from_file` which handles all PEM formats.
    pub async fn load_legacy_from_common_paths(
        fingerprint_str: &str,
    ) -> Result<LegacyPrivateKey, AuthError> {
        let home = dirs::home_dir()
            .ok_or_else(|| AuthError::KeyLoadError("Could not determine home directory".into()))?;

        let ssh_dir = home.join(".ssh");
        Self::scan_ssh_dir_for_key(&ssh_dir, fingerprint_str).await
    }

    /// Scan an SSH directory for a key matching the given fingerprint
    ///
    /// This is the testable core of `load_legacy_from_common_paths`, accepting
    /// an explicit directory path instead of using `~/.ssh/`.
    ///
    /// Discovery strategy (mirrors Node.js `smartdc-auth` `kr-homedir.js`):
    /// 1. Scan all `.pub` files in the directory, match fingerprint
    /// 2. On match, look for corresponding private key (strip `.pub` suffix)
    /// 3. If private key loads OK → return it
    /// 4. If private key is encrypted → return `KeyEncrypted`
    /// 5. Fallback: try hardcoded `id_*` names without `.pub` companions
    pub async fn scan_ssh_dir_for_key(
        ssh_dir: &Path,
        fingerprint_str: &str,
    ) -> Result<LegacyPrivateKey, AuthError> {
        let fingerprint = Fingerprint::parse(fingerprint_str)
            .map_err(|e| AuthError::KeyLoadError(format!("Invalid fingerprint: {}", e)))?;

        let mut scanned_pub = Vec::new();
        let mut scanned_private = Vec::new();

        // Phase 1: Scan all .pub files for fingerprint match
        if let Ok(mut dir) = tokio::fs::read_dir(ssh_dir).await {
            while let Ok(Some(entry)) = dir.next_entry().await {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("pub") {
                    continue;
                }
                let Ok(pub_data) = tokio::fs::read_to_string(&path).await else {
                    continue;
                };
                scanned_pub.push(path.clone());

                // Parse as OpenSSH public key
                let Ok(pub_key) = ssh_key::PublicKey::from_openssh(&pub_data) else {
                    continue;
                };

                if !fingerprint.matches(&pub_key) {
                    continue;
                }

                // Fingerprint matches — look for corresponding private key
                let priv_path = path.with_extension("");
                tracing::debug!(
                    "Public key {} matches fingerprint {}, trying private key {}",
                    path.display(),
                    fingerprint_str,
                    priv_path.display(),
                );

                if !tokio::fs::try_exists(&priv_path).await.unwrap_or(false) {
                    continue;
                }

                // Try loading the private key
                match Self::load_legacy_from_file(&priv_path, None).await {
                    Ok(key) => return Ok(key),
                    Err(AuthError::KeyEncrypted(_)) => {
                        return Err(AuthError::KeyEncrypted(priv_path.display().to_string()));
                    }
                    Err(e) => {
                        // Check if the error message indicates encryption
                        let msg = e.to_string();
                        if msg.contains("encrypted") || msg.contains("Encrypted") {
                            return Err(AuthError::KeyEncrypted(priv_path.display().to_string()));
                        }
                        tracing::debug!(
                            "Failed to load private key {}: {}",
                            priv_path.display(),
                            e,
                        );
                    }
                }
            }
        }

        // Phase 2: Fallback — try hardcoded id_* names (for keys without .pub companions)
        let key_files = ["id_ed25519", "id_ecdsa", "id_rsa", "id_dsa"];

        for key_file in &key_files {
            let path = ssh_dir.join(key_file);
            // Skip if we already tried this via .pub scan
            let pub_path = ssh_dir.join(format!("{}.pub", key_file));
            if scanned_pub.contains(&pub_path) {
                continue;
            }
            scanned_private.push(path.clone());

            if !tokio::fs::try_exists(&path).await.unwrap_or(false) {
                continue;
            }

            match Self::load_legacy_from_file(&path, None).await {
                Ok(key) => {
                    if let Ok(blob) = key.public_key_blob()
                        && fingerprint.matches_bytes(&blob)
                    {
                        tracing::debug!(
                            "Found matching key at {} for fingerprint {}",
                            path.display(),
                            fingerprint_str,
                        );
                        return Ok(key);
                    }
                }
                Err(AuthError::KeyEncrypted(_)) => {
                    // Can't check fingerprint without decrypting — skip
                    tracing::debug!(
                        "Skipping encrypted key {} (no .pub companion for fingerprint check)",
                        path.display(),
                    );
                }
                Err(_) => {}
            }
        }

        // Build a helpful error message listing what was scanned
        let mut scanned_desc = Vec::new();
        for p in &scanned_pub {
            scanned_desc.push(
                p.file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default(),
            );
        }
        for p in &scanned_private {
            scanned_desc.push(
                p.file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default(),
            );
        }

        Err(AuthError::KeyNotFound(format!(
            "No key with fingerprint {} found in {} (scanned: {})",
            fingerprint_str,
            ssh_dir.display(),
            if scanned_desc.is_empty() {
                "no key files found".to_string()
            } else {
                scanned_desc.join(", ")
            },
        )))
    }

    /// List all available key files in ~/.ssh/
    ///
    /// Scans the directory for all `.pub` files and standard `id_*` names.
    pub async fn list_key_files() -> Result<Vec<PathBuf>, AuthError> {
        let home = dirs::home_dir()
            .ok_or_else(|| AuthError::KeyLoadError("Could not determine home directory".into()))?;

        let ssh_dir = home.join(".ssh");
        if !tokio::fs::try_exists(&ssh_dir).await.unwrap_or(false) {
            return Ok(Vec::new());
        }

        let mut keys = Vec::new();
        let mut seen = std::collections::HashSet::new();

        // Scan .pub files and infer private key paths
        if let Ok(mut dir) = tokio::fs::read_dir(&ssh_dir).await {
            while let Ok(Some(entry)) = dir.next_entry().await {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("pub") {
                    let priv_path = path.with_extension("");
                    if tokio::fs::try_exists(&priv_path).await.unwrap_or(false) {
                        seen.insert(priv_path.clone());
                        keys.push(priv_path);
                    }
                }
            }
        }

        // Also check standard names without .pub companions
        let key_patterns = ["id_ed25519", "id_ecdsa", "id_rsa", "id_dsa"];
        for pattern in &key_patterns {
            let path = ssh_dir.join(pattern);
            if !seen.contains(&path) && tokio::fs::try_exists(&path).await.unwrap_or(false) {
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
