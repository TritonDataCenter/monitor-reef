// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Volume-related types

use super::common::{Tags, Timestamp, Uuid};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Path parameter for volume operations
#[derive(Debug, Deserialize, JsonSchema)]
pub struct VolumePath {
    /// Account login name
    pub account: String,
    /// Volume UUID
    pub id: Uuid,
}

/// Volume type
#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, clap::ValueEnum,
)]
#[serde(rename_all = "lowercase")]
pub enum VolumeType {
    Tritonnfs,
    /// Unknown type (forward compatibility)
    #[serde(other)]
    #[clap(skip)]
    Unknown,
}

/// Volume state
#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, clap::ValueEnum,
)]
#[serde(rename_all = "lowercase")]
pub enum VolumeState {
    Creating,
    Ready,
    Failed,
    Deleting,
    #[serde(other)]
    Unknown,
}

/// Volume information
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Volume {
    /// Volume UUID
    pub id: Uuid,
    /// Volume name
    pub name: String,
    /// Owner UUID
    /// Note: CloudAPI returns this as snake_case despite other fields being camelCase
    #[serde(rename = "owner_uuid")]
    pub owner_uuid: Uuid,
    /// Volume type
    #[serde(rename = "type")]
    pub volume_type: VolumeType,
    /// Size in MiB
    pub size: u64,
    /// State
    pub state: VolumeState,
    /// Networks (array of UUIDs)
    #[serde(default)]
    pub networks: Vec<Uuid>,
    /// Filesystem path
    /// Note: CloudAPI returns this as snake_case despite other fields being camelCase
    #[serde(
        rename = "filesystem_path",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub filesystem_path: Option<String>,
    /// Creation timestamp
    pub created: Timestamp,
    /// Tags
    #[serde(default)]
    pub tags: Tags,
    /// References (machines using this volume)
    #[serde(default)]
    pub refs: Vec<Uuid>,
}

/// Volume size option
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VolumeSize {
    /// Size in MiB
    pub size: u64,
}

/// Request to create volume
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateVolumeRequest {
    /// Volume name
    #[serde(default)]
    pub name: Option<String>,
    /// Volume type
    #[serde(default, rename = "type")]
    pub volume_type: Option<VolumeType>,
    /// Size in MiB
    pub size: u64,
    /// Networks (array of UUIDs)
    #[serde(default)]
    pub networks: Option<Vec<Uuid>>,
    /// Tags
    #[serde(default)]
    pub tags: Option<Tags>,
}

/// Volume action for action dispatch
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VolumeAction {
    Update,
}

/// Query parameter for volume actions
#[derive(Debug, Deserialize, JsonSchema)]
pub struct VolumeActionQuery {
    /// Action to perform. Optional in the query string because clients may
    /// send it in the request body instead (matching Restify's mapParams
    /// behavior). Service implementations should check the body first,
    /// then fall back to this query parameter.
    #[serde(default)]
    pub action: Option<VolumeAction>,
}

/// Request to update volume
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct UpdateVolumeRequest {
    /// Volume name
    #[serde(default)]
    pub name: Option<String>,
}
