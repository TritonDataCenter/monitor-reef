// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Common types used across IMGAPI
//!
//! Note: IMGAPI uses snake_case for JSON field names (internal Triton API convention).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// UUID type
pub type Uuid = uuid::Uuid;

/// Path parameter for image operations
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ImagePath {
    /// Image UUID
    pub uuid: Uuid,
}

/// Query parameter for channel selection (used on many endpoints)
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ChannelQuery {
    /// Channel name to scope the request to
    #[serde(default)]
    pub channel: Option<String>,
}

/// Query parameter for account scoping
#[derive(Debug, Deserialize, JsonSchema)]
pub struct AccountQuery {
    /// Account UUID for ownership scoping
    #[serde(default)]
    pub account: Option<Uuid>,
    /// Channel name
    #[serde(default)]
    pub channel: Option<String>,
}

/// Response for operations that create workflow jobs
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct JobResponse {
    /// UUID of the image being operated on
    pub image_uuid: Uuid,
    /// UUID of the workflow job created
    pub job_uuid: Uuid,
}

/// Response for image export operations
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ExportImageResponse {
    /// Manta URL where the image was exported
    pub manta_url: String,
    /// Path to the exported image file in Manta
    pub image_path: String,
    /// Path to the exported manifest file in Manta
    pub manifest_path: String,
}
