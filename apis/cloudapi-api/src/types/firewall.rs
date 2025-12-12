// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Firewall rule types

use super::common::{Timestamp, Uuid};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Path parameter for firewall rule operations
#[derive(Debug, Deserialize, JsonSchema)]
pub struct FirewallRulePath {
    /// Account login name
    pub account: String,
    /// Firewall rule UUID
    pub id: Uuid,
}

/// Firewall rule
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FirewallRule {
    /// Rule UUID
    pub id: Uuid,
    /// Rule text
    pub rule: String,
    /// Enabled
    pub enabled: bool,
    /// Global rule
    #[serde(default)]
    pub global: Option<bool>,
    /// Description
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Creation timestamp
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<Timestamp>,
    /// Last update timestamp
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated: Option<Timestamp>,
}

/// Request to create firewall rule
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateFirewallRuleRequest {
    /// Rule text
    pub rule: String,
    /// Enabled (defaults to false)
    #[serde(default)]
    pub enabled: Option<bool>,
    /// Description
    #[serde(default)]
    pub description: Option<String>,
}

/// Request to update firewall rule
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct UpdateFirewallRuleRequest {
    /// Rule text
    #[serde(default)]
    pub rule: Option<String>,
    /// Enabled
    #[serde(default)]
    pub enabled: Option<bool>,
    /// Description
    #[serde(default)]
    pub description: Option<String>,
}
