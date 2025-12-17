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
    if trimmed.starts_with('[')
        && let Ok(items) = serde_json::from_str::<Vec<T>>(trimmed)
    {
        return items;
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

// =============================================================================
// Write operation test helpers
// =============================================================================

use serde::Deserialize;

/// Image info returned from `triton images -j`
#[derive(Debug, Clone, Deserialize)]
pub struct ImageInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub published_at: Option<String>,
}

/// Package info returned from `triton packages -j`
#[derive(Debug, Clone, Deserialize)]
pub struct PackageInfo {
    pub id: String,
    pub name: String,
    pub memory: u64,
}

/// Instance info returned from `triton instance create -wj`
#[derive(Debug, Clone, Deserialize)]
pub struct InstanceInfo {
    pub id: String,
    pub name: String,
    pub state: String,
    #[serde(default)]
    pub tags: std::collections::HashMap<String, serde_json::Value>,
}

/// Run triton with profile environment and return (stdout, stderr, success)
pub fn run_triton_with_profile<I, S>(args: I) -> (String, String, bool)
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let profile_env = config::get_profile_env();
    let env: Vec<(&str, &str)> = profile_env.iter().map(|(k, v)| (*k, v.as_str())).collect();
    run_triton_with_env(args, &env)
}

/// Safe triton execution - asserts success and empty stderr
/// Returns stdout on success, panics on failure
pub fn safe_triton<I, S>(args: I) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr> + std::fmt::Debug,
{
    let args_vec: Vec<S> = args.into_iter().collect();
    let args_str: Vec<String> = args_vec
        .iter()
        .map(|s| s.as_ref().to_string_lossy().to_string())
        .collect();
    let (stdout, stderr, success) = run_triton_with_profile(&args_vec);
    assert!(
        success,
        "triton command failed: args={:?}\nstdout: {}\nstderr: {}",
        args_str, stdout, stderr
    );
    // Note: Some commands may write to stderr for progress, so we don't assert empty stderr
    stdout
}

/// Find a suitable test image (base or minimal image)
/// Returns the image ID if found
pub fn get_test_image() -> Option<String> {
    let config = config::load_config()?;

    // Check if image is specified in config
    if let Some(ref image) = config.image {
        return Some(image.clone());
    }

    // List images and find a suitable one
    let (stdout, _, success) = run_triton_with_profile(["images", "-j"]);
    if !success {
        return None;
    }

    let images: Vec<ImageInfo> = json_stream_parse(&stdout);

    // Candidate image names in order of preference
    let candidates = [
        "base-64-lts",
        "base-64",
        "minimal-64-lts",
        "minimal-64",
        "base-32-lts",
        "base-32",
        "minimal-32",
        "base",
    ];

    // Find the first matching image (images are typically sorted by published_at desc)
    for candidate in &candidates {
        if let Some(img) = images.iter().find(|i| i.name == *candidate) {
            return Some(img.id.clone());
        }
    }

    // If no candidate found, return the first image if any exist
    images.first().map(|i| i.id.clone())
}

/// Find the smallest available test package (non-KVM)
/// Returns the package ID if found
pub fn get_test_package() -> Option<String> {
    let config = config::load_config()?;

    // Check if package is specified in config
    if let Some(ref package) = config.package {
        return Some(package.clone());
    }

    // List packages and find the smallest one
    let (stdout, _, success) = run_triton_with_profile(["packages", "-j"]);
    if !success {
        return None;
    }

    let mut packages: Vec<PackageInfo> = json_stream_parse(&stdout);

    // Filter out KVM packages
    packages.retain(|p| !p.name.contains("kvm"));

    // Sort by memory (smallest first)
    packages.sort_by_key(|p| p.memory);

    packages.first().map(|p| p.id.clone())
}

/// Find a package suitable for resize testing (different from the base test package)
/// Returns the package name if found
pub fn get_resize_test_package() -> Option<String> {
    let config = config::load_config()?;

    // Check if resize package is specified in config
    if let Some(ref package) = config.resize_package {
        return Some(package.clone());
    }

    // List packages and find a suitable one for resize
    let (stdout, _, success) = run_triton_with_profile(["packages", "-j"]);
    if !success {
        return None;
    }

    let mut packages: Vec<PackageInfo> = json_stream_parse(&stdout);

    // Filter out KVM packages
    packages.retain(|p| !p.name.contains("kvm"));

    // Sort by memory (smallest first)
    packages.sort_by_key(|p| p.memory);

    // Get the base test package to avoid selecting the same one
    let base_pkg_id = get_test_package()?;

    // Find a package that's different from the base package
    // Prefer the second smallest package
    packages
        .iter()
        .find(|p| p.id != base_pkg_id)
        .map(|p| p.name.clone())
}

/// Create a test instance with the given alias and optional extra flags
/// Returns the instance info on success
pub fn create_test_instance(alias: &str, extra_flags: &[&str]) -> Option<InstanceInfo> {
    let img_id = get_test_image()?;
    let pkg_id = get_test_package()?;

    let mut args = vec![
        "instance".to_string(),
        "create".to_string(),
        "-w".to_string(),
        "-j".to_string(),
        "-n".to_string(),
        alias.to_string(),
    ];

    for flag in extra_flags {
        args.push(flag.to_string());
    }

    args.push(img_id);
    args.push(pkg_id);

    let (stdout, stderr, success) = run_triton_with_profile(args.iter().map(|s| s.as_str()));
    if !success {
        eprintln!("Failed to create instance: stderr={}", stderr);
        return None;
    }

    // Parse the JSON stream output - the last line should be the final instance state
    let instances: Vec<InstanceInfo> = json_stream_parse(&stdout);
    instances.into_iter().last()
}

/// Delete a test instance by name or ID (like rm -f, doesn't error if not found)
pub fn delete_test_instance(name_or_id: &str) {
    // First check if the instance exists
    let (stdout, _, success) = run_triton_with_profile(["instance", "get", "-j", name_or_id]);

    if !success {
        // Instance doesn't exist, that's fine
        return;
    }

    // Parse to get the ID
    if let Ok(inst) = serde_json::from_str::<InstanceInfo>(&stdout) {
        // Delete with force and wait
        let _ = run_triton_with_profile(["instance", "rm", "-f", "-w", &inst.id]);
    }
}

/// Check if write actions are allowed in the test config
pub fn allow_write_actions() -> bool {
    config::load_config()
        .map(|c| c.allow_write_actions)
        .unwrap_or(false)
}

/// Get the short ID (first segment before dash) from a UUID
pub fn short_id(uuid: &str) -> String {
    uuid.split('-').next().unwrap_or(uuid).to_string()
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
