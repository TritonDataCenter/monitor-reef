// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Types for `POST /aws-verify` (server-side SigV4 signature verification).

use crate::types::common::Uuid;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Query parameters for `POST /aws-verify`.
///
/// The original request's `method` and `url` are sent here so that the server
/// can reconstruct the canonical SigV4 signing string. The incoming request
/// body is forwarded opaquely and signed headers are read from the request's
/// own header map.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SigV4VerifyQuery {
    /// HTTP method of the original (to-be-verified) request.
    pub method: String,
    /// URL (path + query) of the original (to-be-verified) request.
    pub url: String,
}

/// Result of `POST /aws-verify` on a valid signature.
///
/// The handler always responds with `valid: true` on success (invalid
/// signatures come back as HTTP errors).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SigV4VerifyResult {
    pub valid: bool,
    #[serde(rename = "accessKeyId")]
    pub access_key_id: String,
    #[serde(rename = "userUuid")]
    pub user_uuid: Uuid,
    /// Assumed-role metadata (present only for MSAR temporary credentials).
    #[serde(
        rename = "assumedRole",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub assumed_role: Option<serde_json::Value>,
    /// Principal uuid for temporary credentials.
    #[serde(
        rename = "principalUuid",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub principal_uuid: Option<Uuid>,
    #[serde(
        rename = "isTemporaryCredential",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub is_temporary_credential: Option<bool>,
    /// Derived signing key. Upstream JSON-encodes a Node `Buffer`, so the
    /// on-the-wire shape is `{"type":"Buffer","data":[...]}`. Phase 5 should
    /// confirm whether any caller actually consumes this; we keep it
    /// opaque for now.
    #[serde(
        rename = "signingKey",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub signing_key: Option<serde_json::Value>,
}
