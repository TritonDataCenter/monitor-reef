// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Machine-related types

use super::common::{Brand, Metadata, RoleTags, Tags, Timestamp, Uuid};
use super::misc::DiskSize;
use super::volume::VolumeType;
// Machine output type and ListMachinesQuery use VMAPI's Brand to support internal-only
// brands (e.g., "builder") that may appear in changefeed messages or operator queries.
// Only CreateMachineRequest uses CloudAPI's restrictive Brand to validate provisioning.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use vmapi_api::Brand as VmapiBrand;

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
#[derive(
    Debug,
    Clone,
    Copy,
    Serialize,
    Deserialize,
    JsonSchema,
    PartialEq,
    Eq,
    clap::ValueEnum,
    strum::Display,
)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum MachineState {
    Running,
    Stopped,
    Stopping,
    Provisioning,
    Failed,
    Deleted,
    Offline,
    Ready,
    #[serde(other)]
    Unknown,
}

/// Machine type (virtualization category)
#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, clap::ValueEnum,
)]
#[serde(rename_all = "lowercase")]
pub enum MachineType {
    /// SmartOS zone or Linux container
    Smartmachine,
    /// Hardware VM (KVM or bhyve)
    Virtualmachine,
    /// Unknown type (forward compatibility)
    #[serde(other)]
    #[clap(skip)]
    Unknown,
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
    /// Brand (joyent, kvm, bhyve, lx, joyent-minimal, and internal-only brands)
    ///
    /// Uses VMAPI's Brand enum to support internal-only brands like "builder"
    /// that may be returned by VMAPI but cannot be provisioned via CloudAPI.
    pub brand: VmapiBrand,
    /// Current state
    pub state: MachineState,
    /// Image UUID
    pub image: Uuid,
    /// Package name
    pub package: String,
    /// RAM in MB (may be null for some zone types like LX)
    pub memory: Option<u64>,
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
    /// Note: CloudAPI returns this as snake_case despite other fields being camelCase
    #[serde(
        rename = "firewall_enabled",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub firewall_enabled: Option<bool>,
    /// Deletion protection enabled
    /// Note: CloudAPI returns this as snake_case despite other fields being camelCase
    #[serde(
        rename = "deletion_protection",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub deletion_protection: Option<bool>,
    /// Compute node UUID (server hosting the VM)
    /// Note: CloudAPI returns this as snake_case despite other fields being camelCase
    #[serde(
        rename = "compute_node",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub compute_node: Option<Uuid>,
    /// DNS names (CNS feature)
    /// Note: CloudAPI returns this as snake_case despite other fields being camelCase
    #[serde(rename = "dns_names", default, skip_serializing_if = "Option::is_none")]
    pub dns_names: Option<Vec<String>>,
    /// Free space in bytes (bhyve with flexible disk)
    /// Note: CloudAPI returns this as snake_case despite other fields being camelCase
    #[serde(
        rename = "free_space",
        default,
        skip_serializing_if = "Option::is_none"
    )]
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
    /// Note: CloudAPI returns this as snake_case despite other fields being camelCase
    #[serde(
        rename = "delegate_dataset",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub delegate_dataset: Option<bool>,
    /// Role tags for RBAC
    #[serde(rename = "role-tag", default, skip_serializing_if = "Option::is_none")]
    pub role_tag: Option<RoleTags>,
}

/// Disk attached to a machine (bhyve VMs)
///
/// CloudAPI may omit `size` for the boot disk (which inherits its size
/// from the image) and may return `"remaining"` for flexible disks.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MachineDisk {
    /// Disk UUID
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Uuid>,
    /// Disk size in MB, or "remaining" for all available space
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<DiskSize>,
    /// Block size in bytes
    /// Note: CloudAPI returns this as snake_case despite other fields being camelCase
    #[serde(
        rename = "block_size",
        default,
        skip_serializing_if = "Option::is_none"
    )]
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

/// Volume mount mode
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MountMode {
    /// Read-write
    Rw,
    /// Read-only
    Ro,
    /// Unknown mode (forward compatibility)
    #[serde(other)]
    Unknown,
}

