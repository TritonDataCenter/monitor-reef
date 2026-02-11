// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! JSON output formatting

use serde::Serialize;

/// Print a value as compact JSON (single line)
///
/// Used for `-j` flag output matching node-triton's `JSON.stringify(obj)`.
pub fn print_json<T: Serialize>(value: &T) -> anyhow::Result<()> {
    let json = serde_json::to_string(value)?;
    println!("{}", json);
    Ok(())
}

/// Print a value as pretty-printed JSON (indented)
///
/// Used as the default output for `get` subcommands, matching
/// node-triton's `JSON.stringify(obj, null, 4)`.
pub fn print_json_pretty<T: Serialize>(value: &T) -> anyhow::Result<()> {
    let json = serde_json::to_string_pretty(value)?;
    println!("{}", json);
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
