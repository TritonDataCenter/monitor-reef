// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Operational types: ping, mode, loglevel, cache

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

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

/// SAPI storage backend type
///
/// Returned by GET /ping as `storType`. The value is the JavaScript
/// constructor name of the storage backend.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub enum StorageType {
    /// Local filesystem storage
    LocalStorage,
    /// Moray (remote) storage
    MorayStorage,
    /// Hybrid Moray + local storage
    MorayLocalStorage,
    /// Transitioning between storage backends
    TransitionStorage,
    /// Unknown storage type (forward compatibility)
    #[serde(other)]
    Unknown,
}

/// Response for GET /ping
///
/// Note: PingResponse uses a mix of snake_case and camelCase field names.
/// `storType` and `storAvailable` are camelCase in the wire format.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct PingResponse {
    /// Current SAPI mode
    pub mode: SapiMode,

    /// Storage backend type
    #[serde(rename = "storType")]
    pub stor_type: StorageType,

    /// Whether the storage backend is available
    #[serde(rename = "storAvailable")]
    pub stor_available: bool,
}

/// Request body for POST /mode
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetModeBody {
    /// Mode to set (only "full" is accepted)
    pub mode: SapiMode,
}

/// Response for GET /loglevel
///
/// Bunyan's `log.level()` returns an integer (e.g., 30 for "info"),
/// so `level` is a generic JSON value rather than a string.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct LogLevelResponse {
    /// Current log level (integer from Bunyan)
    pub level: Value,
}

/// Request body for POST /loglevel
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetLogLevelBody {
    /// Log level to set (string name or integer)
    pub level: Value,
}
