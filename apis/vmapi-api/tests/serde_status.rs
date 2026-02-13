// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Deserialization tests for Status and misc types
//!
//! Tests for VmStatus, StatusesResponse, RoleTagsResponse, MetadataType,
//! and PingResponse.

use vmapi_api::types::{
    MetadataType, PingResponse, RoleTagsResponse, StatusesResponse, VmState, VmStatus,
};

/// Test VmStatus deserialization.
#[test]
fn test_vm_status_deserialize() {
    let json = r#"{
        "state": "running",
        "last_modified": "2024-06-15T12:00:00.000Z"
    }"#;

    let status: VmStatus = serde_json::from_str(json).unwrap();
    assert_eq!(status.state, VmState::Running);
    assert_eq!(
        status.last_modified.as_deref(),
        Some("2024-06-15T12:00:00.000Z")
    );
}

/// Test StatusesResponse (HashMap<Uuid, VmStatus>) deserialization.
#[test]
fn test_statuses_response_deserialize() {
    let json = r#"{
        "a1234567-1234-1234-1234-123456789012": {
            "state": "running",
            "last_modified": "2024-06-15T12:00:00.000Z"
        },
        "b2345678-2345-2345-2345-234567890123": {
            "state": "stopped"
        }
    }"#;

    let statuses: StatusesResponse = serde_json::from_str(json).unwrap();
    assert_eq!(statuses.len(), 2);

    let running_uuid = uuid::Uuid::parse_str("a1234567-1234-1234-1234-123456789012").unwrap();
    assert_eq!(statuses[&running_uuid].state, VmState::Running);

    let stopped_uuid = uuid::Uuid::parse_str("b2345678-2345-2345-2345-234567890123").unwrap();
    assert_eq!(statuses[&stopped_uuid].state, VmState::Stopped);
}

/// Test RoleTagsResponse deserialization.
#[test]
fn test_role_tags_response_deserialize() {
    let json = r#"{
        "role_tags": ["admin", "operator", "reader"]
    }"#;

    let response: RoleTagsResponse = serde_json::from_str(json).unwrap();
    assert_eq!(response.role_tags.len(), 3);
    assert_eq!(response.role_tags[0], "admin");
    assert_eq!(response.role_tags[1], "operator");
    assert_eq!(response.role_tags[2], "reader");
}

/// Test MetadataType serialization.
#[test]
fn test_metadata_type_serialize() {
    let json = serde_json::to_string(&MetadataType::CustomerMetadata).unwrap();
    assert_eq!(json, r#""customer_metadata""#);

    let json = serde_json::to_string(&MetadataType::InternalMetadata).unwrap();
    assert_eq!(json, r#""internal_metadata""#);

    let json = serde_json::to_string(&MetadataType::Tags).unwrap();
    assert_eq!(json, r#""tags""#);
}

/// Test MetadataType Display implementation.
#[test]
fn test_metadata_type_display() {
    assert_eq!(
        MetadataType::CustomerMetadata.to_string(),
        "customer_metadata"
    );
    assert_eq!(
        MetadataType::InternalMetadata.to_string(),
        "internal_metadata"
    );
    assert_eq!(MetadataType::Tags.to_string(), "tags");
}

/// Test PingResponse deserialization.
#[test]
fn test_ping_response_deserialize() {
    let json = r#"{
        "status": "OK",
        "healthy": true,
        "backend_status": "online"
    }"#;

    let ping: PingResponse = serde_json::from_str(json).unwrap();
    assert_eq!(ping.status, "OK");
    assert_eq!(ping.healthy, Some(true));
    assert_eq!(ping.backend_status.as_deref(), Some("online"));
}

/// Test PingResponse minimal deserialization.
#[test]
fn test_ping_response_minimal() {
    let json = r#"{"status": "OK"}"#;

    let ping: PingResponse = serde_json::from_str(json).unwrap();
    assert_eq!(ping.status, "OK");
    assert!(ping.healthy.is_none());
    assert!(ping.backend_status.is_none());
}
