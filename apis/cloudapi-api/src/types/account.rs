// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Account-related types

use super::common::{RoleTags, Timestamp, Uuid};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Account information
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    /// Account UUID
    pub id: Uuid,
    /// Account login name
    pub login: String,
    /// Email address
    pub email: String,
    /// Company name
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_name: Option<String>,
    /// First name
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_name: Option<String>,
    /// Last name
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_name: Option<String>,
    /// Postal address
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    /// Postal code
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub postal_code: Option<String>,
    /// City
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub city: Option<String>,
    /// State/Province
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    /// Country
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
    /// Phone number
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phone: Option<String>,
    /// Account creation timestamp
    pub created: Timestamp,
    /// Last update timestamp
    pub updated: Timestamp,
    /// Triton CNS enabled
    #[serde(default)]
    pub triton_cns_enabled: Option<bool>,
}

/// Request to update account
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct UpdateAccountRequest {
    /// Email address
    #[serde(default)]
    pub email: Option<String>,
    /// Company name
    #[serde(default)]
    pub company_name: Option<String>,
    /// First name
    #[serde(default)]
    pub first_name: Option<String>,
    /// Last name
    #[serde(default)]
    pub last_name: Option<String>,
    /// Postal address
    #[serde(default)]
    pub address: Option<String>,
    /// Postal code
    #[serde(default)]
    pub postal_code: Option<String>,
    /// City
    #[serde(default)]
    pub city: Option<String>,
    /// State/Province
    #[serde(default)]
    pub state: Option<String>,
    /// Country
    #[serde(default)]
    pub country: Option<String>,
    /// Phone number
    #[serde(default)]
    pub phone: Option<String>,
    /// Triton CNS enabled
    #[serde(default)]
    pub triton_cns_enabled: Option<bool>,
}

/// Provisioning limits for an account
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProvisioningLimits {
    /// Maximum number of machines
    pub machines: Option<i64>,
    /// Maximum RAM in MB
    pub ram: Option<i64>,
    /// Maximum disk space in MB
    pub disk: Option<i64>,
}

/// Configuration settings
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    /// Default network UUID
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_network: Option<Uuid>,
}

/// Request to update configuration
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct UpdateConfigRequest {
    /// Default network UUID
    #[serde(default)]
    pub default_network: Option<Uuid>,
}

/// Request to replace role tags
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReplaceRoleTagsRequest {
    /// Role tags (list of role names)
    #[serde(rename = "role-tag", default)]
    pub role_tag: RoleTags,
}

/// Response after replacing role tags on a resource
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RoleTagsResponse {
    /// Resource path name
    pub name: String,
    /// List of role names assigned to the resource
    #[serde(rename = "role-tag")]
    pub role_tag: RoleTags,
}
