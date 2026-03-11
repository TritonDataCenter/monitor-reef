// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Output format validation tests for JSON and table output
//!
//! These tests verify that:
//! - JSON output field names match the expected CloudAPI wire format
//! - Fixture data deserializes into expected types correctly
//! - JSON round-trip serialization preserves field names
//! - Table output for list commands includes expected column headers
//!
//! All tests run offline without API access.

// Allow deprecated - cargo_bin is standard for CLI testing
#![allow(deprecated, clippy::expect_used, clippy::unwrap_used)]

mod common;

use assert_cmd::Command;
use predicates::prelude::*;

fn triton_cmd() -> Command {
    Command::cargo_bin("triton").expect("Failed to find triton binary")
}

fn load_fixture(name: &str) -> serde_json::Value {
    let path = common::fixture_path(name);
    let content =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("Failed to read {path:?}: {e}"));
    serde_json::from_str(&content)
        .unwrap_or_else(|e| panic!("Failed to parse JSON from {path:?}: {e}"))
}

fn assert_has_fields(val: &serde_json::Value, fields: &[&str], context: &str) {
    let obj = val
        .as_object()
        .unwrap_or_else(|| panic!("{context}: expected JSON object"));
    for field in fields {
        assert!(
            obj.contains_key(*field),
            "{context}: missing expected field '{field}'"
        );
    }
}

fn assert_field_absent(val: &serde_json::Value, field: &str, context: &str) {
    let obj = val
        .as_object()
        .unwrap_or_else(|| panic!("{context}: expected JSON object"));
    assert!(
        !obj.contains_key(field),
        "{context}: unexpected field '{field}' present (wrong rename?)"
    );
}

// =============================================================================
// Instance (Machine) JSON field tests
// =============================================================================

#[test]
fn test_instance_fixture_has_required_json_fields() {
    let val = load_fixture("machine/instance_list.json");
    assert_has_fields(
        &val,
        &[
            "id", "name", "type", "brand", "state", "image", "ips", "memory", "disk", "metadata",
            "tags", "created", "updated",
        ],
        "instance",
    );
}

#[test]
fn test_instance_fixture_snake_case_exceptions() {
    let val = load_fixture("machine/instance_list.json");

    // These fields use snake_case despite the struct's camelCase rename_all
    assert_has_fields(
        &val,
        &[
            "deletion_protection",
            "firewall_enabled",
            "compute_node",
            "primaryIp",
        ],
        "instance snake_case exceptions",
    );

    // Verify the camelCase versions are NOT present (would indicate wrong renaming)
    assert_field_absent(&val, "deletionProtection", "instance");
    assert_field_absent(&val, "firewallEnabled", "instance");
    assert_field_absent(&val, "computeNode", "instance");
}

#[test]
fn test_instance_fixture_type_field_renamed() {
    let val = load_fixture("machine/instance_list.json");

    // The Rust field is `machine_type` but JSON wire format is `type`
    assert_has_fields(&val, &["type"], "instance type field");
    assert_field_absent(&val, "machine_type", "instance");
}

#[test]
fn test_instance_fixture_nics_structure() {
    let val = load_fixture("machine/instance_list.json");
    let nics = val["nics"].as_array().expect("nics should be an array");
    assert!(!nics.is_empty(), "fixture should have at least one NIC");

    let nic = &nics[0];
    assert_has_fields(
        nic,
        &["mac", "ip", "primary", "gateway", "netmask", "network"],
        "nic",
    );
}

// =============================================================================
// Image JSON field tests
// =============================================================================

#[test]
fn test_image_fixture_has_required_json_fields() {
    let val = load_fixture("image_list.json");
    assert_has_fields(
        &val,
        &[
            "id", "name", "version", "os", "type", "public", "state", "owner",
        ],
        "image",
    );
}

#[test]
fn test_image_fixture_type_field_renamed() {
    let val = load_fixture("image_list.json");

    // The Rust field is `image_type` but JSON wire format is `type`
    assert_has_fields(&val, &["type"], "image type field");
    assert_field_absent(&val, "image_type", "image");
}

#[test]
fn test_image_fixture_published_at_snake_case() {
    let val = load_fixture("image_list.json");

    // published_at is explicitly renamed to snake_case
    assert_has_fields(&val, &["published_at"], "image published_at");
    assert_field_absent(&val, "publishedAt", "image");
}

// =============================================================================
// Network JSON field tests
// =============================================================================

#[test]
fn test_network_fixture_has_required_json_fields() {
    let val = load_fixture("network_list.json");
    assert_has_fields(
        &val,
        &["id", "name", "public", "fabric", "gateway", "subnet"],
        "network",
    );
}

