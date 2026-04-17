// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Direct access to the imgadm on-disk database.
//!
//! The legacy agent uses `imgadm.quickGetImage()` to avoid the cost of
//! spawning the `imgadm` CLI when all it needs is a lookup. That helper
//! reads `/var/imgadm/images/<zpool>-<uuid>.json` directly from disk; we
//! reproduce the same path here.
//!
//! A CN typically has one zpool named `zones`, so the per-zpool suffix is
//! deterministic at the CN level — the legacy code derives it from the
//! sysinfo `Zpool` field but in practice always ends up with `zones`. The
//! [`ImgadmDb::with_zpool`] constructor lets callers override if a CN uses
//! something else.

use std::path::{Path, PathBuf};

use cn_agent_api::Uuid;
use thiserror::Error;

/// Default imgadm database directory. Matches the layout emitted by every
/// SmartOS platform image that ships imgadm 3.x.
pub const DEFAULT_IMGADM_DIR: &str = "/var/imgadm/images";

/// Default zpool name. Triton compute nodes always deploy a single `zones`
/// pool; callers running against a pool with a different name should use
/// [`ImgadmDb::with_zpool`].
pub const DEFAULT_ZPOOL: &str = "zones";

#[derive(Debug, Error)]
pub enum ImgadmError {
    #[error("image manifest {path} not installed")]
    NotInstalled { path: PathBuf },
    #[error("failed to read {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
}

impl ImgadmError {
    pub fn is_not_installed(&self) -> bool {
        matches!(self, ImgadmError::NotInstalled { .. })
    }
}

/// Represents the subset of an imgadm manifest cn-agent cares about.
///
/// The real file has many more fields (nested `manifest.files[]`, `source`,
/// `zpool`, etc.); we expose the raw JSON for callers that need more, and
/// typed accessors for the two fields we actually use (`uuid` and `name`).
#[derive(Debug, Clone)]
pub struct ImageEntry {
    pub raw: serde_json::Value,
}

impl ImageEntry {
    pub fn uuid(&self) -> Option<Uuid> {
        self.raw
            .pointer("/manifest/uuid")
            .and_then(|v| v.as_str())
            .and_then(|s| Uuid::parse_str(s).ok())
    }

    pub fn name(&self) -> Option<&str> {
        self.raw.pointer("/manifest/name").and_then(|v| v.as_str())
    }

    /// Does this entry carry enough metadata to count as a real image?
    ///
    /// Mirrors the legacy heuristic in `getDiskUsage`: imgadm occasionally
    /// leaves behind a near-empty manifest containing just `{"uuid": ...}`
    /// for datasets it doesn't recognize. Those stubs should be treated as
    /// raw datasets, not as installed images.
    pub fn has_real_manifest(&self) -> bool {
        let Some(manifest) = self.raw.get("manifest").and_then(|v| v.as_object()) else {
            return false;
        };
        !(manifest.len() == 1 && manifest.contains_key("uuid"))
    }
}

/// On-disk view of the imgadm image catalog.
#[derive(Debug, Clone)]
pub struct ImgadmDb {
    pub images_dir: PathBuf,
    pub zpool: String,
}

impl Default for ImgadmDb {
    fn default() -> Self {
        Self {
            images_dir: PathBuf::from(DEFAULT_IMGADM_DIR),
            zpool: DEFAULT_ZPOOL.to_string(),
        }
    }
}

impl ImgadmDb {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_dir(dir: impl Into<PathBuf>) -> Self {
        Self {
            images_dir: dir.into(),
            zpool: DEFAULT_ZPOOL.to_string(),
        }
    }

    pub fn with_zpool(mut self, zpool: impl Into<String>) -> Self {
        self.zpool = zpool.into();
        self
    }

    /// Path imgadm would use to store a given UUID's manifest.
    ///
    /// Format: `<images_dir>/<zpool>-<uuid>.json`
    pub fn manifest_path(&self, uuid: &Uuid) -> PathBuf {
        self.images_dir
            .join(format!("{}-{}.json", self.zpool, uuid))
    }

    /// Read a manifest by UUID. Returns `NotInstalled` if the file does not
    /// exist (matches legacy `imgadm.quickGetImage` error codes).
    pub async fn get(&self, uuid: &Uuid) -> Result<ImageEntry, ImgadmError> {
        let path = self.manifest_path(uuid);
        self.get_from_path(&path).await
    }

    async fn get_from_path(&self, path: &Path) -> Result<ImageEntry, ImgadmError> {
        match tokio::fs::read(path).await {
            Ok(bytes) => {
                let raw = serde_json::from_slice(&bytes).map_err(|source| ImgadmError::Parse {
                    path: path.to_path_buf(),
                    source,
                })?;
                Ok(ImageEntry { raw })
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(ImgadmError::NotInstalled {
                path: path.to_path_buf(),
            }),
            Err(source) => Err(ImgadmError::Read {
                path: path.to_path_buf(),
                source,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_MANIFEST: &str = r#"{
        "manifest": {
            "v": 2,
            "uuid": "5135b4bb-da9e-48e2-8965-7424267ad23e",
            "name": "ubuntu-24.04",
            "type": "zvol"
        },
        "zpool": "zones"
    }"#;

    #[test]
    fn manifest_path_uses_zpool_prefix() {
        let db = ImgadmDb::with_dir("/var/imgadm/images");
        let uuid = Uuid::parse_str("5135b4bb-da9e-48e2-8965-7424267ad23e").expect("parse uuid");
        assert_eq!(
            db.manifest_path(&uuid),
            PathBuf::from("/var/imgadm/images/zones-5135b4bb-da9e-48e2-8965-7424267ad23e.json"),
        );
    }

    #[test]
    fn manifest_path_respects_custom_zpool() {
        let db = ImgadmDb::with_dir("/tmp/img").with_zpool("pool42");
        let uuid = Uuid::parse_str("5135b4bb-da9e-48e2-8965-7424267ad23e").expect("parse uuid");
        assert_eq!(
            db.manifest_path(&uuid),
            PathBuf::from("/tmp/img/pool42-5135b4bb-da9e-48e2-8965-7424267ad23e.json"),
        );
    }

    #[tokio::test]
    async fn reads_and_parses_real_manifest() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let db = ImgadmDb::with_dir(tmp.path());
        let uuid = Uuid::parse_str("5135b4bb-da9e-48e2-8965-7424267ad23e").expect("parse uuid");
        std::fs::write(db.manifest_path(&uuid), SAMPLE_MANIFEST).expect("write");

        let entry = db.get(&uuid).await.expect("load");
        assert_eq!(entry.name(), Some("ubuntu-24.04"));
        assert_eq!(entry.uuid(), Some(uuid));
        assert!(entry.has_real_manifest());
    }

    #[tokio::test]
    async fn missing_manifest_is_not_installed() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let db = ImgadmDb::with_dir(tmp.path());
        let uuid = Uuid::parse_str("00000000-0000-0000-0000-000000000000").expect("parse uuid");
        let err = db.get(&uuid).await.unwrap_err();
        assert!(err.is_not_installed());
    }

    #[test]
    fn stub_manifest_is_not_a_real_image() {
        let entry = ImageEntry {
            raw: serde_json::json!({"manifest": {"uuid": "x"}}),
        };
        assert!(!entry.has_real_manifest());
    }
}
