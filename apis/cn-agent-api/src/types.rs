// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Common types shared across cn-agent endpoints.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Type alias for UUIDs in API types.
///
/// The `uuid` crate's serde impl handles lowercase hyphenated serialization
/// automatically, matching the wire format used by every Triton API.
pub type Uuid = uuid::Uuid;

/// Health check response from `GET /ping`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PingResponse {
    /// Agent name (e.g., "cn-agent").
    pub name: String,
    /// Agent version.
    pub version: String,
    /// Server UUID this agent is running on.
    pub server_uuid: Uuid,
    /// Backend name (e.g., "smartos", "dummy").
    pub backend: String,
    /// Whether the agent is currently paused (not accepting new tasks).
    pub paused: bool,
}
