// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Common types shared across Mahi endpoints.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// UUID type alias (follows repo convention).
pub type Uuid = uuid::Uuid;

/// Object types accepted by `GET /uuids?type=`.
///
/// The set is asserted in upstream `redislib.getUuid` and is lowercase on the
/// wire.
#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, clap::ValueEnum,
)]
#[serde(rename_all = "lowercase")]
pub enum ObjectType {
    Role,
    User,
    Policy,
}

/// Internal `type` field stored on every Redis blob. Mahi may grow additional
/// object types over time, so include a catch-all `Unknown` variant.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ObjectTypeTag {
    Account,
    User,
    Role,
    Policy,
    AccessKey,
    /// Catch-all for object types added after this client was compiled.
    #[serde(other)]
    Unknown,
}

/// Credential type for access keys.
///
/// Wire format values are `"permanent"` and `"temporary"`. Temporary
/// credentials are produced by the STS endpoints (MSAR/MSTS prefixes).
#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, clap::ValueEnum,
)]
#[serde(rename_all = "lowercase")]
pub enum CredentialType {
    Permanent,
    Temporary,
    /// Catch-all for credential types added after this client was compiled.
    #[serde(other)]
    #[clap(skip)]
    Unknown,
}

/// ARN partition accepted by Mahi STS endpoints.
///
/// Mirrors `DEFAULT_ARN_PARTITION` and the ARN regex in
/// `validateStsAssumeRoleInputs`. Wire values are lowercase.
#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, clap::ValueEnum,
)]
#[serde(rename_all = "lowercase")]
pub enum ArnPartition {
    Aws,
    Manta,
    Triton,
}

/// Redis-backed account blob (UFDS passthrough).
///
/// The concrete set of fields on accounts varies by deployment and UFDS
/// schema. We model the commonly-observed fields explicitly and capture any
/// extras via `#[serde(flatten)]` into a catch-all map.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Account {
    pub uuid: Uuid,
    pub login: String,
    #[serde(rename = "type")]
    pub account_type: ObjectTypeTag,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_for_provisioning: Option<bool>,
    /// Set by `redislib.getAccount`. Camel-cased on the wire (UFDS bool).
    #[serde(
        rename = "isOperator",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub is_operator: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub groups: Option<Vec<String>>,
    /// Role uuids attached at the account level.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roles: Option<Vec<Uuid>>,
    /// Key fingerprints -> key blobs. Shape is deployment-specific.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keys: Option<HashMap<String, serde_json::Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cn: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub s: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phone: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub postal_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub city: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub given_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sn: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub triton_cns_enabled: Option<bool>,
    /// Any additional UFDS-passthrough fields not explicitly modeled above.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// Redis-backed sub-user blob.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct User {
    pub uuid: Uuid,
    pub login: String,
    /// Owning account uuid.
    pub account: Uuid,
    #[serde(rename = "type")]
    pub user_type: ObjectTypeTag,
    /// Role uuids attached at the user level.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roles: Option<Vec<Uuid>>,
    /// Access keys keyed by access-key id. Values contain secret material.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accesskeys: Option<HashMap<String, crate::types::accesskey::AccessKeySecret>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cn: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub s: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub given_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sn: Option<String>,
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// Redis-backed role blob.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Role {
    pub uuid: Uuid,
    pub name: String,
    /// Owning account uuid.
    pub account: Uuid,
    #[serde(rename = "type")]
    pub role_type: ObjectTypeTag,
    /// Policy uuids attached to this role.
    #[serde(default)]
    pub policies: Vec<Uuid>,
    /// Inline rules (aperture policy expressions).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rules: Option<Vec<String>>,
    /// STS assume-role policy document (JSON string).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assumerolepolicydocument: Option<String>,
    /// RFC3339/UFDS create timestamp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub createtime: Option<String>,
    /// IAM path (default `"/"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Policy metadata attached for STS evaluation. Shape is internal.
    /// Wire name is camelCase (`permissionPolicies`); it is the only
    /// camelCase field on this otherwise-lowercase Redis blob.
    #[serde(
        rename = "permissionPolicies",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub permission_policies: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub members: Option<Vec<serde_json::Value>>,
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// Redis-backed policy blob.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Policy {
    pub uuid: Uuid,
    pub name: String,
    /// Owning account uuid.
    pub account: Uuid,
    #[serde(rename = "type")]
    pub policy_type: ObjectTypeTag,
    /// Aperture rules.
    pub rules: Vec<String>,
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// Auth payload returned by most lookup endpoints.
///
/// Mahi builds this up progressively as `req.auth` and sends it back verbatim.
/// `user` is optional because several routes (and the `fallback=true` branch
/// of `GET /users`) only populate `account` + `roles`. `role` is only
/// populated by `GET /roles`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AuthInfo {
    pub account: Account,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<User>,
    /// Roles map keyed by role uuid.
    #[serde(default)]
    pub roles: HashMap<String, Role>,
    /// Single role populated only by `GET /roles`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<Role>,
}
