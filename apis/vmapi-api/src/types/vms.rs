// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! VM-related types for VMAPI
//!
//! Note: VMAPI uses snake_case for JSON field names (internal Triton API convention).

use super::common::{Brand, MetadataObject, Tags, Timestamp, Uuid, VmState};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ============================================================================
// Path Parameters
// ============================================================================

/// Path parameter for VM operations
#[derive(Debug, Deserialize, JsonSchema)]
pub struct VmPath {
    /// VM UUID
    pub uuid: Uuid,
}

// ============================================================================
// Query Parameters
// ============================================================================

/// Query parameters for listing VMs
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListVmsQuery {
    /// Filter by owner UUID
    #[serde(default)]
    pub owner_uuid: Option<Uuid>,
    /// Filter by server UUID
    #[serde(default)]
    pub server_uuid: Option<Uuid>,
    /// Filter by state
    #[serde(default)]
    pub state: Option<VmState>,
    /// Filter by brand
    #[serde(default)]
    pub brand: Option<Brand>,
    /// Filter by alias (VM name)
    #[serde(default)]
    pub alias: Option<String>,
    /// Filter by image UUID
    #[serde(default)]
    pub image_uuid: Option<Uuid>,
    /// Filter by billing UUID (package)
    #[serde(default)]
    pub billing_id: Option<Uuid>,
    /// Filter by RAM in MB
    #[serde(default)]
    pub ram: Option<u64>,
    /// Filter by tag (format: key=value)
    #[serde(default)]
    pub tag: Option<String>,
    /// Filter to include destroyed VMs
    #[serde(default)]
    pub include_destroyed: Option<bool>,
    /// Pagination offset
    #[serde(default)]
    pub offset: Option<u64>,
    /// Pagination limit (max results)
    #[serde(default)]
    pub limit: Option<u64>,
    /// Field selection (comma-separated list of fields to return)
    #[serde(default)]
    pub fields: Option<String>,
    /// Predicate query filter
    #[serde(default)]
    pub predicate: Option<String>,
}

/// Query parameters for VM action dispatch
///
/// The `action` field is optional in the query string because clients may
/// send it in the request body instead. Body takes precedence over the
/// query parameter.
// Implementation note: matches Restify's mapParams behavior.
// Service implementations should check the body first, then fall back
// to this query parameter.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct VmActionQuery {
    /// Action to perform. Optional in the query string because clients may
    /// send it in the request body instead.
    #[serde(default)]
    pub action: Option<VmAction>,
    /// If true, wait for job completion before returning (default: false)
    #[serde(default)]
    pub sync: Option<bool>,
}

/// VM actions available via POST /vms/:uuid
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VmAction {
    Start,
    Stop,
    Kill,
    Reboot,
    Reprovision,
    Update,
    AddNics,
    UpdateNics,
    RemoveNics,
    CreateSnapshot,
    RollbackSnapshot,
    DeleteSnapshot,
    CreateDisk,
    ResizeDisk,
    DeleteDisk,
    Migrate,
    /// Unknown action (forward compatibility)
    #[serde(other)]
    Unknown,
}

// ============================================================================
// Action Request Types
// ============================================================================

/// Request body for `start` action
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct StartVmRequest {
    /// If true, don't error if VM is already running (default: false)
    #[serde(default)]
    pub idempotent: Option<bool>,
}

/// Request body for `stop` action
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct StopVmRequest {
    /// If true, don't error if VM is already stopped (default: false)
    #[serde(default)]
    pub idempotent: Option<bool>,
}

/// Request body for `kill` action
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct KillVmRequest {
    /// Signal to send (default: SIGKILL). Examples: "SIGTERM", "SIGKILL"
    #[serde(default)]
    pub signal: Option<String>,
    /// If true, don't error if VM is already stopped (default: false)
    #[serde(default)]
    pub idempotent: Option<bool>,
}

/// Request body for `reboot` action
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct RebootVmRequest {
    /// If true, don't error if VM is not running (default: false)
    #[serde(default)]
    pub idempotent: Option<bool>,
}

/// Request body for `reprovision` action
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ReprovisionVmRequest {
    /// Image UUID to reprovision with (required)
    pub image_uuid: Uuid,
}