/// Volume mount specification for instance creation
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VolumeMount {
    /// Volume name
    pub name: String,
    /// Mount mode (defaults to read-write)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<MountMode>,
    /// Mount point inside the instance
    pub mountpoint: String,
    /// Volume type (defaults to tritonnfs)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(rename = "type")]
    pub volume_type: Option<VolumeType>,
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
///
/// This struct supports both the modern nested format and the legacy flattened format:
///
/// **Modern format (Rust clients):**
/// ```json
/// {"image": "...", "tags": {"foo": "bar"}, "metadata": {"key": "value"}}
/// ```
///
/// **Legacy format (Node.js clients):**
/// ```json
/// {"image": "...", "tag.foo": "bar", "metadata.key": "value"}
/// ```
///
/// Use the `tags()` and `metadata()` methods to get the merged result from both formats.
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
    /// Metadata (modern format - nested object)
    #[serde(default)]
    pub metadata: Option<Metadata>,
    /// Tags (modern format - nested object)
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
    /// Extra fields for legacy format support (tag.*, metadata.*)
    /// These are captured by serde's flatten and processed by helper methods.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

impl CreateMachineRequest {
    /// Get all tags, merging modern `tags` field with legacy `tag.*` fields
    ///
    /// Legacy `tag.KEY=VALUE` fields take precedence over the modern `tags` object
    /// to maintain backwards compatibility.
    pub fn tags(&self) -> Tags {
        let mut result = self.tags.clone().unwrap_or_default();

        // Extract legacy tag.* fields
        for (key, value) in &self.extra {
            if let Some(tag_key) = key.strip_prefix("tag.") {
                result.insert(tag_key.to_string(), value.clone());
            }
        }

        result
    }

    /// Get all metadata, merging modern `metadata` field with legacy `metadata.*` fields
    ///
    /// Legacy `metadata.KEY=VALUE` fields take precedence over the modern `metadata` object
    /// to maintain backwards compatibility.
    pub fn metadata(&self) -> Metadata {
        let mut result = self.metadata.clone().unwrap_or_default();

        // Extract legacy metadata.* fields (excluding *_pw for passwords)
        for (key, value) in &self.extra {
            if let Some(meta_key) = key.strip_prefix("metadata.") {
                // Skip password fields (handled separately)
                if !meta_key.ends_with("_pw") {
                    if let Some(s) = value.as_str() {
                        result.insert(meta_key.to_string(), s.to_string());
                    } else {
                        // Convert non-string values to string
                        result.insert(meta_key.to_string(), value.to_string());
                    }
                }
            }
        }

        result
    }

    /// Check if this request has any tags (from either format)
    pub fn has_tags(&self) -> bool {
        if self.tags.as_ref().is_some_and(|t| !t.is_empty()) {
            return true;
        }
        self.extra.keys().any(|k| k.starts_with("tag."))
    }

    /// Check if this request has any metadata (from either format)
    pub fn has_metadata(&self) -> bool {
        if self.metadata.as_ref().is_some_and(|m| !m.is_empty()) {
            return true;
        }
        self.extra
            .keys()
            .any(|k| k.starts_with("metadata.") && !k.ends_with("_pw"))
    }
}

/// Machine action for action dispatch
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
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
    /// Unknown action (forward compatibility)
    #[serde(other)]
    Unknown,
}

/// Query parameter for machine actions
#[derive(Debug, Deserialize, JsonSchema)]
pub struct MachineActionQuery {
    /// Action to perform. Optional in the query string because clients may
    /// send it in the request body instead (matching Restify's mapParams
    /// behavior). Service implementations should check the body first,
    /// then fall back to this query parameter.
    #[serde(default)]
    pub action: Option<MachineAction>,
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
///
/// This struct supports both the modern single-tag format and the legacy multi-tag format:
///
/// **Modern format:** `?tag=key=value` (single tag filter)
///
/// **Legacy format:** `?tag.env=prod&tag.role=web` (multiple tag filters)
///
/// Use the `tag_filters()` method to get all tag filters from both formats.
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
    /// Filter by brand (accepts any brand including internal-only brands)
    #[serde(default)]
    pub brand: Option<VmapiBrand>,
    /// Pagination offset
    #[serde(default)]
    pub offset: Option<u64>,
    /// Pagination limit
    #[serde(default)]
    pub limit: Option<u64>,
    /// Filter by tag (modern format: key=value)
    #[serde(default)]
    pub tag: Option<String>,
    /// Filter by docker flag (added in CloudAPI 8.0.0)
    #[serde(default)]
    pub docker: Option<bool>,
    /// Include generated credentials in response
    #[serde(default)]
    pub credentials: Option<bool>,
    /// Include destroyed/tombstone machines
    #[serde(default)]
    pub tombstone: Option<bool>,
    /// Extra fields for legacy format support (tag.*)
    /// These are captured by serde's flatten and processed by helper methods.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, String>,
}

impl ListMachinesQuery {
    /// Get all tag filters, merging modern `tag` field with legacy `tag.*` fields
    ///
    /// Returns a HashMap of tag_key -> expected_value for filtering.
    ///
    /// # Examples
    ///
    /// Modern format `?tag=env=prod` returns `{"env": "prod"}`
    /// Legacy format `?tag.env=prod&tag.role=web` returns `{"env": "prod", "role": "web"}`
    pub fn tag_filters(&self) -> std::collections::HashMap<String, String> {
        let mut result = std::collections::HashMap::new();

        // Parse modern format: tag=key=value
        if let Some(tag_str) = &self.tag
            && let Some((key, value)) = tag_str.split_once('=')
        {
            result.insert(key.to_string(), value.to_string());
        }

        // Extract legacy tag.* fields
        for (key, value) in &self.extra {
            if let Some(tag_key) = key.strip_prefix("tag.") {
                result.insert(tag_key.to_string(), value.clone());
            }
        }

        result
    }

    /// Check if this query has any tag filters
    pub fn has_tag_filters(&self) -> bool {
        self.tag.is_some() || self.extra.keys().any(|k| k.starts_with("tag."))
    }
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
