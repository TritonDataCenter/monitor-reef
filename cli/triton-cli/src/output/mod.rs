// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

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

/// Display string for unknown/missing state values.
///
/// Matches the wire-format serialization of `#[serde(other)] Unknown` enum variants.
pub const UNKNOWN_DISPLAY: &str = "unknown";

/// Parse a string into a serde enum, with a user-friendly error listing valid variants.
///
/// Uses serde deserialization for parsing (matching wire-format names) and
/// `clap::ValueEnum` to enumerate valid variants for the error message.
/// Variants marked with `#[clap(skip)]` or named "unknown" are automatically
/// excluded from the list of valid values.
pub fn parse_filter_enum<T>(field_name: &str, value: &str) -> anyhow::Result<T>
where
    T: serde::de::DeserializeOwned + clap::ValueEnum,
{
    let valid_values = || -> Vec<String> {
        T::value_variants()
            .iter()
            .filter_map(|v| v.to_possible_value())
            .map(|p| p.get_name().to_string())
            .filter(|name| name != UNKNOWN_DISPLAY)
            .collect()
    };

    // Reject "unknown" even though it deserializes successfully — it's a
    // forward-compatibility catch-all, not a user-selectable value.
    if value == UNKNOWN_DISPLAY {
        anyhow::bail!(
            "Invalid {} value '{}': expected {}",
            field_name,
            value,
            valid_values().join(", ")
        );
    }

    serde_json::from_value::<T>(serde_json::Value::String(value.to_string())).map_err(|_| {
        anyhow::anyhow!(
            "Invalid {} value '{}': expected {}",
            field_name,
            value,
            valid_values().join(", ")
        )
    })
}

/// Convert a serde-serializable enum value to its wire-format string.
///
/// Uses serde_json to get the exact rename (lowercase, kebab-case, snake_case, etc.)
/// matching what the API produces on the wire.
pub fn enum_to_display<T: serde::Serialize + std::fmt::Debug>(val: &T) -> String {
    serde_json::to_value(val)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| format!("{:?}", val))
}

/// Convert an optional serde-serializable enum to its wire-format display string.
///
/// Returns [`UNKNOWN_DISPLAY`] when the value is `None`.
pub fn opt_enum_to_display<T: serde::Serialize + std::fmt::Debug>(val: Option<&T>) -> String {
    match val {
        Some(v) => enum_to_display(v),
        None => UNKNOWN_DISPLAY.to_string(),
    }
}

/// Format megabytes as human-readable size
///
/// Matches node-triton do_package/do_list.js:115-126 which uses binary
/// `* 1024 * 1024` then `humanSizeFromBytes({narrow: true})` (common.js:355-407).
/// This converts MiB to bytes, then picks the best unit from B/K/M/G/T/P using
/// `floor(log(bytes) / log(1024))`. The fractional part is **truncated** (not
/// rounded) to 1 decimal place, matching node-triton's string-slice behavior.
/// When the value has no fractional part, the `.0` is omitted.
///
/// Examples:
/// - 512 MiB -> "512M"
/// - 1024 MiB -> "1G"
/// - 1536 MiB -> "1.5G"
/// - 4096 MiB -> "4G"
/// - 25600 MiB -> "25G"
/// - 1048576 MiB -> "1T"
/// - 1638400 MiB -> "1.5T" (truncated, not rounded to 1.6T)
pub fn format_mb(mb: u64) -> String {
    let bytes = mb as f64 * 1024.0 * 1024.0;
    if bytes == 0.0 {
        return "0M".to_string();
    }
    let units = ['B', 'K', 'M', 'G', 'T', 'P'];
    let i = (bytes.ln() / 1024_f64.ln()).floor() as usize;
    let i = i.min(units.len() - 1);
    let val = bytes / 1024_f64.powi(i as i32);
    // node-triton truncates to 1 decimal place (string slice, not rounding)
    let truncated = (val * 10.0).floor() / 10.0;
    let s = format!("{:.1}", truncated);
    // In narrow mode, node-triton omits trailing ".0" for whole numbers
    let s = s.strip_suffix(".0").unwrap_or(&s);
    format!("{}{}", s, units[i])
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
    use chrono::Utc;
    format_age_since(timestamp, Utc::now())
}

