// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Manifest types for SAPI

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::common::Uuid;

/// A SAPI configuration manifest
///
/// Manifests define templates that are rendered with metadata to produce
/// configuration files for instances.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct Manifest {
    /// Manifest UUID
    pub uuid: Uuid,

    /// Manifest name
    pub name: String,

    /// Path where the rendered config file is placed in the zone
    pub path: String,

    /// Template content (can be a string template or a JSON object)
    pub template: Value,

    /// Command to run after rendering the template
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_cmd: Option<String>,

    /// Command to run after rendering on Linux (lx-branded zones)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_cmd_linux: Option<String>,

    /// Manifest version (semver, defaults to "1.0.0")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,

    /// Whether this is a master record
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub master: Option<bool>,
}

/// Request body for creating a manifest
#[derive(Debug, Deserialize, JsonSchema)]
pub struct CreateManifestBody {
    /// Manifest name (required)
    pub name: String,

    /// Path where the rendered config file is placed (required)
    pub path: String,

    /// Template content (required; can be a string or JSON object)
    pub template: Value,

    /// Command to run after rendering
    #[serde(default)]
    pub post_cmd: Option<String>,

    /// Command to run after rendering on Linux
    #[serde(default)]
    pub post_cmd_linux: Option<String>,

    /// Manifest version (defaults to "1.0.0")
    #[serde(default)]
    pub version: Option<String>,
}

/// Query parameters for listing manifests
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListManifestsQuery {
    /// Include master records from remote datacenter
    #[serde(default)]
    pub include_master: Option<bool>,
}
