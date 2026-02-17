// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Serde tests for action-dispatch types
//!
//! CloudAPI uses `POST ...?action=<enum>` to dispatch multiple operations from
//! one endpoint. Each action enum has a specific wire format that must be
//! preserved exactly for the dispatch to work. These tests verify that the
//! enums serialize to the correct wire-format strings.

use cloudapi_api::types::{
    CloneImageRequest, ExportImageRequest, ImportImageRequest, ShareImageRequest,
    UnshareImageRequest, UpdateImageRequest,
};
use cloudapi_api::types::{
    DisableDeletionProtectionRequest, DisableFirewallRequest, DiskAction, DiskActionQuery,
    EnableDeletionProtectionRequest, EnableFirewallRequest, ImageAction, ImageActionQuery,
    MachineAction, MachineActionQuery, RebootMachineRequest, RenameMachineRequest,
    ResizeDiskRequest, ResizeMachineRequest, StartMachineRequest, StopMachineRequest,
    UpdateVolumeRequest, VolumeAction, VolumeActionQuery,
};
use uuid::Uuid;

// --- MachineAction tests ---

#[test]
fn test_machine_action_wire_format() {
    // MachineAction uses snake_case
    let cases = [
        (MachineAction::Start, "start"),
        (MachineAction::Stop, "stop"),
        (MachineAction::Reboot, "reboot"),
        (MachineAction::Resize, "resize"),
        (MachineAction::Rename, "rename"),
        (MachineAction::EnableFirewall, "enable_firewall"),
        (MachineAction::DisableFirewall, "disable_firewall"),
        (
            MachineAction::EnableDeletionProtection,
            "enable_deletion_protection",
        ),
        (
            MachineAction::DisableDeletionProtection,
            "disable_deletion_protection",
        ),
    ];

    for (variant, expected_wire) in cases {
        let serialized = serde_json::to_value(&variant).unwrap();
        assert_eq!(
            serialized,
            serde_json::Value::String(expected_wire.to_string()),
            "MachineAction::{:?} should serialize to {:?}",
            variant,
            expected_wire
        );
    }
}

#[test]
fn test_machine_action_round_trip() {
    let actions = [
        MachineAction::Start,
        MachineAction::Stop,
        MachineAction::Reboot,
        MachineAction::Resize,
        MachineAction::Rename,
        MachineAction::EnableFirewall,
        MachineAction::DisableFirewall,
        MachineAction::EnableDeletionProtection,
        MachineAction::DisableDeletionProtection,
    ];

    for action in actions {
        let json = serde_json::to_string(&action).unwrap();
        let parsed: MachineAction = serde_json::from_str(&json).unwrap();
        let re_json = serde_json::to_string(&parsed).unwrap();
        assert_eq!(
            json, re_json,
            "MachineAction round-trip failed for {:?}",
            action
        );
    }
}

#[test]
fn test_machine_action_query_deserialize() {
    let json = r#"{"action": "enable_firewall"}"#;
    let query: MachineActionQuery = serde_json::from_str(json).unwrap();
    let serialized = serde_json::to_value(&query.action).unwrap();
    assert_eq!(serialized, "enable_firewall");
}

// --- MachineAction request body tests ---

#[test]
fn test_start_machine_request() {
    // Empty body (origin is optional)
    let json = r#"{}"#;
    let req: StartMachineRequest = serde_json::from_str(json).unwrap();
    assert!(req.origin.is_none());

    // With origin
    let json = r#"{"origin": "cloudapi"}"#;
    let req: StartMachineRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.origin.as_deref(), Some("cloudapi"));
}

#[test]
fn test_stop_machine_request() {
    let json = r#"{}"#;
    let req: StopMachineRequest = serde_json::from_str(json).unwrap();
    assert!(req.origin.is_none());
}

#[test]
fn test_reboot_machine_request() {
    let json = r#"{}"#;
    let req: RebootMachineRequest = serde_json::from_str(json).unwrap();
    assert!(req.origin.is_none());
}

#[test]
fn test_resize_machine_request() {
    let json = r#"{"package": "g4-highcpu-4G"}"#;
    let req: ResizeMachineRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.package, "g4-highcpu-4G");
    assert!(req.origin.is_none());
}

#[test]
fn test_rename_machine_request() {
    let json = r#"{"name": "new-name"}"#;
    let req: RenameMachineRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.name, "new-name");
    assert!(req.origin.is_none());
}

#[test]
fn test_enable_firewall_request() {
    let json = r#"{}"#;
    let req: EnableFirewallRequest = serde_json::from_str(json).unwrap();
    assert!(req.origin.is_none());
}

#[test]
fn test_disable_firewall_request() {
    let json = r#"{}"#;
    let req: DisableFirewallRequest = serde_json::from_str(json).unwrap();
    assert!(req.origin.is_none());
}

#[test]
fn test_enable_deletion_protection_request() {
    let json = r#"{}"#;
    let req: EnableDeletionProtectionRequest = serde_json::from_str(json).unwrap();
    assert!(req.origin.is_none());
}

#[test]
fn test_disable_deletion_protection_request() {
    let json = r#"{}"#;
    let req: DisableDeletionProtectionRequest = serde_json::from_str(json).unwrap();
    assert!(req.origin.is_none());
}

// --- ImageAction tests ---

#[test]
fn test_image_action_wire_format() {
    // ImageAction uses kebab-case
    let cases = [
        (ImageAction::Update, "update"),
        (ImageAction::Export, "export"),
        (ImageAction::Clone, "clone"),
        (ImageAction::ImportFromDatacenter, "import-from-datacenter"),
        (ImageAction::Share, "share"),
        (ImageAction::Unshare, "unshare"),
    ];

    for (variant, expected_wire) in cases {
        let serialized = serde_json::to_value(&variant).unwrap();
        assert_eq!(
            serialized,
            serde_json::Value::String(expected_wire.to_string()),
            "ImageAction::{:?} should serialize to {:?}",
            variant,
            expected_wire
        );
    }
}