/// Request body for `update` action
///
/// Contains many optional fields that can be updated on a VM.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct UpdateVmRequest {
    /// New alias/name for the VM
    #[serde(default)]
    pub alias: Option<String>,
    /// RAM in MB
    #[serde(default)]
    pub ram: Option<u64>,
    /// CPU cap (percentage of CPU, e.g., 100 = 1 full CPU)
    #[serde(default)]
    pub cpu_cap: Option<u32>,
    /// Quota in GB (disk quota)
    #[serde(default)]
    pub quota: Option<u64>,
    /// Max swap in MB
    #[serde(default)]
    pub max_swap: Option<u64>,
    /// Max physical memory in MB
    #[serde(default)]
    pub max_physical_memory: Option<u64>,
    /// Max locked memory in MB
    #[serde(default)]
    pub max_locked_memory: Option<u64>,
    /// Max LWPs (lightweight processes)
    #[serde(default)]
    pub max_lwps: Option<u64>,
    /// ZFS I/O priority
    #[serde(default)]
    pub zfs_io_priority: Option<u64>,
    /// Billing ID (package UUID)
    #[serde(default)]
    pub billing_id: Option<Uuid>,
    /// Resolvers (DNS servers)
    #[serde(default)]
    pub resolvers: Option<Vec<String>>,
    /// Firewall enabled
    #[serde(default)]
    pub firewall_enabled: Option<bool>,
    /// Do not reboot when updating (default: false, will reboot if required)
    #[serde(default)]
    pub do_not_reboot: Option<bool>,
    /// Owner UUID (transfer VM ownership)
    #[serde(default)]
    pub owner_uuid: Option<Uuid>,
    /// Customer metadata updates
    #[serde(default)]
    pub customer_metadata: Option<MetadataObject>,
    /// Internal metadata updates
    #[serde(default)]
    pub internal_metadata: Option<MetadataObject>,
    /// Tags updates
    #[serde(default)]
    pub tags: Option<Tags>,
    /// Maintain resolvers flag
    #[serde(default)]
    pub maintain_resolvers: Option<bool>,
}

/// NIC specification for add/update operations
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct NicSpec {
    /// MAC address (for update operations)
    #[serde(default)]
    pub mac: Option<String>,
    /// Network UUID to add NIC on
    #[serde(default)]
    pub network_uuid: Option<Uuid>,
    /// Specific IP address to use
    #[serde(default)]
    pub ip: Option<String>,
    /// Mark as primary NIC
    #[serde(default)]
    pub primary: Option<bool>,
    /// NIC model (e.g., "virtio")
    #[serde(default)]
    pub model: Option<String>,
    /// Gateway address
    #[serde(default)]
    pub gateway: Option<String>,
    /// Whether this NIC is allowed to have spoofed IP addresses
    #[serde(default)]
    pub allow_ip_spoofing: Option<bool>,
    /// Whether this NIC is allowed to have spoofed MAC addresses
    #[serde(default)]
    pub allow_mac_spoofing: Option<bool>,
    /// Whether this NIC allows restricted traffic
    #[serde(default)]
    pub allow_restricted_traffic: Option<bool>,
}

/// Request body for `add_nics` action
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct AddNicsRequest {
    /// Networks to add NICs for.
    ///
    /// Each entry can be a UUID string or an object like
    /// `{"uuid": "...", "primary": true}` or `{"name": "...", "primary": true}`.
    #[serde(default)]
    pub networks: Option<Vec<serde_json::Value>>,
    /// MAC addresses of pre-created NICs to add
    #[serde(default)]
    pub macs: Option<Vec<String>>,
}

/// Request body for `update_nics` action
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct UpdateNicsRequest {
    /// Array of NIC updates (must include mac to identify which NIC)
    pub nics: Vec<NicSpec>,
}

/// Request body for `remove_nics` action
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct RemoveNicsRequest {
    /// MAC addresses of NICs to remove
    pub macs: Vec<String>,
}

/// Request body for `create_snapshot` action
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct CreateSnapshotRequest {
    /// Snapshot name (optional, auto-generated if not provided)
    #[serde(default)]
    pub snapshot_name: Option<String>,
}

/// Request body for `rollback_snapshot` action
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct RollbackSnapshotRequest {
    /// Name of the snapshot to rollback to (required)
    pub snapshot_name: String,
}

/// Request body for `delete_snapshot` action
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct DeleteSnapshotRequest {
    /// Name of the snapshot to delete (required)
    pub snapshot_name: String,
}

