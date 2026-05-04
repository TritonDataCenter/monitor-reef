// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Image manifest types for IMGAPI
//!
//! The Image struct represents the full image manifest as returned by IMGAPI.
//! All fields use snake_case in the wire format (no `rename_all` needed).

use super::common::Uuid;
use super::file::ImageFile;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// ============================================================================
// Enums
// ============================================================================

/// Image lifecycle state
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ImageState {
    Active,
    Unactivated,
    Disabled,
    Creating,
    Failed,
    /// Catch-all for states added after this client was compiled
    #[serde(other)]
    Unknown,
}

/// Image type (virtualization/container technology)
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ImageType {
    #[serde(rename = "zone-dataset")]
    ZoneDataset,
    #[serde(rename = "lx-dataset")]
    LxDataset,
    Zvol,
    Docker,
    Lxd,
    Other,
    /// Catch-all for types added after this client was compiled
    #[serde(other)]
    Unknown,
}

/// Image operating system
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ImageOs {
    Smartos,
    Linux,
    Windows,
    Bsd,
    Illumos,
    Other,
    /// Catch-all for OS types added after this client was compiled
    #[serde(other)]
    Unknown,
}

// ============================================================================
// Image Manifest (Response)
// ============================================================================

/// User entry in image manifest
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ImageUser {
    /// Username
    pub name: String,
}

