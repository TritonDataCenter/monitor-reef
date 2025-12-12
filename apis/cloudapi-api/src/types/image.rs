// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Image/dataset related types

use super::common::{Tags, Timestamp, Uuid};
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

/// Image/dataset information
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Image {
    /// Image UUID
    pub id: Uuid,
    /// Image name
    pub name: String,
    /// Image version
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Operating system
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os: Option<String>,
    /// Image type
    #[serde(rename = "type")]
    pub image_type: ImageType,
    /// Description
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Requirements
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requirements: Option<serde_json::Value>,
    /// Homepage URL
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    /// Published timestamp
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_at: Option<Timestamp>,
    /// Owner UUID
    pub owner: Uuid,
    /// Public image
    #[serde(default)]
    pub public: Option<bool>,
    /// Image state
    pub state: ImageState,
    /// Tags
    #[serde(default)]
    pub tags: Tags,
    /// EULA URL
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eula: Option<String>,
    /// ACL (list of account UUIDs with access)
    #[serde(default)]
    pub acl: Option<Vec<Uuid>>,
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
