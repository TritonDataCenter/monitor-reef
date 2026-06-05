// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Types for the Triton Packages API (PAPI).
//!
//! All fields use snake_case in the JSON wire format (no rename_all needed).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// UUID type alias.
pub type Uuid = uuid::Uuid;

// ============================================================================
// Enums
// ============================================================================

/// VM brand for a package.
///
/// Determines which virtualization technology is used.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub enum Brand {
    #[serde(rename = "bhyve")]
    Bhyve,
    #[serde(rename = "joyent")]
    Joyent,
    #[serde(rename = "joyent-minimal")]
    JoyentMinimal,
    #[serde(rename = "kvm")]
    Kvm,
    #[serde(rename = "lx")]
    Lx,
    /// Catch-all for brands added after this client was compiled.
    #[serde(other)]
    Unknown,
}

/// Server allocation spread strategy.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub enum AllocServerSpread {
    #[serde(rename = "min-ram")]
    MinRam,
    #[serde(rename = "random")]
    Random,
    #[serde(rename = "min-owner")]
    MinOwner,
    /// Catch-all for strategies added after this client was compiled.
    #[serde(other)]
    Unknown,
}

/// Backend health status from the ping endpoint.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum BackendStatus {
    Up,
    Down,
    /// Catch-all for statuses added after this client was compiled.
    #[serde(other)]
    Unknown,
}

/// Sort order for list queries.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub enum SortOrder {
    #[serde(rename = "ASC")]
    Asc,
    #[serde(rename = "DESC")]
    Desc,
}

/// Disk size: either a positive integer (MiB) or the literal string "remaining".
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(untagged)]
pub enum DiskSize {
    /// Size in MiB.
    Size(u64),
    /// Use all remaining disk space.
    Remaining(DiskSizeRemaining),
}

/// Helper enum for the "remaining" string variant of DiskSize.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub enum DiskSizeRemaining {
    #[serde(rename = "remaining")]
    Remaining,
}

// ============================================================================
// Resource Types
// ============================================================================

/// A disk specification within a package (used with flexible_disk).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DiskSpec {
    /// Disk size in MiB, or "remaining" to use all remaining space.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<DiskSize>,
}

/// A package definition.
///
/// Packages define resource allocations (RAM, CPU, disk, etc.) used by
/// other services to provision VMs.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Package {
    /// Unique package identifier.
    pub uuid: Uuid,
    /// Package name.
    pub name: String,
    /// Semver version string.
    pub version: String,
    /// Whether the package can be used for provisioning.
    pub active: bool,
    /// CPU cap value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_cap: Option<u64>,
    /// Maximum number of lightweight processes.
    pub max_lwps: u64,
    /// Maximum physical memory in MiB.
    pub max_physical_memory: u64,
    /// Maximum swap in MiB.
    pub max_swap: u64,
    /// Disk quota in MiB (must be a multiple of 1024).
    pub quota: u64,
    /// ZFS I/O priority (0..16383).
    pub zfs_io_priority: u64,
    /// Schema version (always 1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub v: Option<u64>,

    // Optional fields
    /// VM brand.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brand: Option<Brand>,
    /// Owner UUIDs that can use this package. Absent means universal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_uuids: Option<Vec<Uuid>>,
    /// Number of virtual CPUs (1..64).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vcpus: Option<u64>,
    /// Deprecated: SDC 6.5 default flag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<bool>,
    /// Package grouping name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// Human-readable description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Display name shown in the portal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub common_name: Option<String>,
    /// Network UUIDs available to this package.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub networks: Option<Vec<Uuid>>,
    /// Operating system.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os: Option<String>,
    /// Minimum platform versions: maps SDC version to platform date.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_platform: Option<HashMap<String, String>>,
    /// Parent package name or UUID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// Freeform JSON object for DAPI traits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub traits: Option<serde_json::Value>,
    /// CPU shares (aka cpu_shares).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fss: Option<u64>,
    /// CPU burst ratio.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_burst_ratio: Option<f64>,
    /// RAM ratio.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ram_ratio: Option<f64>,
    /// Creation timestamp (ISO 8601).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    /// Last update timestamp (ISO 8601).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    /// Opaque billing tag string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub billing_tag: Option<String>,
    /// Server allocation spread strategy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alloc_server_spread: Option<AllocServerSpread>,
    /// Whether flexible disk mode is enabled (bhyve only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flexible_disk: Option<bool>,
    /// Disk specifications (only valid when flexible_disk is true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disks: Option<Vec<DiskSpec>>,
}

