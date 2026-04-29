// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Machine-related types

use super::common::{Brand, Metadata, RoleTags, Tags, Timestamp, Uuid};
use super::misc::DiskSize;
use super::network::NetworkIds;
use super::volume::VolumeType;

/// Affinity rule strings (used by CreateMachineRequest and MigrateRequest).
///
/// Newtype wrapper rather than a type alias so the generated OpenAPI spec
/// carries a single named `AffinityRules` schema shared between machine
/// provisioning and migration.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AffinityRules(pub Vec<String>);

impl std::ops::Deref for AffinityRules {
    type Target = Vec<String>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for AffinityRules {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl From<Vec<String>> for AffinityRules {
    fn from(v: Vec<String>) -> Self {
        AffinityRules(v)
    }
}

impl<S: Into<String>> FromIterator<S> for AffinityRules {
    fn from_iter<I: IntoIterator<Item = S>>(iter: I) -> Self {
        AffinityRules(iter.into_iter().map(Into::into).collect())
    }
}

impl IntoIterator for AffinityRules {
    type Item = String;
    type IntoIter = std::vec::IntoIter<String>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<'a> IntoIterator for &'a AffinityRules {
    type Item = &'a String;
    type IntoIter = std::slice::Iter<'a, String>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}
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
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, strum::Display)]
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
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MachineType {
    /// SmartOS zone or Linux container
    Smartmachine,
    /// Hardware VM (KVM or bhyve)
    Virtualmachine,
    /// Unknown type (forward compatibility)
    #[serde(other)]
    Unknown,
}

/// Machine information
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
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
    pub networks: Option<NetworkIds>,
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

    #[serde(
        rename = "firewall_enabled",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub firewall_enabled: Option<bool>,
    /// Deletion protection enabled

    #[serde(
        rename = "deletion_protection",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub deletion_protection: Option<bool>,
    /// Compute node UUID (server hosting the VM)

    #[serde(
        rename = "compute_node",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub compute_node: Option<Uuid>,
    /// DNS names (CNS feature)

    #[serde(rename = "dns_names", default, skip_serializing_if = "Option::is_none")]
    pub dns_names: Option<Vec<String>>,
    /// Free space in bytes (bhyve with flexible disk, may be negative)

    #[serde(
        rename = "free_space",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub free_space: Option<i64>,
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
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MachineDisk {
    /// Disk UUID
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Uuid>,
    /// Disk size in MB, or "remaining" for all available space
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<DiskSize>,
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
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
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

/// Network object for instance creation
///
/// Matches the CloudAPI wire format for the `networks` array.
/// Each entry specifies a network UUID and optional IP address.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct NetworkObject {
    /// Network UUID
    pub ipv4_uuid: Uuid,
    /// IP addresses (array with max 1 element, matching CloudAPI format)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ipv4_ips: Option<Vec<String>>,
    /// Mark as primary NIC
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary: Option<bool>,
}

/// Request to create a machine
///
/// This struct supports both the modern nested format and the legacy flattened format:
///
/// **Modern format:**
/// ```json
/// {"image": "...", "tags": {"foo": "bar"}, "metadata": {"key": "value"}}
/// ```
///
/// **Legacy format:**
/// ```json
/// {"image": "...", "tag.foo": "bar", "metadata.key": "value"}
/// ```
#[derive(Debug, Deserialize, JsonSchema)]
pub struct CreateMachineRequest {
    /// Machine alias/name
    #[serde(default)]
    pub name: Option<String>,
    /// Image UUID
    pub image: Uuid,
    /// Package name or UUID
    pub package: String,
    /// Networks - array of network objects matching CloudAPI wire format
    #[serde(default)]
    pub networks: Option<Vec<NetworkObject>>,
    /// Affinity rules for instance placement (added in CloudAPI v8.3.0)
    ///
    /// Rules follow the pattern: `<key><operator><value>` where:
    /// - key: 'instance', 'container', or a tag name
    /// - operator: '==' (must), '!=' (must not), '==~' (prefer), '!=~' (prefer not)
    /// - value: exact string, glob pattern (*), or regex (/pattern/)
    ///
    /// Examples: `instance==myvm`, `role!=database`, `instance!=~foo*`
    #[serde(default)]
    pub affinity: Option<AffinityRules>,
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
    // These are captured by serde's flatten and processed by helper methods.
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
                    result.insert(meta_key.to_string(), value.clone());
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
    /// send it in the request body instead. Body takes precedence over the
    /// query parameter.
    // Implementation note: matches Restify's mapParams behavior.
    // Service implementations should check the body first, then fall back
    // to this query parameter.
    #[serde(default)]
    pub action: Option<MachineAction>,
}

/// Request to start a machine
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct StartMachineRequest {
    /// Origin identifier (defaults to 'cloudapi')
    #[serde(default)]
    pub origin: Option<String>,
}

/// Request to stop a machine
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct StopMachineRequest {
    /// Origin identifier (defaults to 'cloudapi')
    #[serde(default)]
    pub origin: Option<String>,
}

/// Request to reboot a machine
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct RebootMachineRequest {
    /// Origin identifier (defaults to 'cloudapi')
    #[serde(default)]
    pub origin: Option<String>,
}

/// Request to resize a machine
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ResizeMachineRequest {
    /// New package name or UUID
    pub package: String,
    /// Origin identifier (defaults to 'cloudapi')
    #[serde(default)]
    pub origin: Option<String>,
}

/// Request to rename a machine
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct RenameMachineRequest {
    /// New machine alias/name (max 189 chars, or 63 if CNS enabled)
    pub name: String,
    /// Origin identifier (defaults to 'cloudapi')
    #[serde(default)]
    pub origin: Option<String>,
}

/// Request to enable firewall
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct EnableFirewallRequest {
    /// Origin identifier (defaults to 'cloudapi')
    #[serde(default)]
    pub origin: Option<String>,
}

/// Request to disable firewall
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct DisableFirewallRequest {
    /// Origin identifier (defaults to 'cloudapi')
    #[serde(default)]
    pub origin: Option<String>,
}

/// Request to enable deletion protection
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct EnableDeletionProtectionRequest {
    /// Origin identifier (defaults to 'cloudapi')
    #[serde(default)]
    pub origin: Option<String>,
}

/// Request to disable deletion protection
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
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
#[derive(Debug, Deserialize, JsonSchema)]
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
    // These are captured by serde's flatten and processed by helper methods.
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

/// Success status for an audit entry.
///
/// CloudAPI sends `"yes"` or `"no"` strings on the wire (not booleans).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum AuditSuccess {
    Yes,
    No,
    #[serde(other)]
    Unknown,
}

/// Audit entry for a machine
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
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
    pub success: Option<AuditSuccess>,
}
