// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Deserialization tests for Network types
//!
//! CloudAPI returns Network objects in snake_case (no `rename_all` on the struct).
//! Tests verify correct field mapping including `vlan_id`, `role-tag`, and
//! fabric-specific fields.

mod common;

use cloudapi_api::types::{
    CreateFabricNetworkRequest, FabricVlan, Network, NetworkIp, Nic, NicState,
};
use uuid::Uuid;

#[test]
fn test_network_public_deserialize() {
    let network: Network = common::deserialize_fixture("network", "public.json");

    assert_eq!(
        network.id,
        Uuid::parse_str("3985900d-15a8-42d8-a997-1f7e8df2d0af").unwrap()
    );
    assert_eq!(network.name, "external");
    assert!(network.public);
    assert_eq!(
        network.description.as_deref(),
        Some("Public internet network")
    );
    assert_eq!(network.gateway.as_deref(), Some("67.158.54.1"));
    assert_eq!(network.provision_start_ip.as_deref(), Some("67.158.54.10"));
    assert_eq!(network.provision_end_ip.as_deref(), Some("67.158.54.254"));
    assert_eq!(network.subnet.as_deref(), Some("67.158.54.0/24"));
    assert_eq!(network.netmask.as_deref(), Some("255.255.255.0"));
}

#[test]
fn test_network_public_no_fabric_fields() {
    let network: Network = common::deserialize_fixture("network", "public.json");

    assert!(network.fabric.is_none());
    assert!(network.vlan_id.is_none());
    assert!(network.internet_nat.is_none());
    assert!(network.suffixes.is_none());
    assert!(network.routes.is_none());
    assert!(network.role_tag.is_none());
}

#[test]
fn test_network_fabric_deserialize() {
    let network: Network = common::deserialize_fixture("network", "fabric.json");

    assert_eq!(
        network.id,
        Uuid::parse_str("ac336e0f-8532-4e0e-a19e-7cd5bdd62817").unwrap()
    );
    assert_eq!(network.name, "my-fabric-network");
    assert!(!network.public);
    assert_eq!(network.fabric, Some(true));
    assert_eq!(network.internet_nat, Some(true));
}

/// Test that `vlan_id` (snake_case with explicit rename) deserializes correctly.
#[test]
fn test_network_vlan_id() {
    let network: Network = common::deserialize_fixture("network", "fabric.json");

    assert!(
        network.vlan_id.is_some(),
        "vlan_id should deserialize from snake_case"
    );
    assert_eq!(network.vlan_id, Some(100));
}

/// Test that `role-tag` (hyphenated) deserializes correctly.
#[test]
fn test_network_role_tags() {
    let network: Network = common::deserialize_fixture("network", "fabric.json");

    assert!(
        network.role_tag.is_some(),
        "role-tag should deserialize from hyphenated key"
    );
    assert_eq!(
        network.role_tag.as_ref().unwrap(),
        &vec!["admin".to_string()]
    );
}

#[test]
fn test_network_fabric_suffixes() {
    let network: Network = common::deserialize_fixture("network", "fabric.json");

    let suffixes = network.suffixes.expect("suffixes should be present");
    assert_eq!(suffixes.len(), 2);
    assert_eq!(suffixes[0], "inst.triton.zone");
    assert_eq!(suffixes[1], "svc.triton.zone");
}

#[test]
fn test_network_fabric_routes() {
    let network: Network = common::deserialize_fixture("network", "fabric.json");

    let routes = network.routes.expect("routes should be present");
    assert!(routes.is_object());
    assert_eq!(routes["10.0.0.0/8"], "192.168.128.1");
}

#[test]
fn test_network_resolvers() {
    let network: Network = common::deserialize_fixture("network", "public.json");

    let resolvers = network.resolvers.expect("resolvers should be present");
    assert_eq!(resolvers, vec!["8.8.8.8", "8.8.4.4"]);
}

/// Test deserialization of a network list.
#[test]
fn test_network_list_deserialize() {
    let json = format!(
        "[{}, {}]",
        common::load_fixture("network", "public.json"),
        common::load_fixture("network", "fabric.json")
    );

    let networks: Vec<Network> = serde_json::from_str(&json).expect("Failed to parse network list");
    assert_eq!(networks.len(), 2);
    assert!(networks[0].public);
    assert!(!networks[1].public);
}

/// Test FabricVlan deserialization.
#[test]
fn test_fabric_vlan_deserialize() {
    let json = r#"{
        "vlan_id": 100,
        "name": "my-vlan",
        "description": "My fabric VLAN"
    }"#;

    let vlan: FabricVlan = serde_json::from_str(json).unwrap();
    assert_eq!(vlan.vlan_id, 100);
    assert_eq!(vlan.name, "my-vlan");
    assert_eq!(vlan.description.as_deref(), Some("My fabric VLAN"));
}

