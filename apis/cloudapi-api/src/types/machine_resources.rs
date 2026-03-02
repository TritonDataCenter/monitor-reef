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

// SnapshotState is defined in vmapi-api and re-exported via cloudapi-api::lib.rs.
// CloudAPI and VMAPI share the same snapshot state definitions.
use vmapi_api::SnapshotState;

/// Snapshot information
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    /// Snapshot name
    pub name: String,
    /// Snapshot state
    pub state: SnapshotState,
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

/// Disk state
#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, clap::ValueEnum,
)]
#[serde(rename_all = "lowercase")]
pub enum DiskState {
    Creating,
    Running,
    Resizing,
    Failed,
    Deleted,
    #[serde(other)]
    #[clap(skip)]
    Unknown,
}

/// Disk information
///
/// Note: CloudAPI returns all disk fields in snake_case, matching the
/// VMAPI wire format passed through the `translate()` function.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct Disk {
    /// Disk UUID
    pub id: Uuid,
    /// Size in MB
    pub size: u64,
    /// Block size in bytes
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_size: Option<u64>,
    /// PCI slot
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pci_slot: Option<String>,
    /// Boot disk
    #[serde(default)]
    pub boot: Option<bool>,
    /// State
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<DiskState>,
}

/// Request to create disk
#[derive(Debug, Deserialize, JsonSchema)]
pub struct CreateDiskRequest {
    /// Size in MB
    pub size: u64,
    /// PCI slot (optional)
    #[serde(default)]
    pub pci_slot: Option<String>,
}

/// Disk action for action dispatch
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DiskAction {
    Resize,
    /// Unknown action (forward compatibility)
    #[serde(other)]
    Unknown,
}

/// Query parameter for disk actions
#[derive(Debug, Deserialize, JsonSchema)]
pub struct DiskActionQuery {
    /// Action to perform. Optional in the query string because clients may
    /// send it in the request body instead. Body takes precedence over the
    /// query parameter.
    // Implementation note: matches Restify's mapParams behavior.
    // Service implementations should check the body first, then fall back
    // to this query parameter.
    #[serde(default)]
    pub action: Option<DiskAction>,
}

/// Request to resize disk
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
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
