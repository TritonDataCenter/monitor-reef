// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! JSON output formatting

use serde::Serialize;
use serde_json::ser::{PrettyFormatter, Serializer};

/// Print a value as compact JSON (single line)
///
/// Used for `-j` flag output matching node-triton's `JSON.stringify(obj)`.
pub fn print_json<T: Serialize>(value: &T) -> anyhow::Result<()> {
    let json = serde_json::to_string(value)?;
    println!("{}", json);
    Ok(())
}

/// Serialize a value as pretty-printed JSON with 4-space indent.
///
/// Matches node-triton's `JSON.stringify(obj, null, 4)`.
pub fn to_json_pretty<T: Serialize>(value: &T) -> anyhow::Result<String> {
    let mut buf = Vec::new();
    let formatter = PrettyFormatter::with_indent(b"    ");
    let mut ser = Serializer::with_formatter(&mut buf, formatter);
    value.serialize(&mut ser)?;
    Ok(String::from_utf8(buf)?)
}

/// Print a value as pretty-printed JSON (4-space indent)
///
/// Used as the default output for `get` subcommands, matching
/// node-triton's `JSON.stringify(obj, null, 4)`.
pub fn print_json_pretty<T: Serialize>(value: &T) -> anyhow::Result<()> {
    println!("{}", to_json_pretty(value)?);
    Ok(())
}

/// Print a slice as newline-delimited JSON (NDJSON)
///
/// This matches node-triton's jsonStream() output format where each
/// element is printed on a separate line as compact JSON.
pub fn print_json_stream<T: Serialize>(items: &[T]) -> anyhow::Result<()> {
    for item in items {
        let json = serde_json::to_string(item)?;
        println!("{}", json);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_to_json_pretty_uses_4_space_indent() {
        let value = json!({"name": "test", "nested": {"key": "value"}});
        let output = to_json_pretty(&value).unwrap();
        // Verify 4-space indent (not 2-space)
        assert!(
            output.contains("    \"name\""),
            "expected 4-space indent, got:\n{}",
            output
        );
        assert!(
            output.contains("        \"key\""),
            "expected 8-space indent for nested, got:\n{}",
            output
        );
        // Make sure it's NOT 2-space
        assert!(
            !output.contains("  \"name\":\n"),
            "should not use 2-space indent"
        );
    }
}