fn format_age_since(timestamp: &str, now: chrono::DateTime<chrono::Utc>) -> String {
    use chrono::DateTime;

    if let Ok(created_dt) = DateTime::parse_from_rfc3339(timestamp) {
        let created_utc = created_dt.with_timezone(&chrono::Utc);
        let duration = now.signed_duration_since(created_utc);

        let seconds = duration.num_seconds();
        if seconds < 0 {
            return "-".to_string();
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_mb_megabytes() {
        assert_eq!(format_mb(512), "512M");
        assert_eq!(format_mb(256), "256M");
        assert_eq!(format_mb(1), "1M");
    }

    #[test]
    fn test_format_mb_gigabytes() {
        assert_eq!(format_mb(1024), "1G");
        assert_eq!(format_mb(2048), "2G");
        assert_eq!(format_mb(4096), "4G");
        assert_eq!(format_mb(25600), "25G");
    }

    #[test]
    fn test_format_mb_gigabytes_fractional() {
        assert_eq!(format_mb(1536), "1.5G");
        assert_eq!(format_mb(3584), "3.5G");
    }

    #[test]
    fn test_format_mb_terabytes() {
        // 1 TiB = 1024 GiB = 1048576 MiB
        assert_eq!(format_mb(1_048_576), "1T");
        // 1.5 TiB = 1536 GiB = 1572864 MiB
        assert_eq!(format_mb(1_572_864), "1.5T");
    }

    #[test]
    fn test_format_mb_truncates_not_rounds() {
        // 1638400 MiB = 1600 GiB = 1.5625 TiB
        // node-triton truncates to "1.5T", not rounds to "1.6T"
        assert_eq!(format_mb(1_638_400), "1.5T");
    }

    #[test]
    fn test_format_mb_travis_sample_package() {
        // sample-64G package: memory=65536, swap=262144, disk=1638400
        assert_eq!(format_mb(65_536), "64G");
        assert_eq!(format_mb(262_144), "256G");
        assert_eq!(format_mb(1_638_400), "1.5T");
    }

    #[test]
    fn test_format_mb_zero() {
        assert_eq!(format_mb(0), "0M");
    }

    // format_age tests using format_age_since with deterministic reference times
    mod format_age {
        use super::super::format_age_since;
        use chrono::{TimeZone, Utc};

        // Pi Day 2026: 2026-03-14T00:00:00Z
        fn pi_day_2026() -> chrono::DateTime<Utc> {
            Utc.with_ymd_and_hms(2026, 3, 14, 0, 0, 0).unwrap()
        }

        // Tau Day 2026: 2026-06-28T00:00:00Z
        fn tau_day_2026() -> chrono::DateTime<Utc> {
            Utc.with_ymd_and_hms(2026, 6, 28, 0, 0, 0).unwrap()
        }

        #[test]
        fn test_format_age_years() {
            // Pi Day 2024 is 2 years before Pi Day 2026
            let result = format_age_since("2024-03-14T00:00:00Z", pi_day_2026());
            assert_eq!(result, "2y");
        }

        #[test]
        fn test_format_age_weeks() {
            // 2 weeks before Tau Day 2026
            let result = format_age_since("2026-06-14T00:00:00Z", tau_day_2026());
            assert_eq!(result, "2w");
        }

        #[test]
        fn test_format_age_days() {
            // 3 days before Pi Day 2026
            let result = format_age_since("2026-03-11T00:00:00Z", pi_day_2026());
            assert_eq!(result, "3d");
        }

        #[test]
        fn test_format_age_hours() {
            // 5 hours before Tau Day 2026
            let result = format_age_since("2026-06-27T19:00:00Z", tau_day_2026());
            assert_eq!(result, "5h");
        }

        #[test]
        fn test_format_age_minutes() {
            // 10 minutes before Pi Day 2026
            let result = format_age_since("2026-03-13T23:50:00Z", pi_day_2026());
            assert_eq!(result, "10m");
        }

        #[test]
        fn test_format_age_seconds() {
            // 30 seconds before Tau Day 2026
            let result = format_age_since("2026-06-27T23:59:30Z", tau_day_2026());
            assert_eq!(result, "30s");
        }

        #[test]
        fn test_format_age_zero() {
            // Same instant as Pi Day 2026
            let result = format_age_since("2026-03-14T00:00:00Z", pi_day_2026());
            assert_eq!(result, "0s");
        }

        #[test]
        fn test_format_age_future() {
            // 1 hour after Pi Day 2026
            let result = format_age_since("2026-03-14T01:00:00Z", pi_day_2026());
            assert_eq!(result, "-");
        }

        #[test]
        fn test_format_age_invalid() {
            let result = format_age_since("not-a-date", pi_day_2026());
            assert_eq!(result, "-");
        }

        #[test]
        fn test_format_age_empty() {
            let result = format_age_since("", pi_day_2026());
            assert_eq!(result, "-");
        }
    }
}
