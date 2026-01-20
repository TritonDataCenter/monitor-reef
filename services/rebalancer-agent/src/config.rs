// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2020 Joyent, Inc.
// Copyright 2026 Edgecast Cloud LLC.

//! Agent configuration

use std::path::PathBuf;

/// Default data directory for agent storage
const DEFAULT_DATA_DIR: &str = "/var/tmp/rebalancer";

/// Default number of concurrent download tasks
const DEFAULT_CONCURRENT_DOWNLOADS: usize = 4;

/// Default HTTP timeout for downloads (seconds)
const DEFAULT_DOWNLOAD_TIMEOUT_SECS: u64 = 300;

/// Agent configuration
#[derive(Clone, Debug)]
pub struct AgentConfig {
    /// Directory for storing assignments and downloaded objects
    pub data_dir: PathBuf,
    /// Number of concurrent download tasks
    pub concurrent_downloads: usize,
    /// HTTP timeout for downloads
    pub download_timeout_secs: u64,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            data_dir: PathBuf::from(DEFAULT_DATA_DIR),
            concurrent_downloads: DEFAULT_CONCURRENT_DOWNLOADS,
            download_timeout_secs: DEFAULT_DOWNLOAD_TIMEOUT_SECS,
        }
    }
}

impl AgentConfig {
    /// Load configuration from environment variables
    pub fn from_env() -> Self {
        let data_dir = std::env::var("DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(DEFAULT_DATA_DIR));

        let concurrent_downloads = std::env::var("CONCURRENT_DOWNLOADS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(DEFAULT_CONCURRENT_DOWNLOADS);

        let download_timeout_secs = std::env::var("DOWNLOAD_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(DEFAULT_DOWNLOAD_TIMEOUT_SECS);

        Self {
            data_dir,
            concurrent_downloads,
            download_timeout_secs,
        }
    }

    /// Get the path to the SQLite database
    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("assignments.db")
    }

    /// Get the path to store downloaded objects
    pub fn objects_dir(&self) -> PathBuf {
        self.data_dir.join("objects")
    }
}
