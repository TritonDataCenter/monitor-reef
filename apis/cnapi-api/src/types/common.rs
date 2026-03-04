// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// UUID type alias
pub type Uuid = uuid::Uuid;

/// Pagination parameters used across list endpoints
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub struct PaginationParams {
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub offset: Option<u32>,
}

/// Generic task response returned by endpoints that dispatch work to cn-agent
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TaskResponse {
    /// The task ID that can be used to poll for status
    pub id: String,
}

/// Ping response
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PingResponse {
    pub ready: bool,
    pub services: PingServices,
}

/// Service status in ping response
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PingServices {
    pub workflow: String,
    pub moray: String,
    pub amqp: String,
}
