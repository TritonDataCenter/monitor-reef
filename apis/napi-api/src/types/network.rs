// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Network types

use super::common::Uuid;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Network address family
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum NetworkFamily {
    Ipv4,
    Ipv6,
    /// Catch-all for families added after this client was compiled
    #[serde(other)]
    Unknown,
}

/// A network
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct Network {
    pub family: NetworkFamily,
    pub mtu: u32,
    pub nic_tag: String,
    pub name: String,
    pub provision_end_ip: String,
    pub provision_start_ip: String,
    pub subnet: String,
    pub uuid: Uuid,
    pub vlan_id: u32,
    pub resolvers: Vec<String>,
    /// Whether this is a fabric network
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fabric: Option<bool>,
    /// VXLAN Network Identifier (fabric networks only)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vnet_id: Option<u32>,
    /// Whether internet NAT is enabled (fabric networks only)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub internet_nat: Option<bool>,
    /// Whether the gateway has been provisioned
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway_provisioned: Option<bool>,
    /// Gateway IP address
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway: Option<String>,
    /// Static routes (destination -> gateway)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routes: Option<HashMap<String, String>>,
    /// Human-readable description
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// UUIDs of owners who can provision on this network
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_uuids: Option<Vec<Uuid>>,
    /// Owner UUID (only present for fabric network serialization)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_uuid: Option<Uuid>,
    /// Netmask (IPv4 only)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub netmask: Option<String>,
}

/// Request body for creating a network (POST /networks)
#[derive(Debug, Deserialize, JsonSchema)]
pub struct CreateNetworkBody {
    /// Network name (required)
    pub name: String,
    /// NIC tag to use (required)
    pub nic_tag: String,
    /// VLAN ID (required)
    pub vlan_id: u32,
    /// Subnet CIDR (required unless subnet_alloc is true)
    #[serde(default)]
    pub subnet: Option<String>,
    /// Start of provisionable IP range
    #[serde(default)]
    pub provision_start_ip: Option<String>,
    /// End of provisionable IP range
    #[serde(default)]
    pub provision_end_ip: Option<String>,
    /// Human-readable description
    #[serde(default)]
    pub description: Option<String>,
    /// Whether this is a fabric network
    #[serde(default)]
    pub fabric: Option<bool>,
    /// Whether to automatically allocate a subnet
    #[serde(default)]
    pub subnet_alloc: Option<bool>,
    /// Address family for subnet allocation
    #[serde(default)]
    pub family: Option<NetworkFamily>,
    /// Subnet prefix length for subnet allocation
    #[serde(default)]
    pub subnet_prefix: Option<u32>,
    /// Fields to include in response
    #[serde(default)]
    pub fields: Option<Vec<String>>,
    /// Gateway IP address
    #[serde(default)]
    pub gateway: Option<String>,
    /// Whether to enable internet NAT
    #[serde(default)]
    pub internet_nat: Option<bool>,
    /// MTU (defaults based on NIC tag)
    #[serde(default)]
    pub mtu: Option<u32>,
    /// UUIDs of owners who can provision on this network
    #[serde(default)]
    pub owner_uuids: Option<Vec<Uuid>>,
    /// Static routes (destination -> gateway)
    #[serde(default)]
    pub routes: Option<HashMap<String, String>>,
    /// DNS resolvers
    #[serde(default)]
    pub resolvers: Option<Vec<String>>,
    /// UUID (auto-generated if omitted)
    #[serde(default)]
    pub uuid: Option<Uuid>,
    /// VXLAN Network Identifier
    #[serde(default)]
    pub vnet_id: Option<u32>,
}

/// Request body for updating a network (PUT /networks/:uuid)
#[derive(Debug, Deserialize, JsonSchema)]
pub struct UpdateNetworkBody {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub gateway: Option<String>,
    #[serde(default)]
    pub owner_uuids: Option<Vec<Uuid>>,
    #[serde(default)]
    pub provision_start_ip: Option<String>,
    #[serde(default)]
    pub provision_end_ip: Option<String>,
    #[serde(default)]
    pub resolvers: Option<Vec<String>>,
    #[serde(default)]
    pub routes: Option<HashMap<String, String>>,
}

/// Path parameter for network endpoints
#[derive(Debug, Deserialize, JsonSchema)]
pub struct NetworkPath {
    /// Network UUID (or "admin" for the admin network)
    pub uuid: String,
}

/// Query parameters for listing networks (GET /networks)
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListNetworksQuery {
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub offset: Option<u32>,
    /// UUID prefix filter
    #[serde(default)]
    pub uuid: Option<String>,
    #[serde(default)]
    pub fabric: Option<bool>,
    #[serde(default)]
    pub family: Option<NetworkFamily>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub nic_tag: Option<String>,
    #[serde(default)]
    pub owner_uuid: Option<String>,
    /// UUID of owner to check provisionability
    #[serde(default)]
    pub provisionable_by: Option<String>,
    #[serde(default)]
    pub vlan_id: Option<u32>,
}

/// Path parameter for network sub-resource endpoints
///
/// Note: uses `uuid` (not `network_uuid`) because Dropshot requires
/// consistent variable names at the same path level as `/networks/{uuid}`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct NetworkSubPath {
    pub uuid: Uuid,
}
