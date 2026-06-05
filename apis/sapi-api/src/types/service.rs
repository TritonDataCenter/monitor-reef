// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Service types for SAPI

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

use super::common::{ServiceType, UpdateAction, Uuid};

/// A SAPI service
///
/// Services belong to an application and contain instances.
/// Application -> Service -> Instance
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct Service {
    /// Service UUID
    pub uuid: Uuid,

    /// Service name (e.g., "manatee")
    pub name: String,

    /// Parent application UUID
    pub application_uuid: Uuid,

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

    /// Service type: "vm" or "agent" (v2+ only)
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub service_type: Option<ServiceType>,
}

/// Request body for creating a service
#[derive(Debug, Deserialize, JsonSchema)]
pub struct CreateServiceBody {
    /// Service name (required)
    pub name: String,

    /// Parent application UUID (required)
    pub application_uuid: Uuid,

    /// Service UUID (optional, auto-generated if not provided)
    #[serde(default)]
    pub uuid: Option<Uuid>,

    /// Zone parameters
    #[serde(default)]
    pub params: Option<HashMap<String, Value>>,

    /// Key-value metadata
    #[serde(default)]
    pub metadata: Option<HashMap<String, Value>>,

    /// Manifest UUID mappings
    #[serde(default)]
    pub manifests: Option<HashMap<String, String>>,

    /// Service type: "vm" or "agent"
    #[serde(default, rename = "type")]
    pub service_type: Option<ServiceType>,

    /// Whether this is a master record (from remote datacenter)
    #[serde(default)]
    pub master: Option<bool>,
}

/// Request body for updating a service
///
/// The `action` field controls how attributes are modified:
/// - `update` (default): merge changes into existing attributes
/// - `replace`: replace entire attribute sections wholesale
/// - `delete`: delete specified keys from attributes
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct UpdateServiceBody {
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

/// Query parameters for listing services
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListServicesQuery {
    /// Filter by service name
    #[serde(default)]
    pub name: Option<String>,

    /// Filter by parent application UUID
    #[serde(default)]
    pub application_uuid: Option<Uuid>,

    /// Filter by service type ("vm" or "agent")
    #[serde(default, rename = "type")]
    pub service_type: Option<ServiceType>,

    /// Include master records from remote datacenter
    #[serde(default)]
    pub include_master: Option<bool>,
}
