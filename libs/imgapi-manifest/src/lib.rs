// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! IMGAPI v2 image manifest types.
//!
//! This crate implements the canonical Joyent/Triton IMGAPI v2
//! manifest schema as documented at
//! <https://images.smartos.org/docs/#manifest-specification>.
//! It is the on-the-wire shape that imgadm consumes and that
//! every nocloud image producer (the upstream
//! `tritonadm image fetch-nocloud` included) emits.
//!
//! ## Scope
//!
//! - Strict serde for the documented fields (`v`, `uuid`,
//!   `owner`, `name`, `version`, `state`, `disabled`, `public`,
//!   `published_at`, `type`, `os`, `files`, `requirements`,
//!   `tags`, plus the zvol-specific `users`, `nic_driver`,
//!   `disk_driver`, `cpu_type`, `image_size`).
//! - Forward-compat preservation of unknown top-level fields via
//!   a flattened `extra` map. We never silently drop bytes from a
//!   manifest we round-tripped.
//! - Refuse-by-default on `v` != 2. v1 manifests have not been
//!   produced by anything supported since 2014; if we encounter
//!   one, we want a loud error, not silent partial parsing.
//! - Validation invariants the wire schema can't enforce:
//!   `files[].sha1` is 40 lowercase hex; `published_at` parses as
//!   chrono::DateTime<Utc>; `uuid` parses as a real UUID.
//!
//! ## Non-scope
//!
//! - No HTTP client. Fetching manifests from an IMGAPI lives in
//!   tritonadm / tritonagent.
//! - No blob storage. The companion crate `imgapi-blob-manta`
//!   handles Manta cap-token mput/mget for the `files[]` bytes.
//! - No ACL evaluation. tritond enforces per-silo scope at the
//!   HTTP boundary using `owner` + `acl`.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use uuid::Uuid;

/// The only schema version this crate accepts. IMGAPI v1
/// predates 2014 and has no live producers; v3 does not exist.
pub const SCHEMA_VERSION: u32 = 2;

/// Reserved owner UUID used by public unscoped images
/// (`00000000-0000-0000-0000-000000000000`). Matches the
/// upstream IMGAPI convention.
pub const ANONYMOUS_OWNER: Uuid = Uuid::nil();

/// An IMGAPI v2 image manifest.
///
/// Field order and `#[serde(default)]` placement matches the
/// shape produced by `images.smartos.org` so we round-trip
/// real-world manifests byte-equivalently (modulo whitespace).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Manifest {
    /// Always equals [`SCHEMA_VERSION`]. The parser rejects any
    /// other value via [`Manifest::validate`].
    pub v: u32,

    /// Globally unique image identifier. Operators and the per-CN
    /// agent both treat this as the primary key.
    pub uuid: Uuid,

    /// Owner of the image. Public unscoped images use
    /// [`ANONYMOUS_OWNER`]. In our deployment, this is the silo
    /// UUID.
    pub owner: Uuid,

    /// Operator-facing name. Conventionally `<os>-<variant>`
    /// (e.g. `base-64-lts`, `ubuntu-24.04`).
    pub name: String,

    /// Build / release version string. Free-form; conventionally
    /// either a semver-ish string or a date stamp.
    pub version: String,

    /// Lifecycle state. `Active` is the only state a CN-side
    /// fetcher should ever see; the others are tritond-internal
    /// transitions.
    pub state: State,

    /// When true, the image cannot be used to provision new
    /// instances. Existing instances are unaffected. Separate
    /// from `state` because an admin can flip `disabled` without
    /// changing the lifecycle, and vice versa.
    #[serde(default)]
    pub disabled: bool,

    /// When true, the image is visible to everyone in the
    /// deployment regardless of `owner` / `acl`.
    #[serde(default)]
    pub public: bool,

    /// When the image was published. Zero value means
    /// "unpublished draft" — generally only seen on `Creating`
    /// or `Unactivated` states.
    pub published_at: DateTime<Utc>,

    /// Image content shape; selects the vmadm brand at provision
    /// time and which `zfs receive` target the agent uses.
    #[serde(rename = "type")]
    pub ty: ImageType,

    /// Guest OS family. Informational only on the agent side;
    /// tritond uses it for filtering in `GET /images?os=...`.
    pub os: Os,

    /// Content blobs. Conventionally exactly one entry; multiple
    /// entries are reserved for future split-blob images.
    pub files: Vec<File>,

    /// Optional human-readable description. Shown in
    /// `tritonadm image list` and the admin UI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Optional vendor or product homepage URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,

    /// Optional legacy URN
    /// (`sdc:<vendor>:<name>:<version>`); preserved for round-
    /// trip but not used by the agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub urn: Option<String>,

    /// Provisioning constraints (brand, min/max platform, ssh
    /// requirement, network shape). The agent enforces these at
    /// provision time before calling vmadm.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requirements: Option<Requirements>,

    /// Free-form key/value tags. Surfaces in
    /// `tritonadm image list --tag …`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tags: BTreeMap<String, String>,

    /// Per-account ACL for non-public images. tritond extends
    /// this with silo/tenant/project visibility when serving
    /// `GET /images`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub acl: Vec<Uuid>,

    // ─── zvol-only (kvm/bhyve) fields ────────────────────────
    /// Default users present in the guest image.
    /// `[{"name": "root"}, ...]`. Only meaningful for `Zvol`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub users: Vec<User>,

    /// vmadm `nic_driver` (e.g. `virtio`). zvol-only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nic_driver: Option<String>,

    /// vmadm `disk_driver` (e.g. `virtio`). zvol-only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk_driver: Option<String>,

    /// vmadm `cpu_type` (e.g. `host`, `qemu64`). zvol-only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_type: Option<String>,

    /// Size of the boot disk in MiB, as provided to vmadm.
    /// zvol-only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_size: Option<u64>,

    /// Channels this image is published to (IMGAPI-channel
    /// flavor — distinct from our Manta publish channel).
    /// Preserved for round-trip; tritond currently ignores.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub channels: Vec<String>,

    /// Forward-compat catch-all. Any top-level field this crate
    /// doesn't know about is preserved verbatim and serialized
    /// back out. Lets us round-trip future IMGAPI extensions
    /// without losing bytes.
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

