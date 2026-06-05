// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Access-key and AWS SigV4 auth types.

use crate::types::common::{Account, CredentialType, Role, User, Uuid};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Secret material stored alongside an access key id.
///
/// Lives under `user.accesskeys[keyId]`. For temporary credentials, the
/// `sessionToken`, `expiration`, `principalUuid`, and `assumedRole` fields are
/// populated; for permanent credentials only `secret` and `type` are present.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AccessKeySecret {
    /// Secret access key (base64).
    pub secret: String,
    #[serde(rename = "type")]
    pub credential_type: CredentialType,
    /// Expiration timestamp (ISO8601) for temporary credentials.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expiration: Option<String>,
    /// Opaque session-token blob returned to clients using STS credentials.
    #[serde(
        rename = "sessionToken",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub session_token: Option<String>,
    /// Uuid of the principal (user) the temporary credential belongs to.
    #[serde(
        rename = "principalUuid",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub principal_uuid: Option<Uuid>,
    /// Assumed-role metadata for MSAR-prefixed temporary credentials.
    #[serde(
        rename = "assumedRole",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub assumed_role: Option<serde_json::Value>,
}

/// Policy entry attached to an assumed role.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PolicyEntry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uuid: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rules: Option<Vec<String>>,
}

/// Assumed-role context attached to temporary credentials.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AssumedRole {
    pub arn: String,
    #[serde(
        rename = "sessionName",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub session_name: Option<String>,
    #[serde(rename = "roleUuid", default, skip_serializing_if = "Option::is_none")]
    pub role_uuid: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policies: Option<Vec<PolicyEntry>>,
}

/// Result of `GET /aws-auth/:accesskeyid`.
///
/// Contains the full principal context plus, when applicable, assumed-role /
/// temporary-credential metadata.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AwsAuthResult {
    pub account: Account,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<User>,
    #[serde(default)]
    pub roles: HashMap<String, Role>,
    #[serde(
        rename = "assumedRole",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub assumed_role: Option<AssumedRole>,
    #[serde(
        rename = "isTemporaryCredential",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub is_temporary_credential: Option<bool>,
    #[serde(
        rename = "sessionName",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub session_name: Option<String>,
}
