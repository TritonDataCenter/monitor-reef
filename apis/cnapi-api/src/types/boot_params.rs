// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Path parameter for boot param endpoints.
/// Uses String because the path can be "default" or a server UUID.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct BootParamsPath {
    pub server_uuid: String,
}

/// Boot parameters response
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct BootParams {
    #[serde(default)]
    pub platform: Option<String>,
    #[serde(default)]
    pub kernel_args: Option<serde_json::Value>,
    #[serde(default)]
    pub kernel_flags: Option<serde_json::Value>,
    #[serde(default)]
    pub boot_modules: Option<Vec<String>>,
    #[serde(flatten)]
    pub extra: Option<serde_json::Value>,
}

/// Body for POST/PUT boot params endpoints
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct BootParamsBody {
    #[serde(flatten)]
    pub payload: serde_json::Value,
}