// ============================================================================
// Ping
// ============================================================================

/// Response from the /ping health check endpoint.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct PingResponse {
    /// Process ID of the PAPI service.
    pub pid: u32,
    /// Backend (Moray) health status.
    pub backend: BackendStatus,
    /// Backend error message, if backend is down.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend_error: Option<String>,
}

// ============================================================================
// Request / Query Types
// ============================================================================

/// Request body for creating a new package.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct CreatePackageRequest {
    /// Package UUID (auto-generated if not provided).
    #[serde(default)]
    pub uuid: Option<Uuid>,
    /// Package name.
    pub name: String,
    /// Semver version string.
    pub version: String,
    /// Whether the package is active for provisioning.
    pub active: bool,
    /// CPU cap value.
    #[serde(default)]
    pub cpu_cap: Option<u64>,
    /// Maximum number of lightweight processes (min: 250).
    pub max_lwps: u64,
    /// Maximum physical memory in MiB (min: 64).
    pub max_physical_memory: u64,
    /// Maximum swap in MiB (min: 128, must be >= max_physical_memory).
    pub max_swap: u64,
    /// Disk quota in MiB (min: 1024, must be multiple of 1024).
    pub quota: u64,
    /// ZFS I/O priority (0..16383).
    pub zfs_io_priority: u64,

    // Optional fields
    /// VM brand.
    #[serde(default)]
    pub brand: Option<Brand>,
    /// Owner UUIDs. Absent means universal package.
    #[serde(default)]
    pub owner_uuids: Option<Vec<Uuid>>,
    /// Number of virtual CPUs (1..64).
    #[serde(default)]
    pub vcpus: Option<u64>,
    /// Deprecated: SDC 6.5 default flag.
    #[serde(default)]
    pub default: Option<bool>,
    /// Package grouping.
    #[serde(default)]
    pub group: Option<String>,
    /// Human-readable description.
    #[serde(default)]
    pub description: Option<String>,
    /// Display name in portal.
    #[serde(default)]
    pub common_name: Option<String>,
    /// Network UUIDs.
    #[serde(default)]
    pub networks: Option<Vec<Uuid>>,
    /// Operating system.
    #[serde(default)]
    pub os: Option<String>,
    /// Minimum platform versions.
    #[serde(default)]
    pub min_platform: Option<HashMap<String, String>>,
    /// Parent package name or UUID.
    #[serde(default)]
    pub parent: Option<String>,
    /// Freeform DAPI traits object.
    #[serde(default)]
    pub traits: Option<serde_json::Value>,
    /// CPU shares.
    #[serde(default)]
    pub fss: Option<u64>,
    /// CPU burst ratio.
    #[serde(default)]
    pub cpu_burst_ratio: Option<f64>,
    /// RAM ratio.
    #[serde(default)]
    pub ram_ratio: Option<f64>,
    /// Billing tag.
    #[serde(default)]
    pub billing_tag: Option<String>,
    /// Server allocation spread strategy.
    #[serde(default)]
    pub alloc_server_spread: Option<AllocServerSpread>,
    /// Enable flexible disk mode (bhyve only).
    #[serde(default)]
    pub flexible_disk: Option<bool>,
    /// Disk specifications (only valid when flexible_disk is true).
    #[serde(default)]
    pub disks: Option<Vec<DiskSpec>>,
    /// Skip validation step.
    #[serde(default)]
    pub skip_validation: Option<bool>,
}

