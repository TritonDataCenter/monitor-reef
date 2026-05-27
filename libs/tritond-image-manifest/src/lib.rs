// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Triton Cloud image bundle format.
//!
//! A bundle is an uncompressed tar containing exactly two
//! entries:
//!
//! ```text
//! manifest.json     <- this crate's [`Manifest`] serialised as JSON
//! content.zfs.gz    <- the gzipped `zfs send` stream of the image
//! ```
//!
//! Operators build bundles with the `tritonimg-build` CLI on a
//! host that already has the source dataset; tritond ingests
//! them via `POST /v1/silos/.../images { "bundle_url": "..." }`,
//! pulls the content sha256 + name + version + compatibility
//! straight off the manifest, and stores the bundle's URL on the
//! Image record so the per-CN agent can fetch on demand at
//! provision time.
//!
//! ## Why an explicit format and not Joyent's manifest
//!
//! Joyent's imgadm manifest is rich (sha1 in `files`, owner UUIDs,
//! IMGAPI hub-protocol fields, requirements with version-pinned
//! min_platform per major). 80% of those fields are protocol
//! baggage we don't need; the load-bearing 20% (brand,
//! min_platform, file integrity) we re-express here in a shape
//! that's friendlier to our model. See `STATUS.md` slice B
//! design notes for the full comparison.
//!
//! ## Schema versioning
//!
//! The top-level `schema` field literal-matches one known
//! string ([`SCHEMA_V1`]). Future versions add a new literal;
//! the parser refuses anything else. Wire change → bump the
//! literal → both server-side ingest and the build CLI need a
//! coordinated update.

use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Schema version literal carried in every Triton image
/// manifest. Refuse-by-default for any other value so future
/// schema migrations don't silently misparse.
pub const SCHEMA_V1: &str = "tritond-image-v1";

/// Filename of the manifest entry inside the bundle tar.
pub const MANIFEST_FILENAME: &str = "manifest.json";

/// Filename of the content entry inside the bundle tar.
pub const CONTENT_FILENAME: &str = "content.zfs.gz";

/// The full image bundle manifest. Every field except
/// `description` is load-bearing for either ingest or
/// agent-side compatibility checks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Manifest {
    /// Always equals [`SCHEMA_V1`] for this crate version.
    /// Future versions add a new literal.
    pub schema: String,
    /// Operator-friendly image name. Surfaces in
    /// `tcadm silo image list`. Conventionally `<os>-<release>`
    /// (e.g. `ubuntu-24.04-base`) but the wire is free-form.
    pub name: String,
    /// Build/release version. Conventionally a date stamp +
    /// optional build counter (`20240612.1`); no semantics
    /// imposed here.
    pub version: String,
    /// Optional human-readable description. Surfaces in
    /// `tcadm silo image get` and the audit chain. Free-form.
    #[serde(default)]
    pub description: Option<String>,
    /// Content addressing + format declaration. The agent
    /// re-hashes the downloaded bytes against `sha256` before
    /// installing — mismatch is an unrecoverable failure.
    pub content: Content,
    /// Host-compatibility constraints. The per-CN agent
    /// rejects an image whose `brand` doesn't match the
    /// instance's brand or whose `min_smartos_platform` is
    /// newer than the host's platform buildstamp.
    pub compatibility: Compatibility,
    /// Guest-OS metadata. Currently informational; will drive
    /// mdata-fetch defaults (which accounts to inject ssh keys
    /// for) in a follow-up slice.
    pub guest: Guest,
}

/// Content addressing for the `content.zfs.gz` payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Content {
    /// On-wire content format. Phase 0 supports
    /// `"zfs-send-gz"` (zone-dataset images for joyent /
    /// joyent-minimal brands). Future: `"zvol-raw-gz"` for
    /// bhyve/kvm.
    pub format: String,
    /// Lowercase hex SHA-256 of the gzipped content bytes —
    /// exactly what the agent re-hashes.
    pub sha256: String,
    /// Size of the gzipped content in bytes. Surfaced to
    /// operators for storage-budget visibility; not used by
    /// the agent's integrity check (sha256 is the truth).
    pub size: u64,
}