/// Request body for `create_disk` action (bhyve only)
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct CreateDiskRequest {
    /// Disk size in MB, or the literal "remaining" for remaining space
    // Use serde_json::Value because this can be a number or the string "remaining"
    pub size: serde_json::Value,
    /// PCI slot (optional, auto-assigned if not specified)
    #[serde(default)]
    pub pci_slot: Option<String>,
    /// Disk UUID (optional, auto-generated if not specified)
    #[serde(default)]
    pub disk_uuid: Option<Uuid>,
}

/// Request body for `resize_disk` action (bhyve only)
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ResizeDiskRequest {
    /// PCI slot of disk to resize (required)
    pub pci_slot: String,
    /// New size in MB (required)
    pub size: u64,
    /// Allow shrinking (dangerous operation, default: false)
    #[serde(default)]
    pub dangerous_allow_shrink: Option<bool>,
}

/// Request body for `delete_disk` action (bhyve only)
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct DeleteDiskRequest {
    /// PCI slot of disk to delete (required)
    pub pci_slot: String,
}

// ============================================================================
// VM Entity Types
// ============================================================================

/// NIC attached to a VM
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct Nic {
    /// MAC address
    pub mac: String,
    /// IP address (IPv4)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ip: Option<String>,
    /// IPv6 addresses
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ips: Option<Vec<String>>,
    /// Netmask
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub netmask: Option<String>,
    /// Gateway
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway: Option<String>,
    /// VLAN ID
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vlan_id: Option<u16>,
    /// NIC tag
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nic_tag: Option<String>,
    /// Network UUID
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_uuid: Option<Uuid>,
    /// Whether this is the primary NIC
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary: Option<bool>,
    /// NIC model (e.g., "virtio")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// Disk attached to a VM (bhyve VMs)
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct Disk {
    /// Disk UUID
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk_uuid: Option<Uuid>,
    /// PCI slot
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pci_slot: Option<String>,
    /// Disk path (zfs zvol path)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Disk size in MB
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// Boot disk flag
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boot: Option<bool>,
    /// Image UUID (for image-backed disks)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_uuid: Option<Uuid>,
}

/// Snapshot state
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SnapshotState {
    Queued,
    Creating,
    Created,
    Failed,
    Deleted,
    /// Unknown state (forward compatibility)
    #[serde(other)]
    Unknown,
}

/// Snapshot of a VM
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct Snapshot {
    /// Snapshot name
    pub name: String,
    /// Creation timestamp
    pub created_at: Timestamp,
    /// Snapshot state
    pub state: SnapshotState,
}

/// VM object returned by VMAPI
///
/// This is a large object with many fields. All fields use snake_case.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct Vm {
    /// VM UUID
    pub uuid: Uuid,
    /// VM alias/name
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    /// Brand (bhyve, kvm, joyent, joyent-minimal, lx)
    pub brand: Brand,
    /// Current state
    pub state: VmState,
    /// Owner UUID
    pub owner_uuid: Uuid,
    /// Image UUID
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_uuid: Option<Uuid>,
    /// Server UUID (compute node hosting the VM)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_uuid: Option<Uuid>,
    /// Billing ID (package UUID)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub billing_id: Option<Uuid>,
    /// RAM in MB
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ram: Option<u64>,
    /// Max physical memory in MB
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_physical_memory: Option<u64>,
    /// CPU cap (percentage, e.g., 100 = 1 full CPU)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_cap: Option<u32>,
    /// Disk quota in MB
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quota: Option<u64>,
    /// Max swap in MB
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_swap: Option<u64>,
    /// Max locked memory in MB
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_locked_memory: Option<u64>,
    /// Max LWPs (lightweight processes)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_lwps: Option<u64>,
    /// ZFS I/O priority
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zfs_io_priority: Option<u64>,
    /// Number of VCPUs
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vcpus: Option<u32>,
    /// Network interfaces
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nics: Option<Vec<Nic>>,
    /// Disks (bhyve VMs)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disks: Option<Vec<Disk>>,
    /// Snapshots
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshots: Option<Vec<Snapshot>>,
    /// Customer metadata
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub customer_metadata: Option<MetadataObject>,
    /// Internal metadata
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub internal_metadata: Option<MetadataObject>,
    /// Tags
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<Tags>,
    /// Firewall enabled
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub firewall_enabled: Option<bool>,
    /// DNS domain
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dns_domain: Option<String>,
    /// Resolvers (DNS servers)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolvers: Option<Vec<String>>,
    /// Autoboot flag
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub autoboot: Option<bool>,
    /// Platform build timestamp
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform_buildstamp: Option<String>,
    /// Creation timestamp
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub create_timestamp: Option<Timestamp>,
    /// Last modified timestamp
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_modified: Option<Timestamp>,
    /// Destroyed timestamp (if destroyed)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destroyed: Option<Timestamp>,
    /// Zone dataset UUID
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zonedataset: Option<String>,
    /// Zone path
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zonepath: Option<String>,
    /// Delegate dataset flag
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegate_dataset: Option<bool>,
    /// Docker container flag
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub docker: Option<bool>,
    /// Internal metadata namespaces
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub internal_metadata_namespaces: Option<Vec<String>>,
    /// Maintain resolvers flag
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maintain_resolvers: Option<bool>,
    /// Do not inventory flag (for operators)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub do_not_inventory: Option<bool>,
    /// Indestructible zoneroot flag
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indestructible_zoneroot: Option<bool>,
    /// Indestructible delegated flag
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indestructible_delegated: Option<bool>,
    /// Limit privilege flag
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit_priv: Option<String>,
    /// Free space in bytes (bhyve with flexible disk, may be negative)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub free_space: Option<i64>,
    /// Flexible disk mode (bhyve only)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flexible_disk_size: Option<u64>,
}

