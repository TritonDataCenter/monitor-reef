// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Image/dataset related types

use super::common::{RoleTags, Tags, Timestamp, Uuid};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Path parameter for image operations
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ImagePath {
    /// Account login name
    pub account: String,
    /// Image UUID
    pub dataset: Uuid,
}

/// Image state
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ImageState {
    Active,
    Unactivated,
    Disabled,
    Creating,
    Failed,
}

/// Image type
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ImageType {
    #[serde(rename = "zone-dataset")]
    ZoneDataset,
    #[serde(rename = "lx-dataset")]
    LxDataset,
    #[serde(rename = "zvol")]
    Zvol,
    #[serde(rename = "other")]
    Other,
}

/// Image requirements
///
/// Specifies hardware/software requirements for an image.
#[derive(Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
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
    pub brand: Option<String>,
    /// Required bootrom (for bhyve)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bootrom: Option<String>,
}

/// Image file information
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ImageFile {
    /// Compression type (gzip, bzip2, none)
    pub compression: String,
    /// SHA1 checksum
    pub sha1: String,
    /// File size in bytes
    pub size: u64,
}

/// Image error information (for failed image creation)
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ImageError {
    /// Error code
    pub code: String,
    /// Error message
    pub message: String,
}

/// Image/dataset information
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
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
    pub error: Option<ImageError>,
    /// Role tags for RBAC
    #[serde(rename = "role-tag", default, skip_serializing_if = "Option::is_none")]
    pub role_tag: Option<RoleTags>,
}

/// Request to create image from machine
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateImageRequest {
    /// Machine UUID to create image from
    pub machine: Uuid,
    /// Image name
    pub name: String,
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

/// Image action for action dispatch
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ImageAction {
    Update,
    Export,
    Clone,
    ImportFromDatacenter,
    /// Share image with another account
    Share,
    /// Unshare image from another account
    Unshare,
}

/// Query parameter for image actions
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ImageActionQuery {
    pub action: ImageAction,
}

/// Request to update an image
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
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
#[serde(rename_all = "camelCase")]
pub struct ExportImageRequest {
    /// Manta path for export destination
    pub manta_path: String,
}

/// Request to clone an image
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CloneImageRequest {}

/// Request to import image from datacenter
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ImportImageRequest {
    /// Source datacenter name
    pub datacenter: String,
    /// Image UUID in source datacenter
    pub id: Uuid,
}

/// Request to share an image with another account
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ShareImageRequest {
    /// Account UUID to share the image with
    pub account: Uuid,
}

/// Request to unshare an image from an account
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct UnshareImageRequest {
    /// Account UUID to unshare the image from
    pub account: Uuid,
}

/// Query parameters for listing images
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
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
