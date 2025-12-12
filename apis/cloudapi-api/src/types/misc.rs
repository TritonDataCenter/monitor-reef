// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Miscellaneous types (packages, datacenters, services, migrations)

use super::common::{Timestamp, Uuid};
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
    /// VCPUs
    #[serde(default)]
    pub vcpus: Option<u32>,
    /// Lightweight workload
    #[serde(default)]
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
    /// Default
    #[serde(default)]
    pub default: Option<bool>,
}

/// Datacenter information
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

/// Service information
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Service {
    /// Service name
    pub name: String,
    /// Service URL
    pub endpoint: String,
}

/// Migration information
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Migration {
    /// Machine UUID being migrated
    pub vm_uuid: Uuid,
    /// Migration phase
    pub phase: String,
    /// Migration state
    pub state: String,
    /// Progress percentage
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress_percent: Option<f64>,
    /// Creation timestamp
    pub created_timestamp: Timestamp,
    /// Last update timestamp
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_timestamp: Option<Timestamp>,
    /// Automatic migration
    #[serde(default)]
    pub automatic: Option<bool>,
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
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MigrateRequest {
    /// Action (must be "migrate")
    pub action: String,
    /// Affinity rules
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
