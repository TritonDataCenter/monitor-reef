// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! SSH key and access key types

use super::common::{RoleTags, Timestamp};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Path parameter for key operations
#[derive(Debug, Deserialize, JsonSchema)]
pub struct KeyPath {
    /// Account login name
    pub account: String,
    /// Key name or fingerprint
    pub name: String,
}

/// Path parameter for access key operations
#[derive(Debug, Deserialize, JsonSchema)]
pub struct AccessKeyPath {
    /// Account login name
    pub account: String,
    /// Access key ID
    pub accesskeyid: String,
}

/// Path parameter for user access key operations
#[derive(Debug, Deserialize, JsonSchema)]
pub struct UserAccessKeyPath {
    /// Account login name
    pub account: String,
    /// User UUID or login
    pub uuid: String,
    /// Access key ID
    pub accesskeyid: String,
}

/// SSH key information
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SshKey {
    /// Key name
    pub name: String,
    /// SSH public key material
    pub key: String,
    /// Key fingerprint
    pub fingerprint: String,
    /// Creation timestamp
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<Timestamp>,
    /// Role tags for RBAC
    #[serde(rename = "role-tag", default, skip_serializing_if = "Option::is_none")]
    pub role_tag: Option<RoleTags>,
}

/// Request to create SSH key
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateSshKeyRequest {
    /// Key name
    pub name: String,
    /// SSH public key material
    pub key: String,
}

/// Access key status
// Note: No `rename_all` needed — Node.js CloudAPI uses PascalCase for status
// values ("Active", "Inactive", "Expired"), which matches serde's default.
// See `sdc-cloudapi/lib/endpoints/accesskeys.js` `translateAccessKey()`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub enum AccessKeyStatus {
    Active,
    Inactive,
    Expired,
    #[serde(other)]
    Unknown,
}

/// Credential type
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CredentialType {
    Permanent,
    Temporary,
    #[serde(other)]
    Unknown,
}

/// Access key information
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct AccessKey {
    /// Access key ID
    pub accesskeyid: String,
    /// Status
    pub status: AccessKeyStatus,
    /// Credential type
    pub credentialtype: CredentialType,
    /// Description
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Creation timestamp
    pub created: Timestamp,
    /// Last updated timestamp
    pub updated: Timestamp,
    /// Expiration timestamp (null for permanent keys)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expiration: Option<Timestamp>,
}

/// Request to create access key
#[derive(Debug, Deserialize, JsonSchema)]
pub struct CreateAccessKeyRequest {
    /// Initial status (defaults to Active)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<AccessKeyStatus>,
    /// Description
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Response when creating access key (includes secret shown only once)
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct CreateAccessKeyResponse {
    /// Access key ID
    pub accesskeyid: String,
    /// Access key secret (only provided on creation)
    pub accesskeysecret: String,
    /// Status
    pub status: AccessKeyStatus,
    /// Credential type
    pub credentialtype: CredentialType,
    /// Description
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Creation timestamp
    pub created: Timestamp,
    /// Last updated timestamp
    pub updated: Timestamp,
    /// Expiration timestamp
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expiration: Option<Timestamp>,
}

/// Request to update an access key
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct UpdateAccessKeyRequest {
    /// New status
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<AccessKeyStatus>,
    /// New description
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}
