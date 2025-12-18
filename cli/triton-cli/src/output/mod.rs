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

/// Format a timestamp as human-readable age (matches node-triton format)
///
/// node-triton uses years > weeks > days > hours > minutes > seconds
/// with single letter suffixes (y, w, d, h, m, s)
///
/// Examples:
/// - 365+ days ago -> "1y"
/// - 7+ days ago -> "2w"
/// - 1+ days ago -> "3d"
/// - 1+ hours ago -> "4h"
/// - 1+ minutes ago -> "5m"
/// - recent -> "30s"
pub fn format_age(timestamp: &str) -> String {
    use chrono::{DateTime, Utc};

    if let Ok(created_dt) = DateTime::parse_from_rfc3339(timestamp) {
        let now = Utc::now();
        let created_utc = created_dt.with_timezone(&Utc);
        let duration = now.signed_duration_since(created_utc);

        let seconds = duration.num_seconds();
        let years = seconds / 60 / 60 / 24 / 365;
        if years > 0 {
            return format!("{}y", years);
        }

        let weeks = seconds / 60 / 60 / 24 / 7;
        if weeks > 0 {
            return format!("{}w", weeks);
        }

        let days = seconds / 60 / 60 / 24;
        if days > 0 {
            return format!("{}d", days);
        }

        let hours = seconds / 60 / 60;
        if hours > 0 {
            return format!("{}h", hours);
        }

        let minutes = seconds / 60;
        if minutes > 0 {
            return format!("{}m", minutes);
        }

        if seconds > 0 {
            return format!("{}s", seconds);
        }

        "0s".to_string()
    } else {
        "-".to_string()
    }
}