/// Test NIC deserialization with camelCase fields.
#[test]
fn test_nic_deserialize() {
    let json = r#"{
        "mac": "90:b8:d0:80:ec:ae",
        "primary": true,
        "ip": "67.158.54.228",
        "netmask": "255.255.255.0",
        "gateway": "67.158.54.1",
        "network": "3985900d-15a8-42d8-a997-1f7e8df2d0af",
        "state": "running"
    }"#;

    let nic: Nic = serde_json::from_str(json).unwrap();
    assert_eq!(nic.mac, "90:b8:d0:80:ec:ae");
    assert!(nic.primary);
    assert_eq!(nic.ip, "67.158.54.228");
    assert_eq!(nic.state, Some(NicState::Running));
}

/// Test NicState enum variants.
#[test]
fn test_nic_state_variants() {
    let cases = [
        ("provisioning", NicState::Provisioning),
        ("running", NicState::Running),
        ("stopped", NicState::Stopped),
    ];

    for (json_value, expected) in cases {
        let json = format!(r#""{}""#, json_value);
        let parsed: NicState = serde_json::from_str(&json)
            .unwrap_or_else(|_| panic!("Failed to parse NIC state: {}", json_value));
        assert_eq!(parsed, expected);
    }
}

/// Test forward compatibility: unknown NIC states deserialize as Unknown.
#[test]
fn test_nic_state_unknown_variant() {
    let json = r#""migrating""#;
    let parsed: NicState = serde_json::from_str(json).unwrap();
    assert_eq!(parsed, NicState::Unknown);
}

/// Test NetworkIp deserialization with snake_case wire format.
///
/// CloudAPI's translateIp() passes NAPI fields through directly in snake_case.
#[test]
fn test_network_ip_deserialize() {
    let json = r#"{
        "ip": "67.158.54.100",
        "reserved": true,
        "managed": false,
        "owner_uuid": "9dce1460-0c4c-4417-ab8b-25ca478c5a78",
        "belongs_to_uuid": "a1234567-1234-1234-1234-123456789012",
        "belongs_to_type": "zone"
    }"#;

    let ip: NetworkIp = serde_json::from_str(json).unwrap();
    assert_eq!(ip.ip, "67.158.54.100");
    assert!(ip.reserved);
    assert_eq!(ip.managed, Some(false));
    assert!(ip.owner_uuid.is_some());
    assert_eq!(ip.belongs_to_type.as_deref(), Some("zone"));

    // Verify round-trip uses snake_case wire format
    let serialized = serde_json::to_value(&ip).unwrap();
    assert!(
        serialized.get("owner_uuid").is_some(),
        "should serialize as 'owner_uuid' (snake_case)"
    );
    assert!(
        serialized.get("ownerUuid").is_none(),
        "should not serialize as 'ownerUuid' (camelCase)"
    );
    assert!(
        serialized.get("belongs_to_uuid").is_some(),
        "should serialize as 'belongs_to_uuid' (snake_case)"
    );
    assert!(
        serialized.get("belongs_to_type").is_some(),
        "should serialize as 'belongs_to_type' (snake_case)"
    );
}

/// Test that CreateFabricNetworkRequest serializes field names as snake_case,
/// matching the CloudAPI wire format.
#[test]
fn test_create_fabric_network_request_snake_case() {
    let req = CreateFabricNetworkRequest {
        name: "my-network".to_string(),
        description: None,
        subnet: "192.168.128.0/22".to_string(),
        provision_start_ip: "192.168.128.5".to_string(),
        provision_end_ip: "192.168.131.250".to_string(),
        gateway: Some("192.168.128.1".to_string()),
        resolvers: Some(vec!["8.8.8.8".to_string()]),
        routes: None,
        internet_nat: Some(true),
    };

    let json = serde_json::to_value(&req).unwrap();
    let obj = json.as_object().unwrap();

    // Verify snake_case field names (not camelCase)
    assert!(
        obj.contains_key("provision_start_ip"),
        "expected snake_case 'provision_start_ip'"
    );
    assert!(
        obj.contains_key("provision_end_ip"),
        "expected snake_case 'provision_end_ip'"
    );
    assert!(
        obj.contains_key("internet_nat"),
        "expected snake_case 'internet_nat'"
    );

    // Verify camelCase is NOT present
    assert!(
        !obj.contains_key("provisionStartIp"),
        "unexpected camelCase 'provisionStartIp'"
    );
    assert!(
        !obj.contains_key("provisionEndIp"),
        "unexpected camelCase 'provisionEndIp'"
    );
    assert!(
        !obj.contains_key("internetNat"),
        "unexpected camelCase 'internetNat'"
    );

    // Verify values
    assert_eq!(obj["provision_start_ip"], "192.168.128.5");
    assert_eq!(obj["provision_end_ip"], "192.168.131.250");
    assert_eq!(obj["internet_nat"], true);
}

/// Test round-trip serialization/deserialization preserves data.
#[test]
fn test_network_round_trip() {
    let original: Network = common::deserialize_fixture("network", "fabric.json");
    let serialized = serde_json::to_string(&original).unwrap();
    let deserialized: Network = serde_json::from_str(&serialized).unwrap();

    assert_eq!(original.id, deserialized.id);
    assert_eq!(original.name, deserialized.name);
    assert_eq!(original.public, deserialized.public);
    assert_eq!(original.vlan_id, deserialized.vlan_id);
    assert_eq!(original.role_tag, deserialized.role_tag);
}
