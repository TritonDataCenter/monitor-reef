// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! NIC tag types

use super::common::Uuid;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A NIC tag
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct NicTag {
    /// MTU for the NIC tag
    pub mtu: u32,
    /// Name of the NIC tag
    pub name: String,
    /// Unique identifier
    pub uuid: Uuid,
}

/// Request body for creating a NIC tag (POST /nic_tags)
#[derive(Debug, Deserialize, JsonSchema)]
pub struct CreateNicTagBody {
    /// NIC tag name (required)
    pub name: String,
    /// UUID (auto-generated if omitted)
    #[serde(default)]
    pub uuid: Option<Uuid>,
    /// MTU (defaults to 1500 if omitted)
    #[serde(default)]
    pub mtu: Option<u32>,
}

/// Request body for updating a NIC tag (PUT /nic_tags/:oldname)
#[derive(Debug, Deserialize, JsonSchema)]
pub struct UpdateNicTagBody {
    /// New name for the NIC tag
    #[serde(default)]
    pub name: Option<String>,
    /// New MTU
    #[serde(default)]
    pub mtu: Option<u32>,
}

/// Path parameter for NIC tag endpoints using :name
#[derive(Debug, Deserialize, JsonSchema)]
pub struct NicTagPath {
    pub name: String,
}

/// Query parameters for listing NIC tags (GET /nic_tags)
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListNicTagsQuery {
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub offset: Option<u32>,
}
