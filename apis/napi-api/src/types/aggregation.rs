// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Aggregation types

use super::common::Uuid;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// LACP (Link Aggregation Control Protocol) mode
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LacpMode {
    Off,
    Active,
    Passive,
    /// Catch-all for modes added after this client was compiled
    #[serde(other)]
    Unknown,
}

/// A link aggregation (bond)
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct Aggregation {
    /// UUID of the server this aggregation belongs to
    pub belongs_to_uuid: Uuid,
    /// Aggregation ID (format: `{belongs_to_uuid}-{name}`)
    pub id: String,
    /// LACP mode
    pub lacp_mode: LacpMode,
    /// Aggregation name
    pub name: String,
    /// MAC addresses of member NICs (colon-separated strings)
    pub macs: Vec<String>,
    /// NIC tags this aggregation provides
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nic_tags_provided: Option<Vec<String>>,
}

/// Request body for creating an aggregation (POST /aggregations)
#[derive(Debug, Deserialize, JsonSchema)]
pub struct CreateAggregationBody {
    /// Aggregation name (required)
    pub name: String,
    /// MAC addresses of member NICs (required)
    pub macs: Vec<String>,
    /// LACP mode (defaults to "off")
    #[serde(default)]
    pub lacp_mode: Option<LacpMode>,
    /// NIC tags this aggregation provides
    #[serde(default)]
    pub nic_tags_provided: Option<Vec<String>>,
}

/// Request body for updating an aggregation (PUT /aggregations/:id)
#[derive(Debug, Deserialize, JsonSchema)]
pub struct UpdateAggregationBody {
    #[serde(default)]
    pub lacp_mode: Option<LacpMode>,
    #[serde(default)]
    pub macs: Option<Vec<String>>,
    #[serde(default)]
    pub nic_tags_provided: Option<Vec<String>>,
}

/// Path parameter for aggregation endpoints
#[derive(Debug, Deserialize, JsonSchema)]
pub struct AggregationPath {
    /// Aggregation ID (format: `{belongs_to_uuid}-{name}`)
    pub id: String,
}

/// Query parameters for listing aggregations (GET /aggregations)
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListAggregationsQuery {
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub offset: Option<u32>,
    #[serde(default)]
    pub belongs_to_uuid: Option<String>,
    #[serde(default)]
    pub macs: Option<String>,
    #[serde(default)]
    pub nic_tags_provided: Option<String>,
}
