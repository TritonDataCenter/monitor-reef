// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Deserialization tests for Volume types
//!
//! These tests verify that CloudAPI JSON responses for volumes deserialize
//! correctly, including the `type` → `volume_type` rename and camelCase fields.

mod common;

use cloudapi_api::types::{CreateVolumeRequest, ListVolumesQuery, Volume, VolumeState, VolumeType};
use uuid::Uuid;

#[test]
fn test_volume_basic_deserialize() {
    let volume: Volume = common::deserialize_fixture("volume", "basic.json");

    assert_eq!(
        volume.id,
        Uuid::parse_str("f1e2d3c4-b5a6-9788-7654-321098fedcba").unwrap()
    );
    assert_eq!(volume.name, "my-data-volume");
    assert_eq!(
        volume.owner_uuid,
        Uuid::parse_str("9dce1460-0c4c-4417-ab8b-25ca478c5a78").unwrap()
    );
    assert_eq!(volume.volume_type, VolumeType::Tritonnfs);
    assert_eq!(volume.size, 10240);
    assert_eq!(volume.state, VolumeState::Ready);
}

/// Test that `type` → `volume_type` rename works.
#[test]
fn test_volume_type_rename() {
    let volume: Volume = common::deserialize_fixture("volume", "basic.json");
    assert_eq!(volume.volume_type, VolumeType::Tritonnfs);

    // Verify round-trip serializes back to "type"
    let serialized = serde_json::to_value(&volume).unwrap();
    assert!(
        serialized.get("type").is_some(),
        "should serialize as 'type'"
    );
    assert!(
        serialized.get("volume_type").is_none(),
        "should not serialize as 'volume_type'"
    );
}

#[test]
fn test_volume_networks() {
    let volume: Volume = common::deserialize_fixture("volume", "basic.json");

    assert_eq!(volume.networks.len(), 1);
    assert_eq!(
        volume.networks[0],
        Uuid::parse_str("ac336e0f-8532-4e0e-a19e-7cd5bdd62817").unwrap()
    );
}

#[test]
fn test_volume_filesystem_path() {
    let volume: Volume = common::deserialize_fixture("volume", "basic.json");

    assert_eq!(
        volume.filesystem_path.as_deref(),
        Some("nfs://10.88.88.10/exports/f1e2d3c4-b5a6-9788-7654-321098fedcba")
    );
}

#[test]
fn test_volume_tags() {
    let volume: Volume = common::deserialize_fixture("volume", "basic.json");

    assert_eq!(volume.tags.len(), 2);
    assert_eq!(volume.tags["env"], "production");
    assert_eq!(volume.tags["app"], "myservice");
}

#[test]
fn test_volume_refs() {
    let volume: Volume = common::deserialize_fixture("volume", "basic.json");

    assert_eq!(volume.refs.len(), 2);
    assert_eq!(
        volume.refs[0],
        Uuid::parse_str("a1234567-1234-1234-1234-123456789012").unwrap()
    );
}

/// Test minimal volume with empty collections.
#[test]
fn test_volume_minimal_deserialize() {
    let volume: Volume = common::deserialize_fixture("volume", "minimal.json");

    assert_eq!(volume.name, "empty-volume");
    assert_eq!(volume.state, VolumeState::Creating);
    assert!(volume.networks.is_empty());
    assert!(volume.tags.is_empty());
    assert!(volume.refs.is_empty());
    assert!(volume.filesystem_path.is_none());
}