/// Request body for updating a package.
///
/// All fields are optional. Immutable fields can only be changed when
/// `force` is true.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct UpdatePackageRequest {
    /// Whether the package is active for provisioning.
    #[serde(default)]
    pub active: Option<bool>,
    /// Owner UUIDs.
    #[serde(default)]
    pub owner_uuids: Option<Vec<Uuid>>,
    /// Deprecated: SDC 6.5 default flag.
    #[serde(default)]
    pub default: Option<bool>,
    /// Package grouping.
    #[serde(default)]
    pub group: Option<String>,
    /// Human-readable description.
    #[serde(default)]
    pub description: Option<String>,
    /// Display name in portal.
    #[serde(default)]
    pub common_name: Option<String>,
    /// Network UUIDs.
    #[serde(default)]
    pub networks: Option<Vec<Uuid>>,
    /// Minimum platform versions.
    #[serde(default)]
    pub min_platform: Option<HashMap<String, String>>,
    /// Parent package name or UUID.
    #[serde(default)]
    pub parent: Option<String>,
    /// Freeform DAPI traits object.
    #[serde(default)]
    pub traits: Option<serde_json::Value>,
    /// CPU shares.
    #[serde(default)]
    pub fss: Option<u64>,
    /// CPU burst ratio.
    #[serde(default)]
    pub cpu_burst_ratio: Option<f64>,
    /// RAM ratio.
    #[serde(default)]
    pub ram_ratio: Option<f64>,
    /// Billing tag.
    #[serde(default)]
    pub billing_tag: Option<String>,
    /// Server allocation spread strategy.
    #[serde(default)]
    pub alloc_server_spread: Option<AllocServerSpread>,
    /// Enable flexible disk mode (bhyve only).
    #[serde(default)]
    pub flexible_disk: Option<bool>,
    /// Disk specifications (only valid when flexible_disk is true).
    #[serde(default)]
    pub disks: Option<Vec<DiskSpec>>,

    // Control parameters
    /// Allow modifying immutable fields.
    #[serde(default)]
    pub force: Option<bool>,
    /// Skip validation step.
    #[serde(default)]
    pub skip_validation: Option<bool>,
}

/// Query parameters for listing packages.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListPackagesQuery {
    /// Limit result count.
    #[serde(default)]
    pub limit: Option<u64>,
    /// Skip N results.
    #[serde(default)]
    pub offset: Option<u64>,
    /// Field name to sort by.
    #[serde(default)]
    pub sort: Option<String>,
    /// Sort order (ASC or DESC).
    #[serde(default)]
    pub order: Option<SortOrder>,
    /// Raw LDAP search filter (bypasses param-based filter).
    #[serde(default)]
    pub filter: Option<String>,
    /// Filter by package name.
    #[serde(default)]
    pub name: Option<String>,
    /// Filter by version.
    #[serde(default)]
    pub version: Option<String>,
    /// Filter by active status.
    #[serde(default)]
    pub active: Option<bool>,
    /// Filter by brand.
    #[serde(default)]
    pub brand: Option<Brand>,
    /// Filter by owner UUIDs. Also returns packages with no owner_uuids.
    #[serde(default)]
    pub owner_uuids: Option<String>,
    /// Filter by group.
    #[serde(default)]
    pub group: Option<String>,
    /// Filter by OS.
    #[serde(default)]
    pub os: Option<String>,
    /// Filter by flexible_disk.
    #[serde(default)]
    pub flexible_disk: Option<bool>,
}

/// Query parameters for getting a single package.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetPackageQuery {
    /// Owner UUIDs for access filtering (JSON-encoded array or single UUID).
    #[serde(default)]
    pub owner_uuids: Option<String>,
}

/// Query parameters for deleting a package.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct DeletePackageQuery {
    /// Must be true to allow deletion. Returns 405 if not set.
    #[serde(default)]
    pub force: Option<bool>,
}

/// Path parameter for package-specific endpoints.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct PackagePath {
    /// Package UUID.
    pub uuid: Uuid,
}
