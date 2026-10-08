// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! [`Image::validate`] checks what serde cannot: the manifest version,
//! the form of each file's sha1, and that an active image has a file.
//! The checks match smartos-live's `imgapi-manifest` crate, so a caller
//! moving to these types keeps them.

use imgapi_types::{Image, ImageState, ManifestError};
use serde_json::Value;

fn real_manifests() -> Vec<Image> {
    let all: Vec<Value> = serde_json::from_str(include_str!("fixtures/real-manifests.json"))
        .unwrap_or_else(|e| panic!("fixtures are not JSON: {e}"));
    all.into_iter()
        .map(|m| serde_json::from_value(m).unwrap_or_else(|e| panic!("{e}")))
        .collect()
}

fn one() -> Image {
    real_manifests()
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("no fixtures"))
}

#[test]
fn real_manifests_are_valid() {
    for image in real_manifests() {
        assert!(
            image.validate().is_ok(),
            "{}: {:?}",
            image.uuid,
            image.validate()
        );
    }
}

#[test]
fn a_manifest_version_other_than_2_is_rejected() {
    let mut image = one();
    image.v = 1;
    assert!(matches!(
        image.validate(),
        Err(ManifestError::SchemaMismatch {
            got: 1,
            expected: 2
        })
    ));
}

#[test]
fn an_uppercase_sha1_is_rejected() {
    let mut image = one();
    image.files[0].sha1 = image.files[0].sha1.to_uppercase();
    assert!(matches!(
        image.validate(),
        Err(ManifestError::BadSha1 { index: 0, .. })
    ));
}

#[test]
fn a_short_sha1_is_rejected() {
    let mut image = one();
    image.files[0].sha1 = "abc123".to_string();
    assert!(matches!(
        image.validate(),
        Err(ManifestError::BadSha1 { index: 0, .. })
    ));
}

#[test]
fn an_active_image_without_files_is_rejected() {
    let mut image = one();
    image.state = ImageState::Active;
    image.files.clear();
    assert!(matches!(
        image.validate(),
        Err(ManifestError::NoFilesForActive)
    ));
}

#[test]
fn an_image_still_being_created_may_have_no_files() {
    let mut image = one();
    image.state = ImageState::Unactivated;
    image.files.clear();
    assert!(image.validate().is_ok());
}
