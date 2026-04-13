// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Image/dataset related types

use super::common::{RoleTags, Tags, Timestamp, Uuid};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use vmapi_api::Brand as VmapiBrand;

/// Path parameter for image operations
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ImagePath {
    /// Account login name
    pub account: String,
    /// Image UUID
    pub dataset: Uuid,
}

/// Image state
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ImageState {
    /// Return images in all states (query filter only, not a real state)
    All,
    Active,
    Unactivated,
    Disabled,
    Creating,
    Failed,
    #[serde(other)]
    Unknown,
}

/// Image type
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ImageType {
    #[serde(rename = "zone-dataset")]
    ZoneDataset,
    #[serde(rename = "lx-dataset")]
    LxDataset,
    #[serde(rename = "zvol")]
    Zvol,
    #[serde(rename = "docker")]
    Docker,
    #[serde(rename = "lxd")]
    Lxd,
    #[serde(rename = "other")]
    Other,
    /// Unknown type (forward compatibility)
    #[serde(other)]
    Unknown,
}

/// Image requirements
///
/// Specifies hardware/software requirements for an image.
#[derive(Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct ImageRequirements {
    /// Minimum RAM in MB
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_ram: Option<u64>,
    /// Maximum RAM in MB
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_ram: Option<u64>,
    /// Minimum memory (alias for min_ram)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_memory: Option<u64>,
    /// Maximum memory (alias for max_ram)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_memory: Option<u64>,
    /// Required brand
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brand: Option<VmapiBrand>,
    /// Required bootrom (for bhyve)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bootrom: Option<String>,
}

/// Image file information
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ImageFile {
    /// Compression type (gzip, bzip2, none)
    pub compression: String,
    /// SHA1 checksum
    pub sha1: String,
    /// File size in bytes
    pub size: u64,
}

/// Image error information (for failed image creation)
// Note: Named `ImageErrorInfo` rather than `ImageError` to distinguish this DTO
// from Rust error types.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ImageErrorInfo {
    /// Error code
    pub code: String,
    /// Error message
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Image/dataset information
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct Image {
    /// Image UUID
    pub id: Uuid,
    /// Image name
    pub name: String,
    /// Image version
    pub version: String,
    /// Operating system
    pub os: String,
    /// Image type
    #[serde(rename = "type")]
    pub image_type: ImageType,
    /// Description
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Requirements (always present, may be empty)
    #[serde(default)]
    pub requirements: ImageRequirements,
    /// Homepage URL
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    /// Published timestamp
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_at: Option<Timestamp>,
    /// Owner UUID (API version >= 7.1.0)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<Uuid>,
    /// Public image (API version >= 7.1.0)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public: Option<bool>,
    /// Image state (API version >= 7.1.0)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<ImageState>,
    /// Tags
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<Tags>,
    /// EULA URL (API version >= 7.1.0)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eula: Option<String>,
    /// ACL - list of account UUIDs with access (API version >= 7.1.0)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acl: Option<Vec<Uuid>>,
    /// Origin image UUID (API version >= 7.1.0)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<Uuid>,
    /// Image size in bytes (zvol images only)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_size: Option<u64>,
    /// Files array (contains compression, sha1, size)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files: Option<Vec<ImageFile>>,
    /// Error information (if image creation failed, API version >= 7.1.0)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ImageErrorInfo>,
    /// Role tags for RBAC
    #[serde(rename = "role-tag", default, skip_serializing_if = "Option::is_none")]
    pub role_tag: Option<RoleTags>,
}

/// Request to create image from machine
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct CreateImageRequest {
    /// Machine UUID to create image from
    pub machine: Uuid,
    /// Image name
    pub name: String,
    /// Image version
    pub version: String,
    /// Description
    #[serde(default)]
    pub description: Option<String>,
    /// Homepage URL
    #[serde(default)]
    pub homepage: Option<String>,
    /// EULA URL
    #[serde(default)]
    pub eula: Option<String>,
    /// ACL
    #[serde(default)]
    pub acl: Option<Vec<Uuid>>,
    /// Tags
    #[serde(default)]
    pub tags: Option<Tags>,
}

/// Image action for action dispatch
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ImageAction {
    Update,
    Export,
    Clone,
    ImportFromDatacenter,
    /// Unknown action (forward compatibility)
    #[serde(other)]
    Unknown,
}

/// Query parameter for image actions
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ImageActionQuery {
    /// Action to perform. Optional in the query string because clients may
    /// send it in the request body instead. Body takes precedence over the
    /// query parameter.
    // Implementation note: matches Restify's mapParams behavior.
    // Service implementations should check the body first, then fall back
    // to this query parameter.
    #[serde(default)]
    pub action: Option<ImageAction>,
}

/// Request to update an image
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct UpdateImageRequest {
    /// Image name
    #[serde(default)]
    pub name: Option<String>,
    /// Image version
    #[serde(default)]
    pub version: Option<String>,
    /// Description
    #[serde(default)]
    pub description: Option<String>,
    /// Homepage URL
    #[serde(default)]
    pub homepage: Option<String>,
    /// EULA URL
    #[serde(default)]
    pub eula: Option<String>,
    /// ACL
    #[serde(default)]
    pub acl: Option<Vec<Uuid>>,
    /// Tags
    #[serde(default)]
    pub tags: Option<Tags>,
}

/// Request to export an image
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ExportImageRequest {
    /// Manta path for export destination
    pub manta_path: String,
}

/// Request to clone an image
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct CloneImageRequest {}

/// Request to import image from datacenter
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ImportImageRequest {
    /// Source datacenter name
    pub datacenter: String,
    /// Image UUID in source datacenter
    pub id: Uuid,
}

/// Query parameters for image collection actions (POST /{account}/images)
///
/// The real sdc-cloudapi routes both "create from machine" and
/// "import-from-datacenter" through the same `POST /{account}/images`
/// endpoint, distinguished by query params.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ImageCollectionActionQuery {
    /// Action to perform (e.g., import-from-datacenter). When absent,
    /// the endpoint behaves as create-image-from-machine.
    #[serde(default)]
    pub action: Option<ImageAction>,
    /// Source datacenter name (for import-from-datacenter)
    #[serde(default)]
    pub datacenter: Option<String>,
    /// Image UUID in the source datacenter (for import-from-datacenter)
    #[serde(default)]
    pub id: Option<Uuid>,
}

/// Query parameters for listing images
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListImagesQuery {
    /// Filter by image name
    #[serde(default)]
    pub name: Option<String>,
    /// Filter by OS
    #[serde(default)]
    pub os: Option<String>,
    /// Filter by version
    #[serde(default)]
    pub version: Option<String>,
    /// Filter by public/private
    #[serde(default)]
    pub public: Option<bool>,
    /// Filter by state
    #[serde(default)]
    pub state: Option<ImageState>,
    /// Filter by owner
    #[serde(default)]
    pub owner: Option<Uuid>,
    /// Filter by type
    #[serde(default, rename = "type")]
    pub image_type: Option<ImageType>,
}
