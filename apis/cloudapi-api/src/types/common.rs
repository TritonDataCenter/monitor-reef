// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Common types used across CloudAPI

use schemars::JsonSchema;
use serde::Deserialize;
use std::collections::HashMap;

/// UUID type
pub type Uuid = String;

/// RFC3339 timestamp
pub type Timestamp = String;

/// Key-value tags
pub type Tags = HashMap<String, String>;

/// Key-value metadata
pub type Metadata = HashMap<String, String>;

/// Role tags for RBAC
pub type RoleTags = Vec<String>;

/// Path parameter for account
#[derive(Debug, Deserialize, JsonSchema)]
pub struct AccountPath {
    /// Account login name
    pub account: String,
}

/// Path parameter for datacenter
#[derive(Debug, Deserialize, JsonSchema)]
pub struct DatacenterPath {
    /// Account login name
    pub account: String,
    /// Datacenter name
    pub dc: String,
}