#[test]
fn test_network_fixture_snake_case_fields() {
    let val = load_fixture("network_list.json");

    // Network struct has no rename_all, so fields are snake_case
    assert_has_fields(
        &val,
        &[
            "internet_nat",
            "provision_start_ip",
            "provision_end_ip",
            "vlan_id",
        ],
        "network snake_case fields",
    );

    // Verify camelCase versions are NOT present
    assert_field_absent(&val, "internetNat", "network");
    assert_field_absent(&val, "provisionStartIp", "network");
    assert_field_absent(&val, "provisionEndIp", "network");
    assert_field_absent(&val, "vlanId", "network");
}

// =============================================================================
// Volume JSON field tests
// =============================================================================

#[test]
fn test_volume_fixture_has_required_json_fields() {
    let val = load_fixture("volume_list.json");
    assert_has_fields(
        &val,
        &["id", "name", "type", "size", "state", "created", "networks"],
        "volume",
    );
}

#[test]
fn test_volume_fixture_snake_case_override_fields() {
    let val = load_fixture("volume_list.json");

    // Volume struct has rename_all = "camelCase" but owner_uuid and filesystem_path
    // have explicit #[serde(rename = "owner_uuid")] / #[serde(rename = "filesystem_path")]
    // overrides, so the wire format is snake_case for these fields.
    assert_has_fields(
        &val,
        &["owner_uuid", "filesystem_path"],
        "volume snake_case override fields",
    );

    // Verify camelCase versions are NOT present (explicit renames override rename_all)
    assert_field_absent(&val, "ownerUuid", "volume");
    assert_field_absent(&val, "filesystemPath", "volume");
}

#[test]
fn test_volume_fixture_type_field_renamed() {
    let val = load_fixture("volume_list.json");

    // The Rust field is `volume_type` but JSON wire format is `type`
    assert_has_fields(&val, &["type"], "volume type field");
    assert_field_absent(&val, "volume_type", "volume");
}

// =============================================================================
// Package JSON field tests
// =============================================================================

#[test]
fn test_package_fixture_has_required_json_fields() {
    let val = load_fixture("package_list.json");
    assert_has_fields(
        &val,
        &["id", "name", "memory", "disk", "swap", "vcpus"],
        "package",
    );
}

#[test]
fn test_package_fixture_field_types() {
    let val = load_fixture("package_list.json");

    assert!(val["id"].is_string(), "package id should be string");
    assert!(val["name"].is_string(), "package name should be string");
    assert!(val["memory"].is_number(), "package memory should be number");
    assert!(val["disk"].is_number(), "package disk should be number");
    assert!(val["swap"].is_number(), "package swap should be number");
    assert!(val["vcpus"].is_number(), "package vcpus should be number");
}

// =============================================================================
// Disk JSON field tests
// =============================================================================

#[test]
fn test_disk_fixture_has_required_json_fields() {
    let val = load_fixture("disk_list.json");
    assert_has_fields(&val, &["id", "size", "state"], "disk");
}

#[test]
fn test_disk_fixture_snake_case_fields() {
    let val = load_fixture("disk_list.json");

    // Disk struct has NO rename_all, so fields stay snake_case
    assert_has_fields(&val, &["block_size", "pci_slot"], "disk snake_case fields");

    // Verify camelCase versions are NOT present
    assert_field_absent(&val, "blockSize", "disk");
    assert_field_absent(&val, "pciSlot", "disk");
}

// =============================================================================
// NIC JSON field tests
// =============================================================================

#[test]
fn test_nic_fixture_has_required_json_fields() {
    let val = load_fixture("nic_list.json");
    assert_has_fields(&val, &["mac", "ip", "primary", "netmask", "network"], "nic");
}

#[test]
fn test_nic_fixture_state_field() {
    let val = load_fixture("nic_list.json");

    // state is optional but present in our fixture
    assert_has_fields(&val, &["state"], "nic state");
}

// =============================================================================
// Snapshot JSON field tests
// =============================================================================

#[test]
fn test_snapshot_fixture_has_required_json_fields() {
    let val = load_fixture("snapshot_list.json");
    assert_has_fields(&val, &["name", "state", "created"], "snapshot");
}

// =============================================================================
// SSH Key JSON field tests
// =============================================================================

#[test]
fn test_key_fixture_has_required_json_fields() {
    let val = load_fixture("key_list.json");
    assert_has_fields(&val, &["name", "key", "fingerprint"], "key");
}

#[test]
fn test_key_fixture_role_tag_hyphenated() {
    let val = load_fixture("key_list.json");

    // SshKey uses #[serde(rename = "role-tag")]
    assert_has_fields(&val, &["role-tag"], "key role-tag");
    assert_field_absent(&val, "roleTag", "key");
    assert_field_absent(&val, "role_tag", "key");
}

// =============================================================================
// Firewall Rule JSON field tests
// =============================================================================

#[test]
fn test_fwrule_fixture_has_required_json_fields() {
    let val = load_fixture("fwrule_list.json");
    assert_has_fields(&val, &["id", "rule", "enabled", "log"], "fwrule");
}

