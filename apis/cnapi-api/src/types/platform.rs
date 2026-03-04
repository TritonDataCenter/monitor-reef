// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Platform list response.
/// The original response is a JSON object keyed by platform image name
/// with arrays of server UUIDs as values.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PlatformListResponse {
    #[serde(flatten)]
    pub platforms: serde_json::Value,
}
