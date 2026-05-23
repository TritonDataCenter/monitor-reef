// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! IMGAPI manifest ingest: translate a
//! [`NewImageFromImgapi`](tritond_api::NewImageFromImgapi) body
//! into a [`NewImage`] suitable for the existing store path.
//!
//! Unlike [`crate::bundle::ingest_bundle`], this does NOT fetch
//! the blob at ingest time. The operator (or `tcadm image
//! fetch-nocloud` once it lands) has already uploaded the blob
//! to Manta and supplied the SHA-256 in the request body.
//! tritond trusts that hash and persists it on the Image record;
//! the per-CN agent re-verifies the bytes at provision time as
//! the actual integrity boundary.
//!
//! ## Compatibility derivation
//!
//! IMGAPI v2 manifests don't carry a single `brand` field;
//! brand falls out of `manifest.type` plus an optional
//! `requirements.brand`. We map:
//!
//! | `manifest.type` | derived brand |
//! |---|---|
//! | `ZoneDataset` | `requirements.brand` or `"joyent-minimal"` |
//! | `LxDataset`   | `"lx"` |
//! | `Zvol`        | `requirements.brand` or `"bhyve"` |
//! | `Docker`      | rejected (no agent path) |
//! | `Other`       | rejected (unknown) |
//!
//! `arch` is hardcoded to `"x86_64"`; IMGAPI manifests don't
//! carry arch explicitly, and Phase 0 is amd64-only. When we
//! gain aarch64 hosts, this becomes a manifest extension or a
//! body field.
//!
//! `min_smartos_platform` takes the largest value from
//! `requirements.min_platform`. The IMGAPI shape is a
//! per-major-line map (`{"7.0": "20141030T081701Z"}`); we
//! lex-sort and pick the max so a manifest constraining
//! multiple majors picks the most-recent floor for any host.

use anyhow::{Result, bail};
use imgapi_manifest::{ImageType, Manifest};
use tritond_api::NewImageFromImgapi;
use tritond_api::types::{ImageCompatibility, NewImage};

/// Translate a [`NewImageFromImgapi`] body into a [`NewImage`].
///
/// Validates body invariants the wire schema cannot express
/// (single-file image; supported brand; sha256 shape) and
/// derives compatibility from the manifest. Returns an
/// `anyhow::Error` on translation failure; the handler maps
/// those to 400 responses.
pub(crate) fn translate(body: NewImageFromImgapi) -> Result<NewImage> {
    let NewImageFromImgapi {
        manifest,
        manta_url,
        sha256,
    } = body;

    // Manifest already passed `Manifest::validate` during
    // serde deserialization (we explicitly call it again here
    // so a hand-constructed body in a test goes through the
    // same gate).
    manifest.validate()?;

    if manifest.files.len() != 1 {
        bail!(
            "IMGAPI manifest must have exactly one file entry, got {}",
            manifest.files.len()
        );
    }
    validate_sha256_hex(&sha256)?;

    let file = &manifest.files[0];
    let brand = derive_brand(&manifest)?;
    let min_platform = pick_min_smartos_platform(&manifest);
    let os_str = os_to_str(&manifest);
    let description = manifest.description.clone().unwrap_or_default();

    Ok(NewImage {
        name: manifest.name.clone(),
        description: Some(description),
        os: os_str,
        version: manifest.version.clone(),
        size_bytes: file.size,
        sha256,
        source_url: Some(manta_url),
        id: Some(manifest.uuid),
        compatibility: Some(ImageCompatibility {
            brand,
            arch: "x86_64".to_string(),
            min_smartos_platform: min_platform,
        }),
    })
}

fn derive_brand(manifest: &Manifest) -> Result<String> {
    if let Some(req) = manifest.requirements.as_ref()
        && let Some(brand) = req.brand.as_ref()
    {
        return Ok(brand.clone());
    }
    match manifest.ty {
        ImageType::ZoneDataset => Ok("joyent-minimal".to_string()),
        ImageType::LxDataset => Ok("lx".to_string()),
        ImageType::Zvol => Ok("bhyve".to_string()),
        ImageType::Docker => bail!("docker image type has no agent provisioning path"),
        ImageType::Other => bail!("unsupported manifest type 'other'"),
    }
}

fn pick_min_smartos_platform(manifest: &Manifest) -> Option<String> {
    manifest
        .requirements
        .as_ref()
        .and_then(|r| r.min_platform.values().max().cloned())
}

fn os_to_str(manifest: &Manifest) -> String {
    // imgapi_manifest::Os is `#[serde(rename_all = "lowercase")]`
    // so a serde round-trip yields the canonical lowercase
    // string the rest of tritond expects on Image.os.
    serde_json::to_value(manifest.os)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| "other".to_string())
}

