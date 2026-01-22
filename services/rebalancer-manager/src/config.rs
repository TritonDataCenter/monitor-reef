// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2020 Joyent, Inc.
// Copyright 2026 Edgecast Cloud LLC.

//! Configuration for the rebalancer manager

use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;
use tokio::sync::watch;

/// Manager configuration loaded from environment variables or JSON file
///
/// Configuration can be loaded from:
/// 1. Environment variables (primary method, see `from_env()`)
/// 2. JSON config file (for SIGUSR1-based reloading, see `from_file()`)
///
/// The JSON config file supports a subset of fields that are safe to change at runtime.
#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct ManagerConfig {
    /// PostgreSQL connection URL
    /// Note: This field is NOT reloadable - changes require restart
    #[serde(skip)]
    pub database_url: String,

    /// Storinfo service URL for discovering storage nodes
    /// Note: This field is NOT reloadable - changes require restart
    #[serde(skip)]
    pub storinfo_url: String,

    /// Maximum concurrent assignments per job
    #[allow(dead_code)]
    pub max_concurrent_assignments: usize,

    /// HTTP client timeout in seconds
    pub http_timeout_secs: u64,

    /// Whether snaplink cleanup is required before evacuate jobs can be created
    ///
    /// When true, job creation requests will be rejected with an error indicating
    /// that snaplink cleanup must be completed first. This prevents data integrity
    /// issues that can occur if objects are evacuated before snaplinks are cleaned up.
    pub snaplink_cleanup_required: bool,

    /// Datacenter names to exclude from destination selection
    ///
    /// Storage nodes in these datacenters will not be used as destinations
    /// during evacuation jobs. Parsed from BLACKLIST_DATACENTERS env var
    /// (comma-separated list).
    pub blacklist_datacenters: Vec<String>,
}

impl Default for ManagerConfig {
    fn default() -> Self {
        Self {
            database_url: String::new(),
            storinfo_url: String::new(),
            max_concurrent_assignments: 10,
            http_timeout_secs: 30,
            snaplink_cleanup_required: false,
            blacklist_datacenters: Vec::new(),
        }
    }
}

impl ManagerConfig {
    /// Load configuration from environment variables
    pub fn from_env() -> Result<Self> {
        let database_url =
            std::env::var("DATABASE_URL").context("DATABASE_URL environment variable required")?;

        let storinfo_url =
            std::env::var("STORINFO_URL").context("STORINFO_URL environment variable required")?;

        let max_concurrent_assignments = std::env::var("MAX_CONCURRENT_ASSIGNMENTS")
            .unwrap_or_else(|_| "10".to_string())
            .parse()
            .context("Invalid MAX_CONCURRENT_ASSIGNMENTS")?;

        let http_timeout_secs = std::env::var("HTTP_TIMEOUT_SECS")
            .unwrap_or_else(|_| "30".to_string())
            .parse()
            .context("Invalid HTTP_TIMEOUT_SECS")?;

        // Parse SNAPLINK_CLEANUP_REQUIRED as a boolean
        // Accepts "true", "1", "yes" (case-insensitive) as true, anything else as false
        let snaplink_cleanup_required = std::env::var("SNAPLINK_CLEANUP_REQUIRED")
            .map(|v| matches!(v.to_lowercase().as_str(), "true" | "1" | "yes"))
            .unwrap_or(false);

        // Parse BLACKLIST_DATACENTERS as a comma-separated list
        // Example: "dc1,dc2,dc3" -> vec!["dc1", "dc2", "dc3"]
        let blacklist_datacenters = std::env::var("BLACKLIST_DATACENTERS")
            .map(|v| {
                v.split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default();

        Ok(Self {
            database_url,
            storinfo_url,
            max_concurrent_assignments,
            http_timeout_secs,
            snaplink_cleanup_required,
            blacklist_datacenters,
        })
    }

    /// Load configuration from a JSON file
    ///
    /// This is used for runtime configuration reloading via SIGUSR1.
    /// Note that some fields (like database_url) cannot be reloaded at runtime
    /// and will retain their original values.
    pub async fn from_file(path: &Path) -> Result<Self> {
        let content = tokio::fs::read_to_string(path)
            .await
            .with_context(|| format!("Failed to read config file: {}", path.display()))?;

        serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse config file: {}", path.display()))
    }

    /// Merge runtime-reloadable fields from another config
    ///
    /// This preserves non-reloadable fields (database_url, storinfo_url) while
    /// updating fields that can be safely changed at runtime.
    pub fn merge_reloadable(&mut self, other: &ManagerConfig) {
        self.max_concurrent_assignments = other.max_concurrent_assignments;
        self.http_timeout_secs = other.http_timeout_secs;
        self.snaplink_cleanup_required = other.snaplink_cleanup_required;
        self.blacklist_datacenters = other.blacklist_datacenters.clone();
    }

    /// Start watching for SIGUSR1 to reload config from file
    ///
    /// When SIGUSR1 is received, the config file is re-read and the new
    /// configuration is sent to subscribers via the watch channel.
    ///
    /// # Arguments
    /// * `config_file` - Path to the JSON config file
    /// * `current_config` - The current configuration (for preserving non-reloadable fields)
    /// * `config_tx` - Channel to send updated configuration
    ///
    /// # Example
    /// ```ignore
    /// let (config_tx, config_rx) = watch::channel(config.clone());
    /// tokio::spawn(ManagerConfig::start_config_watcher(
    ///     PathBuf::from("/etc/rebalancer/config.json"),
    ///     config.clone(),
    ///     config_tx,
    /// ));
    /// ```
    #[cfg(unix)]
    pub async fn start_config_watcher(
        config_file: std::path::PathBuf,
        current_config: Self,
        config_tx: watch::Sender<Self>,
    ) {
        use tokio::signal::unix::{SignalKind, signal};

        let mut sigusr1 = match signal(SignalKind::user_defined1()) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(error = %e, "Failed to register SIGUSR1 handler");
                return;
            }
        };

        let mut config = current_config;

        loop {
            sigusr1.recv().await;
            tracing::info!(
                config_file = %config_file.display(),
                "Received SIGUSR1, reloading config"
            );

            match Self::from_file(&config_file).await {
                Ok(new_config) => {
                    // Merge only reloadable fields
                    config.merge_reloadable(&new_config);

                    if config_tx.send(config.clone()).is_err() {
                        tracing::warn!("No config subscribers, reload had no effect");
                    } else {
                        tracing::info!(
                            blacklist = ?config.blacklist_datacenters,
                            snaplink_cleanup_required = config.snaplink_cleanup_required,
                            "Config reloaded successfully"
                        );
                    }
                }
                Err(e) => {
                    tracing::error!(
                        error = %e,
                        config_file = %config_file.display(),
                        "Failed to reload config"
                    );
                }
            }
        }
    }

