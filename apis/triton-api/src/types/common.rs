// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Common types used across Triton API

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Ping response
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PingResponse {
    /// Ping status (e.g., "OK")
    pub status: String,
    /// Health check status
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub healthy: Option<bool>,
}
