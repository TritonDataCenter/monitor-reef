// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Test helpers for triton-cli integration tests
//!
//! This module provides utilities for testing the triton CLI, including:
//! - Running CLI commands and capturing output
//! - Parsing JSON stream output (newline-delimited JSON)
//! - Creating unique resource names for tests
//! - Loading test configuration

// Allow unused code - these helpers are infrastructure for integration tests
// Allow deprecated - cargo_bin is standard for CLI testing
#![allow(dead_code, deprecated)]

pub mod config;

use assert_cmd::Command;
use serde::de::DeserializeOwned;
use std::ffi::OsStr;

/// Get a Command for running the triton CLI binary
pub fn triton_cmd() -> Command {
    Command::cargo_bin("triton").expect("Failed to find triton binary")
}

/// Run triton with the given arguments and return (stdout, stderr)
pub fn run_triton<I, S>(args: I) -> (String, String)
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = triton_cmd()
        .args(args)
        .output()
        .expect("Failed to execute triton");

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    (stdout, stderr)
}

/// Run triton with environment variables for profile configuration
pub fn run_triton_with_env<I, S>(args: I, env: &[(&str, &str)]) -> (String, String, bool)
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut cmd = triton_cmd();
    cmd.args(args);

    for (key, value) in env {
        cmd.env(key, value);
    }

    let output = cmd.output().expect("Failed to execute triton");

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let success = output.status.success();

    (stdout, stderr, success)
}

/// Parse JSON output into a Vec
/// Handles both NDJSON (newline-delimited JSON) and regular JSON arrays
pub fn json_stream_parse<T: DeserializeOwned>(output: &str) -> Vec<T> {
    let trimmed = output.trim();

    // First, try parsing as a JSON array (Rust CLI format)
    if trimmed.starts_with('[') {
        if let Ok(items) = serde_json::from_str::<Vec<T>>(trimmed) {
            return items;
        }
    }

    // Fall back to NDJSON parsing (Node.js format)
    output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

/// Create a unique resource name for tests using the hostname
pub fn make_resource_name(prefix: &str) -> String {
    let hostname = hostname::get()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_else(|_| "unknown".to_string());
    format!("{}-{}", prefix, hostname)
}

/// Get the path to test fixtures directory
pub fn fixtures_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
}

/// Get the path to a specific fixture file
pub fn fixture_path(name: &str) -> std::path::PathBuf {
    fixtures_dir().join(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_json_stream_parse() {
        let input = r#"{"name": "test1", "id": 1}
{"name": "test2", "id": 2}
"#;
        let parsed: Vec<serde_json::Value> = json_stream_parse(input);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0]["name"], "test1");
        assert_eq!(parsed[1]["id"], 2);
    }

    #[test]
    fn test_make_resource_name() {
        let name = make_resource_name("triton-test");
        assert!(name.starts_with("triton-test-"));
    }

    #[test]
    fn test_fixtures_dir_exists() {
        let dir = fixtures_dir();
        assert!(dir.exists(), "Fixtures directory should exist at {:?}", dir);
    }

    #[test]
    fn test_fixture_path() {
        let path = fixture_path("metadata.json");
        assert!(path.exists(), "metadata.json fixture should exist");
    }
}
