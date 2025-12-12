// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Machine-related types

use super::common::{Metadata, Tags, Timestamp, Uuid};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Path parameter for machine operations
#[derive(Debug, Deserialize, JsonSchema)]
pub struct MachinePath {
    /// Account login name
    pub account: String,
    /// Machine UUID
    pub machine: Uuid,
}

/// Machine state
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum MachineState {
    Running,
    Stopped,
    Deleted,
    Provisioning,
    Failed,
}

/// Machine information
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Machine {
    /// Machine UUID
    pub id: Uuid,
    /// Machine alias/name
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Machine type (smartmachine or virtualmachine)
    #[serde(rename = "type")]
    pub machine_type: String,
    /// Brand (joyent, kvm, bhyve, lx)
    pub brand: String,
    /// Current state
    pub state: MachineState,
    /// Image UUID
    pub image: Uuid,
    /// Package name/UUID
    pub package: String,
    /// RAM in MB
    pub memory: u64,
    /// Disk space in MB
    pub disk: u64,
    /// Metadata
    pub metadata: Metadata,
    /// Tags
    pub tags: Tags,
    /// Creation timestamp
    pub created: Timestamp,
    /// Last update timestamp
    pub updated: Timestamp,
    /// Docker-specific fields
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub docker: Option<bool>,
    /// Firewall enabled
    #[serde(default)]
    pub firewall_enabled: Option<bool>,
    /// Deletion protection enabled
    #[serde(default)]
    pub deletion_protection: Option<bool>,
    /// Compute node UUID
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compute_node: Option<Uuid>,
    /// Primary IP address
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_ip: Option<String>,
    /// Network interfaces
    #[serde(default)]
    pub nics: Vec<MachineNic>,
}

/// Network interface on a machine
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MachineNic {
    /// MAC address
    pub mac: String,
    /// Primary NIC
    pub primary: bool,
    /// IP address
    pub ip: String,
    /// Netmask
    pub netmask: String,
    /// Gateway
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway: Option<String>,
    /// Network UUID
    pub network: Uuid,
}

/// Request to create a machine
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateMachineRequest {
    /// Machine alias/name
    #[serde(default)]
    pub name: Option<String>,
    /// Image UUID
    pub image: Uuid,
    /// Package name or UUID
    pub package: String,
    /// Networks (array of UUIDs)
    #[serde(default)]
    pub networks: Option<Vec<Uuid>>,
    /// Locality hints
    #[serde(default)]
    pub locality: Option<serde_json::Value>,
    /// Metadata
    #[serde(default)]
    pub metadata: Option<Metadata>,
    /// Tags
    #[serde(default)]
    pub tags: Option<Tags>,
    /// Firewall enabled
    #[serde(default)]
    pub firewall_enabled: Option<bool>,
    /// Deletion protection enabled
    #[serde(default)]
    pub deletion_protection: Option<bool>,
}

/// Machine action for action dispatch
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MachineAction {
    Start,
    Stop,
    Reboot,
    Resize,
    Rename,
    EnableFirewall,
    DisableFirewall,
    EnableDeletionProtection,
    DisableDeletionProtection,
}

/// Query parameter for machine actions
#[derive(Debug, Deserialize, JsonSchema)]
pub struct MachineActionQuery {
    pub action: MachineAction,
}

/// Request to start a machine
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct StartMachineRequest {
    /// Origin identifier (defaults to 'cloudapi')
    #[serde(default)]
    pub origin: Option<String>,
}

/// Request to stop a machine
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct StopMachineRequest {
    /// Origin identifier (defaults to 'cloudapi')
    #[serde(default)]
    pub origin: Option<String>,
}

/// Request to reboot a machine
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RebootMachineRequest {
    /// Origin identifier (defaults to 'cloudapi')
    #[serde(default)]
    pub origin: Option<String>,
}

/// Request to resize a machine
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ResizeMachineRequest {
    /// New package name or UUID
    pub package: String,
    /// Origin identifier (defaults to 'cloudapi')
    #[serde(default)]
    pub origin: Option<String>,
}

/// Request to rename a machine
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RenameMachineRequest {
    /// New machine alias/name (max 189 chars, or 63 if CNS enabled)
    pub name: String,
    /// Origin identifier (defaults to 'cloudapi')
    #[serde(default)]
    pub origin: Option<String>,
}

/// Request to enable firewall
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnableFirewallRequest {
    /// Origin identifier (defaults to 'cloudapi')
    #[serde(default)]
    pub origin: Option<String>,
}

/// Request to disable firewall
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DisableFirewallRequest {
    /// Origin identifier (defaults to 'cloudapi')
    #[serde(default)]
    pub origin: Option<String>,
}

/// Request to enable deletion protection
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnableDeletionProtectionRequest {
    /// Origin identifier (defaults to 'cloudapi')
    #[serde(default)]
    pub origin: Option<String>,
}

/// Request to disable deletion protection
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DisableDeletionProtectionRequest {
    /// Origin identifier (defaults to 'cloudapi')
    #[serde(default)]
    pub origin: Option<String>,
}

/// Query parameters for listing machines
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ListMachinesQuery {
    /// Filter by machine name
    #[serde(default)]
    pub name: Option<String>,
    /// Filter by image UUID
    #[serde(default)]
    pub image: Option<Uuid>,
    /// Filter by state
    #[serde(default)]
    pub state: Option<MachineState>,
    /// Filter by memory (MB)
    #[serde(default)]
    pub memory: Option<u64>,
    /// Filter by machine type
    #[serde(default, rename = "type")]
    pub machine_type: Option<String>,
    /// Pagination offset
    #[serde(default)]
    pub offset: Option<u64>,
    /// Pagination limit
    #[serde(default)]
    pub limit: Option<u64>,
    /// Filter by tag (format: key=value)
    #[serde(default)]
    pub tag: Option<String>,
}

/// Audit entry for a machine
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuditEntry {
    /// Action performed
    pub action: String,
    /// Timestamp
    pub time: Timestamp,
    /// Caller information
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller: Option<serde_json::Value>,
    /// Success status
    #[serde(default)]
    pub success: Option<bool>,
}
