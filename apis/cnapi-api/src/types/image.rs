// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::common::Uuid;

/// Path parameter for image endpoints
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ImagePath {
    pub server_uuid: Uuid,
    pub uuid: Uuid,
}

/// Image information returned by GET /servers/:server_uuid/images/:uuid
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ImageInfo {
    #[serde(default)]
    pub uuid: Option<Uuid>,
    #[serde(flatten)]
    pub extra: Option<serde_json::Value>,
}
