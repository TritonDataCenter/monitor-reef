// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Test utilities for cloudapi-api deserialization tests

use std::path::PathBuf;

/// Get the path to the fixtures directory
pub fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
}

/// Load a fixture file as a string
pub fn load_fixture(category: &str, name: &str) -> String {
    let path = fixtures_dir().join(category).join(name);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("Failed to read fixture {}/{}: {}", category, name, e))
}

/// Deserialize a fixture file into a type
pub fn deserialize_fixture<T: serde::de::DeserializeOwned>(category: &str, name: &str) -> T {
    let json = load_fixture(category, name);
    serde_json::from_str(&json)
        .unwrap_or_else(|e| panic!("Failed to parse fixture {}/{}: {}", category, name, e))
}
