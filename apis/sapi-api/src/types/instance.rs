// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Instance types for SAPI

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

use super::common::{ServiceType, UpdateAction, Uuid};

/// A SAPI instance
///
/// Instances are the leaf nodes of SAPI's hierarchy:
/// Application -> Service -> Instance
///
/// An instance typically maps to a VM or agent zone.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct Instance {
    /// Instance UUID
    pub uuid: Uuid,

    /// Parent service UUID
    pub service_uuid: Uuid,

    /// Zone parameters
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<HashMap<String, Value>>,

    /// Key-value metadata
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<HashMap<String, Value>>,

    /// Manifest UUID mappings (name -> manifest UUID)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifests: Option<HashMap<String, String>>,

    /// Whether this is a master record
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub master: Option<bool>,

    /// Instance type: "vm" or "agent" (v2+ only)
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub instance_type: Option<ServiceType>,

    /// Job UUID (only present on async create)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_uuid: Option<Uuid>,
}

/// Request body for creating an instance
#[derive(Debug, Deserialize, JsonSchema)]
pub struct CreateInstanceBody {
    /// Parent service UUID (required)
    pub service_uuid: Uuid,

    /// Zone parameters
    #[serde(default)]
    pub params: Option<HashMap<String, Value>>,

    /// Key-value metadata
    #[serde(default)]
    pub metadata: Option<HashMap<String, Value>>,

    /// Manifest UUID mappings
    #[serde(default)]
    pub manifests: Option<HashMap<String, String>>,

    /// Instance UUID (optional, auto-generated if not provided)
    #[serde(default)]
    pub uuid: Option<Uuid>,

    /// Whether this is a master record (from remote datacenter)
    #[serde(default)]
    pub master: Option<bool>,
}

/// Query parameters for creating an instance
#[derive(Debug, Deserialize, JsonSchema)]
pub struct CreateInstanceQuery {
    /// If true, return immediately with a job_uuid instead of waiting
    /// for provisioning to complete. Default: false.
    #[serde(default, rename = "async")]
    pub async_create: Option<bool>,
}

/// Request body for updating an instance
///
/// The `action` field controls how attributes are modified:
/// - `update` (default): merge changes into existing attributes
/// - `replace`: replace entire attribute sections wholesale
/// - `delete`: delete specified keys from attributes
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct UpdateInstanceBody {
    /// Action to perform (default: update)
    #[serde(default)]
    pub action: Option<UpdateAction>,

    /// Zone parameters
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<HashMap<String, Value>>,

    /// Key-value metadata
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<HashMap<String, Value>>,

    /// Manifest UUID mappings
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifests: Option<HashMap<String, String>>,
}

/// Request body for upgrading an instance image
#[derive(Debug, Deserialize, JsonSchema)]
pub struct UpgradeInstanceBody {
    /// Image UUID to upgrade to (required)
    pub image_uuid: Uuid,
}

/// Query parameters for listing instances
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListInstancesQuery {
    /// Filter by parent service UUID
    #[serde(default)]
    pub service_uuid: Option<Uuid>,

    /// Filter by instance type ("vm" or "agent")
    #[serde(default, rename = "type")]
    pub instance_type: Option<ServiceType>,

    /// Include master records from remote datacenter
    #[serde(default)]
    pub include_master: Option<bool>,
}
