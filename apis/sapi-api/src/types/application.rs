// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Application types for SAPI

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

use super::common::{UpdateAction, Uuid};

/// A SAPI application
///
/// Applications are the top-level container in SAPI's hierarchy:
/// Application -> Service -> Instance
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct Application {
    /// Application UUID
    pub uuid: Uuid,

    /// Application name (e.g., "sdc")
    pub name: String,

    /// Owner UUID
    pub owner_uuid: Uuid,

    /// Zone parameters
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<HashMap<String, Value>>,

    /// Key-value metadata
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<HashMap<String, Value>>,

    /// JSON schema for validating metadata
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata_schema: Option<HashMap<String, Value>>,

    /// Manifest UUID mappings (name -> manifest UUID)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifests: Option<HashMap<String, String>>,

    /// Whether this is a master record (from remote datacenter)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub master: Option<bool>,
}

/// Request body for creating an application
#[derive(Debug, Deserialize, JsonSchema)]
pub struct CreateApplicationBody {
    /// Application name (required)
    pub name: String,

    /// Owner UUID (required)
    pub owner_uuid: Uuid,

    /// Zone parameters
    #[serde(default)]
    pub params: Option<HashMap<String, Value>>,

    /// Key-value metadata
    #[serde(default)]
    pub metadata: Option<HashMap<String, Value>>,

    /// JSON schema for validating metadata
    #[serde(default)]
    pub metadata_schema: Option<HashMap<String, Value>>,

    /// Manifest UUID mappings
    #[serde(default)]
    pub manifests: Option<HashMap<String, String>>,
}

/// Request body for updating an application
///
/// The `action` field controls how attributes are modified:
/// - `update` (default): merge changes into existing attributes
/// - `replace`: replace entire attribute sections wholesale
/// - `delete`: delete specified keys from attributes
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct UpdateApplicationBody {
    /// Action to perform (default: update)
    #[serde(default)]
    pub action: Option<UpdateAction>,

    /// Zone parameters
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<HashMap<String, Value>>,

    /// Key-value metadata
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<HashMap<String, Value>>,

    /// JSON schema for validating metadata
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata_schema: Option<HashMap<String, Value>>,

    /// Manifest UUID mappings
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifests: Option<HashMap<String, String>>,

    /// New owner UUID
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_uuid: Option<Uuid>,
}

/// Query parameters for listing applications
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListApplicationsQuery {
    /// Filter by application name
    #[serde(default)]
    pub name: Option<String>,

    /// Filter by owner UUID
    #[serde(default)]
    pub owner_uuid: Option<Uuid>,

    /// Include master records from remote datacenter
    #[serde(default)]
    pub include_master: Option<bool>,
}
