// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Image creation and management tests.
//!
//! Ported from node-triton test/integration/cli-image-create.test.js
//!
//! These tests require:
//! - `allowWriteActions: true` in test config
//! - `allowImageCreate: true` in test config
//!
//! The workflow:
//! 1. Create an origin instance
//! 2. Create an image from that instance
//! 3. Create a derived instance from the new image
//! 4. Test image share/unshare
//! 5. Test image update
//! 6. Test image tag
//! 7. Cleanup

#![allow(deprecated, clippy::expect_used)]

mod common;

use assert_cmd::Command;
use cloudapi_client::{Image, ImageState, Machine, MachineState};
use predicates::prelude::*;

use common::{
    allow_image_create, allow_write_actions, get_test_image, get_test_package, json_stream_parse,
    make_resource_name, run_triton_with_profile,
};

fn triton_cmd() -> Command {
    Command::cargo_bin("triton").expect("Failed to find triton binary")
}

/// Get test config - returns None if write actions or image create not allowed
fn get_test_config() -> Option<()> {
    if !allow_write_actions() || !allow_image_create() {
        return None;
    }
    Some(())
}

/// Helper to run triton command with profile env and assert success
fn safe_triton(args: &[&str]) -> String {
    let (stdout, stderr, success) = run_triton_with_profile(args.iter().copied());
    assert!(
        success,
        "triton command failed: args={:?}\nstdout: {}\nstderr: {}",
        args, stdout, stderr
    );
    stdout
}

/// Helper to run triton command with profile env (may fail)
fn try_triton(args: &[&str]) -> (String, String, bool) {
    run_triton_with_profile(args.iter().copied())
}

/// Test full image creation workflow
///
/// This test:
/// 1. Creates an origin instance
/// 2. Creates an image from it
/// 3. Creates a derived instance from the image
/// 4. Tests image share/unshare
/// 5. Tests image update
/// 6. Tests image tag
/// 7. Cleans up all resources
#[test]
#[ignore] // Requires API access and allowImageCreate
fn test_image_create_workflow() {
    if get_test_config().is_none() {
        eprintln!("Skipping: requires allowWriteActions and allowImageCreate in config");
        return;
    }

    let origin_alias = make_resource_name("img-origin");
    let image_name = make_resource_name("img-test");
    let image_version = "1.0.0";
    let derived_alias = make_resource_name("img-derived");

    // Clean up any pre-existing resources
    cleanup_instance(&origin_alias);
    cleanup_instance(&derived_alias);
    cleanup_image(&format!("{}@{}", image_name, image_version));

    // Setup: Find test image and package
    let origin_image_id = get_test_image().expect("Failed to get test image");
    let pkg_id = get_test_package().expect("Failed to get test package");

    // Create origin instance with a marker file (via user-script)
    let marker_file = "/triton-rust-test-marker.txt";
    let output = safe_triton(&[
        "create",
        "-wj",
        "-n",
        &origin_alias,
        "-m",
        &format!("user-script=touch {}", marker_file),
        &origin_image_id,
        &pkg_id,
    ]);

    let lines: Vec<Machine> = json_stream_parse(&output);
    assert!(
        lines.len() >= 2,
        "Expected at least 2 JSON objects in output"
    );
    let origin_inst = &lines[lines.len() - 1]; // Last line is final state
    assert_eq!(
        origin_inst.state,
        MachineState::Running,
        "Origin instance should be running"
    );

    let origin_inst_id = origin_inst.id.to_string();
    let origin_image = origin_inst.image;

    // Create image from instance
    let output = safe_triton(&[
        "image",
        "create",
        "-j",
        "-w",
        "-t",
        "testkey=testvalue",
        &origin_inst_id,
        &image_name,
        image_version,
    ]);

    let lines: Vec<Image> = json_stream_parse(&output);
    assert!(!lines.is_empty(), "Expected image output");
    let img = &lines[lines.len() - 1];
    assert_eq!(img.name, image_name, "Image name should match");
    assert_eq!(img.version, image_version, "Image version should match");
    assert!(img.public != Some(true), "Image should not be public");
    assert_eq!(
        img.state,
        Some(ImageState::Active),
        "Image should be active"
    );
    assert_eq!(
        img.origin,
        Some(origin_image),
        "Image origin should match instance's image"
    );

    let img_id = img.id.to_string();

    // Create derived instance from the new image
    let output = safe_triton(&["create", "-wj", "-n", &derived_alias, &img_id, &pkg_id]);

    let lines: Vec<Machine> = json_stream_parse(&output);
    assert!(!lines.is_empty(), "Expected instance output");
    let derived_inst = &lines[lines.len() - 1];
    assert_eq!(
        derived_inst.state,
        MachineState::Running,
        "Derived instance should be running"
    );

    let derived_inst_id = derived_inst.id.to_string();

    // Test image share
    let dummy_uuid_str = "12345678-1234-1234-1234-123456789abc";
    let dummy_uuid: cloudapi_client::Uuid =
        dummy_uuid_str.parse().expect("dummy UUID should parse");
    safe_triton(&["image", "share", &img_id, dummy_uuid_str]);

    let output = safe_triton(&["image", "get", "-j", &img_id]);
    let img: Image = serde_json::from_str(&output).expect("Failed to parse image JSON");
    assert!(
        img.acl
            .as_ref()
            .is_some_and(|acl| acl.contains(&dummy_uuid)),
        "Image ACL should contain the shared UUID"
    );

    // Test image unshare
    safe_triton(&["image", "unshare", &img_id, dummy_uuid_str]);

    let output = safe_triton(&["image", "get", "-j", &img_id]);
    let img: Image = serde_json::from_str(&output).expect("Failed to parse image JSON");
    assert!(
        img.acl
            .as_ref()
            .is_none_or(|acl| !acl.contains(&dummy_uuid)),
        "Image ACL should not contain the unshared UUID"
    );

    // Test image update
    let description = "This is a test description";
    safe_triton(&[
        "image",
        "update",
        &img_id,
        &format!("description={}", description),
    ]);

    let output = safe_triton(&["image", "get", "-j", &img_id]);
    let img: Image = serde_json::from_str(&output).expect("Failed to parse image JSON");
    assert_eq!(
        img.description.as_deref(),
        Some(description),
        "Image description should be updated"
    );

    // Test image tag
    safe_triton(&["image", "tag", &img_id, "foo=bar", "bool=true", "num=42"]);

    let output = safe_triton(&["image", "get", "-j", &img_id]);
    let img: Image = serde_json::from_str(&output).expect("Failed to parse image JSON");
    let tags = img.tags.expect("Image should have tags");
    assert!(tags.contains_key("foo"), "Image should have 'foo' tag");
    assert!(tags.contains_key("bool"), "Image should have 'bool' tag");
    assert!(tags.contains_key("num"), "Image should have 'num' tag");

    // Cleanup: Delete instances
    safe_triton(&["rm", "-f", "-w", &origin_inst_id, &derived_inst_id]);

    // Cleanup: Delete image
    safe_triton(&["image", "rm", "-f", &img_id]);
}

