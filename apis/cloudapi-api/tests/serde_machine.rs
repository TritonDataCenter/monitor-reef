// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Deserialization tests for Machine types
//!
//! These tests verify that CloudAPI JSON responses deserialize correctly,
//! particularly the snake_case fields that differ from the camelCase convention.

mod common;

use cloudapi_api::types::{Machine, MachineState, MachineType, MountMode};
use uuid::Uuid;

#[test]
fn test_machine_basic_deserialize() {
    let machine: Machine = common::deserialize_fixture("machine", "basic.json");

    assert_eq!(
        machine.id,
        Uuid::parse_str("a1234567-1234-1234-1234-123456789012").unwrap()
    );
    assert_eq!(machine.name, "test-machine");
    assert_eq!(machine.machine_type, MachineType::Smartmachine);
    assert_eq!(machine.state, MachineState::Running);
    assert_eq!(machine.memory, Some(1024));
    assert_eq!(machine.disk, 25600);
    assert_eq!(machine.ips, vec!["10.88.88.10"]);
}

/// Critical test: verifies all 6 snake_case fields deserialize correctly.
///
/// CloudAPI returns these specific fields in snake_case format despite using
/// camelCase for most other fields. This test would have caught the bugs where
/// these fields were not being deserialized due to incorrect serde configuration.
#[test]
fn test_machine_snake_case_fields() {
    let machine: Machine = common::deserialize_fixture("machine", "with_snake_case.json");

    // Verify all 6 snake_case fields deserialize correctly
    assert!(
        machine.firewall_enabled.is_some(),
        "firewall_enabled should deserialize from snake_case"
    );
    assert_eq!(machine.firewall_enabled, Some(true));

    assert!(
        machine.deletion_protection.is_some(),
        "deletion_protection should deserialize from snake_case"
    );
    assert_eq!(machine.deletion_protection, Some(false));

    assert!(
        machine.compute_node.is_some(),
        "compute_node should deserialize from snake_case"
    );
    assert_eq!(
        machine.compute_node,
        Some(Uuid::parse_str("d1234567-1234-1234-1234-123456789012").unwrap())
    );

    assert!(
        machine.dns_names.is_some(),
        "dns_names should deserialize from snake_case"
    );
    assert_eq!(
        machine.dns_names,
        Some(vec![
            "test.inst.triton.zone".to_string(),
            "test.svc.triton.zone".to_string()
        ])
    );

    assert!(
        machine.free_space.is_some(),
        "free_space should deserialize from snake_case"
    );
    assert_eq!(machine.free_space, Some(5368709120));

    assert!(
        machine.delegate_dataset.is_some(),
        "delegate_dataset should deserialize from snake_case"
    );
    assert_eq!(machine.delegate_dataset, Some(true));
}

/// Test that machines without snake_case fields still deserialize correctly
/// (the fields should be None when not present in JSON).
#[test]
fn test_machine_missing_optional_fields() {
    let machine: Machine = common::deserialize_fixture("machine", "basic.json");

    assert!(machine.firewall_enabled.is_none());
    assert!(machine.deletion_protection.is_none());
    assert!(machine.compute_node.is_none());
    assert!(machine.dns_names.is_none());
    assert!(machine.free_space.is_none());
    assert!(machine.delegate_dataset.is_none());
    assert!(machine.networks.is_none());
    assert!(machine.primary_ip.is_none());
    assert!(machine.docker.is_none());
    assert!(machine.disks.is_none());
    assert!(machine.encrypted.is_none());
    assert!(machine.flexible.is_none());
}

/// Test that a list of machines can be deserialized from a JSON array.
#[test]
fn test_machine_list_deserialize() {
    let json = format!(
        "[{}, {}]",
        common::load_fixture("machine", "basic.json"),
        common::load_fixture("machine", "with_snake_case.json")
    );

    let machines: Vec<Machine> = serde_json::from_str(&json).expect("Failed to parse machine list");
    assert_eq!(machines.len(), 2);
    assert_eq!(machines[0].name, "test-machine");
    assert_eq!(machines[1].name, "test-machine-snake-case");
}

