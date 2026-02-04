// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Metadata-related types for VMAPI
//!
//! Note: VMAPI uses snake_case for JSON field names (internal Triton API convention).
//!
//! Due to Dropshot routing constraints (cannot have both literal and variable path
//! segments at the same level), metadata endpoints use explicit literal paths for
//! each metadata type (customer_metadata, internal_metadata, tags) rather than
//! a variable `{metadata_type}` path segment.

use super::common::{MetadataObject, Uuid};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// ============================================================================
// Path Parameters
// ============================================================================

/// Metadata type enum (for use in implementations)
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MetadataType {
    CustomerMetadata,
    InternalMetadata,
    Tags,
}

impl std::fmt::Display for MetadataType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MetadataType::CustomerMetadata => write!(f, "customer_metadata"),
            MetadataType::InternalMetadata => write!(f, "internal_metadata"),
            MetadataType::Tags => write!(f, "tags"),
        }
    }
}

/// Path parameter for single metadata/tag key operations
#[derive(Debug, Deserialize, JsonSchema)]
pub struct VmMetadataKeyPath {
    /// VM UUID
    pub uuid: Uuid,
    /// Metadata/tag key
    pub key: String,
}

// ============================================================================
// Request/Response Types
// ============================================================================

/// Request body for adding metadata (POST - merge)
#[derive(Debug, Deserialize, JsonSchema)]
pub struct AddMetadataRequest {
    /// Key-value pairs to add/merge
    #[serde(flatten)]
    pub metadata: MetadataObject,
}

/// Request body for setting metadata (PUT - replace all)
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetMetadataRequest {
    /// Key-value pairs to set (replaces all existing)
    #[serde(flatten)]
    pub metadata: MetadataObject,
}

/// Response containing metadata
pub type MetadataResponse = MetadataObject;

/// Response containing a single metadata value
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct MetadataValueResponse {
    /// The metadata value
    pub value: serde_json::Value,
}
