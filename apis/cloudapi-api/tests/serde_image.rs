// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Deserialization tests for Image types
//!
//! These tests verify that CloudAPI JSON responses for images deserialize
//! correctly, including the snake_case `published_at` field and hyphenated
//! `role-tag` and `type` fields.

mod common;

use cloudapi_api::types::{Image, ImageState, ImageType};
use uuid::Uuid;
use vmapi_api::Brand as VmapiBrand;

#[test]
fn test_image_basic_deserialize() {
    let image: Image = common::deserialize_fixture("image", "basic.json");

    assert_eq!(
        image.id,
        Uuid::parse_str("2b683a82-a066-11e3-97ab-2faa44701c5a").unwrap()
    );
    assert_eq!(image.name, "base-64-lts");
    assert_eq!(image.version, "21.4.0");
    assert_eq!(image.os, "smartos");
    assert_eq!(image.image_type, ImageType::ZoneDataset);
    assert_eq!(
        image.description.as_deref(),
        Some("A 64-bit SmartOS image with just essential packages installed.")
    );
    assert_eq!(image.state, Some(ImageState::Active));
    assert_eq!(image.public, Some(true));
    assert_eq!(
        image.owner,
        Some(Uuid::parse_str("9dce1460-0c4c-4417-ab8b-25ca478c5a78").unwrap())
    );
}

/// Test that `published_at` (snake_case) deserializes correctly.
/// The wire format uses snake_case `published_at` for this field.
#[test]
fn test_image_published_at_snake_case() {
    let image: Image = common::deserialize_fixture("image", "basic.json");

    assert!(
        image.published_at.is_some(),
        "published_at should deserialize from snake_case"
    );
    assert_eq!(
        image.published_at,
        Some(
            chrono::DateTime::parse_from_rfc3339("2021-04-01T00:42:24Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
        )
    );
}

#[test]
fn test_image_requirements() {
    let image: Image = common::deserialize_fixture("image", "basic.json");

    assert_eq!(image.requirements.min_ram, Some(256));
    assert!(image.requirements.max_ram.is_none());
    assert!(image.requirements.brand.is_none());
    assert!(image.requirements.bootrom.is_none());
}

#[test]
fn test_image_zvol_with_requirements() {
    let image: Image = common::deserialize_fixture("image", "zvol.json");

    assert_eq!(image.image_type, ImageType::Zvol);
    assert_eq!(image.requirements.min_ram, Some(1024));
    assert_eq!(image.requirements.max_ram, Some(65536));
    assert_eq!(image.requirements.brand, Some(VmapiBrand::Bhyve));
    assert_eq!(image.requirements.bootrom.as_deref(), Some("uefi"));
    assert_eq!(image.image_size, Some(10240));
}

/// Test that `role-tag` (hyphenated) deserializes correctly.
#[test]
fn test_image_role_tags() {
    let image: Image = common::deserialize_fixture("image", "zvol.json");

    assert!(
        image.role_tag.is_some(),
        "role-tag should deserialize from hyphenated key"
    );
    assert_eq!(
        image.role_tag.as_ref().unwrap(),
        &vec!["admin".to_string(), "operator".to_string()]
    );
}

#[test]
fn test_image_files() {
    let image: Image = common::deserialize_fixture("image", "basic.json");

    let files = image.files.expect("files should be present");
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].compression, "gzip");
    assert_eq!(files[0].sha1, "3bebb6ae2f2e1ac1a960e4b8d0a06e23b0e52b2d");
    assert_eq!(files[0].size, 112713776);
}

#[test]
fn test_image_acl() {
    let image: Image = common::deserialize_fixture("image", "basic.json");

    let acl = image.acl.expect("acl should be present");
    assert_eq!(acl.len(), 1);
    assert_eq!(
        acl[0],
        Uuid::parse_str("b4bb1880-8c2c-11e5-8994-28cfe91f7baf").unwrap()
    );
}

/// Test that minimal images with only required fields deserialize correctly.
#[test]
fn test_image_minimal_deserialize() {
    let image: Image = common::deserialize_fixture("image", "minimal.json");

    assert_eq!(image.name, "test-image");
    assert_eq!(image.version, "1.0.0");
    assert_eq!(image.os, "linux");
    assert_eq!(image.image_type, ImageType::LxDataset);
    assert!(image.description.is_none());
    assert!(image.published_at.is_none());
    assert!(image.owner.is_none());
    assert!(image.public.is_none());
    assert!(image.state.is_none());
    assert!(image.tags.is_none());
    assert!(image.files.is_none());
    assert!(image.acl.is_none());
    assert!(image.role_tag.is_none());
    assert!(image.image_size.is_none());
    assert!(image.error.is_none());
}

/// Test deserialization of all image type enum variants.
#[test]
fn test_image_type_variants() {
    let cases = [
        ("zone-dataset", ImageType::ZoneDataset),
        ("lx-dataset", ImageType::LxDataset),
        ("zvol", ImageType::Zvol),
        ("docker", ImageType::Docker),
        ("lxd", ImageType::Lxd),
        ("other", ImageType::Other),
    ];

    for (json_value, expected) in cases {
        let json = format!(r#""{}""#, json_value);
        let parsed: ImageType = serde_json::from_str(&json)
            .unwrap_or_else(|_| panic!("Failed to parse image type: {}", json_value));
        assert_eq!(parsed, expected);
    }
}

/// Test forward compatibility: unknown image types deserialize as Unknown.
#[test]
fn test_image_type_unknown_variant() {
    let json = r#""new-future-type""#;
    let parsed: ImageType = serde_json::from_str(json).unwrap();
    assert_eq!(parsed, ImageType::Unknown);
}

/// Test deserialization of all image state enum variants.
#[test]
fn test_image_state_variants() {
    let cases = [
        ("active", ImageState::Active),
        ("unactivated", ImageState::Unactivated),
        ("disabled", ImageState::Disabled),
        ("creating", ImageState::Creating),
        ("failed", ImageState::Failed),
    ];

    for (json_value, expected) in cases {
        let json = format!(r#""{}""#, json_value);
        let parsed: ImageState = serde_json::from_str(&json)
            .unwrap_or_else(|_| panic!("Failed to parse image state: {}", json_value));
        assert_eq!(parsed, expected);
    }
}

/// Test forward compatibility: unknown image states deserialize as Unknown.
#[test]
fn test_image_state_unknown_variant() {
    let json = r#""some-new-state""#;
    let parsed: ImageState = serde_json::from_str(json).unwrap();
    assert_eq!(parsed, ImageState::Unknown);
}

/// Test deserialization of an image list.
#[test]
fn test_image_list_deserialize() {
    let json = format!(
        "[{}, {}]",
        common::load_fixture("image", "basic.json"),
        common::load_fixture("image", "minimal.json")
    );

    let images: Vec<Image> = serde_json::from_str(&json).expect("Failed to parse image list");
    assert_eq!(images.len(), 2);
    assert_eq!(images[0].name, "base-64-lts");
    assert_eq!(images[1].name, "test-image");
}

/// Test round-trip serialization/deserialization preserves data.
#[test]
fn test_image_round_trip() {
    let original: Image = common::deserialize_fixture("image", "basic.json");
    let serialized = serde_json::to_string(&original).unwrap();
    let deserialized: Image = serde_json::from_str(&serialized).unwrap();

    assert_eq!(original.id, deserialized.id);
    assert_eq!(original.name, deserialized.name);
    assert_eq!(original.version, deserialized.version);
    assert_eq!(original.image_type, deserialized.image_type);
    assert_eq!(original.state, deserialized.state);
    assert_eq!(original.published_at, deserialized.published_at);
}
