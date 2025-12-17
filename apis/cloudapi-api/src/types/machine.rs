// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Machine-related types

use super::common::{Brand, Metadata, RoleTags, Tags, Timestamp, Uuid};
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
///
/// These states reflect the possible values returned by VMAPI's `translateState()`:
/// - `provisioning`: VM is being created (includes configured, incomplete, unavailable)
/// - `ready`: VM is ready but not started (occurs during reboot)
/// - `running`: VM is running
/// - `stopping`: VM is in the process of stopping (includes halting, shutting_down)
/// - `stopped`: VM is stopped (includes off, down, installed)
/// - `offline`: VM is offline (agent not responding, unreachable)
/// - `deleted`: VM has been destroyed
/// - `failed`: VM is in a failed state
/// - `unknown`: VM state cannot be determined
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MachineState {
    Running,
    Stopped,
    Stopping,
    Provisioning,
    Failed,
    Deleted,
    Offline,
    Ready,
    Unknown,
}

/// Machine type (virtualization category)
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MachineType {
    /// SmartOS zone or Linux container
    Smartmachine,
    /// Hardware VM (KVM or bhyve)
    Virtualmachine,
}

/// Machine information
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Machine {
    /// Machine UUID
    pub id: Uuid,
    /// Machine alias/name
    pub name: String,
    /// Machine type (smartmachine or virtualmachine)
    #[serde(rename = "type")]
    pub machine_type: MachineType,
    /// Brand (joyent, kvm, bhyve, lx, joyent-minimal)
    pub brand: Brand,
    /// Current state
    pub state: MachineState,
    /// Image UUID
    pub image: Uuid,
    /// Package name
    pub package: String,
    /// RAM in MB
    pub memory: u64,
    /// Disk space in MB
    pub disk: u64,
    /// IP addresses (always present)
    pub ips: Vec<String>,
    /// Metadata
    pub metadata: Metadata,
    /// Tags
    pub tags: Tags,
    /// Creation timestamp
    pub created: Timestamp,
    /// Last update timestamp
    pub updated: Timestamp,
    /// Network UUIDs (API version >= 7.1.0)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub networks: Option<Vec<Uuid>>,
    /// Primary IP address
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_ip: Option<String>,
    /// Network interfaces
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nics: Vec<MachineNic>,
    /// Docker container
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub docker: Option<bool>,
    /// Firewall enabled
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub firewall_enabled: Option<bool>,
    /// Deletion protection enabled
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deletion_protection: Option<bool>,
    /// Compute node UUID (server hosting the VM)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compute_node: Option<Uuid>,
    /// DNS names (CNS feature)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dns_names: Option<Vec<String>>,
    /// Free space in bytes (bhyve with flexible disk)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub free_space: Option<u64>,
    /// Disks (bhyve VMs only)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disks: Option<Vec<MachineDisk>>,
    /// Whether the VM uses encrypted storage
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encrypted: Option<bool>,
    /// Whether the VM uses flexible disk mode (bhyve only)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flexible: Option<bool>,
    /// Whether a delegate dataset is present
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegate_dataset: Option<bool>,
    /// Role tags for RBAC
    #[serde(rename = "role-tag", default, skip_serializing_if = "Option::is_none")]
    pub role_tag: Option<RoleTags>,
}

/// Disk attached to a machine (bhyve VMs)
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MachineDisk {
    /// Disk UUID
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Uuid>,
    /// Disk size in MB
    pub size: u64,
    /// Block size in bytes
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_size: Option<u64>,
    /// Boot disk
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boot: Option<bool>,
    /// Image UUID (for image-backed disks)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<Uuid>,
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

/// Volume mount specification for instance creation
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VolumeMount {
    /// Volume name
    pub name: String,
    /// Mount mode (defaults to "rw")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// Mount point inside the instance
    pub mountpoint: String,
    /// Volume type (defaults to "tritonnfs")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(rename = "type")]
    pub volume_type: Option<String>,
}

/// Disk specification for bhyve instance creation
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DiskSpec {
    /// Disk size in MB
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// Image UUID for this disk (for boot disk or additional image-backed disks)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<Uuid>,
    /// Block size in bytes (optional)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_size: Option<u64>,
    /// Mark as boot disk
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boot: Option<bool>,
}

/// NIC specification for instance creation
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NicSpec {
    /// Network UUID
    pub network: Uuid,
    /// Specific IP address (optional)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ip: Option<String>,
    /// Mark as primary NIC
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary: Option<bool>,
    /// Gateway address (optional)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway: Option<String>,
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
    /// Networks (array of UUIDs) - simple network specification
    #[serde(default)]
    pub networks: Option<Vec<Uuid>>,
    /// NICs - advanced NIC specification with IP addresses and options
    #[serde(default)]
    pub nics: Option<Vec<NicSpec>>,
    /// Affinity rules for instance placement (added in CloudAPI v8.3.0)
    ///
    /// Rules follow the pattern: `<key><operator><value>` where:
    /// - key: 'instance', 'container', or a tag name
    /// - operator: '==' (must), '!=' (must not), '==~' (prefer), '!=~' (prefer not)
    /// - value: exact string, glob pattern (*), or regex (/pattern/)
    ///
    /// Examples: `instance==myvm`, `role!=database`, `instance!=~foo*`
    #[serde(default)]
    pub affinity: Option<Vec<String>>,
    /// Locality hints (deprecated, use affinity instead)
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
    /// Brand (bhyve, kvm, joyent, joyent-minimal, lx)
    /// If not specified, inferred from the image
    #[serde(default)]
    pub brand: Option<Brand>,
    /// Volumes to mount on the instance
    #[serde(default)]
    pub volumes: Option<Vec<VolumeMount>>,
    /// Disks for bhyve instances
    #[serde(default)]
    pub disks: Option<Vec<DiskSpec>>,
    /// Create a delegated ZFS dataset for the zone
    /// Only applicable to zone-based instances (joyent, joyent-minimal, lx brands)
    #[serde(default)]
    pub delegate_dataset: Option<bool>,
    /// Request placement on encrypted compute nodes
    #[serde(default)]
    pub encrypted: Option<bool>,
    /// Allow using images shared with this account (not owned by it)
    #[serde(default)]
    pub allow_shared_images: Option<bool>,
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
    /// Filter by machine type (smartmachine or virtualmachine)
    #[serde(default, rename = "type")]
    pub machine_type: Option<MachineType>,
    /// Filter by brand
    #[serde(default)]
    pub brand: Option<Brand>,
    /// Pagination offset
    #[serde(default)]
    pub offset: Option<u64>,
    /// Pagination limit
    #[serde(default)]
    pub limit: Option<u64>,
    /// Filter by tag (format: key=value)
    #[serde(default)]
    pub tag: Option<String>,
    /// Filter by docker flag (added in CloudAPI 8.0.0)
    #[serde(default)]
    pub docker: Option<bool>,
    /// Include generated credentials in response
    #[serde(default)]
    pub credentials: Option<bool>,
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
