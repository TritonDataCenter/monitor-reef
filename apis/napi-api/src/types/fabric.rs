// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Fabric VLAN types

use super::common::Uuid;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A fabric VLAN
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct FabricVlan {
    /// VLAN name
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Owner UUID
    pub owner_uuid: Uuid,
    /// VLAN ID (0-4094)
    pub vlan_id: u32,
    /// VXLAN Network Identifier
    pub vnet_id: u32,
    /// Human-readable description
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Request body for creating a fabric VLAN (POST /fabrics/:owner_uuid/vlans)
#[derive(Debug, Deserialize, JsonSchema)]
pub struct CreateFabricVlanBody {
    /// VLAN ID (required, 0-4094)
    pub vlan_id: u32,
    /// VLAN name
    #[serde(default)]
    pub name: Option<String>,
    /// Human-readable description
    #[serde(default)]
    pub description: Option<String>,
    /// Fields to include in response
    #[serde(default)]
    pub fields: Option<Vec<String>>,
}

/// Request body for updating a fabric VLAN (PUT /fabrics/:owner_uuid/vlans/:vlan_id)
#[derive(Debug, Deserialize, JsonSchema)]
pub struct UpdateFabricVlanBody {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

/// Path parameter for fabric VLAN collection endpoints
#[derive(Debug, Deserialize, JsonSchema)]
pub struct FabricOwnerPath {
    pub owner_uuid: Uuid,
}

/// Path parameter for specific fabric VLAN endpoints
#[derive(Debug, Deserialize, JsonSchema)]
pub struct FabricVlanPath {
    pub owner_uuid: Uuid,
    pub vlan_id: u32,
}

/// Path parameter for fabric network endpoints
#[derive(Debug, Deserialize, JsonSchema)]
pub struct FabricNetworkPath {
    pub owner_uuid: Uuid,
    pub vlan_id: u32,
    pub uuid: Uuid,
}

/// Query parameters for listing fabric VLANs
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListFabricVlansQuery {
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub offset: Option<u32>,
    /// Fields to include in response
    #[serde(default)]
    pub fields: Option<String>,
}

/// Query parameters for listing fabric networks
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListFabricNetworksQuery {
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub offset: Option<u32>,
}

/// Request body for creating a fabric network
/// (POST /fabrics/:owner_uuid/vlans/:vlan_id/networks)
///
/// MTU, nic_tag, and vnet_id are auto-set from the VLAN.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct CreateFabricNetworkBody {
    /// Network name (required)
    pub name: String,
    /// Subnet CIDR (required)
    pub subnet: String,
    /// Start of provisionable IP range (required)
    pub provision_start_ip: String,
    /// End of provisionable IP range (required)
    pub provision_end_ip: String,
    /// Human-readable description
    #[serde(default)]
    pub description: Option<String>,
    /// Gateway IP address
    #[serde(default)]
    pub gateway: Option<String>,
    /// Whether to enable internet NAT (default: true for IPv4)
    #[serde(default)]
    pub internet_nat: Option<bool>,
    /// DNS resolvers
    #[serde(default)]
    pub resolvers: Option<Vec<String>>,
    /// Static routes (destination -> gateway)
    #[serde(default)]
    pub routes: Option<std::collections::HashMap<String, String>>,
    /// UUID (auto-generated if omitted)
    #[serde(default)]
    pub uuid: Option<Uuid>,
}

/// Request body for updating a fabric network
/// (PUT /fabrics/:owner_uuid/vlans/:vlan_id/networks/:uuid)
#[derive(Debug, Deserialize, JsonSchema)]
pub struct UpdateFabricNetworkBody {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub gateway: Option<String>,
    #[serde(default)]
    pub provision_start_ip: Option<String>,
    #[serde(default)]
    pub provision_end_ip: Option<String>,
    #[serde(default)]
    pub resolvers: Option<Vec<String>>,
    #[serde(default)]
    pub routes: Option<std::collections::HashMap<String, String>>,
}
