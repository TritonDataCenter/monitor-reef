// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Deserialization tests for RBAC role types
//!
//! These tests verify that CloudAPI JSON responses for roles deserialize
//! correctly, particularly the `members` and `default_members` fields
//! which are structured MemberRef objects (not plain strings).

mod common;

use cloudapi_api::types::{MemberType, Role};
use uuid::Uuid;

/// Real-world response shape: members are objects with id, login, type.
/// This is the bug that was fixed — previously members was Vec<String>
/// which failed to deserialize the object form.
#[test]
fn test_role_with_structured_members() {
    let role: Role = common::deserialize_fixture("role", "with_members.json");

    assert_eq!(
        role.id,
        Uuid::parse_str("d906f945-9f88-43b8-9b79-559321bb9b2d").unwrap()
    );
    assert_eq!(role.name, "MantaWriter");

    // Members should be structured MemberRef objects
    assert_eq!(role.members.len(), 1);
    let member = &role.members[0];
    assert_eq!(
        member.id,
        Some(Uuid::parse_str("4f7afff0-a651-c643-ee01-ae536b30b59d").unwrap())
    );
    assert_eq!(member.login.as_deref(), Some("manta.www"));
    assert!(matches!(member.member_type, MemberType::Subuser));

    // Default members
    assert_eq!(role.default_members.len(), 1);
    let default_member = &role.default_members[0];
    assert_eq!(default_member.login.as_deref(), Some("ops.admin"));
    assert!(matches!(default_member.member_type, MemberType::Account));
    assert_eq!(default_member.default, Some(true));

    // Policies
    assert_eq!(role.policies.len(), 1);
    assert_eq!(role.policies[0].name.as_deref(), Some("MantaObjRW"));
}

/// Empty members array should deserialize to empty Vec, not fail.
#[test]
fn test_role_empty_members() {
    let role: Role = common::deserialize_fixture("role", "empty_members.json");

    assert_eq!(role.name, "PortalUser");
    assert!(role.members.is_empty());
    // default_members absent from JSON should default to empty via #[serde(default)]
    assert!(role.default_members.is_empty());
    assert_eq!(role.policies.len(), 1);
}

/// Minimal role with no members/default_members/policies fields at all.
/// Tests that #[serde(default)] works for all optional collection fields.
#[test]
fn test_role_minimal() {
    let role: Role = common::deserialize_fixture("role", "minimal.json");

    assert_eq!(
        role.id,
        Uuid::parse_str("e581f508-9f24-c038-dff9-ae255fda2a6a").unwrap()
    );
    assert_eq!(role.name, "ReadOnly");
    assert!(role.members.is_empty());
    assert!(role.default_members.is_empty());
    assert!(role.policies.is_empty());
}

/// Verify that the old format (members as plain strings) fails to
/// deserialize. This documents the wire format contract.
#[test]
fn test_role_string_members_rejected() {
    let json = r#"{
        "id": "d906f945-9f88-43b8-9b79-559321bb9b2d",
        "name": "BadFormat",
        "members": ["plainstring"]
    }"#;

    let result = serde_json::from_str::<Role>(json);
    assert!(
        result.is_err(),
        "plain string members should fail to deserialize into MemberRef"
    );
}

/// MemberType forward compatibility: unknown types fall through to Unknown.
#[test]
fn test_member_type_unknown_variant() {
    let json = r#"{
        "id": "d906f945-9f88-43b8-9b79-559321bb9b2d",
        "name": "FutureRole",
        "members": [
            {
                "type": "service-account",
                "login": "svc.monitor"
            }
        ]
    }"#;

    let role: Role = serde_json::from_str(json).unwrap();
    assert_eq!(role.members.len(), 1);
    assert!(matches!(role.members[0].member_type, MemberType::Unknown));
    assert_eq!(role.members[0].login.as_deref(), Some("svc.monitor"));
}

/// Round-trip: serialize then deserialize preserves all fields.
#[test]
fn test_role_round_trip() {
    let original: Role = common::deserialize_fixture("role", "with_members.json");
    let serialized = serde_json::to_string(&original).unwrap();
    let deserialized: Role = serde_json::from_str(&serialized).unwrap();

    assert_eq!(original.id, deserialized.id);
    assert_eq!(original.name, deserialized.name);
    assert_eq!(original.members.len(), deserialized.members.len());
    assert_eq!(original.members[0].login, deserialized.members[0].login);
    assert_eq!(
        original.default_members.len(),
        deserialized.default_members.len()
    );
    assert_eq!(original.policies.len(), deserialized.policies.len());
}
