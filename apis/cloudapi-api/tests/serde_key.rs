// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Deserialization tests for access key types
//!
//! These tests verify that CloudAPI JSON responses for access keys
//! deserialize correctly, particularly:
//! - `AccessKeyStatus` uses PascalCase ("Active", "Inactive", "Expired")
//!   with no `rename_all` — this matches the Node.js CloudAPI wire format
//! - `CredentialType` uses lowercase ("permanent", "temporary")
//! - Unknown status values fall through to `Unknown` via `#[serde(other)]`

mod common;

use cloudapi_api::types::{AccessKey, AccessKeyStatus, CredentialType};

#[test]
fn test_active_permanent_key() {
    let key: AccessKey = common::deserialize_fixture("accesskey", "active.json");

    assert_eq!(key.accesskeyid, "AKID1234567890");
    assert_eq!(key.status, AccessKeyStatus::Active);
    assert_eq!(key.credentialtype, CredentialType::Permanent);
    assert_eq!(key.description.as_deref(), Some("My test key"));
    assert!(key.expiration.is_none());
}

#[test]
fn test_inactive_key() {
    let key: AccessKey = common::deserialize_fixture("accesskey", "inactive.json");

    assert_eq!(key.status, AccessKeyStatus::Inactive);
    assert_eq!(key.credentialtype, CredentialType::Permanent);
    assert!(key.description.is_none());
    assert!(key.expiration.is_none());
}

#[test]
fn test_expired_temporary_key() {
    let key: AccessKey = common::deserialize_fixture("accesskey", "expired_temporary.json");

    assert_eq!(key.status, AccessKeyStatus::Expired);
    assert_eq!(key.credentialtype, CredentialType::Temporary);
    assert!(key.expiration.is_some());
}

/// Verify that AccessKeyStatus serializes back to PascalCase (no rename_all).
#[test]
fn test_status_round_trip_wire_format() {
    let key: AccessKey = common::deserialize_fixture("accesskey", "active.json");
    let serialized = serde_json::to_value(&key).unwrap();

    assert_eq!(serialized["status"], "Active");
    assert_eq!(serialized["credentialtype"], "permanent");
}

/// Unknown status values must fall through to Unknown, not panic.
#[test]
fn test_unknown_status_forward_compat() {
    let json = r#"{
        "accesskeyid": "AKID0000000000",
        "status": "Suspended",
        "credentialtype": "permanent",
        "created": "2024-01-01T00:00:00.000Z",
        "updated": "2024-01-01T00:00:00.000Z"
    }"#;
    let key: AccessKey = serde_json::from_str(json).unwrap();
    assert_eq!(key.status, AccessKeyStatus::Unknown);
}

/// Unknown credential type values must fall through to Unknown.
#[test]
fn test_unknown_credential_type_forward_compat() {
    let json = r#"{
        "accesskeyid": "AKID0000000000",
        "status": "Active",
        "credentialtype": "session",
        "created": "2024-01-01T00:00:00.000Z",
        "updated": "2024-01-01T00:00:00.000Z"
    }"#;
    let key: AccessKey = serde_json::from_str(json).unwrap();
    assert_eq!(key.credentialtype, CredentialType::Unknown);
}
