// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Types for the AWS-STS-emulating endpoints under `/sts/*`.
//!
//! Request bodies use `caller` (lowerCamel) while response envelopes mirror
//! AWS naming (nested `PascalCase` containers wrapping `PascalCase` fields).

use crate::types::common::Uuid;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// ------------------------------------------------------------------------
// Caller shape (shared by all STS requests)
// ------------------------------------------------------------------------

/// Account identifier attached to STS calls.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CallerAccount {
    pub uuid: Uuid,
    pub login: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(flatten, default)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

/// Optional sub-user identifier attached to STS calls.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CallerUser {
    pub uuid: Uuid,
    pub login: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(flatten, default)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

/// The `caller` object every STS endpoint expects in its body.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Caller {
    pub account: CallerAccount,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<CallerUser>,
}

// ------------------------------------------------------------------------
// AssumeRole
// ------------------------------------------------------------------------

/// Request body for `POST /sts/assume-role`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct AssumeRoleRequest {
    pub caller: Caller,
    #[serde(rename = "RoleArn")]
    pub role_arn: String,
    #[serde(rename = "RoleSessionName")]
    pub role_session_name: String,
    /// Desired lifetime in seconds. Upstream validates the range 900..=43200.
    #[serde(
        rename = "DurationSeconds",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub duration_seconds: Option<u64>,
}

/// Temporary credentials returned by `AssumeRole` and `GetSessionToken`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "PascalCase")]
pub struct StsCredentials {
    pub access_key_id: String,
    pub secret_access_key: String,
    pub session_token: String,
    /// RFC3339/ISO8601 expiration timestamp.
    pub expiration: String,
}

/// Assumed-role principal returned by `AssumeRole`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "PascalCase")]
pub struct AssumedRoleUser {
    pub assumed_role_id: String,
    pub arn: String,
}

/// Inner `AssumeRoleResult` object.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "PascalCase")]
pub struct AssumeRoleResult {
    pub credentials: StsCredentials,
    pub assumed_role_user: AssumedRoleUser,
}

/// Middle envelope wrapping the result.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "PascalCase")]
pub struct AssumeRoleResponseInner {
    pub assume_role_result: AssumeRoleResult,
}

/// Outer response envelope returned by `POST /sts/assume-role`.
///
/// Mahi emulates AWS's XML envelope with a JSON object of the same shape, so
/// callers must navigate two nested `AssumeRoleResponse` / `AssumeRoleResult`
/// keys.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "PascalCase")]
pub struct AssumeRoleResponse {
    pub assume_role_response: AssumeRoleResponseInner,
}

// ------------------------------------------------------------------------
// GetSessionToken
// ------------------------------------------------------------------------

/// Request body for `POST /sts/get-session-token`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct GetSessionTokenRequest {
    pub caller: Caller,
    /// Desired lifetime in seconds. Upstream validates 900..=129600.
    #[serde(
        rename = "DurationSeconds",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub duration_seconds: Option<u64>,
}

/// Inner `GetSessionTokenResult` object.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "PascalCase")]
pub struct GetSessionTokenResult {
    pub credentials: StsCredentials,
}

/// Middle envelope wrapping the result.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "PascalCase")]
pub struct GetSessionTokenResponseInner {
    pub get_session_token_result: GetSessionTokenResult,
}

/// Outer response envelope for `POST /sts/get-session-token`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "PascalCase")]
pub struct GetSessionTokenResponse {
    pub get_session_token_response: GetSessionTokenResponseInner,
}

// ------------------------------------------------------------------------
// GetCallerIdentity
// ------------------------------------------------------------------------

/// Request body for `POST /sts/get-caller-identity`.
///
/// The response is emitted as raw XML with `Content-Type: text/xml`; the
/// trait therefore uses `Result<Response<Body>, HttpError>` for this endpoint
/// and a Phase-2b spec patch reshapes the response schema accordingly.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct GetCallerIdentityRequest {
    pub caller: Caller,
}
