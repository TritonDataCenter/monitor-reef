// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Ping response types for IMGAPI

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Query parameters for the ping endpoint
#[derive(Debug, Deserialize, JsonSchema)]
pub struct PingQuery {
    /// If set, triggers an error response for testing (e.g., "true")
    #[serde(default)]
    pub error: Option<String>,
}

/// Ping response
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct PingResponse {
    /// Always "pong"
    pub ping: String,
    /// IMGAPI version
    pub version: String,
    /// Always true (identifies this as an IMGAPI instance)
    pub imgapi: bool,
    /// Process ID (only for authenticated/datacenter requests)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u64>,
    /// Running user (only for authenticated requests)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
}
