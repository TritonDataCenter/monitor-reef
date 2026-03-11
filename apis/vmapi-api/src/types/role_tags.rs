// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Role tag types for VMAPI
//!
//! Note: VMAPI uses snake_case for JSON field names (internal Triton API convention).

use super::common::Uuid;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// ============================================================================
// Path Parameters
// ============================================================================

/// Path parameter for VM role tags operations
#[derive(Debug, Deserialize, JsonSchema)]
pub struct RoleTagsPath {
    /// VM UUID
    pub uuid: Uuid,
}

/// Path parameter for single role tag operations
#[derive(Debug, Deserialize, JsonSchema)]
pub struct RoleTagPath {
    /// VM UUID
    pub uuid: Uuid,
    /// Role tag value
    pub role_tag: String,
}

// ============================================================================
// Request/Response Types
// ============================================================================

/// Request body for adding role tags (POST - merge)
#[derive(Debug, Deserialize, JsonSchema)]
pub struct AddRoleTagsRequest {
    /// Role tags to add
    pub role_tags: Vec<String>,
}

/// Request body for setting role tags (PUT - replace all)
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetRoleTagsRequest {
    /// Role tags to set (replaces all existing)
    pub role_tags: Vec<String>,
}

/// Response containing role tags
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct RoleTagsResponse {
    /// List of role tags
    pub role_tags: Vec<String>,
}
