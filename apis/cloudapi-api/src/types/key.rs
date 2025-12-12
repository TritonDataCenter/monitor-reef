// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! SSH key and access key types

use super::common::{Timestamp, Uuid};
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

/// Access key information
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AccessKey {
    /// Access key ID
    pub id: String,
    /// User UUID
    pub user: Uuid,
    /// Creation timestamp
    pub created: Timestamp,
}

/// Request to create access key
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateAccessKeyRequest {
    /// Access key name (optional)
    #[serde(default)]
    pub name: Option<String>,
}

/// Response when creating access key (includes secret)
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateAccessKeyResponse {
    /// Access key ID
    pub id: String,
    /// Access key secret (only provided on creation)
    pub secret: String,
    /// User UUID
    pub user: Uuid,
    /// Creation timestamp
    pub created: Timestamp,
}