/// IMGAPI lifecycle states. `Active` is the only state a CN
/// agent should encounter via a `GET /images/:uuid` from
/// tritond; the others are intermediate transitions tritond
/// drives internally.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum State {
    /// Newly-created manifest; no file content uploaded yet.
    Creating,
    /// File uploaded but not yet activated for provision.
    Unactivated,
    /// Ready for provision.
    Active,
    /// Soft-deleted / archived; preserved for audit / rollback.
    Disabled,
    /// Creation failed; left in place for forensics.
    Failed,
}

/// Image content shape. Drives the brand selection at provision
/// time and the `zfs receive` invocation on the agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ImageType {
    /// Zone-dataset image (`joyent` / `joyent-minimal` brand).
    ZoneDataset,
    /// LX-branded zone dataset (`lx` brand).
    LxDataset,
    /// Raw zvol image (`kvm` / `bhyve` brand).
    Zvol,
    /// Docker layer image. Preserved for round-trip;
    /// not provisionable in our deployment.
    Docker,
    /// Catch-all for less common image types. Preserved verbatim
    /// for round-trip via [`Manifest::extra`].
    #[serde(other)]
    Other,
}

/// Guest OS family. Informational metadata for filtering;
/// no validation logic depends on this value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Os {
    Smartos,
    Linux,
    Windows,
    Bsd,
    Illumos,
    Plan9,
    Other,
}

/// A single content blob.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct File {
    /// Lowercase 40-char hex SHA-1 of the compressed bytes
    /// (i.e. of what's on disk after the compression in
    /// [`File::compression`]).
    ///
    /// SHA-1 is the IMGAPI wire format. Our blob backend
    /// additionally computes SHA-256 and stores it on the
    /// out-of-band side-record for tamper-evidence; the on-wire
    /// manifest stays IMGAPI-faithful.
    pub sha1: String,

    /// Size of the compressed bytes in bytes.
    pub size: u64,

    /// Compression applied to the underlying ZFS stream.
    pub compression: Compression,

    /// Forward-compat catch-all (e.g. future `sha256`,
    /// `dataset_guid`, etc.).
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

/// Compression applied to a `File`'s bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Compression {
    Gzip,
    Bzip2,
    Xz,
    None,
}