/// Phase 0 supported content formats.
pub mod content_format {
    /// Gzipped `zfs send` stream of a zone dataset (joyent /
    /// joyent-minimal brand).
    pub const ZFS_SEND_GZ: &str = "zfs-send-gz";
    /// Gzipped raw zvol stream (bhyve/kvm). Reserved; not yet
    /// a working agent code path.
    pub const ZVOL_RAW_GZ: &str = "zvol-raw-gz";
}

/// Host-compatibility constraints. The agent rejects a
/// Provision before vmadm if any of these fail.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Compatibility {
    /// SmartOS brand the image is built for. Phase 0 examples:
    /// `joyent`, `joyent-minimal`, `lx`. Compared against the
    /// instance's requested brand at provision time.
    pub brand: String,
    /// CPU architecture the image is built for. Phase 0:
    /// always `x86_64`. Reserved for future `aarch64`.
    pub arch: String,
    /// SmartOS platform buildstamp (`YYYYMMDDTHHMMSSZ`); the
    /// host's platform must be lexicographically `>=` this
    /// value. Catches "image needs a kernel feature added in
    /// platform release X" failures at provision-time rather
    /// than zone-boot time. `None` means "any platform."
    #[serde(default)]
    pub min_smartos_platform: Option<String>,
}

/// Guest-OS metadata. Informational for v0; future slices
/// drive mdata-fetch defaults from here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Guest {
    /// e.g. `linux`, `smartos`, `windows`. Free-form for
    /// Phase 0; will tighten to an enum once the brand model
    /// settles.
    pub os_family: String,
    /// e.g. `ubuntu-24.04`, `21.4.0`. Free-form.
    pub os_version: String,
    /// Account names mdata-fetch should expect inside the
    /// guest. Phase 0 only logs this; future slices use it to
    /// gate `root_authorized_keys` injection on whether a root
    /// account actually exists.
    #[serde(default)]
    pub default_users: Vec<String>,
}

/// Errors from manifest parsing or bundle extraction.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ManifestError {
    #[error("bundle is missing required entry: {0}")]
    MissingEntry(&'static str),
    #[error("manifest schema is {got:?}; expected {expected:?}")]
    SchemaMismatch { got: String, expected: &'static str },
    #[error("manifest sha256 must be 64 lowercase hex chars, got {got:?}")]
    BadSha256 { got: String },
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("tar: {0}")]
    Tar(String),
}

impl Manifest {
    /// Validate semantic invariants the wire schema can't
    /// enforce on its own. Run on every ingest path.
    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.schema != SCHEMA_V1 {
            return Err(ManifestError::SchemaMismatch {
                got: self.schema.clone(),
                expected: SCHEMA_V1,
            });
        }
        let s = &self.content.sha256;
        let ok = s.len() == 64
            && s.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase());
        if !ok {
            return Err(ManifestError::BadSha256 { got: s.clone() });
        }
        Ok(())
    }
}

/// Successful bundle extraction. The content was unpacked to
/// `content_path` so the agent can `gzip -dc | zfs receive`
/// it without re-buffering.
#[derive(Debug)]
pub struct ExtractedBundle {
    pub manifest: Manifest,
    pub content_path: PathBuf,
}

/// Read a Triton image bundle tar from `bundle_path` and
/// extract `content.zfs.gz` into `dest_dir`. Returns the
/// parsed [`Manifest`] and the path to the extracted content.
///
/// Doesn't verify the content's sha256 — that's the agent's
/// responsibility, after the unpack, against the bytes on
/// disk. The tar format itself doesn't carry an integrity
/// check; we rely on sha256 for that.
pub fn extract_bundle(
    bundle_path: &Path,
    dest_dir: &Path,
) -> Result<ExtractedBundle, ManifestError> {
    let file = std::fs::File::open(bundle_path)?;
    let mut archive = tar::Archive::new(file);
    let mut manifest: Option<Manifest> = None;
    let mut content_path: Option<PathBuf> = None;
    std::fs::create_dir_all(dest_dir)?;
    for entry in archive
        .entries()
        .map_err(|e| ManifestError::Tar(e.to_string()))?
    {
        let mut entry = entry.map_err(|e| ManifestError::Tar(e.to_string()))?;
        let name = entry
            .path()
            .map_err(|e| ManifestError::Tar(e.to_string()))?
            .to_string_lossy()
            .into_owned();
        match name.as_str() {
            MANIFEST_FILENAME => {
                let m: Manifest = serde_json::from_reader(&mut entry)?;
                m.validate()?;
                manifest = Some(m);
            }
            CONTENT_FILENAME => {
                let out = dest_dir.join(CONTENT_FILENAME);
                let mut sink = std::fs::File::create(&out)?;
                std::io::copy(&mut entry, &mut sink)?;
                content_path = Some(out);
            }
            _other => {
                // Ignore unknown entries — forward-compat for
                // future bundle additions (e.g. signature files).
            }
        }
    }
    let manifest = manifest.ok_or(ManifestError::MissingEntry(MANIFEST_FILENAME))?;
    let content_path = content_path.ok_or(ManifestError::MissingEntry(CONTENT_FILENAME))?;
    Ok(ExtractedBundle {
        manifest,
        content_path,
    })
}

