// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Network pool types

use super::common::Uuid;
use super::network::NetworkFamily;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A network pool (grouping of networks for provisioning)
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct NetworkPool {
    pub family: NetworkFamily,
    pub uuid: Uuid,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Network UUIDs in this pool
    pub networks: Vec<Uuid>,
    /// NIC tags present across networks in this pool
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nic_tags_present: Option<Vec<String>>,
    /// Backwards compatibility: first NIC tag from nic_tags_present
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nic_tag: Option<String>,
    /// UUIDs of owners who can provision from this pool
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_uuids: Option<Vec<Uuid>>,
}

/// Request body for creating a network pool (POST /network_pools)
#[derive(Debug, Deserialize, JsonSchema)]
pub struct CreateNetworkPoolBody {
    /// Pool name (required)
    pub name: String,
    /// Network UUIDs to include (required)
    pub networks: Vec<Uuid>,
    /// Human-readable description
    #[serde(default)]
    pub description: Option<String>,
    /// UUIDs of owners who can provision from this pool
    #[serde(default)]
    pub owner_uuids: Option<Vec<Uuid>>,
    /// UUID (auto-generated if omitted)
    #[serde(default)]
    pub uuid: Option<Uuid>,
}

/// Request body for updating a network pool (PUT /network_pools/:uuid)
#[derive(Debug, Deserialize, JsonSchema)]
pub struct UpdateNetworkPoolBody {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub networks: Option<Vec<Uuid>>,
    #[serde(default)]
    pub owner_uuids: Option<Vec<Uuid>>,
}

/// Path parameter for network pool endpoints
#[derive(Debug, Deserialize, JsonSchema)]
pub struct NetworkPoolPath {
    pub uuid: Uuid,
}

/// Query parameters for listing network pools (GET /network_pools)
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListNetworkPoolsQuery {
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub offset: Option<u32>,
    /// UUID prefix filter
    #[serde(default)]
    pub uuid: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub family: Option<NetworkFamily>,
    /// Filter by network UUID membership
    #[serde(default)]
    pub networks: Option<String>,
    /// UUID of owner to check provisionability
    #[serde(default)]
    pub provisionable_by: Option<String>,
}
