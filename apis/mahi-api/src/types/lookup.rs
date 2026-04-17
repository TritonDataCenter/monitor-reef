// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Query, path, body, and response types for the classic Mahi lookup routes.

use crate::types::common::{ObjectType, Uuid};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ------------------------------------------------------------------------
// Path parameters
// ------------------------------------------------------------------------

/// Path parameter for `GET /accounts/{accountid}`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct AccountIdPath {
    pub accountid: Uuid,
}

/// Path parameter for `GET /users/{userid}`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct UserIdPath {
    pub userid: Uuid,
}

/// Path parameters for the deprecated `GET /account/{account}`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct LegacyAccountPath {
    /// Account login (not uuid).
    pub account: String,
}

/// Path parameters for the deprecated `GET /user/{account}/{user}`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct LegacyUserPath {
    /// Account login.
    pub account: String,
    /// Sub-user login.
    pub user: String,
}

// ------------------------------------------------------------------------
// Query structs
// ------------------------------------------------------------------------

/// Query for `GET /accounts` (lookup by login).
#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetAccountQuery {
    /// Account login. Upstream also accepts the historical alias
    /// `account=` for the same value.
    #[serde(default, alias = "account")]
    pub login: Option<String>,
}

/// Query for `GET /users` (lookup sub-user by account+login).
#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetUserQuery {
    /// Account login.
    pub account: String,
    /// Sub-user login.
    pub login: String,
    /// When `true`, missing sub-users are swallowed and the account-only
    /// `AuthInfo` (no `user`, empty `roles`) is returned with HTTP 200.
    /// Defaults to `true` upstream. Accepts boolean `true`/`false` or the
    /// strings `"true"`/`"false"`.
    #[serde(default)]
    pub fallback: Option<bool>,
}

/// Query for `GET /roles` (list members of a role).
#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetRolesQuery {
    pub account: String,
    /// Role name. Upstream also accepts the alias `name=`.
    #[serde(default, alias = "name")]
    pub role: Option<String>,
}

/// Query for `GET /uuids`.
///
/// Upstream accepts a repeated `name=a&name=b` query parameter. Dropshot
/// query structs require scalar fields, so the `name` value arrives here as a
/// single comma-separated string and service implementations must split it
/// themselves. An empty/absent value means "no names — return `{account}`
/// alone".
///
/// **Behavior change from Node.js**: the legacy server accepts either repeated
/// params or a single value. The Rust server accepts the comma-separated
/// form. Phase 2b should add an OpenAPI spec patch declaring
/// `style: form, explode: true` on this parameter so the generated OpenAPI
/// doc reflects the wire format node-mahi actually uses.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct NameToUuidQuery {
    pub account: String,
    /// Type of object to look up.
    #[serde(rename = "type")]
    pub object_type: ObjectType,
    /// Comma-separated list of names, or `None` for "no names".
    #[serde(default)]
    pub name: Option<String>,
}

/// Query for `GET /names`.
///
/// See the note on [`NameToUuidQuery`] for the handling of repeated query
/// parameters. `uuid` is a comma-separated string here; the service layer
/// splits and parses each UUID.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct UuidToNameQuery {
    /// Comma-separated list of UUIDs to resolve.
    #[serde(default)]
    pub uuid: Option<String>,
}

// ------------------------------------------------------------------------
// Body types for deprecated POSTs
// ------------------------------------------------------------------------

/// Accepts either a single value or a JSON array of values (both forms are
/// used by the deprecated `POST /getUuid` and `POST /getName` endpoints).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum StringOrVec {
    One(String),
    Many(Vec<String>),
}

impl StringOrVec {
    /// Flatten to a vec regardless of incoming shape.
    pub fn into_vec(self) -> Vec<String> {
        match self {
            Self::One(s) => vec![s],
            Self::Many(v) => v,
        }
    }
}

/// Body of the deprecated `POST /getUuid`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct NameToUuidBody {
    pub account: String,
    #[serde(rename = "type")]
    pub object_type: ObjectType,
    /// Single name or array of names.
    #[serde(default)]
    pub name: Option<StringOrVec>,
}

/// Body of the deprecated `POST /getName`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct UuidToNameBody {
    /// Single uuid or array of uuids. Kept as `String` here because the body
    /// may carry either a bare string or a JSON array of strings; UUID
    /// parsing happens in the service layer so we can surface a clean 400
    /// for malformed input.
    pub uuid: StringOrVec,
}

// ------------------------------------------------------------------------
// Response types
// ------------------------------------------------------------------------

/// Response for `GET /uuids` and `POST /getUuid`.
///
/// When no `name=` was supplied, only `account` is populated. Otherwise
/// `uuids` contains a map of `name -> uuid` for each requested name.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct NameToUuidResponse {
    pub account: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uuids: Option<HashMap<String, Uuid>>,
}

/// Entry value for `GET /lookup` responses (map keyed by account uuid).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct LookupEntry {
    pub approved: bool,
    pub login: String,
}
