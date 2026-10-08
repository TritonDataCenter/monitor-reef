// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Channel types for IMGAPI

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Channel configuration
///
/// Channels allow organizing images into release tracks (e.g., "release",
/// "staging", "dev"). Only available when IMGAPI is configured with channels.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Channel {
    /// Channel name (unique identifier)
    pub name: String,
    /// Human-readable description
    pub description: String,
    /// Whether this is the default channel
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<bool>,
}
