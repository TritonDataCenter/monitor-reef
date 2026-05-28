// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! IMGAPI manifest builder. Mirrors `target/triton-nocloud-images/manifest.in.json`
//! field-for-field, but the fields are typed instead of `sed`-substituted.

use serde_json::{Value, json};
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

pub fn build(inp: &ManifestInputs) -> Value {
    json!({
        "v": 2,
        "uuid": inp.uuid.to_string(),
        "owner": "00000000-0000-0000-0000-000000000000",
        "name": inp.name,
        "version": inp.version,
        "state": "active",
        "disabled": false,
        "public": true,
        "published_at": inp.published_at,
        "type": "zvol",
        "os": inp.os,
        "files": [{
            "sha1": inp.sha1,
            "size": inp.size,
            "compression": "gzip",
        }],
        "description": inp.description,
        "homepage": inp.homepage,
        "requirements": {
            "networks": [{"name": "net0", "description": "public"}],
            "brand": "bhyve",
            "bootrom": "uefi",
            "ssh_key": inp.ssh_key,
            "min_platform": {"7.0": "20260306T044811Z"},
        },
        "nic_driver": "virtio",
        "disk_driver": "virtio",
        "cpu_type": "host",
        "image_size": inp.image_size_mib,
        "tags": {
            "role": "os",
            "org.smartos:cloudinit_datasource": "nocloud",
        },
    })
}
