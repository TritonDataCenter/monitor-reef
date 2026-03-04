// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::common::Uuid;

/// Path parameter for dataset endpoints
#[derive(Debug, Deserialize, JsonSchema)]
pub struct DatasetPath {
    pub server_uuid: Uuid,
    pub dataset: String,
}

/// Body for POST /servers/:server_uuid/datasets (create)
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DatasetCreateParams {
    pub name: String,
    #[serde(rename = "type", default)]
    pub dataset_type: Option<String>,
    #[serde(default)]
    pub properties: Option<serde_json::Value>,
}

/// Body for POST /servers/:server_uuid/datasets/:dataset/properties
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DatasetPropertiesSetParams {
    #[serde(flatten)]
    pub properties: serde_json::Value,
}