fn validate_sha256_hex(s: &str) -> Result<()> {
    if s.len() != 64
        || !s
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase())
    {
        bail!("sha256 must be 64 lowercase hex chars, got {s:?}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use imgapi_manifest::{
        ANONYMOUS_OWNER, Compression, File as ImgFile, Manifest as ImgManifest, Os, Requirements,
        SCHEMA_VERSION, State,
    };
    use std::collections::BTreeMap;
    use uuid::Uuid;

    fn minimal_zone_dataset() -> ImgManifest {
        ImgManifest {
            v: SCHEMA_VERSION,
            uuid: Uuid::parse_str("c02a2044-c1bd-11e4-bd8c-dfc1db8b0182").unwrap(),
            owner: ANONYMOUS_OWNER,
            name: "test-image".to_string(),
            version: "1.0.0".to_string(),
            state: State::Active,
            disabled: false,
            public: true,
            published_at: chrono::Utc::now(),
            ty: ImageType::ZoneDataset,
            os: Os::Smartos,
            files: vec![ImgFile {
                sha1: "0".repeat(40),
                size: 128_572_734,
                compression: Compression::Gzip,
                extra: BTreeMap::new(),
            }],
            description: Some("smoke fixture".to_string()),
            homepage: None,
            urn: None,
            requirements: None,
            tags: BTreeMap::new(),
            acl: vec![],
            users: vec![],
            nic_driver: None,
            disk_driver: None,
            cpu_type: None,
            image_size: None,
            channels: vec![],
            extra: BTreeMap::new(),
        }
    }

    fn ok_body(m: ImgManifest) -> NewImageFromImgapi {
        NewImageFromImgapi {
            manifest: m,
            manta_url: "https://example.com/images/x/file".to_string(),
            sha256: "a".repeat(64),
        }
    }

    #[test]
    fn translates_zone_dataset_defaults_brand_to_joyent_minimal() {
        let n = translate(ok_body(minimal_zone_dataset())).unwrap();
        let compat = n.compatibility.unwrap();
        assert_eq!(compat.brand, "joyent-minimal");
        assert_eq!(compat.arch, "x86_64");
        assert!(compat.min_smartos_platform.is_none());
        assert_eq!(n.os, "smartos");
        assert_eq!(n.size_bytes, 128_572_734);
        assert_eq!(
            n.source_url.as_deref(),
            Some("https://example.com/images/x/file")
        );
        assert!(n.id.is_some());
    }

    #[test]
    fn translates_lx_dataset() {
        let mut m = minimal_zone_dataset();
        m.ty = ImageType::LxDataset;
        m.os = Os::Linux;
        let n = translate(ok_body(m)).unwrap();
        assert_eq!(n.compatibility.unwrap().brand, "lx");
        assert_eq!(n.os, "linux");
    }

    #[test]
    fn translates_zvol_defaults_brand_to_bhyve() {
        let mut m = minimal_zone_dataset();
        m.ty = ImageType::Zvol;
        m.os = Os::Linux;
        let n = translate(ok_body(m)).unwrap();
        assert_eq!(n.compatibility.unwrap().brand, "bhyve");
    }

    #[test]
    fn explicit_requirements_brand_wins() {
        let mut m = minimal_zone_dataset();
        m.requirements = Some(Requirements {
            brand: Some("kvm".to_string()),
            ..Default::default()
        });
        let n = translate(ok_body(m)).unwrap();
        assert_eq!(n.compatibility.unwrap().brand, "kvm");
    }

    #[test]
    fn picks_max_min_platform_across_majors() {
        let mut m = minimal_zone_dataset();
        let mut min_platform = BTreeMap::new();
        min_platform.insert("7.0".to_string(), "20141030T081701Z".to_string());
        min_platform.insert("20.4.0".to_string(), "20230715T120000Z".to_string());
        m.requirements = Some(Requirements {
            min_platform,
            ..Default::default()
        });
        let n = translate(ok_body(m)).unwrap();
        assert_eq!(
            n.compatibility.unwrap().min_smartos_platform.as_deref(),
            Some("20230715T120000Z")
        );
    }

    #[test]
    fn rejects_docker_image_type() {
        let mut m = minimal_zone_dataset();
        m.ty = ImageType::Docker;
        let err = translate(ok_body(m)).unwrap_err();
        assert!(err.to_string().contains("docker"));
    }

    #[test]
    fn rejects_zero_files() {
        let mut m = minimal_zone_dataset();
        m.files.clear();
        m.state = State::Unactivated;
        let err = translate(ok_body(m)).unwrap_err();
        assert!(err.to_string().contains("exactly one file"));
    }

    #[test]
    fn rejects_two_files() {
        let mut m = minimal_zone_dataset();
        m.files.push(m.files[0].clone());
        let err = translate(ok_body(m)).unwrap_err();
        assert!(err.to_string().contains("exactly one file"));
    }

    #[test]
    fn rejects_uppercase_sha256() {
        let mut body = ok_body(minimal_zone_dataset());
        body.sha256 = "A".repeat(64);
        let err = translate(body).unwrap_err();
        assert!(err.to_string().contains("sha256"));
    }

    #[test]
    fn rejects_short_sha256() {
        let mut body = ok_body(minimal_zone_dataset());
        body.sha256 = "abc".to_string();
        let err = translate(body).unwrap_err();
        assert!(err.to_string().contains("sha256"));
    }
}