/// Test image creation help
#[test]
fn test_image_create_help() {
    triton_cmd()
        .args(["image", "create", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test image share help
#[test]
fn test_image_share_help() {
    triton_cmd()
        .args(["image", "share", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test image unshare help
#[test]
fn test_image_unshare_help() {
    triton_cmd()
        .args(["image", "unshare", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test image update help
#[test]
fn test_image_update_help() {
    triton_cmd()
        .args(["image", "update", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test image tag help
#[test]
fn test_image_tag_help() {
    triton_cmd()
        .args(["image", "tag", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test image delete help
#[test]
fn test_image_delete_help() {
    triton_cmd()
        .args(["image", "delete", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test image delete alias (rm)
#[test]
fn test_image_rm_alias() {
    triton_cmd()
        .args(["image", "rm", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test image wait help
#[test]
fn test_image_wait_help() {
    triton_cmd()
        .args(["image", "wait", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test image clone help
#[test]
fn test_image_clone_help() {
    triton_cmd()
        .args(["image", "clone", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test image copy help
#[test]
fn test_image_copy_help() {
    triton_cmd()
        .args(["image", "copy", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test image export help
#[test]
fn test_image_export_help() {
    triton_cmd()
        .args(["image", "export", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test image create requires arguments
#[test]
fn test_image_create_no_args() {
    triton_cmd().args(["image", "create"]).assert().failure();
}

/// Test image share requires arguments
#[test]
fn test_image_share_no_args() {
    triton_cmd().args(["image", "share"]).assert().failure();
}

/// Test image unshare requires arguments
#[test]
fn test_image_unshare_no_args() {
    triton_cmd().args(["image", "unshare"]).assert().failure();
}

/// Test image update requires arguments
#[test]
fn test_image_update_no_args() {
    triton_cmd().args(["image", "update"]).assert().failure();
}

/// Test image tag requires arguments
#[test]
fn test_image_tag_no_args() {
    triton_cmd().args(["image", "tag"]).assert().failure();
}

/// Test image wait requires arguments
#[test]
fn test_image_wait_no_args() {
    triton_cmd().args(["image", "wait"]).assert().failure();
}

// Helper functions

fn cleanup_instance(alias: &str) {
    // Try to delete existing instance, ignore errors
    let _ = try_triton(&["inst", "rm", "-f", "-w", alias]);
}

fn cleanup_image(name_version: &str) {
    // Try to delete existing image, ignore errors
    let _ = try_triton(&["image", "rm", "-f", name_version]);
}
