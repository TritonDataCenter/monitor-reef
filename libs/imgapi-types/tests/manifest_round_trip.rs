// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Real image manifests must survive a parse and re-serialize through
//! [`imgapi_types::Image`] unchanged: tools that read a manifest, act on
//! it and write it back (imgadm's install path, for one) must not lose
//! or alter anything.
//!
//! `fixtures/real-manifests.json` holds manifests captured on 2026-10-08
//! from `https://images.smartos.org/images` (public images) and
//! `https://updates.tritondatacenter.com/images?channel=release` (Triton
//! core images), chosen to cover every image type and OS seen there,
//! every requirement key, and both string and boolean tag values. A run
//! over all 1,763 manifests from those two sources found none that
//! changed.

use imgapi_types::Image;
use serde_json::Value;

fn fixtures() -> Vec<Value> {
    serde_json::from_str(include_str!("fixtures/real-manifests.json"))
        .unwrap_or_else(|e| panic!("fixtures are not JSON: {e}"))
}

#[test]
fn real_manifests_round_trip_unchanged() {
    for original in fixtures() {
        let uuid = original["uuid"].clone();
        let image: Image = serde_json::from_value(original.clone())
            .unwrap_or_else(|e| panic!("{uuid}: does not parse: {e}"));
        let back = serde_json::to_value(&image)
            .unwrap_or_else(|e| panic!("{uuid}: does not serialize: {e}"));
        assert_eq!(back, original, "{uuid} changed in a round trip");
    }
}

/// Triton core images tag themselves with booleans (e.g.
/// `"smartdc_service": true`), so tag values cannot be typed as strings.
#[test]
fn boolean_tag_values_are_kept() {
    let with_bool = fixtures().into_iter().find(|m| {
        m["tags"]
            .as_object()
            .is_some_and(|t| t.values().any(Value::is_boolean))
    });
    let original = with_bool.unwrap_or_else(|| panic!("no fixture has a boolean tag"));
    let image: Image = serde_json::from_value(original.clone()).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(image.tags, original.get("tags").cloned());
}
