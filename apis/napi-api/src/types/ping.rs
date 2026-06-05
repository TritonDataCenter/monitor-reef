// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Ping/health check types

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Health check response
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct PingResponse {
    /// Configuration flags
    pub config: PingConfig,
    /// Whether the service is healthy overall
    pub healthy: bool,
    /// Status of backing services
    pub services: PingServices,
    /// Overall status
    pub status: PingStatus,
}

/// NAPI configuration flags reported in ping
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct PingConfig {
    /// Whether fabric networking (overlay) is enabled
    pub fabrics_enabled: bool,
    /// Whether automatic subnet allocation is enabled
    pub subnet_alloc_enabled: bool,
}

/// Status of backing services
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct PingServices {
    /// Moray (metadata store) connectivity status
    pub moray: MorayServiceStatus,
}

/// Moray service connectivity status
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MorayServiceStatus {
    Online,
    Offline,
    /// Catch-all for statuses added after this client was compiled
    #[serde(other)]
    Unknown,
}

/// Overall ping status
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub enum PingStatus {
    /// Service is healthy and ready
    OK,
    /// Service is still starting up
    #[serde(rename = "initializing")]
    Initializing,
    /// Catch-all for statuses added after this client was compiled
    #[serde(other)]
    Unknown,
}
