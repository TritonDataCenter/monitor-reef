// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! User, role, and policy types

use super::common::{Timestamp, Uuid};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Path parameter for user operations
#[derive(Debug, Deserialize, JsonSchema)]
pub struct UserPath {
    /// Account login name
    pub account: String,
    /// User UUID or login
    pub uuid: String,
}

/// Path parameter for user key operations
#[derive(Debug, Deserialize, JsonSchema)]
pub struct UserKeyPath {
    /// Account login name
    pub account: String,
    /// User UUID or login
    pub uuid: String,
    /// Key name or fingerprint
    pub name: String,
}

/// Path parameter for role operations
#[derive(Debug, Deserialize, JsonSchema)]
pub struct RolePath {
    /// Account login name
    pub account: String,
    /// Role UUID or name
    pub role: String,
}

/// Path parameter for policy operations
#[derive(Debug, Deserialize, JsonSchema)]
pub struct PolicyPath {
    /// Account login name
    pub account: String,
    /// Policy UUID or name
    pub policy: String,
}

/// User information
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct User {
    /// User UUID
    pub id: Uuid,
    /// User login
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
    /// Phone number
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phone: Option<String>,
    /// Creation timestamp
    pub created: Timestamp,
    /// Last update timestamp
    pub updated: Timestamp,
}

/// Request to create user
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateUserRequest {
    /// User login
    pub login: String,
    /// Email address
    pub email: String,
    /// Password
    pub password: String,
    /// Company name
    #[serde(default)]
    pub company_name: Option<String>,
    /// First name
    #[serde(default)]
    pub first_name: Option<String>,
    /// Last name
    #[serde(default)]
    pub last_name: Option<String>,
    /// Phone number
    #[serde(default)]
    pub phone: Option<String>,
}

/// Request to update user
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct UpdateUserRequest {
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
    /// Phone number
    #[serde(default)]
    pub phone: Option<String>,
}

/// Request to change user password
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChangePasswordRequest {
    /// Current password
    pub password: String,
    /// New password
    pub password_confirmation: String,
}

/// Role information
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Role {
    /// Role UUID
    pub id: Uuid,
    /// Role name
    pub name: String,
    /// Members (user UUIDs or logins)
    #[serde(default)]
    pub members: Vec<String>,
    /// Default members (user UUIDs or logins)
    #[serde(default)]
    pub default_members: Vec<String>,
    /// Policies (policy UUIDs or names)
    #[serde(default)]
    pub policies: Vec<String>,
}

/// Request to create role
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateRoleRequest {
    /// Role name
    pub name: String,
    /// Members (user UUIDs or logins)
    #[serde(default)]
    pub members: Option<Vec<String>>,
    /// Default members (user UUIDs or logins)
    #[serde(default)]
    pub default_members: Option<Vec<String>>,
    /// Policies (policy UUIDs or names)
    #[serde(default)]
    pub policies: Option<Vec<String>>,
}

/// Request to update role
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct UpdateRoleRequest {
    /// Role name
    #[serde(default)]
    pub name: Option<String>,
    /// Members (user UUIDs or logins)
    #[serde(default)]
    pub members: Option<Vec<String>>,
    /// Default members (user UUIDs or logins)
    #[serde(default)]
    pub default_members: Option<Vec<String>>,
    /// Policies (policy UUIDs or names)
    #[serde(default)]
    pub policies: Option<Vec<String>>,
}

/// Policy information
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Policy {
    /// Policy UUID
    pub id: Uuid,
    /// Policy name
    pub name: String,
    /// Policy rules (array of rule strings)
    pub rules: Vec<String>,
    /// Description
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Request to create policy
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreatePolicyRequest {
    /// Policy name
    pub name: String,
    /// Policy rules (array of rule strings)
    pub rules: Vec<String>,
    /// Description
    #[serde(default)]
    pub description: Option<String>,
}

/// Request to update policy
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct UpdatePolicyRequest {
    /// Policy name
    #[serde(default)]
    pub name: Option<String>,
    /// Policy rules (array of rule strings)
    #[serde(default)]
    pub rules: Option<Vec<String>>,
    /// Description
    #[serde(default)]
    pub description: Option<String>,
}