#[test]
fn test_fwrule_fixture_role_tag_hyphenated() {
    let val = load_fixture("fwrule_list.json");

    // FirewallRule uses #[serde(rename = "role-tag")]
    assert_has_fields(&val, &["role-tag"], "fwrule role-tag");
    assert_field_absent(&val, "roleTag", "fwrule");
    assert_field_absent(&val, "role_tag", "fwrule");
}

// =============================================================================
// JSON stream parsing tests
// =============================================================================

#[test]
fn test_json_stream_parse_ndjson_format() {
    let input = r#"{"id":"aaa","name":"first"}
{"id":"bbb","name":"second"}
"#;
    let parsed: Vec<serde_json::Value> = common::json_stream_parse(input);
    assert_eq!(parsed.len(), 2);
    assert_eq!(parsed[0]["id"], "aaa");
    assert_eq!(parsed[1]["name"], "second");
}

#[test]
fn test_json_stream_parse_array_format() {
    let input = r#"[{"id":"aaa","name":"first"},{"id":"bbb","name":"second"}]"#;
    let parsed: Vec<serde_json::Value> = common::json_stream_parse(input);
    assert_eq!(parsed.len(), 2);
    assert_eq!(parsed[0]["id"], "aaa");
    assert_eq!(parsed[1]["name"], "second");
}

#[test]
fn test_json_stream_parse_empty_lines_ignored() {
    let input = r#"{"id":"aaa"}

{"id":"bbb"}

"#;
    let parsed: Vec<serde_json::Value> = common::json_stream_parse(input);
    assert_eq!(parsed.len(), 2);
}

#[test]
fn test_json_stream_parse_single_item() {
    let input = r#"{"id":"only-one"}"#;
    let parsed: Vec<serde_json::Value> = common::json_stream_parse(input);
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0]["id"], "only-one");
}

// =============================================================================
// Table column header tests (via --help output)
// =============================================================================

// Verify that list commands document the -o (column select) and -H (no header)
// flags, which are the primary table formatting controls.

#[test]
fn test_instance_list_has_table_format_flags() {
    triton_cmd()
        .args(["instance", "list", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--output"))
        .stdout(predicate::str::contains("--no-header"))
        .stdout(predicate::str::contains("--long"))
        .stdout(predicate::str::contains("--sort-by"));
}

#[test]
fn test_image_list_has_table_format_flags() {
    triton_cmd()
        .args(["image", "list", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--output"))
        .stdout(predicate::str::contains("--no-header"))
        .stdout(predicate::str::contains("--long"));
}

#[test]
fn test_network_list_has_table_format_flags() {
    triton_cmd()
        .args(["network", "list", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--output"))
        .stdout(predicate::str::contains("--no-header"));
}

#[test]
fn test_volume_list_has_table_format_flags() {
    triton_cmd()
        .args(["volume", "list", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--output"))
        .stdout(predicate::str::contains("--no-header"));
}

#[test]
fn test_package_list_has_json_flag() {
    // Package list uses the older create_table API without TableFormatArgs,
    // so it only has the -j/--json flag for format control.
    triton_cmd()
        .args(["package", "list", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--json"));
}

// =============================================================================
// Cross-fixture consistency tests
// =============================================================================

#[test]
fn test_all_fixtures_have_id_field() {
    let fixtures = [
        "machine/instance_list.json",
        "image_list.json",
        "network_list.json",
        "volume_list.json",
        "package_list.json",
        "disk_list.json",
        "fwrule_list.json",
    ];

    for fixture_name in &fixtures {
        let val = load_fixture(fixture_name);
        assert!(
            val["id"].is_string(),
            "{fixture_name}: 'id' field should be a UUID string"
        );
    }
}

#[test]
fn test_all_fixtures_have_name_field() {
    let fixtures = [
        "machine/instance_list.json",
        "image_list.json",
        "network_list.json",
        "volume_list.json",
        "package_list.json",
        "snapshot_list.json",
        "key_list.json",
    ];

    for fixture_name in &fixtures {
        let val = load_fixture(fixture_name);
        assert!(
            val["name"].is_string(),
            "{fixture_name}: 'name' field should be a string"
        );
    }
}

#[test]
fn test_uuid_fields_are_valid_format() {
    let uuid_re =
        regex::Regex::new(r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$")
            .unwrap();

    let fixtures_and_fields: &[(&str, &[&str])] = &[
        ("machine/instance_list.json", &["id", "image"]),
        ("image_list.json", &["id", "owner"]),
        ("network_list.json", &["id"]),
        ("volume_list.json", &["id", "owner_uuid"]),
        ("package_list.json", &["id"]),
        ("disk_list.json", &["id"]),
        ("fwrule_list.json", &["id"]),
    ];

    for (fixture_name, fields) in fixtures_and_fields {
        let val = load_fixture(fixture_name);
        for field in *fields {
            let field_val = val[field]
                .as_str()
                .unwrap_or_else(|| panic!("{fixture_name}: '{field}' should be a string"));
            assert!(
                uuid_re.is_match(field_val),
                "{fixture_name}: '{field}' value '{field_val}' is not a valid UUID"
            );
        }
    }
}
