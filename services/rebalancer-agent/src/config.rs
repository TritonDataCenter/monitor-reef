// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2020 Joyent, Inc.
// Copyright 2026 Edgecast Cloud LLC.

//! Agent configuration

use std::path::PathBuf;

/// Default data directory for agent storage (database, temp files)
const DEFAULT_DATA_DIR: &str = "/var/tmp/rebalancer";

/// Default root directory for Manta objects
/// Objects are stored as {manta_root}/{owner}/{object_id}
const DEFAULT_MANTA_ROOT: &str = "/manta";

/// Default number of concurrent download tasks
const DEFAULT_CONCURRENT_DOWNLOADS: usize = 4;

/// Default HTTP timeout for downloads (seconds)
const DEFAULT_DOWNLOAD_TIMEOUT_SECS: u64 = 300;

/// Agent configuration
#[derive(Clone, Debug)]
pub struct AgentConfig {
    /// Directory for agent state (database, temp files)
    pub data_dir: PathBuf,
    /// Root directory for Manta objects (objects stored as {manta_root}/{owner}/{object_id})
    pub manta_root: PathBuf,
    /// Number of concurrent download tasks
    pub concurrent_downloads: usize,
    /// HTTP timeout for downloads
    pub download_timeout_secs: u64,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            data_dir: PathBuf::from(DEFAULT_DATA_DIR),
            manta_root: PathBuf::from(DEFAULT_MANTA_ROOT),
            concurrent_downloads: DEFAULT_CONCURRENT_DOWNLOADS,
            download_timeout_secs: DEFAULT_DOWNLOAD_TIMEOUT_SECS,
        }
    }
}

impl AgentConfig {
    /// Load configuration from environment variables
    ///
    /// Environment variables:
    /// - `DATA_DIR`: Directory for agent state (default: /var/tmp/rebalancer)
    /// - `MANTA_ROOT`: Root directory for Manta objects (default: /manta)
    /// - `CONCURRENT_DOWNLOADS`: Number of concurrent downloads (default: 4)
    /// - `DOWNLOAD_TIMEOUT_SECS`: HTTP timeout in seconds (default: 300)
    pub fn from_env() -> Self {
        let data_dir = std::env::var("DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(DEFAULT_DATA_DIR));

        let manta_root = std::env::var("MANTA_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(DEFAULT_MANTA_ROOT));

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
            manta_root,
            concurrent_downloads,
            download_timeout_secs,
        }
    }

    /// Get the path to the SQLite database
    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("assignments.db")
    }

    /// Get the Manta file path for an object
    ///
    /// Returns `{manta_root}/{owner}/{object_id}` which is where the
    /// object will be stored on the storage node's filesystem.
    pub fn manta_file_path(&self, owner: &str, object_id: &str) -> PathBuf {
        self.manta_root.join(owner).join(object_id)
    }

    /// Get the temporary file path for downloading an object
    ///
    /// Returns `{manta_root}/{owner}/{object_id}.tmp` which is used during
    /// download to ensure atomic writes. The file is renamed to the final
    /// path only after successful MD5 verification.
    #[allow(clippy::unwrap_used)] // Path always has a filename component here
    pub fn manta_tmp_path(&self, owner: &str, object_id: &str) -> PathBuf {
        let mut path = self.manta_file_path(owner, object_id);
        let mut filename = path.file_name().unwrap().to_os_string();
        filename.push(".tmp");
        path.set_file_name(filename);
        path
    }

    /// Get the directory for temporary downloads (for cleanup purposes)
    ///
    /// Note: Temp files are stored alongside final files with `.tmp` extension,
    /// not in a separate directory. This method returns the manta_root for
    /// scanning during cleanup.
    pub fn temp_dir(&self) -> &PathBuf {
        &self.manta_root
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = AgentConfig::default();

        assert_eq!(config.data_dir, PathBuf::from("/var/tmp/rebalancer"));
        assert_eq!(config.manta_root, PathBuf::from("/manta"));
        assert_eq!(config.concurrent_downloads, 4);
        assert_eq!(config.download_timeout_secs, 300);
    }

    #[test]
    fn test_db_path() {
        let config = AgentConfig {
            data_dir: PathBuf::from("/test/data"),
            manta_root: PathBuf::from("/test/manta"),
            concurrent_downloads: 4,
            download_timeout_secs: 300,
        };

        assert_eq!(config.db_path(), PathBuf::from("/test/data/assignments.db"));
    }

    #[test]
    fn test_manta_file_path() {
        let config = AgentConfig {
            data_dir: PathBuf::from("/test/data"),
            manta_root: PathBuf::from("/manta"),
            concurrent_downloads: 4,
            download_timeout_secs: 300,
        };

        let path = config.manta_file_path("owner123", "object456");
        assert_eq!(path, PathBuf::from("/manta/owner123/object456"));
    }

    #[test]
    fn test_manta_tmp_path() {
        let config = AgentConfig {
            data_dir: PathBuf::from("/test/data"),
            manta_root: PathBuf::from("/manta"),
            concurrent_downloads: 4,
            download_timeout_secs: 300,
        };

        let path = config.manta_tmp_path("owner123", "object456");
        assert_eq!(path, PathBuf::from("/manta/owner123/object456.tmp"));
    }

    #[test]
    fn test_temp_dir() {
        let config = AgentConfig {
            data_dir: PathBuf::from("/test/data"),
            manta_root: PathBuf::from("/manta"),
            concurrent_downloads: 4,
            download_timeout_secs: 300,
        };

        assert_eq!(config.temp_dir(), &PathBuf::from("/manta"));
    }
}
