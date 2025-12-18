// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Output formatting utilities

pub mod json;
pub mod table;

/// Output format selection
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, Default)]
pub enum OutputFormat {
    #[default]
    Table,
    Json,
}

/// Format megabytes as human-readable size (matches node-triton format)
///
/// node-triton uses `humanSizeFromBytes` with `narrow: true` and `precision: 1`.
/// This converts MB to bytes, then formats with units like G, M, K.
/// When the value is a whole number (no decimal needed), the `.0` is omitted.
///
/// Examples:
/// - 512 MB -> "512M"
/// - 1024 MB -> "1G"
/// - 1536 MB -> "1.5G"
/// - 4096 MB -> "4G"
/// - 25600 MB -> "25G"
pub fn format_mb(mb: u64) -> String {
    if mb >= 1024 {
        let gb = mb as f64 / 1024.0;
        if gb.fract() == 0.0 {
            format!("{}G", gb as u64)
        } else {
            format!("{:.1}G", gb)
        }
    } else {
        format!("{}M", mb)
    }
}
