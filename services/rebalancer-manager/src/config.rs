// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2020 Joyent, Inc.
// Copyright 2026 Edgecast Cloud LLC.

//! Configuration for the rebalancer manager

use anyhow::{Context, Result};

/// Manager configuration loaded from environment variables
#[derive(Clone)]
pub struct ManagerConfig {
    /// PostgreSQL connection URL
    pub database_url: String,

    /// Storinfo service URL for discovering storage nodes
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

        Ok(Self {
            database_url,
            storinfo_url,
            max_concurrent_assignments,
            http_timeout_secs,
            snaplink_cleanup_required,
        })
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
}
