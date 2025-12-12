// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Network-related types (networks, fabric VLANs, NICs, IPs)

use super::common::Uuid;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Path parameter for network operations
#[derive(Debug, Deserialize, JsonSchema)]
pub struct NetworkPath {
    /// Account login name
    pub account: String,
    /// Network UUID
    pub network: Uuid,
}

/// Path parameter for fabric VLAN operations
#[derive(Debug, Deserialize, JsonSchema)]
pub struct FabricVlanPath {
    /// Account login name
    pub account: String,
    /// VLAN ID
    pub vlan_id: u16,
}

/// Path parameter for fabric network operations
#[derive(Debug, Deserialize, JsonSchema)]
pub struct FabricNetworkPath {
    /// Account login name
    pub account: String,
    /// VLAN ID
    pub vlan_id: u16,
    /// Network UUID
    pub id: Uuid,
}

/// Path parameter for NIC operations
#[derive(Debug, Deserialize, JsonSchema)]
pub struct NicPath {
    /// Account login name
    pub account: String,
    /// Machine UUID
    pub machine: Uuid,
    /// NIC MAC address
    pub mac: String,
}

/// Path parameter for network IP operations
#[derive(Debug, Deserialize, JsonSchema)]
pub struct NetworkIpPath {
    /// Account login name
    pub account: String,
    /// Network UUID
    pub network: Uuid,
    /// IP address
    pub ip_address: String,
}

/// Network information
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Network {
    /// Network UUID
    pub id: Uuid,
    /// Network name
    pub name: String,
    /// Public network
    pub public: bool,
    /// Fabric network
    #[serde(default)]
    pub fabric: Option<bool>,
    /// Gateway
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway: Option<String>,
    /// Internet NAT
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub internet_nat: Option<bool>,
    /// Provision start IP
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provision_start_ip: Option<String>,
    /// Provision end IP
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provision_end_ip: Option<String>,
    /// Subnet
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subnet: Option<String>,
    /// Netmask
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub netmask: Option<String>,
    /// VLAN ID
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vlan_id: Option<u16>,
    /// Resolvers
    #[serde(default)]
    pub resolvers: Option<Vec<String>>,
    /// Routes
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routes: Option<serde_json::Value>,
}

/// Fabric VLAN information
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FabricVlan {
    /// VLAN ID
    pub vlan_id: u16,
    /// VLAN name
    pub name: String,
    /// Description
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Request to create fabric VLAN
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateFabricVlanRequest {
    /// VLAN ID
    pub vlan_id: u16,
    /// VLAN name
    pub name: String,
    /// Description
    #[serde(default)]
    pub description: Option<String>,
}

/// Request to update fabric VLAN
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct UpdateFabricVlanRequest {
    /// VLAN name
    #[serde(default)]
    pub name: Option<String>,
    /// Description
    #[serde(default)]
    pub description: Option<String>,
}

/// Request to create fabric network
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateFabricNetworkRequest {
    /// Network name
    pub name: String,
    /// Description
    #[serde(default)]
    pub description: Option<String>,
    /// Subnet (CIDR)
    pub subnet: String,
    /// Provision start IP
    pub provision_start_ip: String,
    /// Provision end IP
    pub provision_end_ip: String,
    /// Gateway
    #[serde(default)]
    pub gateway: Option<String>,
    /// Resolvers
    #[serde(default)]
    pub resolvers: Option<Vec<String>>,
    /// Routes
    #[serde(default)]
    pub routes: Option<serde_json::Value>,
    /// Internet NAT
    #[serde(default)]
    pub internet_nat: Option<bool>,
}

/// Request to update fabric network
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct UpdateFabricNetworkRequest {
    /// Network name
    #[serde(default)]
    pub name: Option<String>,
    /// Description
    #[serde(default)]
    pub description: Option<String>,
    /// Gateway
    #[serde(default)]
    pub gateway: Option<String>,
    /// Provision start IP
    #[serde(default)]
    pub provision_start_ip: Option<String>,
    /// Provision end IP
    #[serde(default)]
    pub provision_end_ip: Option<String>,
    /// Resolvers
    #[serde(default)]
    pub resolvers: Option<Vec<String>>,
    /// Routes
    #[serde(default)]
    pub routes: Option<serde_json::Value>,
}

/// NIC information
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Nic {
    /// MAC address
    pub mac: String,
    /// Primary NIC
    pub primary: bool,
    /// IP address
    pub ip: String,
    /// Netmask
    pub netmask: String,
    /// Gateway
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway: Option<String>,
    /// Network UUID
    pub network: Uuid,
    /// State
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
}

/// Request to add NIC
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AddNicRequest {
    /// Network UUID
    pub network: Uuid,
}

/// Network IP information
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NetworkIp {
    /// IP address
    pub ip: String,
    /// Reserved
    pub reserved: bool,
    /// Managed
    #[serde(default)]
    pub managed: Option<bool>,
    /// Owner UUID
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_uuid: Option<Uuid>,
    /// Belongs to UUID
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub belongs_to_uuid: Option<Uuid>,
    /// Belongs to type
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub belongs_to_type: Option<String>,
}

/// Request to update network IP
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct UpdateNetworkIpRequest {
    /// Reserved
    pub reserved: bool,
}
