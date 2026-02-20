// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Miscellaneous types (packages, datacenters, services, migrations)

use super::common::{RoleTags, Timestamp, Uuid};
// Package output type uses VMAPI's Brand to accurately represent the brand
// requirement stored in the system, which may include internal-only brands.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use vmapi_api::Brand as VmapiBrand;

/// Path parameter for package operations
#[derive(Debug, Deserialize, JsonSchema)]
pub struct PackagePath {
    /// Account login name
    pub account: String,
    /// Package name or UUID
    pub package: String,
}

/// Disk size value in a package definition
///
/// CloudAPI returns disk sizes as either a numeric value (megabytes)
/// or a string like "remaining" indicating use of remaining space.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum DiskSize {
    /// Size in megabytes
    Megabytes(u64),

    /// Named size value (e.g. "remaining")
    Named(String),
}

/// Disk configuration in a package (bhyve only)
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PackageDisk {
    /// Disk size in MB or a named value like "remaining"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<DiskSize>,

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
    /// Brand (joyent, bhyve, kvm, etc., including internal-only brands)
    ///
    /// Uses VMAPI's Brand enum to support internal-only brands like "builder"
    /// that may exist in the system.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brand: Option<VmapiBrand>,
    /// Flexible disk mode (bhyve only)
    #[serde(
        rename = "flexible_disk",
        default,
        skip_serializing_if = "Option::is_none"
    )]
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

/// Migration state
///
/// Represents the current state of a migration operation.
#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, clap::ValueEnum,
)]
#[serde(rename_all = "lowercase")]
pub enum MigrationState {
    Running,
    Paused,
    Successful,
    Finished,
    Failed,
    Aborted,
    #[serde(other)]
    Unknown,
}