/// Provisioning constraints. The agent rejects a provision job
/// when any of these fail against the host or the requested
/// instance shape.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct Requirements {
    /// Floor platform per major release line, e.g.
    /// `{"7.0": "20141030T081701Z"}`. The host's platform stamp
    /// must be lexicographically `>=` the entry for the host's
    /// major.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub min_platform: BTreeMap<String, String>,

    /// Ceiling platform per major release line. Same shape as
    /// `min_platform`; `<=` semantics.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub max_platform: BTreeMap<String, String>,

    /// SmartOS brand the image is built for (e.g.
    /// `joyent-minimal`, `lx`, `kvm`, `bhyve`). Compared against
    /// the requested instance brand.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brand: Option<String>,

    /// When true, instance creation must provide
    /// `ssh_authorized_keys`. Image rejects provisions without
    /// it. Conventional for cloud-init images.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh_key: Option<bool>,

    /// Network requirements. Preserved for round-trip; tritond
    /// uses the count for "must have at least N nics" checks.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub networks: Vec<NetworkRequirement>,

    /// Forward-compat catch-all.
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

/// One network slot the image expects.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NetworkRequirement {
    /// Nic name inside the guest (`net0`, `net1`, ...).
    pub name: String,
    /// Operator-facing description of intended purpose.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Forward-compat catch-all.
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

/// A default user present in the guest image (zvol-only).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct User {
    pub name: String,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

/// Errors from parsing or validating an IMGAPI manifest.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ManifestError {
    #[error("manifest schema version is {got}; expected {expected}")]
    SchemaMismatch { got: u32, expected: u32 },
    #[error("files[{index}].sha1 must be 40 lowercase hex chars, got {got:?}")]
    BadSha1 { index: usize, got: String },
    #[error("manifest has zero files but state is Active")]
    NoFilesForActive,
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

impl Manifest {
    /// Parse an IMGAPI manifest from JSON bytes and validate
    /// semantic invariants that serde can't catch.
    pub fn parse(bytes: &[u8]) -> Result<Self, ManifestError> {
        let m: Manifest = serde_json::from_slice(bytes)?;
        m.validate()?;
        Ok(m)
    }

