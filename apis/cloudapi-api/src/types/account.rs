// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

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
    /// Note: This field uses snake_case in the API response, not camelCase
    #[serde(default, rename = "triton_cns_enabled")]
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
    #[serde(default, rename = "triton_cns_enabled")]
    pub triton_cns_enabled: Option<bool>,
}

/// A single provisioning limit entry.
///
/// Each limit constrains a specific dimension (VM count, RAM, or disk quota),
/// optionally filtered by brand, image, or OS. A `value` of `-1` blocks all
/// matching provisions; `0` means unlimited (filtered out before the response
/// reaches the client).
///
/// Units for `value` and `used` depend on `by`:
/// - absent / `"machines"` → count of VMs
/// - `"ram"` → MiB
/// - `"quota"` → GiB
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ProvisioningLimit {
    /// The limit value (threshold).
    pub value: i64,
    /// Current usage against this limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub used: Option<i64>,
    /// What dimension the limit counts: `"ram"`, `"quota"`, or `"machines"`.
    /// When absent, defaults to counting VMs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
    /// Type of filter applied: `"brand"`, `"image"`, or `"os"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<String>,
    /// Brand filter value (when `check` is `"brand"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brand: Option<String>,
    /// Image filter value (when `check` is `"image"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    /// OS filter value (when `check` is `"os"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os: Option<String>,
}

/// Provisioning limits for an account — an array of limit entries.
pub type ProvisioningLimits = Vec<ProvisioningLimit>;

/// Configuration settings
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct Config {
    /// Default network UUID
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_network: Option<Uuid>,
}

/// Request to update configuration
#[derive(Debug, Deserialize, JsonSchema)]
pub struct UpdateConfigRequest {
    /// Default network UUID
    #[serde(default)]
    pub default_network: Option<Uuid>,
}

/// Request to replace role tags
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReplaceRoleTagsRequest {
    /// Role tags (list of role names)
    #[serde(rename = "role-tag", default)]
    pub role_tag: RoleTags,
}

/// Response after replacing role tags on a resource
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RoleTagsResponse {
    /// Resource path name
    pub name: String,
    /// List of role names assigned to the resource
    #[serde(rename = "role-tag")]
    pub role_tag: RoleTags,
}
