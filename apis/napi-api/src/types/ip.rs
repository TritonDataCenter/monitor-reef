// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! IP address record types

use super::common::Uuid;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// An IP address record within a network
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct Ip {
    /// IP address string
    pub ip: String,
    /// UUID of the network this IP belongs to
    pub network_uuid: Uuid,
    /// Whether this IP is reserved
    pub reserved: bool,
    /// Whether this IP is free (unassigned)
    pub free: bool,
    /// Type of object this IP is assigned to (when assigned)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub belongs_to_type: Option<String>,
    /// UUID of the object this IP is assigned to (when assigned)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub belongs_to_uuid: Option<Uuid>,
    /// UUID of the owner (when assigned)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_uuid: Option<Uuid>,
}

/// Request body for updating an IP (PUT /networks/:network_uuid/ips/:ip_addr)
///
/// Setting `free=true` triggers deletion (unassignment) of the IP rather than
/// an update. The `unassign` parameter is mutually exclusive with `free`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct UpdateIpBody {
    #[serde(default)]
    pub belongs_to_type: Option<String>,
    #[serde(default)]
    pub belongs_to_uuid: Option<Uuid>,
    #[serde(default)]
    pub check_owner: Option<bool>,
    #[serde(default)]
    pub owner_uuid: Option<Uuid>,
    #[serde(default)]
    pub reserved: Option<bool>,
    /// Set to true to free (unassign) this IP
    #[serde(default)]
    pub free: Option<bool>,
    /// Set to true to unassign this IP (mutually exclusive with `free`)
    #[serde(default)]
    pub unassign: Option<bool>,
}

/// Path parameter for IP address endpoints
///
/// Note: uses `uuid` (not `network_uuid`) because Dropshot requires
/// consistent variable names at the same path level as `/networks/{uuid}`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct IpPath {
    pub uuid: Uuid,
    /// IP address string
    pub ip_addr: String,
}

/// Query parameters for listing IPs (GET /networks/:network_uuid/ips)
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListIpsQuery {
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub offset: Option<u32>,
    #[serde(default)]
    pub belongs_to_type: Option<String>,
    #[serde(default)]
    pub belongs_to_uuid: Option<String>,
    #[serde(default)]
    pub owner_uuid: Option<String>,
}

/// Query parameters for searching IPs (GET /search/ips)
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchIpsQuery {
    /// IP address to search for (required)
    pub ip: String,
    #[serde(default)]
    pub belongs_to_type: Option<String>,
    #[serde(default)]
    pub belongs_to_uuid: Option<String>,
    #[serde(default)]
    pub fabric: Option<bool>,
    #[serde(default)]
    pub owner_uuid: Option<String>,
}