    /// Return a display-safe version of the database URL (password masked)
    pub fn database_url_display(&self) -> String {
        // Simple masking - replace password portion
        // URL format: scheme://[user[:password]@]host[:port]/path
        // We need to find the colon between user and password, not the scheme colon

        // First, find where the authority section starts (after "://")
        let authority_start = match self.database_url.find("://") {
            Some(pos) => pos + 3,
            None => return self.database_url.clone(),
        };

        // Find the @ sign (end of userinfo section)
        let at_pos = match self.database_url[authority_start..].find('@') {
            Some(pos) => authority_start + pos,
            None => return self.database_url.clone(),
        };

        // Find the last colon in the userinfo section (between user and password)
        // This colon must be after authority_start and before at_pos
        if let Some(relative_colon_pos) = self.database_url[authority_start..at_pos].rfind(':') {
            let colon_pos = authority_start + relative_colon_pos;
            let prefix = &self.database_url[..colon_pos + 1];
            let suffix = &self.database_url[at_pos..];
            return format!("{}****{}", prefix, suffix);
        }

        self.database_url.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // =========================================================================
    // Configuration Tests
    // =========================================================================
    //
    // Note: We deliberately avoid testing `from_env()` directly because:
    //
    // 1. In Rust 2024 edition, `std::env::set_var` and `std::env::remove_var`
    //    are marked as `unsafe` due to potential data races with other threads
    //    reading environment variables.
    //
    // 2. The `from_env()` function is straightforward - it reads env vars and
    //    parses them. The interesting logic to test is in `database_url_display()`.
    //
    // 3. Integration tests or manual testing can verify env var parsing works
    //    correctly in a real deployment context.
    //
    // =========================================================================

    // -------------------------------------------------------------------------
    // Helper to create a ManagerConfig directly for testing
    // -------------------------------------------------------------------------

    fn make_config(database_url: &str) -> ManagerConfig {
        ManagerConfig {
            database_url: database_url.to_string(),
            storinfo_url: "http://storinfo.local:8080".to_string(),
            max_concurrent_assignments: 10,
            http_timeout_secs: 30,
            snaplink_cleanup_required: false,
            blacklist_datacenters: Vec::new(),
        }
    }

    // -------------------------------------------------------------------------
    // Test 1: database_url_display_masks_password
    // -------------------------------------------------------------------------

    #[test]
    fn database_url_display_masks_password() {
        let config = make_config("postgres://user:supersecretpassword@localhost:5432/db");

        let display = config.database_url_display();

        // Should contain the user but not the password
        assert!(display.contains("user:"));
        assert!(!display.contains("supersecretpassword"));
        assert!(display.contains("****"));
        assert!(display.contains("@localhost:5432/db"));

        // Full expected format
        assert_eq!(display, "postgres://user:****@localhost:5432/db");
    }

    // -------------------------------------------------------------------------
    // Test 2: database_url_display_no_password
    // -------------------------------------------------------------------------

    #[test]
    fn database_url_display_no_password() {
        // URL without password should be unchanged
        let config = make_config("postgres://localhost/db");

        let display = config.database_url_display();

        // Should be unchanged when no password present
        assert_eq!(display, "postgres://localhost/db");
    }

    // -------------------------------------------------------------------------
    // Test 3: database_url_display_user_no_password
    // -------------------------------------------------------------------------

    #[test]
    fn database_url_display_user_no_password() {
        // URL with user but no password should be unchanged
        let config = make_config("postgres://user@localhost/db");

        let display = config.database_url_display();

        // No colon before @, so no password to mask
        assert_eq!(display, "postgres://user@localhost/db");
    }

    // -------------------------------------------------------------------------
    // Test 4: database_url_display_with_port
    // -------------------------------------------------------------------------

    #[test]
    fn database_url_display_with_port() {
        // URL with port in the host section
        let config = make_config("postgres://admin:secret123@db.example.com:5432/mydb");

        let display = config.database_url_display();

        assert_eq!(display, "postgres://admin:****@db.example.com:5432/mydb");
    }

    // -------------------------------------------------------------------------
    // Test 5: database_url_display_complex_password
    // -------------------------------------------------------------------------

    #[test]
    fn database_url_display_complex_password() {
        // Password with special characters
        let config = make_config("postgres://user:p@ss:word!@localhost/db");

        let display = config.database_url_display();

        // Should mask everything between user: and @localhost
        // The last @ is the delimiter, so "p@ss:word!" is the password
        assert!(!display.contains("p@ss:word!"));
        assert!(display.contains("****"));
    }

    // -------------------------------------------------------------------------
    // Test 6: database_url_display_empty_password
    // -------------------------------------------------------------------------

    #[test]
    fn database_url_display_empty_password() {
        // Empty password (user:@host)
        let config = make_config("postgres://user:@localhost/db");

        let display = config.database_url_display();

        // Should still mask the (empty) password section
        assert_eq!(display, "postgres://user:****@localhost/db");
    }

    // -------------------------------------------------------------------------
    // Test 7: merge_reloadable preserves non-reloadable fields
    // -------------------------------------------------------------------------

    #[test]
    fn merge_reloadable_preserves_connection_urls() {
        let mut original = ManagerConfig {
            database_url: "postgres://original:pass@localhost/db".to_string(),
            storinfo_url: "http://original-storinfo:8080".to_string(),
            max_concurrent_assignments: 5,
            http_timeout_secs: 10,
            snaplink_cleanup_required: false,
            blacklist_datacenters: vec!["dc1".to_string()],
        };

        let new_config = ManagerConfig {
            database_url: "postgres://new:pass@localhost/db".to_string(), // Should be ignored
            storinfo_url: "http://new-storinfo:8080".to_string(),         // Should be ignored
            max_concurrent_assignments: 20,
            http_timeout_secs: 60,
            snaplink_cleanup_required: true,
            blacklist_datacenters: vec!["dc2".to_string(), "dc3".to_string()],
        };

        original.merge_reloadable(&new_config);

        // Non-reloadable fields should be preserved
        assert_eq!(
            original.database_url,
            "postgres://original:pass@localhost/db"
        );
        assert_eq!(original.storinfo_url, "http://original-storinfo:8080");

        // Reloadable fields should be updated
        assert_eq!(original.max_concurrent_assignments, 20);
        assert_eq!(original.http_timeout_secs, 60);
        assert!(original.snaplink_cleanup_required);
        assert_eq!(original.blacklist_datacenters, vec!["dc2", "dc3"]);
    }

    // -------------------------------------------------------------------------
    // Test 8: JSON deserialization uses defaults for skipped fields
    // -------------------------------------------------------------------------

    #[test]
    fn json_deserialization_uses_defaults() {
        // JSON config only includes reloadable fields
        let json = r#"{
            "max_concurrent_assignments": 15,
            "http_timeout_secs": 45,
            "snaplink_cleanup_required": true,
            "blacklist_datacenters": ["east-1", "west-2"]
        }"#;

        let config: ManagerConfig = serde_json::from_str(json).unwrap();

        // Skipped fields should get default (empty) values
        assert!(config.database_url.is_empty());
        assert!(config.storinfo_url.is_empty());

        // Specified fields should be parsed
        assert_eq!(config.max_concurrent_assignments, 15);
        assert_eq!(config.http_timeout_secs, 45);
        assert!(config.snaplink_cleanup_required);
        assert_eq!(config.blacklist_datacenters, vec!["east-1", "west-2"]);
    }

    // -------------------------------------------------------------------------
    // Test 9: Default implementation has sensible values
    // -------------------------------------------------------------------------

    #[test]
    fn default_config_has_sensible_values() {
        let config = ManagerConfig::default();

        assert_eq!(config.max_concurrent_assignments, 10);
        assert_eq!(config.http_timeout_secs, 30);
        assert!(!config.snaplink_cleanup_required);
        assert!(config.blacklist_datacenters.is_empty());
    }
}
