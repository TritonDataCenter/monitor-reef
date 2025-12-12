// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Machine sub-resources (snapshots, tags, metadata, disks)

use super::common::{Metadata, Tags, Timestamp, Uuid};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Path parameter for snapshot operations
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SnapshotPath {
    /// Account login name
    pub account: String,
    /// Machine UUID
    pub machine: Uuid,
    /// Snapshot name
    pub name: String,
}

/// Path parameter for disk operations
#[derive(Debug, Deserialize, JsonSchema)]
pub struct DiskPath {
    /// Account login name
    pub account: String,
    /// Machine UUID
    pub machine: Uuid,
    /// Disk UUID
    pub disk: Uuid,
}

/// Path parameter for metadata key operations
#[derive(Debug, Deserialize, JsonSchema)]
pub struct MetadataKeyPath {
    /// Account login name
    pub account: String,
    /// Machine UUID
    pub machine: Uuid,
    /// Metadata key
    pub key: String,
}

/// Path parameter for tag operations
#[derive(Debug, Deserialize, JsonSchema)]
pub struct TagPath {
    /// Account login name
    pub account: String,
    /// Machine UUID
    pub machine: Uuid,
    /// Tag key
    pub tag: String,
}

/// Snapshot information
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    /// Snapshot name
    pub name: String,
    /// Snapshot state
    pub state: String,
    /// Creation timestamp
    pub created: Timestamp,
    /// Last update timestamp
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated: Option<Timestamp>,
}

/// Request to create snapshot
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateSnapshotRequest {
    /// Snapshot name
    #[serde(default)]
    pub name: Option<String>,
}

/// Disk information
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Disk {
    /// Disk UUID
    pub id: Uuid,
    /// Size in MB
    pub size: u64,
    /// PCI slot
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pci_slot: Option<String>,
    /// Boot disk
    #[serde(default)]
    pub boot: Option<bool>,
    /// State
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
}

/// Request to create disk
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateDiskRequest {
    /// Size in MB
    pub size: u64,
    /// PCI slot (optional)
    #[serde(default)]
    pub pci_slot: Option<String>,
}

/// Disk action for action dispatch
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DiskAction {
    Resize,
}

/// Query parameter for disk actions
#[derive(Debug, Deserialize, JsonSchema)]
pub struct DiskActionQuery {
    pub action: DiskAction,
}

/// Request to resize disk
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ResizeDiskRequest {
    /// New size in MB
    pub size: u64,
    /// Allow dangerous shrink operation
    #[serde(default)]
    pub dangerous_allow_shrink: Option<bool>,
}

/// Request to add machine metadata
#[derive(Debug, Deserialize, JsonSchema)]
pub struct AddMetadataRequest {
    /// Metadata key-value pairs
    #[serde(flatten)]
    pub metadata: Metadata,
}

/// Request to add/replace machine tags
#[derive(Debug, Deserialize, JsonSchema)]
pub struct TagsRequest {
    /// Tags key-value pairs
    #[serde(flatten)]
    pub tags: Tags,
}