/// Error information for failed images
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ImageErrorInfo {
    /// Error message
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Error code
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// URL with more information
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// Image requirements
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ImageRequirements {
    /// Required networks
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub networks: Option<Vec<NetworkRequirement>>,
    /// Required VM brand
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brand: Option<String>,
    /// Whether SSH key is required
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh_key: Option<bool>,
    /// Minimum RAM in MB
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_ram: Option<u64>,
    /// Maximum RAM in MB
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_ram: Option<u64>,
    /// Minimum platform version per SDC version
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_platform: Option<std::collections::HashMap<String, String>>,
    /// Maximum platform version per SDC version
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_platform: Option<std::collections::HashMap<String, String>>,
    /// Required bootrom firmware
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bootrom: Option<String>,
}

/// Network requirement
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct NetworkRequirement {
    /// Network name
    pub name: String,
    /// Network description
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Full image manifest as returned by IMGAPI
///
/// All fields use snake_case wire format (standard for Triton internal APIs).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Image {
    /// Manifest version
    pub v: u32,
    /// Image UUID
    pub uuid: Uuid,
    /// Owner account UUID
    pub owner: Uuid,
    /// Image name
    pub name: String,
    /// Image version string
    pub version: String,
    /// Image lifecycle state
    pub state: ImageState,
    /// Whether the image is disabled
    pub disabled: bool,
    /// Whether the image is public
    pub public: bool,
    /// ISO 8601 timestamp when the image was published (present if activated)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_at: Option<String>,
    /// Image type (absent if originally "null")
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub image_type: Option<ImageType>,
    /// Operating system (absent if originally "null")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os: Option<ImageOs>,
    /// Image files (storage artifacts)
    #[serde(default)]
    pub files: Vec<ImageFile>,

    // Optional fields
    /// Access control list (account UUIDs granted access)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acl: Option<Vec<Uuid>>,
    /// Image description
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Homepage URL
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    /// End-user license agreement URL
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eula: Option<String>,
    /// Whether this image has an icon
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<bool>,
    /// Legacy URN identifier
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub urn: Option<String>,
    /// Image requirements (min RAM, networks, platform, etc.)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requirements: Option<ImageRequirements>,
    /// Default user accounts to create
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub users: Option<Vec<ImageUser>>,
    /// Whether to generate random passwords for users
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generate_passwords: Option<bool>,
    /// Directories to inherit from the delegated dataset
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inherited_directories: Option<Vec<String>>,
    /// Origin image UUID (for incremental images)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<Uuid>,
    /// NIC driver (zvol images only)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nic_driver: Option<String>,
    /// Disk driver (zvol images only)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk_driver: Option<String>,
    /// CPU type (zvol images only)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_type: Option<String>,
    /// Image size in MB (zvol images only)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_size: Option<u64>,
    /// Key-value tags
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<serde_json::Value>,
    /// Billing tags (non-public mode only)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub billing_tags: Option<Vec<String>>,
    /// Traits for placement (non-public mode only)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub traits: Option<serde_json::Value>,
    /// Error details (only when state=failed)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ImageErrorInfo>,
    /// Channels this image belongs to
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channels: Option<Vec<String>>,
}

// ============================================================================
// Create Image Request
// ============================================================================

/// Request body for POST /images (create image from manifest)
///
/// Contains all settable fields for creating a new image.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct CreateImageRequest {
    /// Manifest version (optional, defaults to current)
    #[serde(default)]
    pub v: Option<u32>,
    /// Explicit UUID for the image (optional, auto-generated if omitted)
    #[serde(default)]
    pub uuid: Option<Uuid>,
    /// Owner account UUID
    pub owner: Uuid,
    /// Image name
    pub name: String,
    /// Image version string
    pub version: String,
    /// Image type
    #[serde(default, rename = "type")]
    pub image_type: Option<ImageType>,
    /// Operating system
    #[serde(default)]
    pub os: Option<ImageOs>,
    /// Whether the image is public (default: false)
    #[serde(default)]
    pub public: Option<bool>,
    /// Whether the image is disabled (default: false)
    #[serde(default)]
    pub disabled: Option<bool>,
    /// Access control list (account UUIDs)
    #[serde(default)]
    pub acl: Option<Vec<Uuid>>,
    /// Image description
    #[serde(default)]
    pub description: Option<String>,
    /// Homepage URL
    #[serde(default)]
    pub homepage: Option<String>,
    /// EULA URL
    #[serde(default)]
    pub eula: Option<String>,
    /// Whether this image has an icon
    #[serde(default)]
    pub icon: Option<bool>,
    /// Error details
    #[serde(default)]
    pub error: Option<ImageErrorInfo>,
    /// Image requirements
    #[serde(default)]
    pub requirements: Option<ImageRequirements>,
    /// Default user accounts
    #[serde(default)]
    pub users: Option<Vec<ImageUser>>,
    /// Traits for placement
    #[serde(default)]
    pub traits: Option<serde_json::Value>,
    /// Key-value tags
    #[serde(default)]
    pub tags: Option<serde_json::Value>,
    /// Billing tags
    #[serde(default)]
    pub billing_tags: Option<Vec<String>>,
    /// Whether to generate random passwords
    #[serde(default)]
    pub generate_passwords: Option<bool>,
    /// Directories to inherit from delegated dataset
    #[serde(default)]
    pub inherited_directories: Option<Vec<String>>,
    /// Origin image UUID (for incremental images)
    #[serde(default)]
    pub origin: Option<Uuid>,
    /// Channels
    #[serde(default)]
    pub channels: Option<Vec<String>>,
    /// NIC driver (zvol only)
    #[serde(default)]
    pub nic_driver: Option<String>,
    /// Disk driver (zvol only)
    #[serde(default)]
    pub disk_driver: Option<String>,
    /// CPU type (zvol only)
    #[serde(default)]
    pub cpu_type: Option<String>,
    /// Image size in MB (zvol only)
    #[serde(default)]
    pub image_size: Option<u64>,
}

/// Query parameters for listing images
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListImagesQuery {
    /// Filter by owner UUID
    #[serde(default)]
    pub owner: Option<Uuid>,
    /// Filter by name
    #[serde(default)]
    pub name: Option<String>,
    /// Filter by version
    #[serde(default)]
    pub version: Option<String>,
    /// Filter by state
    #[serde(default)]
    pub state: Option<ImageState>,
    /// Filter by type
    #[serde(default, rename = "type")]
    pub image_type: Option<ImageType>,
    /// Filter by OS
    #[serde(default)]
    pub os: Option<ImageOs>,
    /// Filter public images only
    #[serde(default)]
    pub public: Option<bool>,
    /// Account UUID for scoping
    #[serde(default)]
    pub account: Option<Uuid>,
    /// Channel name
    #[serde(default)]
    pub channel: Option<String>,
    /// Pagination limit
    #[serde(default)]
    pub limit: Option<u64>,
    /// Pagination marker (UUID of last image in previous page)
    #[serde(default)]
    pub marker: Option<Uuid>,
    /// Filter by tag (key=value format)
    #[serde(default)]
    pub tag: Option<String>,
    /// Filter by billing tag
    #[serde(default)]
    pub billing_tag: Option<String>,
}
