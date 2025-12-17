// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Miscellaneous types (packages, datacenters, services, migrations)

use super::common::{Brand, RoleTags, Timestamp, Uuid};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Path parameter for package operations
#[derive(Debug, Deserialize, JsonSchema)]
pub struct PackagePath {
    /// Account login name
    pub account: String,
    /// Package name or UUID
    pub package: String,
}

/// Disk configuration in a package (bhyve only)
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PackageDisk {
    /// Disk size in MB
    pub size: u64,
    /// Block size in bytes
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_size: Option<u64>,
}

/// Package information
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Package {
    /// Package UUID
    pub id: Uuid,
    /// Package name
    pub name: String,
    /// Memory in MB
    pub memory: u64,
    /// Disk space in MB
    pub disk: u64,
    /// Swap in MB
    pub swap: u64,
    /// VCPUs (defaults to 0)
    #[serde(default)]
    pub vcpus: u32,
    /// Lightweight processes limit
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lwps: Option<u32>,
    /// Version
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Group
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// Description
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Default package
    #[serde(default)]
    pub default: bool,
    /// Brand (joyent, bhyve, kvm, etc.)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brand: Option<Brand>,
    /// Flexible disk mode (bhyve only)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flexible_disk: Option<bool>,
    /// Disk configuration (bhyve only)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disks: Option<Vec<PackageDisk>>,
    /// Role tags for RBAC
    #[serde(rename = "role-tag", default, skip_serializing_if = "Option::is_none")]
    pub role_tag: Option<RoleTags>,
}

/// Datacenter map: name -> URL
///
/// The CloudAPI returns datacenters as a map where keys are datacenter names
/// and values are their URLs. Example:
/// ```json
/// {"us-central-1": "https://us-central-1.api.mnx.io"}
/// ```
pub type Datacenters = std::collections::HashMap<String, String>;

/// Datacenter information (used for add_foreign_datacenter response)
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Datacenter {
    /// Datacenter name
    pub name: String,
    /// Datacenter URL
    pub url: String,
}

/// Request to add foreign datacenter
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AddForeignDatacenterRequest {
    /// Datacenter name
    pub name: String,
    /// Datacenter URL
    pub url: String,
}

/// Services map: name -> URL
///
/// The CloudAPI returns services as a map where keys are service names
/// and values are their URLs. Example:
/// ```json
/// {"cmon": "https://cmon.example.com:9163", "docker": "tcp://docker.example.com:2376"}
/// ```
pub type Services = std::collections::HashMap<String, String>;

/// Service information (individual service entry, for documentation)
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Service {
    /// Service name
    pub name: String,
    /// Service URL
    pub endpoint: String,
}

/// Migration action to perform
///
/// These actions control the migration lifecycle:
/// - `begin`: Start a new migration
/// - `sync`: Sync data to target server
/// - `switch`: Switch instance to new server (finalize the migration)
/// - `automatic`: Perform automatic migration (begin + sync + switch)
/// - `abort`: Cancel the migration and clean up
/// - `pause`: Pause an in-progress migration
/// - `finalize`: Clean up after a successful switch
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum MigrationAction {
    /// Start a new migration
    Begin,
    /// Sync data to target server
    Sync,
    /// Switch instance to new server
    Switch,
    /// Perform automatic migration
    Automatic,
    /// Cancel the migration
    Abort,
    /// Pause the migration
    Pause,
    /// Clean up after switch
    Finalize,
}

/// Migration information
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Migration {
    /// Machine UUID being migrated
    ///
    /// Note: The Node.js CloudAPI translates VMAPI's `vm_uuid` to `machine`.
    /// We keep `vm_uuid` here for direct VMAPI compatibility, but the field
    /// may be renamed to `machine` in the response.
    #[serde(alias = "machine")]
    pub vm_uuid: Uuid,
    /// Migration phase (e.g., "start", "sync", "switch")
    pub phase: String,
    /// Migration state (e.g., "running", "paused", "successful", "failed")
    pub state: String,
    /// Progress percentage (0-100)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress_percent: Option<f64>,
    /// Creation timestamp
    pub created_timestamp: Timestamp,
    /// Last update timestamp
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_timestamp: Option<Timestamp>,
    /// Finished timestamp
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_timestamp: Option<Timestamp>,
    /// Whether this is an automatic migration
    #[serde(default)]
    pub automatic: Option<bool>,
    /// Progress history for detailed tracking
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress_history: Option<Vec<MigrationProgressEntry>>,
}

/// Progress entry in migration history
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct MigrationProgressEntry {
    /// Current progress value
    pub current_progress: u64,
    /// Total progress value
    pub total_progress: u64,
    /// Phase this entry belongs to
    pub phase: String,
    /// State of this phase
    pub state: String,
    /// Progress message
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// When this phase started
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_timestamp: Option<Timestamp>,
    /// When this phase finished
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_timestamp: Option<Timestamp>,
}

/// Migration estimate request
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MigrationEstimateRequest {
    /// Affinity rules
    #[serde(default)]
    pub affinity: Option<Vec<String>>,
}

/// Migration estimate response
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MigrationEstimate {
    /// Estimated migration size in bytes
    pub size: u64,
    /// Estimated duration in seconds
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<u64>,
}

/// Migration request
///
/// Used to perform migration actions on an instance. The `action` field
/// specifies which migration operation to perform.
///
/// # Examples
///
/// Start a new migration:
/// ```json
/// {"action": "begin"}
/// ```
///
/// Start migration with affinity rules:
/// ```json
/// {"action": "begin", "affinity": ["instance!=web-*"]}
/// ```
///
/// Switch to the new server:
/// ```json
/// {"action": "switch"}
/// ```
///
/// Abort an in-progress migration:
/// ```json
/// {"action": "abort"}
/// ```
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MigrateRequest {
    /// Migration action to perform
    pub action: MigrationAction,
    /// Affinity rules (only valid for "begin" and "automatic" actions)
    ///
    /// These rules influence which server the instance will be migrated to.
    #[serde(default)]
    pub affinity: Option<Vec<String>>,
}

/// Path parameter for resource role tag operations
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ResourcePath {
    /// Account login name
    pub account: String,
    /// Resource name (e.g., "machines", "images")
    pub resource_name: String,
}

/// Path parameter for specific resource role tag operations
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ResourceIdPath {
    /// Account login name
    pub account: String,
    /// Resource name (e.g., "machines", "images")
    pub resource_name: String,
    /// Resource UUID
    pub resource_id: Uuid,
}

/// Path parameter for user key resource role tag operations
#[derive(Debug, Deserialize, JsonSchema)]
pub struct UserKeyResourcePath {
    /// Account login name
    pub account: String,
    /// User UUID or login
    pub uuid: String,
    /// Resource ID (key name/fingerprint)
    pub resource_id: String,
}