/// Response when VM creation or action returns a job
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct JobResponse {
    /// VM UUID
    pub vm_uuid: Uuid,
    /// Job UUID (for async operations)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_uuid: Option<Uuid>,
}

/// Request body for creating a new VM
#[derive(Debug, Deserialize, JsonSchema)]
pub struct CreateVmRequest {
    /// Owner UUID (required)
    pub owner_uuid: Uuid,
    /// VM alias/name
    #[serde(default)]
    pub alias: Option<String>,
    /// Brand (if not inferred from image)
    #[serde(default)]
    pub brand: Option<Brand>,
    /// Image UUID
    #[serde(default)]
    pub image_uuid: Option<Uuid>,
    /// Billing ID (package UUID)
    #[serde(default)]
    pub billing_id: Option<Uuid>,
    /// RAM in MB
    #[serde(default)]
    pub ram: Option<u64>,
    /// CPU cap
    #[serde(default)]
    pub cpu_cap: Option<u32>,
    /// Disk quota in MB
    #[serde(default)]
    pub quota: Option<u64>,
    /// Server UUID (for placement)
    #[serde(default)]
    pub server_uuid: Option<Uuid>,
    /// Networks to attach
    #[serde(default)]
    pub networks: Option<Vec<serde_json::Value>>,
    /// NICs configuration
    #[serde(default)]
    pub nics: Option<Vec<NicSpec>>,
    /// Disks configuration (bhyve)
    #[serde(default)]
    pub disks: Option<Vec<serde_json::Value>>,
    /// Firewall enabled
    #[serde(default)]
    pub firewall_enabled: Option<bool>,
    /// Customer metadata
    #[serde(default)]
    pub customer_metadata: Option<MetadataObject>,
    /// Internal metadata
    #[serde(default)]
    pub internal_metadata: Option<MetadataObject>,
    /// Tags
    #[serde(default)]
    pub tags: Option<Tags>,
    /// Delegate dataset flag
    #[serde(default)]
    pub delegate_dataset: Option<bool>,
    /// VCPUs
    #[serde(default)]
    pub vcpus: Option<u32>,
    /// Additional fields are passed through
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// Request body for PUT /vms (bulk update VMs for a server)
#[derive(Debug, Deserialize, JsonSchema)]
pub struct PutVmsRequest {
    /// Server UUID
    pub server_uuid: Uuid,
    /// Array of VMs to update
    pub vms: Vec<serde_json::Value>,
}

/// Request body for PUT /vms/:uuid (replace VM object)
#[derive(Debug, Deserialize, JsonSchema)]
pub struct PutVmRequest {
    /// Full VM object to replace
    #[serde(flatten)]
    pub vm: HashMap<String, serde_json::Value>,
}
