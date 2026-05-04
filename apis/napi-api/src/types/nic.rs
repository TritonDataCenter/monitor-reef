// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! NIC types

use super::common::Uuid;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// NIC state
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum NicState {
    Provisioning,
    Stopped,
    Running,
    /// Catch-all for states added after this client was compiled
    #[serde(other)]
    Unknown,
}

/// What type of object owns this NIC
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum BelongsToType {
    Other,
    Server,
    Zone,
    /// Catch-all for types added after this client was compiled
    #[serde(other)]
    Unknown,
}

/// A NIC (network interface card) record
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct Nic {
    pub belongs_to_type: BelongsToType,
    pub belongs_to_uuid: Uuid,
    /// MAC address as colon-separated string (e.g., "90:b8:d0:c0:ff:ee")
    pub mac: String,
    pub owner_uuid: Uuid,
    /// Whether this is the primary NIC
    pub primary: bool,
    pub state: NicState,
    /// ISO 8601 creation timestamp
    pub created_timestamp: String,
    /// ISO 8601 last-modified timestamp
    pub modified_timestamp: String,
    /// IP address (present when NIC is on a network)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ip: Option<String>,
    /// Whether the NIC is on a fabric network
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fabric: Option<bool>,
    /// Gateway IP from the network
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway: Option<String>,
    /// Whether the gateway has been provisioned
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway_provisioned: Option<bool>,
    /// Whether internet NAT is enabled
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub internet_nat: Option<bool>,
    /// MTU from the network
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mtu: Option<u32>,
    /// Netmask from the network
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub netmask: Option<String>,
    /// NIC tag from the network
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nic_tag: Option<String>,
    /// DNS resolvers from the network
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolvers: Option<Vec<String>>,
    /// VLAN ID from the network
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vlan_id: Option<u32>,
    /// UUID of the network this NIC is on
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_uuid: Option<Uuid>,
    /// Static routes from the network
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routes: Option<HashMap<String, String>>,
    /// Compute node UUID
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cn_uuid: Option<Uuid>,
    /// NIC model (e.g., "virtio")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// NIC tags this NIC provides
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nic_tags_provided: Option<Vec<String>>,
    /// Allow DHCP spoofing
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_dhcp_spoofing: Option<bool>,
    /// Allow IP spoofing
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_ip_spoofing: Option<bool>,
    /// Allow MAC spoofing
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_mac_spoofing: Option<bool>,
    /// Allow restricted traffic
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_restricted_traffic: Option<bool>,
    /// Allow unfiltered promiscuous mode
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_unfiltered_promisc: Option<bool>,
    /// Whether this is an underlay NIC
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub underlay: Option<bool>,
}

/// Request body for creating a NIC (POST /nics)
#[derive(Debug, Deserialize, JsonSchema)]
pub struct CreateNicBody {
    pub belongs_to_uuid: Uuid,
    pub belongs_to_type: BelongsToType,
    pub owner_uuid: Uuid,
    #[serde(default)]
    pub allow_dhcp_spoofing: Option<bool>,
    #[serde(default)]
    pub allow_ip_spoofing: Option<bool>,
    #[serde(default)]
    pub allow_mac_spoofing: Option<bool>,
    #[serde(default)]
    pub allow_restricted_traffic: Option<bool>,
    #[serde(default)]
    pub allow_unfiltered_promisc: Option<bool>,
    #[serde(default)]
    pub check_owner: Option<bool>,
    #[serde(default)]
    pub cn_uuid: Option<Uuid>,
    /// IP address to assign (auto-assigned if omitted)
    #[serde(default)]
    pub ip: Option<String>,
    /// MAC address (auto-generated if omitted)
    #[serde(default)]
    pub mac: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub network_uuid: Option<Uuid>,
    #[serde(default)]
    pub nic_tag: Option<String>,
    #[serde(default)]
    pub nic_tags_available: Option<Vec<String>>,
    #[serde(default)]
    pub nic_tags_provided: Option<Vec<String>>,
    #[serde(default)]
    pub primary: Option<bool>,
    #[serde(default)]
    pub reserved: Option<bool>,
    #[serde(default)]
    pub state: Option<NicState>,
    #[serde(default)]
    pub underlay: Option<bool>,
    #[serde(default)]
    pub vlan_id: Option<u32>,
}

