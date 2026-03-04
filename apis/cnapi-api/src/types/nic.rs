// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Body for PUT /servers/:server_uuid/nics
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct NicUpdateParams {
    #[serde(flatten)]
    pub payload: serde_json::Value,
}

/// Body for POST /servers/:server_uuid/nics/update
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct NicUpdateTaskParams {
    #[serde(flatten)]
    pub payload: serde_json::Value,
}
