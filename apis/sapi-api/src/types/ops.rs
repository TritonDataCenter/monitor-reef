// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Operational types: ping, mode, loglevel, cache

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// SAPI operating mode
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SapiMode {
    /// Prototype mode (initial setup, limited functionality)
    Proto,
    /// Full mode (normal operation)
    Full,
    /// Unknown mode (forward compatibility)
    #[serde(other)]
    Unknown,
}

/// Response for GET /ping
///
/// Note: PingResponse uses a mix of snake_case and camelCase field names.
/// `storType` and `storAvailable` are camelCase in the wire format.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct PingResponse {
    /// Current SAPI mode ("proto" or "full")
    pub mode: String,

    /// Storage backend type (e.g., "MorayLocalStorage")
    #[serde(rename = "storType")]
    pub stor_type: String,

    /// Whether the storage backend is available
    #[serde(rename = "storAvailable")]
    pub stor_available: bool,
}

/// Response for GET /mode
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ModeResponse {
    /// Current SAPI mode
    pub mode: SapiMode,
}

/// Request body for POST /mode
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetModeBody {
    /// Mode to set (only "full" is accepted)
    pub mode: SapiMode,
}

/// Response for GET /loglevel
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct LogLevelResponse {
    /// Current log level
    pub level: String,
}

/// Request body for POST /loglevel
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetLogLevelBody {
    /// Log level to set
    pub level: String,
}