/// Write a Triton image bundle to `bundle_path`. Used by the
/// `tritonimg-build` CLI: caller has already produced the
/// gzipped content stream at `content_path` and computed its
/// sha256; this just packs the two entries into a tar.
pub fn write_bundle(
    bundle_path: &Path,
    manifest: &Manifest,
    content_path: &Path,
) -> Result<(), ManifestError> {
    manifest.validate()?;
    let file = std::fs::File::create(bundle_path)?;
    let mut builder = tar::Builder::new(file);
    let manifest_bytes = serde_json::to_vec_pretty(manifest)?;
    let mut header = tar::Header::new_gnu();
    header.set_size(manifest_bytes.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    builder
        .append_data(&mut header, MANIFEST_FILENAME, manifest_bytes.as_slice())
        .map_err(|e| ManifestError::Tar(e.to_string()))?;
    let mut content = std::fs::File::open(content_path)?;
    builder
        .append_file(CONTENT_FILENAME, &mut content)
        .map_err(|e| ManifestError::Tar(e.to_string()))?;
    builder
        .finish()
        .map_err(|e| ManifestError::Tar(e.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_manifest() -> Manifest {
        Manifest {
            schema: SCHEMA_V1.to_string(),
            name: "test-image".to_string(),
            version: "1.0.0".to_string(),
            description: Some("smoke fixture".to_string()),
            content: Content {
                format: content_format::ZFS_SEND_GZ.to_string(),
                sha256: "0".repeat(64),
                size: 1024,
            },
            compatibility: Compatibility {
                brand: "joyent-minimal".to_string(),
                arch: "x86_64".to_string(),
                min_smartos_platform: Some("20240101T000000Z".to_string()),
            },
            guest: Guest {
                os_family: "smartos".to_string(),
                os_version: "21.4.0".to_string(),
                default_users: vec!["root".to_string()],
            },
        }
    }

    #[test]
    fn validate_accepts_canonical_manifest() {
        sample_manifest()
            .validate()
            .expect("canonical sample must validate");
    }

    #[test]
    fn validate_rejects_wrong_schema() {
        let mut m = sample_manifest();
        m.schema = "tritond-image-v999".to_string();
        let err = m.validate().expect_err("non-v1 schema must reject");
        assert!(matches!(err, ManifestError::SchemaMismatch { .. }));
    }

    #[test]
    fn validate_rejects_uppercase_sha256() {
        let mut m = sample_manifest();
        m.content.sha256 = "A".repeat(64);
        let err = m.validate().expect_err("uppercase sha256 must reject");
        assert!(matches!(err, ManifestError::BadSha256 { .. }));
    }

    #[test]
    fn validate_rejects_short_sha256() {
        let mut m = sample_manifest();
        m.content.sha256 = "abc".to_string();
        let err = m.validate().expect_err("short sha256 must reject");
        assert!(matches!(err, ManifestError::BadSha256 { .. }));
    }

    #[test]
    fn round_trip_through_tar() {
        let dir = tempfile::tempdir().unwrap();
        let bundle_path = dir.path().join("bundle.tar");
        let content_path = dir.path().join("content.zfs.gz");
        std::fs::write(&content_path, b"this is not really a zfs stream").unwrap();
        let manifest = sample_manifest();

        write_bundle(&bundle_path, &manifest, &content_path).unwrap();

        let dest = dir.path().join("extracted");
        let extracted = extract_bundle(&bundle_path, &dest).unwrap();
        assert_eq!(extracted.manifest, manifest);
        let body = std::fs::read(&extracted.content_path).unwrap();
        assert_eq!(body, b"this is not really a zfs stream");
    }
}
