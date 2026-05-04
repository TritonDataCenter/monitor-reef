// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Deserialization tests for ListPackagesQuery
//!
//! The CloudAPI list_packages and head_packages endpoints accept query
//! parameters to filter the package list. These are passed through to PAPI.
//! These tests verify serde deserialization of the query struct, which
//! mirrors what Dropshot does when parsing query strings.

use cloudapi_api::types::ListPackagesQuery;

/// Test that all ListPackagesQuery fields deserialize correctly.
#[test]
fn test_list_packages_query_all_fields() {
    let json = r#"{
        "name": "sample-1G",
        "memory": 1024,
        "disk": 25600,
        "swap": 4096,
        "lwps": 4000,
        "vcpus": 1,
        "version": "1.0.0",
        "group": "sample",
        "flexible_disk": true,
        "brand": "bhyve"
    }"#;
    let query: ListPackagesQuery = serde_json::from_str(json).unwrap();
    assert_eq!(query.name.as_deref(), Some("sample-1G"));
    assert_eq!(query.memory, Some(1024));
    assert_eq!(query.disk, Some(25600));
    assert_eq!(query.swap, Some(4096));
    assert_eq!(query.lwps, Some(4000));
    assert_eq!(query.vcpus, Some(1));
    assert_eq!(query.version.as_deref(), Some("1.0.0"));
    assert_eq!(query.group.as_deref(), Some("sample"));
    assert_eq!(query.flexible_disk, Some(true));
    assert_eq!(query.brand.as_deref(), Some("bhyve"));
}

/// Test that an empty query string deserializes with all fields as None.
#[test]
fn test_list_packages_query_empty() {
    let json = r#"{}"#;
    let query: ListPackagesQuery = serde_json::from_str(json).unwrap();
    assert!(query.name.is_none());
    assert!(query.memory.is_none());
    assert!(query.disk.is_none());
    assert!(query.swap.is_none());
    assert!(query.lwps.is_none());
    assert!(query.vcpus.is_none());
    assert!(query.version.is_none());
    assert!(query.group.is_none());
    assert!(query.flexible_disk.is_none());
    assert!(query.brand.is_none());
}

/// Test filtering by name only (common terraform-provider-triton use case).
#[test]
fn test_list_packages_query_name_only() {
    let json = r#"{"name": "g4-highcpu-4G"}"#;
    let query: ListPackagesQuery = serde_json::from_str(json).unwrap();
    assert_eq!(query.name.as_deref(), Some("g4-highcpu-4G"));
    assert!(query.memory.is_none());
    assert!(query.disk.is_none());
}

/// Test filtering by numeric fields (memory, disk, swap).
#[test]
fn test_list_packages_query_numeric_fields() {
    let json = r#"{"memory": 2048, "disk": 51200, "swap": 8192}"#;
    let query: ListPackagesQuery = serde_json::from_str(json).unwrap();
    assert_eq!(query.memory, Some(2048));
    assert_eq!(query.disk, Some(51200));
    assert_eq!(query.swap, Some(8192));
    assert!(query.name.is_none());
}

/// Test that flexible_disk accepts both true and false.
#[test]
fn test_list_packages_query_flexible_disk_values() {
    let json = r#"{"flexible_disk": true}"#;
    let query: ListPackagesQuery = serde_json::from_str(json).unwrap();
    assert_eq!(query.flexible_disk, Some(true));

    let json = r#"{"flexible_disk": false}"#;
    let query: ListPackagesQuery = serde_json::from_str(json).unwrap();
    assert_eq!(query.flexible_disk, Some(false));
}

/// Test that brand is a string (not an enum) at the query param level,
/// matching how CloudAPI passes it through to PAPI without validation.
#[test]
fn test_list_packages_query_brand_is_passthrough_string() {
    let json = r#"{"brand": "bhyve"}"#;
    let query: ListPackagesQuery = serde_json::from_str(json).unwrap();
    assert_eq!(query.brand.as_deref(), Some("bhyve"));

    // Arbitrary brand string should be accepted
    let json = r#"{"brand": "some-future-brand"}"#;
    let query: ListPackagesQuery = serde_json::from_str(json).unwrap();
    assert_eq!(query.brand.as_deref(), Some("some-future-brand"));
}

/// Test combining multiple filter criteria (common PAPI query pattern).
#[test]
fn test_list_packages_query_multi_filter() {
    let json = r#"{"brand": "bhyve", "flexible_disk": true, "group": "production"}"#;
    let query: ListPackagesQuery = serde_json::from_str(json).unwrap();
    assert_eq!(query.brand.as_deref(), Some("bhyve"));
    assert_eq!(query.flexible_disk, Some(true));
    assert_eq!(query.group.as_deref(), Some("production"));
    assert!(query.name.is_none());
    assert!(query.memory.is_none());
}