/// Test deserialization of machine type enum variants.
#[test]
fn test_machine_type_deserialize() {
    let json = r#""smartmachine""#;
    let mt: MachineType = serde_json::from_str(json).unwrap();
    assert_eq!(mt, MachineType::Smartmachine);

    let json = r#""virtualmachine""#;
    let mt: MachineType = serde_json::from_str(json).unwrap();
    assert_eq!(mt, MachineType::Virtualmachine);
}

/// Test deserialization of all machine state enum variants.
#[test]
fn test_machine_state_deserialize() {
    let states = [
        ("running", MachineState::Running),
        ("stopped", MachineState::Stopped),
        ("stopping", MachineState::Stopping),
        ("provisioning", MachineState::Provisioning),
        ("failed", MachineState::Failed),
        ("deleted", MachineState::Deleted),
        ("offline", MachineState::Offline),
        ("ready", MachineState::Ready),
        ("unknown", MachineState::Unknown),
    ];

    for (json_value, expected_state) in states {
        let json = format!(r#""{}""#, json_value);
        let state: MachineState = serde_json::from_str(&json)
            .unwrap_or_else(|_| panic!("Failed to parse {}", json_value));
        assert_eq!(state, expected_state);
    }
}

/// Test deserialization of a real CloudAPI response (from `triton instance get`).
///
/// This fixture is derived from actual CloudAPI output and tests that all fields
/// including snake_case fields, NICs, networks, and metadata deserialize correctly.
#[test]
fn test_real_cloudapi_response() {
    let machine: Machine = common::deserialize_fixture("machine", "instance_get.json");

    // Basic fields
    assert_eq!(
        machine.id,
        Uuid::parse_str("8a5918c3-84a2-4122-9ed5-60d76d7a8525").unwrap()
    );
    assert_eq!(machine.name, "deploy-424bd6a9");
    assert_eq!(machine.machine_type, MachineType::Smartmachine);
    assert_eq!(machine.state, MachineState::Running);
    assert_eq!(machine.memory, Some(1024));
    assert_eq!(machine.disk, 10240);
    assert_eq!(machine.package, "g1.micro");

    // Snake_case fields (the critical ones)
    assert_eq!(machine.firewall_enabled, Some(false));
    assert_eq!(machine.deletion_protection, Some(false));
    assert_eq!(
        machine.compute_node,
        Some(Uuid::parse_str("44454c4c-5300-1057-8050-b7c04f533532").unwrap())
    );
    assert!(machine.dns_names.is_some());
    assert_eq!(machine.dns_names.as_ref().unwrap().len(), 6);

    // IPs and networks
    assert_eq!(machine.ips.len(), 2);
    assert_eq!(machine.ips[0], "67.158.54.228");
    assert_eq!(machine.primary_ip, Some("67.158.54.228".to_string()));
    assert!(machine.networks.is_some());
    assert_eq!(machine.networks.as_ref().unwrap().len(), 2);

    // NICs
    assert_eq!(machine.nics.len(), 2);
    assert!(machine.nics[0].primary);
    assert_eq!(machine.nics[0].ip, "67.158.54.228");
    assert!(!machine.nics[1].primary);

    // Metadata and tags
    assert!(machine.metadata.contains_key("root_authorized_keys"));
    assert!(machine.tags.contains_key("github_url"));
    assert!(machine.tags.contains_key("triton_deploy_app_id"));
}

/// Round-trip test: serialize then deserialize a Machine and verify all fields survive.
///
/// This is critical because Machine has mixed naming conventions: camelCase for most
/// fields, explicit snake_case renames for 6 fields, and `type` -> `machine_type`.
/// A round-trip failure would indicate a serde configuration bug.
#[test]
fn test_machine_round_trip() {
    let original: Machine = common::deserialize_fixture("machine", "instance_get.json");
    let serialized = serde_json::to_string(&original).unwrap();
    let deserialized: Machine = serde_json::from_str(&serialized).unwrap();

    // Core fields
    assert_eq!(original.id, deserialized.id);
    assert_eq!(original.name, deserialized.name);
    assert_eq!(original.machine_type, deserialized.machine_type);
    assert_eq!(original.state, deserialized.state);
    assert_eq!(original.image, deserialized.image);
    assert_eq!(original.package, deserialized.package);
    assert_eq!(original.memory, deserialized.memory);
    assert_eq!(original.disk, deserialized.disk);
    assert_eq!(original.ips, deserialized.ips);

    // Snake_case renamed fields (the most fragile)
    assert_eq!(original.firewall_enabled, deserialized.firewall_enabled);
    assert_eq!(
        original.deletion_protection,
        deserialized.deletion_protection
    );
    assert_eq!(original.compute_node, deserialized.compute_node);
    assert_eq!(original.dns_names, deserialized.dns_names);
    assert_eq!(original.free_space, deserialized.free_space);
    assert_eq!(original.delegate_dataset, deserialized.delegate_dataset);

    // Other optional fields
    assert_eq!(original.primary_ip, deserialized.primary_ip);
    assert_eq!(original.networks, deserialized.networks);
    assert_eq!(original.docker, deserialized.docker);
    assert_eq!(original.encrypted, deserialized.encrypted);
    assert_eq!(original.flexible, deserialized.flexible);

    // NICs round-trip
    assert_eq!(original.nics.len(), deserialized.nics.len());
    for (orig_nic, deser_nic) in original.nics.iter().zip(deserialized.nics.iter()) {
        assert_eq!(orig_nic.mac, deser_nic.mac);
        assert_eq!(orig_nic.ip, deser_nic.ip);
        assert_eq!(orig_nic.primary, deser_nic.primary);
        assert_eq!(orig_nic.netmask, deser_nic.netmask);
        assert_eq!(orig_nic.gateway, deser_nic.gateway);
        assert_eq!(orig_nic.network, deser_nic.network);
    }
}

/// Verify that serialized Machine JSON uses the correct key names.
///
/// This catches bugs where the Rust field name leaks into the JSON instead of the
/// serde-renamed wire format key.
#[test]
fn test_machine_serialized_keys() {
    let machine: Machine = common::deserialize_fixture("machine", "instance_get.json");
    let serialized = serde_json::to_string(&machine).unwrap();
    let json: serde_json::Value = serde_json::from_str(&serialized).unwrap();
    let obj = json.as_object().unwrap();

    // `type` not `machine_type`
    assert!(obj.contains_key("type"), "should serialize as 'type'");
    assert!(
        !obj.contains_key("machine_type"),
        "should not use Rust field name"
    );

    // Snake_case fields should keep their explicit rename
    assert!(
        obj.contains_key("firewall_enabled"),
        "should serialize as 'firewall_enabled'"
    );
    assert!(
        obj.contains_key("deletion_protection"),
        "should serialize as 'deletion_protection'"
    );
    assert!(
        obj.contains_key("compute_node"),
        "should serialize as 'compute_node'"
    );
    assert!(
        obj.contains_key("dns_names"),
        "should serialize as 'dns_names'"
    );

    // camelCase fields should be camelCase
    assert!(
        obj.contains_key("primaryIp"),
        "should serialize as 'primaryIp'"
    );
}

/// Test MountMode wire format serialization.
#[test]
fn test_mount_mode_wire_format() {
    let cases = [("rw", MountMode::Rw), ("ro", MountMode::Ro)];

    for (json_value, expected) in cases {
        let json = format!(r#""{}""#, json_value);
        let parsed: MountMode = serde_json::from_str(&json)
            .unwrap_or_else(|_| panic!("Failed to parse mount mode: {}", json_value));
        assert_eq!(parsed, expected);

        // Round-trip
        let serialized = serde_json::to_string(&expected).unwrap();
        assert_eq!(serialized, json);
    }
}

/// Test forward compatibility: unknown mount modes deserialize as Unknown.
#[test]
fn test_mount_mode_unknown_variant() {
    let json = r#""rwx""#;
    let parsed: MountMode = serde_json::from_str(json).unwrap();
    assert_eq!(parsed, MountMode::Unknown);
}