#[test]
fn test_image_action_round_trip() {
    let actions = [
        ImageAction::Update,
        ImageAction::Export,
        ImageAction::Clone,
        ImageAction::ImportFromDatacenter,
        ImageAction::Share,
        ImageAction::Unshare,
    ];

    for action in actions {
        let json = serde_json::to_string(&action).unwrap();
        let parsed: ImageAction = serde_json::from_str(&json).unwrap();
        let re_json = serde_json::to_string(&parsed).unwrap();
        assert_eq!(
            json, re_json,
            "ImageAction round-trip failed for {:?}",
            action
        );
    }
}

#[test]
fn test_image_action_query_deserialize() {
    let json = r#"{"action": "import-from-datacenter"}"#;
    let query: ImageActionQuery = serde_json::from_str(json).unwrap();
    let serialized = serde_json::to_value(&query.action).unwrap();
    assert_eq!(serialized, "import-from-datacenter");
}

// --- ImageAction request body tests ---

#[test]
fn test_update_image_request() {
    let json = r#"{"name": "new-name", "version": "2.0.0", "description": "Updated"}"#;
    let req: UpdateImageRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.name.as_deref(), Some("new-name"));
    assert_eq!(req.version.as_deref(), Some("2.0.0"));
    assert_eq!(req.description.as_deref(), Some("Updated"));
    assert!(req.homepage.is_none());
    assert!(req.eula.is_none());
    assert!(req.acl.is_none());
    assert!(req.tags.is_none());
}

#[test]
fn test_export_image_request() {
    let json = r#"{"mantaPath": "/user/stor/images/export"}"#;
    let req: ExportImageRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.manta_path, "/user/stor/images/export");
}

#[test]
fn test_clone_image_request() {
    let json = r#"{}"#;
    let _req: CloneImageRequest = serde_json::from_str(json).unwrap();
}

#[test]
fn test_import_image_request() {
    let json = r#"{"datacenter": "us-east-1", "id": "a1234567-1234-1234-1234-123456789012"}"#;
    let req: ImportImageRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.datacenter, "us-east-1");
    assert_eq!(
        req.id,
        Uuid::parse_str("a1234567-1234-1234-1234-123456789012").unwrap()
    );
}

#[test]
fn test_share_image_request() {
    let json = r#"{"account": "b1234567-1234-1234-1234-123456789012"}"#;
    let req: ShareImageRequest = serde_json::from_str(json).unwrap();
    assert_eq!(
        req.account,
        Uuid::parse_str("b1234567-1234-1234-1234-123456789012").unwrap()
    );
}

#[test]
fn test_unshare_image_request() {
    let json = r#"{"account": "b1234567-1234-1234-1234-123456789012"}"#;
    let req: UnshareImageRequest = serde_json::from_str(json).unwrap();
    assert_eq!(
        req.account,
        Uuid::parse_str("b1234567-1234-1234-1234-123456789012").unwrap()
    );
}

// --- DiskAction tests ---

#[test]
fn test_disk_action_wire_format() {
    // DiskAction uses snake_case
    let serialized = serde_json::to_value(&DiskAction::Resize).unwrap();
    assert_eq!(serialized, "resize");
}

#[test]
fn test_disk_action_round_trip() {
    let json = serde_json::to_string(&DiskAction::Resize).unwrap();
    let parsed: DiskAction = serde_json::from_str(&json).unwrap();
    let re_json = serde_json::to_string(&parsed).unwrap();
    assert_eq!(json, re_json);
}

#[test]
fn test_disk_action_query_deserialize() {
    let json = r#"{"action": "resize"}"#;
    let query: DiskActionQuery = serde_json::from_str(json).unwrap();
    let serialized = serde_json::to_value(&query.action).unwrap();
    assert_eq!(serialized, "resize");
}

#[test]
fn test_resize_disk_request() {
    let json = r#"{"size": 51200}"#;
    let req: ResizeDiskRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.size, 51200);
    assert!(req.dangerous_allow_shrink.is_none());

    let json = r#"{"size": 25600, "dangerousAllowShrink": true}"#;
    let req: ResizeDiskRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.size, 25600);
    assert_eq!(req.dangerous_allow_shrink, Some(true));
}

// --- VolumeAction tests ---

#[test]
fn test_volume_action_wire_format() {
    // VolumeAction uses snake_case
    let serialized = serde_json::to_value(&VolumeAction::Update).unwrap();
    assert_eq!(serialized, "update");
}

#[test]
fn test_volume_action_round_trip() {
    let json = serde_json::to_string(&VolumeAction::Update).unwrap();
    let parsed: VolumeAction = serde_json::from_str(&json).unwrap();
    let re_json = serde_json::to_string(&parsed).unwrap();
    assert_eq!(json, re_json);
}

#[test]
fn test_volume_action_query_deserialize() {
    let json = r#"{"action": "update"}"#;
    let query: VolumeActionQuery = serde_json::from_str(json).unwrap();
    let serialized = serde_json::to_value(&query.action).unwrap();
    assert_eq!(serialized, "update");
}

#[test]
fn test_update_volume_request() {
    let json = r#"{"name": "new-volume-name"}"#;
    let req: UpdateVolumeRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.name.as_deref(), Some("new-volume-name"));

    let json = r#"{}"#;
    let req: UpdateVolumeRequest = serde_json::from_str(json).unwrap();
    assert!(req.name.is_none());
}