/// Request body for updating a NIC (PUT /nics/:mac)
#[derive(Debug, Deserialize, JsonSchema)]
pub struct UpdateNicBody {
    #[serde(default)]
    pub belongs_to_type: Option<BelongsToType>,
    #[serde(default)]
    pub belongs_to_uuid: Option<Uuid>,
    #[serde(default)]
    pub owner_uuid: Option<Uuid>,
    #[serde(default)]
    pub allow_dhcp_spoofing: Option<bool>,
    #[serde(default)]
    pub allow_ip_spoofing: Option<bool>,
    #[serde(default)]
    pub allow_mac_spoofing: Option<bool>,
    #[serde(default)]
    pub allow_restricted_traffic: Option<bool>,
    #[serde(default)]
    pub allow_unfiltered_promisc: Option<bool>,
    #[serde(default)]
    pub check_owner: Option<bool>,
    #[serde(default)]
    pub cn_uuid: Option<Uuid>,
    #[serde(default)]
    pub ip: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub network_uuid: Option<Uuid>,
    #[serde(default)]
    pub nic_tag: Option<String>,
    #[serde(default)]
    pub nic_tags_provided: Option<Vec<String>>,
    #[serde(default)]
    pub primary: Option<bool>,
    #[serde(default)]
    pub reserved: Option<bool>,
    #[serde(default)]
    pub state: Option<NicState>,
    #[serde(default)]
    pub underlay: Option<bool>,
    #[serde(default)]
    pub vlan_id: Option<u32>,
}

/// Path parameter for NIC endpoints
#[derive(Debug, Deserialize, JsonSchema)]
pub struct NicPath {
    /// MAC address (colon-separated, dash-separated, or bare hex)
    pub mac: String,
}

/// Query parameters for listing NICs (GET /nics)
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListNicsQuery {
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub offset: Option<u32>,
    #[serde(default)]
    pub allow_dhcp_spoofing: Option<bool>,
    #[serde(default)]
    pub allow_ip_spoofing: Option<bool>,
    #[serde(default)]
    pub allow_mac_spoofing: Option<bool>,
    #[serde(default)]
    pub allow_restricted_traffic: Option<bool>,
    #[serde(default)]
    pub allow_unfiltered_promisc: Option<bool>,
    #[serde(default)]
    pub belongs_to_type: Option<String>,
    #[serde(default)]
    pub belongs_to_uuid: Option<String>,
    #[serde(default)]
    pub cn_uuid: Option<String>,
    #[serde(default)]
    pub network_uuid: Option<String>,
    #[serde(default)]
    pub nic_tag: Option<String>,
    #[serde(default)]
    pub nic_tags_provided: Option<String>,
    #[serde(default)]
    pub owner_uuid: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub underlay: Option<bool>,
}

/// Request body for provisioning a NIC on a specific network
/// (POST /networks/:network_uuid/nics)
#[derive(Debug, Deserialize, JsonSchema)]
pub struct CreateNetworkNicBody {
    pub belongs_to_uuid: Uuid,
    pub belongs_to_type: BelongsToType,
    pub owner_uuid: Uuid,
    #[serde(default)]
    pub allow_dhcp_spoofing: Option<bool>,
    #[serde(default)]
    pub allow_ip_spoofing: Option<bool>,
    #[serde(default)]
    pub allow_mac_spoofing: Option<bool>,
    #[serde(default)]
    pub allow_restricted_traffic: Option<bool>,
    #[serde(default)]
    pub allow_unfiltered_promisc: Option<bool>,
    #[serde(default)]
    pub check_owner: Option<bool>,
    #[serde(default)]
    pub cn_uuid: Option<Uuid>,
    #[serde(default)]
    pub ip: Option<String>,
    #[serde(default)]
    pub mac: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub nic_tag: Option<String>,
    #[serde(default)]
    pub nic_tags_provided: Option<Vec<String>>,
    #[serde(default)]
    pub primary: Option<bool>,
    #[serde(default)]
    pub reserved: Option<bool>,
    #[serde(default)]
    pub state: Option<NicState>,
    #[serde(default)]
    pub underlay: Option<bool>,
    #[serde(default)]
    pub vlan_id: Option<u32>,
}
