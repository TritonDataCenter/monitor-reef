// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Deserialization tests for firewall rule types
//!
//! These tests verify that CloudAPI JSON responses for firewall rules
//! deserialize correctly, particularly the hyphenated `role-tag` field
//! which uses an explicit `#[serde(rename = "role-tag")]`.

mod common;

use cloudapi_api::types::FirewallRule;
use uuid::Uuid;

#[test]
fn test_firewall_rule_full() {
    let rule: FirewallRule = common::deserialize_fixture("firewall", "basic.json");

    assert_eq!(
        rule.id,
        Uuid::parse_str("38de17c4-39e8-48c7-a168-0f58083de860").unwrap()
    );
    assert_eq!(rule.rule, "FROM any TO all vms ALLOW tcp PORT 22");
    assert!(rule.enabled);
    assert!(!rule.log);
    assert_eq!(rule.global, Some(true));
    assert_eq!(rule.description.as_deref(), Some("Allow SSH from anywhere"));
    assert!(rule.created.is_some());
    assert!(rule.updated.is_some());

    let tags = rule.role_tag.expect("role_tag should be present");
    assert_eq!(tags, vec!["admin", "operator"]);
}

#[test]
fn test_firewall_rule_minimal() {
    let rule: FirewallRule = common::deserialize_fixture("firewall", "minimal.json");

    assert_eq!(
        rule.id,
        Uuid::parse_str("c8cec95e-5f49-4a52-b850-0f40c8b90a65").unwrap()
    );
    assert!(!rule.enabled);
    assert!(rule.role_tag.is_none());
    assert!(rule.created.is_none());
    assert!(rule.description.is_none());
}

/// Verify `role-tag` serializes with the hyphen, not as camelCase `roleTag`.
#[test]
fn test_firewall_rule_role_tag_wire_format() {
    let rule: FirewallRule = common::deserialize_fixture("firewall", "basic.json");
    let serialized = serde_json::to_value(&rule).unwrap();

    assert!(
        serialized.get("role-tag").is_some(),
        "should serialize as hyphenated 'role-tag'"
    );
    assert!(
        serialized.get("roleTag").is_none(),
        "should not serialize as camelCase 'roleTag'"
    );
    assert!(
        serialized.get("role_tag").is_none(),
        "should not serialize as snake_case 'role_tag'"
    );
}

/// Verify camelCase fields work alongside the hyphenated role-tag override.
#[test]
fn test_firewall_rule_round_trip() {
    let original: FirewallRule = common::deserialize_fixture("firewall", "basic.json");
    let serialized = serde_json::to_string(&original).unwrap();
    let deserialized: FirewallRule = serde_json::from_str(&serialized).unwrap();

    assert_eq!(original.id, deserialized.id);
    assert_eq!(original.rule, deserialized.rule);
    assert_eq!(original.enabled, deserialized.enabled);
    assert_eq!(original.role_tag, deserialized.role_tag);
}
