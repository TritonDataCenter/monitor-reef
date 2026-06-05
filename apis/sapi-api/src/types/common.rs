// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Common types shared across SAPI resources
//!
//! SAPI uses snake_case for all JSON field names (internal Triton API convention).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// UUID type alias
pub type Uuid = uuid::Uuid;

/// Generic path parameter for UUID-based resource lookup
#[derive(Debug, Deserialize, JsonSchema)]
pub struct UuidPath {
    /// Resource UUID
    pub uuid: Uuid,
}

/// Action for update endpoints (applications, services, instances)
///
/// Controls how attributes (`params`, `metadata`, `manifests`, etc.) are modified.
/// Default action is `update` when not specified.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum UpdateAction {
    /// Merge changes into existing attributes (default)
    Update,
    /// Replace entire attribute sections wholesale
    Replace,
    /// Delete specified keys from attributes
    Delete,
}

/// Shared attribute fields used in update request bodies
///
/// Applications, services, and instances all share this same attribute
/// update pattern via the `attributes.js` module.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct UpdateAttributesBody {
    /// Action to perform on attributes (default: update)
    #[serde(default)]
    pub action: Option<UpdateAction>,

    /// Zone parameters to update/replace/delete
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<HashMap<String, Value>>,

    /// Key-value metadata to update/replace/delete
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<HashMap<String, Value>>,

    /// Manifest UUID mappings to update/replace/delete
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifests: Option<HashMap<String, String>>,
}

/// Service/instance type
///
/// Distinguishes between VM-based services and agent services.
/// Agents are only visible in API v2+.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ServiceType {
    /// VM-based service (default)
    Vm,
    /// Agent service (only visible in API v2+)
    Agent,
    /// Unknown type (forward compatibility)
    #[serde(other)]
    Unknown,
}
