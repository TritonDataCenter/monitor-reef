// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Types for the AWS-IAM-emulating endpoints under `/iam/*`.
//!
//! IAM responses expose role objects with AWS-style PascalCase fields while
//! request bodies use lowerCamelCase. Some list responses mix casings at the
//! top level (lowercase `roles`, PascalCase `IsTruncated`/`Marker`).

use crate::types::common::Uuid;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// ------------------------------------------------------------------------
// Role object (shared by all IAM responses)
// ------------------------------------------------------------------------

/// IAM role object (PascalCase on the wire).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "PascalCase")]
pub struct IamRole {
    /// IAM path (default `"/"`).
    pub path: String,
    pub role_name: String,
    pub role_id: Uuid,
    pub arn: String,
    /// RFC3339 create date.
    pub create_date: String,
    /// JSON-stringified trust policy document.
    pub assume_role_policy_document: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_session_duration: Option<u32>,
}

// ------------------------------------------------------------------------
// Path params
// ------------------------------------------------------------------------

/// Path parameter for role-scoped routes (`roleName`).
#[derive(Debug, Deserialize, JsonSchema)]
pub struct RoleNamePath {
    pub role_name: String,
}

/// Path parameters for `GET /iam/get-role-policy/{roleName}/{policyName}`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct RolePolicyPath {
    pub role_name: String,
    pub policy_name: String,
}

// ------------------------------------------------------------------------
// Query types
// ------------------------------------------------------------------------

/// Query for endpoints that only need an account uuid.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AccountUuidQuery {
    pub account_uuid: Uuid,
}

/// Query for `DEL /iam/delete-role-policy`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeleteRolePolicyQuery {
    pub role_name: String,
    pub policy_name: String,
    pub account_uuid: Uuid,
}

/// Query for `GET /iam/list-roles`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ListRolesQuery {
    pub account_uuid: Uuid,
    /// Upstream default is 100; server caps at 1000.
    #[serde(default)]
    pub max_items: Option<u32>,
    #[serde(default)]
    pub marker: Option<String>,
    /// Alternate name accepted by upstream (`?startingToken=`).
    #[serde(default)]
    pub starting_token: Option<String>,
}

/// Query for `GET /iam/list-role-policies/{roleName}`.
///
/// **Naming quirk**: the upstream implementation uses `maxitems` (lowercase)
/// rather than `maxItems`. We honour the primary name and accept `maxItems`
/// as an alias so callers written against either spelling keep working.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ListRolePoliciesQuery {
    pub account_uuid: Uuid,
    #[serde(default)]
    pub marker: Option<String>,
    /// Maximum number of policy names to return. Upstream is `maxitems`
    /// (lowercase) in the handler; this alias accepts the typo-free spelling
    /// for forward compatibility.
    #[serde(default, rename = "maxitems", alias = "maxItems")]
    pub maxitems: Option<u32>,
}

// ------------------------------------------------------------------------
// Request bodies
// ------------------------------------------------------------------------

/// Request body for `POST /iam/create-role`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateRoleRequest {
    pub role_name: String,
    pub account_uuid: Uuid,
    /// Trust policy document (JSON-encoded string).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assume_role_policy_document: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// IAM path. Upstream defaults to `"/"` when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// Mahi-native policy payload attached to a role via `PutRolePolicy`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct MantaPolicy {
    pub id: Uuid,
    pub name: String,
    /// Aperture rule strings.
    pub rules: Vec<String>,
}

/// Request body for `POST /iam/put-role-policy`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PutRolePolicyRequest {
    pub role_name: String,
    pub policy_name: String,
    /// Raw IAM policy document (JSON string).
    pub policy_document: String,
    /// Mahi-native policy object stored under `/policy/:id`.
    pub manta_policy: MantaPolicy,
    pub account_uuid: Uuid,
}

// ------------------------------------------------------------------------
// Response types
// ------------------------------------------------------------------------

/// Response for `POST /iam/create-role`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "PascalCase")]
pub struct CreateRoleResponse {
    pub role: IamRole,
}

/// Response for `GET /iam/get-role/{roleName}`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "PascalCase")]
pub struct GetRoleResponse {
    pub role: IamRole,
}

/// Response for `POST /iam/put-role-policy`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PutRolePolicyResponse {
    pub message: String,
    #[serde(rename = "roleName")]
    pub role_name: String,
    #[serde(rename = "policyName")]
    pub policy_name: String,
}

/// Response for `DEL /iam/delete-role/{roleName}`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DeleteRoleResponse {
    pub message: String,
    #[serde(rename = "roleName")]
    pub role_name: String,
}

/// Response for `DEL /iam/delete-role-policy`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DeleteRolePolicyResponse {
    pub message: String,
    #[serde(rename = "roleName")]
    pub role_name: String,
    #[serde(rename = "policyName")]
    pub policy_name: String,
}

/// Response for `GET /iam/list-roles`.
///
/// **Mixed casing** at the top level: `roles` is lowercase, `IsTruncated` and
/// `Marker` are PascalCase. Apply field-level renames rather than a struct
/// `rename_all`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ListRolesResponse {
    /// Lowercase on the wire.
    pub roles: Vec<IamRole>,
    #[serde(rename = "IsTruncated")]
    pub is_truncated: bool,
    #[serde(rename = "Marker", default, skip_serializing_if = "Option::is_none")]
    pub marker: Option<String>,
}

/// Response for `GET /iam/list-role-policies/{roleName}`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "PascalCase")]
pub struct ListRolePoliciesResponse {
    pub policy_names: Vec<String>,
    pub is_truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub marker: Option<String>,
}

/// Response for `GET /iam/get-role-policy/{roleName}/{policyName}`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "PascalCase")]
pub struct GetRolePolicyResponse {
    pub role_name: String,
    pub policy_name: String,
    /// Policy document (JSON string).
    pub policy_document: String,
}
