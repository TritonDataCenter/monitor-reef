// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Deserialization tests for VM types
//!
//! VMAPI uses snake_case for all JSON field names (internal Triton API convention).
//! These tests verify that Vm, Nic, Disk, Snapshot, and related types
//! deserialize correctly from VMAPI-style responses.

mod common;

use uuid::Uuid;
use vmapi_api::types::{Brand, SnapshotState, Vm, VmState};

#[test]
fn test_vm_smartos_deserialize() {
    let vm: Vm = common::deserialize_fixture("vm", "smartos.json");

    assert_eq!(
        vm.uuid,
        Uuid::parse_str("a1234567-1234-1234-1234-123456789012").unwrap()
    );
    assert_eq!(vm.alias.as_deref(), Some("test-zone"));
    assert_eq!(vm.brand, Brand::Joyent);
    assert_eq!(vm.state, VmState::Running);
    assert_eq!(
        vm.owner_uuid,
        Uuid::parse_str("9dce1460-0c4c-4417-ab8b-25ca478c5a78").unwrap()
    );
    assert_eq!(
        vm.image_uuid,
        Some(Uuid::parse_str("2b683a82-a066-11e3-97ab-2faa44701c5a").unwrap())
    );
    assert_eq!(
        vm.server_uuid,
        Some(Uuid::parse_str("44454c4c-5300-1057-8050-b7c04f533532").unwrap())
    );
}

#[test]
fn test_vm_smartos_resources() {
    let vm: Vm = common::deserialize_fixture("vm", "smartos.json");

    assert_eq!(vm.ram, Some(1024));
    assert_eq!(vm.max_physical_memory, Some(1024));
    assert_eq!(vm.cpu_cap, Some(100));
    assert_eq!(vm.quota, Some(25600));
    assert_eq!(vm.max_swap, Some(2048));
    assert_eq!(vm.max_locked_memory, Some(1024));
    assert_eq!(vm.max_lwps, Some(4000));
    assert_eq!(vm.zfs_io_priority, Some(100));
}

#[test]
fn test_vm_smartos_nics() {
    let vm: Vm = common::deserialize_fixture("vm", "smartos.json");

    let nics = vm.nics.expect("nics should be present");
    assert_eq!(nics.len(), 2);

    assert_eq!(nics[0].mac, "90:b8:d0:80:ec:ae");
    assert_eq!(nics[0].ip.as_deref(), Some("67.158.54.228"));
    assert_eq!(nics[0].primary, Some(true));
    assert_eq!(nics[0].nic_tag.as_deref(), Some("external"));
    assert_eq!(
        nics[0].network_uuid,
        Some(Uuid::parse_str("3985900d-15a8-42d8-a997-1f7e8df2d0af").unwrap())
    );

    assert_eq!(nics[1].primary, Some(false));
    assert_eq!(nics[1].vlan_id, Some(100));
}

#[test]
fn test_vm_smartos_metadata() {
    let vm: Vm = common::deserialize_fixture("vm", "smartos.json");

    let customer_metadata = vm
        .customer_metadata
        .expect("customer_metadata should be present");
    assert!(customer_metadata.contains_key("root_authorized_keys"));

    let internal_metadata = vm
        .internal_metadata
        .expect("internal_metadata should be present");
    assert!(internal_metadata.contains_key("sdc:operator-script"));

    let tags = vm.tags.expect("tags should be present");
    assert_eq!(tags["env"], "production");
    assert_eq!(tags["app"], "webserver");
}

#[test]
fn test_vm_smartos_misc_fields() {
    let vm: Vm = common::deserialize_fixture("vm", "smartos.json");

    assert_eq!(vm.firewall_enabled, Some(true));
    assert_eq!(vm.dns_domain.as_deref(), Some("inst.triton.zone"));
    assert_eq!(
        vm.resolvers.as_ref().unwrap(),
        &vec!["8.8.8.8".to_string(), "8.8.4.4".to_string()]
    );
    assert_eq!(vm.autoboot, Some(true));
    assert!(vm.create_timestamp.is_some());
    assert!(vm.last_modified.is_some());
}

#[test]
fn test_vm_bhyve_deserialize() {
    let vm: Vm = common::deserialize_fixture("vm", "bhyve.json");

    assert_eq!(vm.brand, Brand::Bhyve);
    assert_eq!(vm.state, VmState::Stopped);
    assert_eq!(vm.vcpus, Some(4));
    assert_eq!(vm.ram, Some(4096));
}

#[test]
fn test_vm_bhyve_disks() {
    let vm: Vm = common::deserialize_fixture("vm", "bhyve.json");

    let disks = vm.disks.expect("disks should be present");
    assert_eq!(disks.len(), 2);

    assert!(disks[0].boot.unwrap_or(false));
    assert_eq!(disks[0].size, Some(10240));
    assert_eq!(disks[0].pci_slot.as_deref(), Some("4:0"));
    assert!(disks[0].image_uuid.is_some());

    assert!(!disks[1].boot.unwrap_or(false));
    assert_eq!(disks[1].size, Some(40960));
    assert!(disks[1].image_uuid.is_none());
}

#[test]
fn test_vm_bhyve_snapshots() {
    let vm: Vm = common::deserialize_fixture("vm", "bhyve.json");

    let snapshots = vm.snapshots.expect("snapshots should be present");
    assert_eq!(snapshots.len(), 1);
    assert_eq!(snapshots[0].name, "pre-upgrade");
    assert_eq!(snapshots[0].state, SnapshotState::Created);
}

