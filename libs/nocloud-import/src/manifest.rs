// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! IMGAPI manifest builder. Mirrors `target/triton-nocloud-images/manifest.in.json`
//! field-for-field, built as the shared IMGAPI `Image` type instead of
//! `sed`-substituted text.

use std::collections::HashMap;

use anyhow::{Context, Result};
use imgapi_types::{
    FileCompression, Image, ImageFile, ImageOs, ImageRequirements, ImageState, ImageType,
    NetworkRequirement,
};
use serde_json::json;
use uuid::Uuid;

pub struct ManifestInputs {
    pub uuid: Uuid,
    pub name: String,
    pub version: String,
    pub published_at: String,
    pub os: String,
    pub sha1: String,
    pub size: u64,
    pub description: String,
    pub homepage: String,
    pub ssh_key: bool,
    /// Virtual disk size in MiB. Reflects what was actually allocated
    /// for the zvol, so the bhyve guest sees a disk of this size.
    pub image_size_mib: u64,
}

/// The manifest for one nocloud image. Fails if `inp.os` is not an OS
/// IMGAPI knows, which would otherwise be written as `unknown`.
pub fn build(inp: &ManifestInputs) -> Result<Image> {
    let os: ImageOs =
        serde_json::from_value(json!(inp.os)).with_context(|| format!("image os {:?}", inp.os))?;
    if os == ImageOs::Unknown {
        anyhow::bail!("image os {:?} is not one IMGAPI knows", inp.os);
    }
    Ok(Image {
        v: 2,
        uuid: inp.uuid,
        owner: Uuid::nil(),
        name: inp.name.clone(),
        version: inp.version.clone(),
        state: ImageState::Active,
        disabled: false,
        public: true,
        published_at: Some(inp.published_at.clone()),
        image_type: Some(ImageType::Zvol),
        os: Some(os),
        files: vec![ImageFile {
            sha1: inp.sha1.clone(),
            size: inp.size,
            compression: FileCompression::Gzip,
            dataset_guid: None,
            stor: None,
            digest: None,
            uncompressed_digest: None,
        }],
        acl: None,
        description: Some(inp.description.clone()),
        homepage: Some(inp.homepage.clone()),
        eula: None,
        icon: None,
        urn: None,
        requirements: Some(ImageRequirements {
            networks: Some(vec![NetworkRequirement {
                name: "net0".to_string(),
                description: Some("public".to_string()),
            }]),
            brand: Some("bhyve".to_string()),
            ssh_key: Some(inp.ssh_key),
            min_ram: None,
            max_ram: None,
            min_platform: Some(HashMap::from([(
                "7.0".to_string(),
                "20260306T044811Z".to_string(),
            )])),
            max_platform: None,
            bootrom: Some("uefi".to_string()),
        }),
        users: None,
        generate_passwords: None,
        inherited_directories: None,
        origin: None,
        nic_driver: Some("virtio".to_string()),
        disk_driver: Some("virtio".to_string()),
        cpu_type: Some("host".to_string()),
        image_size: Some(inp.image_size_mib),
        tags: Some(json!({
            "role": "os",
            "org.smartos:cloudinit_datasource": "nocloud",
        })),
        billing_tags: None,
        traits: None,
        error: None,
        channels: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs() -> ManifestInputs {
        ManifestInputs {
            uuid: Uuid::parse_str("2ab26a8c-616b-4b2f-9a30-b121c044c278")
                .unwrap_or_else(|e| panic!("{e}")),
            name: "alpine-3.23-nocloud".to_string(),
            version: "20260311".to_string(),
            published_at: "2026-03-11T00:00:00Z".to_string(),
            os: "linux".to_string(),
            sha1: "0123456789abcdef0123456789abcdef01234567".to_string(),
            size: 123456,
            description: "Alpine Linux 3.23".to_string(),
            homepage: "https://alpinelinux.org/".to_string(),
            ssh_key: true,
            image_size_mib: 10240,
        }
    }

    /// The manifest the nocloud pipeline writes, field for field.
    #[test]
    fn manifest_has_every_field_imgadm_and_imgapi_expect() {
        let expected = json!({
            "v": 2,
            "uuid": "2ab26a8c-616b-4b2f-9a30-b121c044c278",
            "owner": "00000000-0000-0000-0000-000000000000",
            "name": "alpine-3.23-nocloud",
            "version": "20260311",
            "state": "active",
            "disabled": false,
            "public": true,
            "published_at": "2026-03-11T00:00:00Z",
            "type": "zvol",
            "os": "linux",
            "files": [{
                "sha1": "0123456789abcdef0123456789abcdef01234567",
                "size": 123456,
                "compression": "gzip",
            }],
            "description": "Alpine Linux 3.23",
            "homepage": "https://alpinelinux.org/",
            "requirements": {
                "networks": [{"name": "net0", "description": "public"}],
                "brand": "bhyve",
                "bootrom": "uefi",
                "ssh_key": true,
                "min_platform": {"7.0": "20260306T044811Z"},
            },
            "nic_driver": "virtio",
            "disk_driver": "virtio",
            "cpu_type": "host",
            "image_size": 10240,
            "tags": {
                "role": "os",
                "org.smartos:cloudinit_datasource": "nocloud",
            },
        });
        let image = build(&inputs()).unwrap_or_else(|e| panic!("{e:#}"));
        let written = serde_json::to_value(&image).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(written, expected);
    }

    /// An OS IMGAPI does not know would be written as `unknown`; refuse
    /// it instead.
    #[test]
    fn an_os_imgapi_does_not_know_is_an_error() {
        let mut inp = inputs();
        inp.os = "plan9".to_string();
        assert!(build(&inp).is_err());
    }
}
