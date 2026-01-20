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

        Ok(Self {
            database_url,
            storinfo_url,
            max_concurrent_assignments,
            http_timeout_secs,
        })
    }

    /// Return a display-safe version of the database URL (password masked)
    pub fn database_url_display(&self) -> String {
        // Simple masking - replace password portion
        if let Some(at_pos) = self.database_url.find('@') {
            if let Some(colon_pos) = self.database_url[..at_pos].rfind(':') {
                let prefix = &self.database_url[..colon_pos + 1];
                let suffix = &self.database_url[at_pos..];
                return format!("{}****{}", prefix, suffix);
            }
        }
        self.database_url.clone()
    }
}