#[test]
fn test_vm_bhyve_flexible_disk() {
    let vm: Vm = common::deserialize_fixture("vm", "bhyve.json");

    assert_eq!(vm.free_space, Some(20480));
    assert_eq!(vm.flexible_disk_size, Some(51200));
}

/// Test minimal VM with only required fields.
#[test]
fn test_vm_minimal_deserialize() {
    let vm: Vm = common::deserialize_fixture("vm", "minimal.json");

    assert_eq!(
        vm.uuid,
        Uuid::parse_str("e5678901-5678-5678-5678-567890123456").unwrap()
    );
    assert_eq!(vm.brand, Brand::Lx);
    assert_eq!(vm.state, VmState::Provisioning);
    assert!(vm.alias.is_none());
    assert!(vm.image_uuid.is_none());
    assert!(vm.server_uuid.is_none());
    assert!(vm.nics.is_none());
    assert!(vm.disks.is_none());
    assert!(vm.tags.is_none());
}

/// Test deserialization of all Brand enum variants.
#[test]
fn test_brand_variants() {
    let cases = [
        ("bhyve", Brand::Bhyve),
        ("builder", Brand::Builder),
        ("joyent", Brand::Joyent),
        ("joyent-minimal", Brand::JoyentMinimal),
        ("kvm", Brand::Kvm),
        ("lx", Brand::Lx),
    ];

    for (json_value, expected) in cases {
        let json = format!(r#""{}""#, json_value);
        let parsed: Brand = serde_json::from_str(&json)
            .unwrap_or_else(|_| panic!("Failed to parse brand: {}", json_value));
        assert_eq!(parsed, expected);
    }
}

/// Test forward compatibility: unknown brands deserialize as Unknown.
#[test]
fn test_brand_unknown_variant() {
    let json = r#""future-brand""#;
    let parsed: Brand = serde_json::from_str(json).unwrap();
    assert_eq!(parsed, Brand::Unknown);
}

/// Test deserialization of all VmState enum variants.
#[test]
fn test_vm_state_variants() {
    let cases = [
        ("running", VmState::Running),
        ("stopped", VmState::Stopped),
        ("stopping", VmState::Stopping),
        ("provisioning", VmState::Provisioning),
        ("failed", VmState::Failed),
        ("destroyed", VmState::Destroyed),
        ("incomplete", VmState::Incomplete),
        ("configured", VmState::Configured),
        ("ready", VmState::Ready),
        ("receiving", VmState::Receiving),
    ];

    for (json_value, expected) in cases {
        let json = format!(r#""{}""#, json_value);
        let parsed: VmState = serde_json::from_str(&json)
            .unwrap_or_else(|_| panic!("Failed to parse VM state: {}", json_value));
        assert_eq!(parsed, expected);
    }
}

/// Test forward compatibility: unknown VM states deserialize as Unknown.
#[test]
fn test_vm_state_unknown_variant() {
    let json = r#""migrating""#;
    let parsed: VmState = serde_json::from_str(json).unwrap();
    assert_eq!(parsed, VmState::Unknown);
}

/// Test VmState Display implementation.
#[test]
fn test_vm_state_display() {
    assert_eq!(VmState::Running.to_string(), "running");
    assert_eq!(VmState::Stopped.to_string(), "stopped");
    assert_eq!(VmState::Destroyed.to_string(), "destroyed");
    assert_eq!(VmState::Unknown.to_string(), "unknown");
}

/// Test VmState FromStr implementation.
#[test]
fn test_vm_state_from_str() {
    assert_eq!("running".parse::<VmState>().unwrap(), VmState::Running);
    assert_eq!("STOPPED".parse::<VmState>().unwrap(), VmState::Stopped);
    // Unknown strings parse as Unknown, consistent with serde(other) behavior
    assert_eq!("invalid".parse::<VmState>().unwrap(), VmState::Unknown);
}

/// Test VmState helper methods.
#[test]
fn test_vm_state_helpers() {
    assert!(VmState::Failed.is_failed());
    assert!(!VmState::Running.is_failed());
    assert!(VmState::Destroyed.is_destroyed());
    assert!(!VmState::Stopped.is_destroyed());
}

/// Test deserialization of a VM list.
#[test]
fn test_vm_list_deserialize() {
    let json = format!(
        "[{}, {}, {}]",
        common::load_fixture("vm", "smartos.json"),
        common::load_fixture("vm", "bhyve.json"),
        common::load_fixture("vm", "minimal.json")
    );

    let vms: Vec<Vm> = serde_json::from_str(&json).expect("Failed to parse VM list");
    assert_eq!(vms.len(), 3);
    assert_eq!(vms[0].brand, Brand::Joyent);
    assert_eq!(vms[1].brand, Brand::Bhyve);
    assert_eq!(vms[2].brand, Brand::Lx);
}

/// Test round-trip serialization/deserialization preserves data.
#[test]
fn test_vm_round_trip() {
    let original: Vm = common::deserialize_fixture("vm", "smartos.json");
    let serialized = serde_json::to_string(&original).unwrap();
    let deserialized: Vm = serde_json::from_str(&serialized).unwrap();

    assert_eq!(original.uuid, deserialized.uuid);
    assert_eq!(original.alias, deserialized.alias);
    assert_eq!(original.brand, deserialized.brand);
    assert_eq!(original.state, deserialized.state);
    assert_eq!(original.ram, deserialized.ram);
    assert_eq!(original.firewall_enabled, deserialized.firewall_enabled);
}