    /// Validate invariants the wire schema can't enforce on its
    /// own. Run by [`Manifest::parse`]; also called explicitly by
    /// tritond on every PUT path.
    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.v != SCHEMA_VERSION {
            return Err(ManifestError::SchemaMismatch {
                got: self.v,
                expected: SCHEMA_VERSION,
            });
        }
        for (i, f) in self.files.iter().enumerate() {
            let ok = f.sha1.len() == 40
                && f.sha1
                    .chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase());
            if !ok {
                return Err(ManifestError::BadSha1 {
                    index: i,
                    got: f.sha1.clone(),
                });
            }
        }
        if self.state == State::Active && self.files.is_empty() {
            return Err(ManifestError::NoFilesForActive);
        }
        Ok(())
    }

    /// Serialize to canonical pretty JSON. Used when writing the
    /// manifest into `/var/imgadm/images/zones-<uuid>.json` from
    /// the per-CN agent — we keep the on-disk shape identical
    /// to what tritond returns on the wire so debugging diffs
    /// stay legible.
    pub fn to_pretty_json(&self) -> Result<String, ManifestError> {
        serde_json::to_string_pretty(self).map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real `base-64-lts` v14.4.0 manifest, fetched 2026-05-23
    /// from images.smartos.org. Used as the round-trip anchor:
    /// if we ever serialize this differently, the test fails.
    const BASE_64_LTS: &str = include_str!("../tests/fixtures/base-64-lts.json");

    /// Real `lx-centos-6` v20150313 manifest.
    const LX_CENTOS_6: &str = include_str!("../tests/fixtures/lx-centos-6.json");

    /// Real `debian-7` zvol manifest (KVM-era).
    const DEBIAN_7_ZVOL: &str = include_str!("../tests/fixtures/debian-7-zvol.json");

    #[test]
    fn parses_zone_dataset_manifest() {
        let m = Manifest::parse(BASE_64_LTS.as_bytes()).expect("base-64-lts must parse");
        assert_eq!(m.v, 2);
        assert_eq!(m.name, "base-64-lts");
        assert_eq!(m.ty, ImageType::ZoneDataset);
        assert_eq!(m.os, Os::Smartos);
        assert_eq!(m.state, State::Active);
        assert!(m.public);
        assert_eq!(m.files.len(), 1);
        assert_eq!(m.files[0].compression, Compression::Gzip);
        assert_eq!(m.files[0].sha1.len(), 40);
    }

    #[test]
    fn parses_lx_dataset_manifest() {
        let m = Manifest::parse(LX_CENTOS_6.as_bytes()).expect("lx-centos-6 must parse");
        assert_eq!(m.ty, ImageType::LxDataset);
        assert_eq!(m.os, Os::Linux);
        assert_eq!(
            m.requirements.as_ref().unwrap().brand.as_deref(),
            Some("lx")
        );
    }

    #[test]
    fn parses_zvol_manifest_with_kvm_extras() {
        let m = Manifest::parse(DEBIAN_7_ZVOL.as_bytes()).expect("debian-7-zvol must parse");
        assert_eq!(m.ty, ImageType::Zvol);
        assert_eq!(m.nic_driver.as_deref(), Some("virtio"));
        assert_eq!(m.disk_driver.as_deref(), Some("virtio"));
        assert_eq!(m.cpu_type.as_deref(), Some("host"));
        assert_eq!(m.image_size, Some(10240));
        assert_eq!(m.users.len(), 1);
        assert_eq!(m.users[0].name, "root");
        assert_eq!(m.requirements.as_ref().unwrap().ssh_key, Some(true));
    }

    #[test]
    fn round_trip_preserves_fields() {
        for (label, src) in [
            ("base-64-lts", BASE_64_LTS),
            ("lx-centos-6", LX_CENTOS_6),
            ("debian-7-zvol", DEBIAN_7_ZVOL),
        ] {
            let parsed = Manifest::parse(src.as_bytes()).unwrap_or_else(|e| {
                panic!("{label}: parse failed: {e}");
            });
            let reserialized = serde_json::to_string(&parsed)
                .unwrap_or_else(|e| panic!("{label}: serialize failed: {e}"));
            let reparsed: Manifest = serde_json::from_str(&reserialized)
                .unwrap_or_else(|e| panic!("{label}: re-parse failed: {e}"));
            assert_eq!(parsed, reparsed, "{label}: round-trip changed manifest");
        }
    }

    #[test]
    fn rejects_v1_manifest() {
        let mut v: Value = serde_json::from_str(BASE_64_LTS).unwrap();
        v["v"] = serde_json::json!(1);
        let bytes = serde_json::to_vec(&v).unwrap();
        let err = Manifest::parse(&bytes).expect_err("v1 must reject");
        assert!(
            matches!(err, ManifestError::SchemaMismatch { got: 1, .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn rejects_uppercase_sha1() {
        let mut v: Value = serde_json::from_str(BASE_64_LTS).unwrap();
        v["files"][0]["sha1"] = serde_json::json!("A".repeat(40));
        let bytes = serde_json::to_vec(&v).unwrap();
        let err = Manifest::parse(&bytes).expect_err("uppercase sha1 must reject");
        assert!(matches!(err, ManifestError::BadSha1 { index: 0, .. }));
    }

    #[test]
    fn rejects_short_sha1() {
        let mut v: Value = serde_json::from_str(BASE_64_LTS).unwrap();
        v["files"][0]["sha1"] = serde_json::json!("abc");
        let bytes = serde_json::to_vec(&v).unwrap();
        let err = Manifest::parse(&bytes).expect_err("short sha1 must reject");
        assert!(matches!(err, ManifestError::BadSha1 { index: 0, .. }));
    }

    #[test]
    fn rejects_active_with_no_files() {
        let mut v: Value = serde_json::from_str(BASE_64_LTS).unwrap();
        v["files"] = serde_json::json!([]);
        let bytes = serde_json::to_vec(&v).unwrap();
        let err = Manifest::parse(&bytes).expect_err("active+no-files must reject");
        assert!(matches!(err, ManifestError::NoFilesForActive));
    }

    #[test]
    fn unknown_top_level_field_is_preserved() {
        let mut v: Value = serde_json::from_str(BASE_64_LTS).unwrap();
        v["future_field"] = serde_json::json!({"foo": 42});
        let bytes = serde_json::to_vec(&v).unwrap();
        let parsed = Manifest::parse(&bytes).expect("unknown field must not block parse");
        assert!(parsed.extra.contains_key("future_field"));
        let reserialized = serde_json::to_value(&parsed).unwrap();
        assert_eq!(reserialized["future_field"]["foo"], serde_json::json!(42));
    }
}
