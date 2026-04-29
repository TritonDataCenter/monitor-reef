// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Deserialization tests for Account types
//!
//! These tests verify that CloudAPI JSON responses for accounts deserialize
//! correctly, particularly the snake_case `triton_cns_enabled` field that
//! differs from the camelCase convention.

mod common;

use chrono::TimeZone;
use cloudapi_api::types::Account;
use uuid::Uuid;

#[test]
fn test_account_full_deserialize() {
    let account: Account = common::deserialize_fixture("account", "full.json");

    assert_eq!(
        account.id,
        Uuid::parse_str("9dce1460-0c4c-4417-ab8b-25ca478c5a78").unwrap()
    );
    assert_eq!(account.login, "testuser");
    assert_eq!(account.email, "test@example.com");
    assert_eq!(account.company_name.as_deref(), Some("Acme Corp"));
    assert_eq!(account.first_name.as_deref(), Some("Test"));
    assert_eq!(account.last_name.as_deref(), Some("User"));
    assert_eq!(account.address.as_deref(), Some("123 Main St"));
    assert_eq!(account.postal_code.as_deref(), Some("94105"));
    assert_eq!(account.city.as_deref(), Some("San Francisco"));
    assert_eq!(account.state.as_deref(), Some("CA"));
    assert_eq!(account.country.as_deref(), Some("US"));
    assert_eq!(account.phone.as_deref(), Some("+1-555-555-5555"));
}

/// Critical test: verifies the `triton_cns_enabled` snake_case field.
/// This field uses an explicit `#[serde(rename = "triton_cns_enabled")]`
/// because the Account struct uses `rename_all = "camelCase"`.
#[test]
fn test_account_triton_cns_enabled() {
    let account: Account = common::deserialize_fixture("account", "full.json");

    assert!(
        account.triton_cns_enabled.is_some(),
        "triton_cns_enabled should deserialize from snake_case"
    );
    assert_eq!(account.triton_cns_enabled, Some(true));
}

#[test]
fn test_account_timestamps() {
    let account: Account = common::deserialize_fixture("account", "full.json");

    assert_eq!(
        account.created,
        chrono::Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap()
    );
    assert_eq!(
        account.updated,
        chrono::Utc.with_ymd_and_hms(2024, 6, 15, 12, 0, 0).unwrap()
    );
}

/// Test minimal account with only required fields.
#[test]
fn test_account_minimal_deserialize() {
    let account: Account = common::deserialize_fixture("account", "minimal.json");

    assert_eq!(
        account.id,
        Uuid::parse_str("b4bb1880-8c2c-11e5-8994-28cfe91f7baf").unwrap()
    );
    assert_eq!(account.login, "admin");
    assert_eq!(account.email, "admin@example.com");

    // All optional fields should be None
    assert!(account.company_name.is_none());
    assert!(account.first_name.is_none());
    assert!(account.last_name.is_none());
    assert!(account.address.is_none());
    assert!(account.postal_code.is_none());
    assert!(account.city.is_none());
    assert!(account.state.is_none());
    assert!(account.country.is_none());
    assert!(account.phone.is_none());
    assert!(account.triton_cns_enabled.is_none());
}

/// Test round-trip serialization/deserialization preserves data.
#[test]
fn test_account_round_trip() {
    let original: Account = common::deserialize_fixture("account", "full.json");
    let serialized = serde_json::to_string(&original).unwrap();
    let deserialized: Account = serde_json::from_str(&serialized).unwrap();

    assert_eq!(original.id, deserialized.id);
    assert_eq!(original.login, deserialized.login);
    assert_eq!(original.email, deserialized.email);
    assert_eq!(original.triton_cns_enabled, deserialized.triton_cns_enabled);
}

/// Test that `triton_cns_enabled` serializes back to snake_case.
#[test]
fn test_account_triton_cns_enabled_round_trip() {
    let account: Account = common::deserialize_fixture("account", "full.json");
    let serialized = serde_json::to_value(&account).unwrap();

    assert!(
        serialized.get("triton_cns_enabled").is_some(),
        "triton_cns_enabled should serialize as snake_case"
    );
    assert!(
        serialized.get("tritonCnsEnabled").is_none(),
        "should not serialize as camelCase"
    );
}
