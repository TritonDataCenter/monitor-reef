// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Management endpoint types

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Response from the garbage collection endpoint (GET /manage/gc)
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct GcResponse {
    /// Memory usage before GC
    pub start: MemoryUsage,
    /// Memory usage after GC
    pub end: MemoryUsage,
}

/// Node.js process.memoryUsage() snapshot
///
/// Note: `heapTotal` and `heapUsed` use camelCase because they come from
/// Node.js `process.memoryUsage()` which returns camelCase field names.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct MemoryUsage {
    /// Resident set size in bytes
    pub rss: u64,
    /// Total heap size in bytes
    #[serde(rename = "heapTotal")]
    pub heap_total: u64,
    /// Used heap size in bytes
    #[serde(rename = "heapUsed")]
    pub heap_used: u64,
    /// Timestamp (milliseconds)
    pub time: u64,
}