/// Test deserialization of all volume state enum variants.
#[test]
fn test_volume_state_variants() {
    let cases = [
        ("creating", VolumeState::Creating),
        ("ready", VolumeState::Ready),
        ("failed", VolumeState::Failed),
        ("deleting", VolumeState::Deleting),
    ];

    for (json_value, expected) in cases {
        let json = format!(r#""{}""#, json_value);
        let parsed: VolumeState = serde_json::from_str(&json)
            .unwrap_or_else(|_| panic!("Failed to parse volume state: {}", json_value));
        assert_eq!(parsed, expected);
    }
}

/// Test VolumeType wire format serialization.
#[test]
fn test_volume_type_wire_format() {
    let json = r#""tritonnfs""#;
    let parsed: VolumeType = serde_json::from_str(json).unwrap();
    assert_eq!(parsed, VolumeType::Tritonnfs);

    // Round-trip
    let serialized = serde_json::to_string(&VolumeType::Tritonnfs).unwrap();
    assert_eq!(serialized, r#""tritonnfs""#);
}

/// Test forward compatibility: unknown volume types deserialize as Unknown.
#[test]
fn test_volume_type_unknown_variant() {
    let json = r#""somefuturetype""#;
    let parsed: VolumeType = serde_json::from_str(json).unwrap();
    assert_eq!(parsed, VolumeType::Unknown);
}

/// Test forward compatibility: unknown volume states deserialize as Unknown.
#[test]
fn test_volume_state_unknown_variant() {
    let json = r#""migrating""#;
    let parsed: VolumeState = serde_json::from_str(json).unwrap();
    assert_eq!(parsed, VolumeState::Unknown);
}

/// Test deserialization of a volume list.
#[test]
fn test_volume_list_deserialize() {
    let json = format!(
        "[{}, {}]",
        common::load_fixture("volume", "basic.json"),
        common::load_fixture("volume", "minimal.json")
    );

    let volumes: Vec<Volume> = serde_json::from_str(&json).expect("Failed to parse volume list");
    assert_eq!(volumes.len(), 2);
    assert_eq!(volumes[0].state, VolumeState::Ready);
    assert_eq!(volumes[1].state, VolumeState::Creating);
}

/// Test round-trip serialization/deserialization preserves data.
#[test]
fn test_volume_round_trip() {
    let original: Volume = common::deserialize_fixture("volume", "basic.json");
    let serialized = serde_json::to_string(&original).unwrap();
    let deserialized: Volume = serde_json::from_str(&serialized).unwrap();

    assert_eq!(original.id, deserialized.id);
    assert_eq!(original.name, deserialized.name);
    assert_eq!(original.volume_type, deserialized.volume_type);
    assert_eq!(original.state, deserialized.state);
    assert_eq!(original.size, deserialized.size);
}

/// Test that CreateVolumeRequest deserializes correctly without a size field.
///
/// CloudAPI treats size as optional in volume creation requests. When omitted,
/// the server selects the smallest available NFS billing package size. This
/// test ensures our Rust type accepts requests without size, matching the
/// actual sdc-cloudapi behavior (lib/endpoints/volumes.js validates size only
/// when `reqParams.size !== undefined`).
#[test]
fn test_create_volume_request_without_size() {
    let json = r#"{"name": "my-vol", "type": "tritonnfs"}"#;
    let req: CreateVolumeRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.name.as_deref(), Some("my-vol"));
    assert_eq!(req.volume_type, Some(VolumeType::Tritonnfs));
    assert!(req.size.is_none(), "size should be None when omitted");
}

/// Test that CreateVolumeRequest deserializes correctly with an explicit size.
#[test]
fn test_create_volume_request_with_size() {
    let json = r#"{"name": "my-vol", "type": "tritonnfs", "size": 10240}"#;
    let req: CreateVolumeRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.size, Some(10240));
}

// --- ListVolumesQuery tests ---

/// Test that all ListVolumesQuery fields deserialize correctly.
#[test]
fn test_list_volumes_query_all_fields() {
    let json = r#"{
        "name": "my-data-volume",
        "state": "ready",
        "size": 10240,
        "type": "tritonnfs",
        "predicate": "{\"eq\": [\"name\", \"my-data-volume\"]}"
    }"#;
    let query: ListVolumesQuery = serde_json::from_str(json).unwrap();
    assert_eq!(query.name.as_deref(), Some("my-data-volume"));
    assert_eq!(query.state.as_deref(), Some("ready"));
    assert_eq!(query.size, Some(10240));
    assert_eq!(query.volume_type.as_deref(), Some("tritonnfs"));
    assert!(query.predicate.is_some());
}

/// Test that an empty query string deserializes with all fields as None.
#[test]
fn test_list_volumes_query_empty() {
    let json = r#"{}"#;
    let query: ListVolumesQuery = serde_json::from_str(json).unwrap();
    assert!(query.name.is_none());
    assert!(query.state.is_none());
    assert!(query.size.is_none());
    assert!(query.volume_type.is_none());
    assert!(query.predicate.is_none());
}

/// Test that `type` in the query maps to `volume_type` via serde rename.
#[test]
fn test_list_volumes_query_type_rename() {
    // Must use "type" (the wire name), not "volume_type"
    let json = r#"{"type": "tritonnfs"}"#;
    let query: ListVolumesQuery = serde_json::from_str(json).unwrap();
    assert_eq!(query.volume_type.as_deref(), Some("tritonnfs"));

    // "volume_type" should NOT work — it's not the wire name
    let json = r#"{"volume_type": "tritonnfs"}"#;
    let query: ListVolumesQuery = serde_json::from_str(json).unwrap();
    assert!(
        query.volume_type.is_none(),
        "should not accept 'volume_type' — wire format uses 'type'"
    );
}

/// Test that state and type are strings (not enums) at the query param level,
/// matching how the Node.js CloudAPI passes them through without validation.
#[test]
fn test_list_volumes_query_passthrough_strings() {
    // Arbitrary state string should be accepted — CloudAPI doesn't validate
    let json = r#"{"state": "some-future-state", "type": "some-future-type"}"#;
    let query: ListVolumesQuery = serde_json::from_str(json).unwrap();
    assert_eq!(query.state.as_deref(), Some("some-future-state"));
    assert_eq!(query.volume_type.as_deref(), Some("some-future-type"));
}

/// Test individual filter fields in isolation.
#[test]
fn test_list_volumes_query_single_fields() {
    let json = r#"{"name": "backup-vol"}"#;
    let query: ListVolumesQuery = serde_json::from_str(json).unwrap();
    assert_eq!(query.name.as_deref(), Some("backup-vol"));
    assert!(query.state.is_none());

    let json = r#"{"size": 20480}"#;
    let query: ListVolumesQuery = serde_json::from_str(json).unwrap();
    assert_eq!(query.size, Some(20480));
    assert!(query.name.is_none());
}