/// Migration phase
///
/// Represents the current phase of a migration operation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MigrationPhase {
    Begin,
    Sync,
    Switch,
    #[serde(other)]
    Unknown,
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
    /// Migration phase
    pub phase: MigrationPhase,
    /// Migration state
    pub state: MigrationState,
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
    pub phase: MigrationPhase,
    /// State of this phase
    pub state: MigrationState,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Test that a simple package (without disks) can be deserialized.
    ///
    /// This is the format returned for standard SmartOS/LX packages.
    #[test]
    fn test_package_deserialize_simple() {
        let json = r#"{
            "default": false,
            "disk": 3072,
            "id": "a50fa089-2bb6-47d5-9a68-dc71c7b0cd03",
            "lwps": 4000,
            "memory": 128,
            "name": "sample-128M",
            "swap": 512,
            "vcpus": 1,
            "version": "1.0.0"
        }"#;

        let package: Package = serde_json::from_str(json).expect("should deserialize");
        assert_eq!(package.name, "sample-128M");
        assert_eq!(package.memory, 128);
        assert_eq!(package.disk, 3072);
        assert!(!package.default);
        assert!(package.disks.is_none());
    }

    /// Test that a bhyve package with empty disk entry can be deserialized.
    ///
    /// The CloudAPI returns bhyve packages with `disks` arrays that may contain
    /// empty objects `{}` to indicate "use remaining space". This test verifies
    /// that such packages can be deserialized correctly.
    ///
    #[test]
    fn test_package_deserialize_bhyve_with_empty_disk() {
        let json = r#"{
            "brand": "bhyve",
            "default": true,
            "disk": 24576,
            "id": "d4cab42f-3a39-4b4c-9d9b-d40cb202f0eb",
            "lwps": 4000,
            "memory": 1024,
            "name": "sample-bhyve-flexible-1G",
            "swap": 4096,
            "vcpus": 1,
            "version": "1.0.0",
            "flexible_disk": true,
            "disks": [{}]
        }"#;

        let package: Package = serde_json::from_str(json).expect("should deserialize");
        assert_eq!(package.name, "sample-bhyve-flexible-1G");
        assert_eq!(package.flexible_disk, Some(true));
        assert!(package.disks.is_some());
    }

    /// Test that the full package list response from CloudAPI can be deserialized.
    ///
    /// This test uses the exact JSON payload that caused the "Invalid Response Payload"
    /// error during `triton instance create`. The error occurred at column 2064 which
    /// corresponds to a bhyve package with `"disks":[{}]` - an empty disk object that
    /// is missing the required `size` field.
    ///
    /// The node-triton client handles this correctly, but the Rust client fails because
    /// `PackageDisk.size` is required.
    ///
    #[test]
    fn test_package_list_deserialize_with_bhyve_empty_disk() {
        // This is the exact JSON from the error message during `triton instance create`
        let json = r#"[
            {
                "default": false,
                "disk": 3072,
                "id": "a50fa089-2bb6-47d5-9a68-dc71c7b0cd03",
                "lwps": 4000,
                "memory": 128,
                "name": "sample-128M",
                "swap": 512,
                "vcpus": 1,
                "version": "1.0.0"
            },
            {
                "default": false,
                "disk": 6144,
                "id": "c5cd8a4a-c155-4145-8c3c-add3084eff6d",
                "lwps": 4000,
                "memory": 256,
                "name": "sample-256M",
                "swap": 1024,
                "vcpus": 1,
                "version": "1.0.0"
            },
            {
                "default": false,
                "disk": 12288,
                "id": "5256392c-2d60-4ab6-b479-006be74eb50a",
                "lwps": 4000,
                "memory": 512,
                "name": "sample-512M",
                "swap": 2048,
                "vcpus": 1,
                "version": "1.0.0"
            },
            {
                "default": true,
                "disk": 25600,
                "id": "5cc453da-fca8-4cd8-87b1-9bf15826d3a6",
                "lwps": 4000,
                "memory": 1024,
                "name": "sample-1G",
                "swap": 4096,
                "vcpus": 1,
                "version": "1.0.0"
            },
            {
                "default": false,
                "disk": 51200,
                "id": "a6342267-49ac-4904-bb5e-fe1cdc5f14a7",
                "lwps": 4000,
                "memory": 2048,
                "name": "sample-2G",
                "swap": 8192,
                "vcpus": 1,
                "version": "1.0.0"
            },
            {
                "default": false,
                "disk": 102400,
                "id": "fa5fc249-d1d8-4247-8aba-766fb39c96f4",
                "lwps": 4000,
                "memory": 4096,
                "name": "sample-4G",
                "swap": 16384,
                "vcpus": 1,
                "version": "1.0.0"
            },
            {
                "default": false,
                "disk": 204800,
                "id": "0d8198d8-903d-4597-a92b-3042ae5e2c31",
                "lwps": 4000,
                "memory": 8192,
                "name": "sample-8G",
                "swap": 32768,
                "vcpus": 1,
                "version": "1.0.0"
            },
            {
                "default": false,
                "disk": 409600,
                "id": "1ae37cc9-db17-4b02-a9d2-5146496e0802",
                "lwps": 4000,
                "memory": 16384,
                "name": "sample-16G",
                "swap": 65536,
                "vcpus": 1,
                "version": "1.0.0"
            },
            {
                "default": false,
                "disk": 819200,
                "id": "c2e0b9c5-4f6f-47ba-8f1f-5cd901447ea8",
                "lwps": 4000,
                "memory": 32768,
                "name": "sample-32G",
                "swap": 131072,
                "vcpus": 1,
                "version": "1.0.0"
            },
            {
                "default": false,
                "disk": 1638400,
                "id": "0efa587b-7124-4142-bd0d-4c923e6fc18f",
                "lwps": 4000,
                "memory": 65536,
                "name": "sample-64G",
                "swap": 262144,
                "vcpus": 1,
                "version": "1.0.0"
            },
            {
                "brand": "bhyve",
                "default": true,
                "disk": 24576,
                "id": "d4cab42f-3a39-4b4c-9d9b-d40cb202f0eb",
                "lwps": 4000,
                "memory": 1024,
                "name": "sample-bhyve-flexible-1G",
                "swap": 4096,
                "vcpus": 1,
                "version": "1.0.0",
                "flexible_disk": true
            },
            {
                "brand": "bhyve",
                "default": true,
                "disk": 40960,
                "id": "1b00c697-49f9-484b-89a5-51126911ed6e",
                "lwps": 4000,
                "memory": 1024,
                "name": "sample-bhyve-reserved-snapshots-space",
                "swap": 4096,
                "vcpus": 1,
                "version": "1.0.0",
                "flexible_disk": true,
                "disks": [{}, {"size": 10240}]
            },
            {
                "brand": "bhyve",
                "default": true,
                "disk": 24576,
                "id": "de4c471c-bc46-4497-b4ed-8c4f06867a56",
                "lwps": 4000,
                "memory": 1024,
                "name": "sample-bhyve-three-disks",
                "swap": 4096,
                "vcpus": 1,
                "version": "1.0.0",
                "flexible_disk": true,
                "disks": [{}, {"size": 6144}, {"size": "remaining"}]
            }
        ]"#;

        let packages: Vec<Package> =
            serde_json::from_str(json).expect("should deserialize package list");
        assert_eq!(packages.len(), 13);

        // Verify the problematic bhyve packages are present
        let reserved_snapshots = packages
            .iter()
            .find(|p| p.name == "sample-bhyve-reserved-snapshots-space")
            .expect("should find reserved-snapshots package");
        assert!(reserved_snapshots.disks.is_some());

        let three_disks = packages
            .iter()
            .find(|p| p.name == "sample-bhyve-three-disks")
            .expect("should find three-disks package");
        assert!(three_disks.disks.is_some());
    }
}
