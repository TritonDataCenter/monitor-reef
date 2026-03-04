// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Image CLI tests
//!
//! Tests for `triton image` commands.
//!
//! Tests are split into:
//! - Offline tests (help, usage) - run without API access
//! - API tests (list, get) - marked with #[ignore], require config.json

// Allow deprecated - cargo_bin is standard for CLI testing
#![allow(deprecated, clippy::expect_used)]

mod common;

use assert_cmd::Command;
use cloudapi_client::Image;
use predicates::prelude::*;
use serde_json::Value;

fn triton_cmd() -> Command {
    Command::cargo_bin("triton").expect("Failed to find triton binary")
}

// =============================================================================
// Offline tests - no API access required
// =============================================================================

/// Test `triton image -h` shows help
#[test]
fn test_image_help_short() {
    triton_cmd()
        .args(["image", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"))
        .stdout(predicate::str::contains("image"));
}

/// Test `triton image --help` shows help
#[test]
fn test_image_help_long() {
    triton_cmd()
        .args(["image", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton help image` shows help
#[test]
fn test_help_image() {
    triton_cmd()
        .args(["help", "image"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"))
        .stdout(predicate::str::contains("image"));
}

/// Test `triton image list -h` shows help
#[test]
fn test_image_list_help() {
    triton_cmd()
        .args(["image", "list", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton image get -h` shows help
#[test]
fn test_image_get_help() {
    triton_cmd()
        .args(["image", "get", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton image help get` shows help
#[test]
fn test_image_help_get() {
    triton_cmd()
        .args(["image", "help", "get"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton image get` without args shows error
#[test]
fn test_image_get_no_args() {
    triton_cmd()
        .args(["image", "get"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("required"));
}

/// Test `triton img` alias for image
#[test]
fn test_img_alias() {
    triton_cmd()
        .args(["img", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton img ls` alias
#[test]
fn test_img_ls_alias() {
    triton_cmd()
        .args(["img", "ls", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton imgs` shortcut alias
#[test]
fn test_imgs_shortcut() {
    triton_cmd()
        .args(["imgs", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton image cp` alias for copy
#[test]
fn test_image_cp_alias() {
    triton_cmd()
        .args(["image", "cp", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton image create -h` shows help
#[test]
fn test_image_create_help() {
    triton_cmd()
        .args(["image", "create", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

/// Test `triton image delete -h` shows help
#[test]
fn test_image_delete_help() {
    triton_cmd()
        .args(["image", "delete", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

// =============================================================================
// API tests - require config.json with valid profile
// These tests are ignored by default and run with `make triton-test-api`
// =============================================================================

/// Run triton with profile from test config
fn triton_with_profile() -> Command {
    let mut cmd = triton_cmd();

    // Load profile environment from config
    let env_vars = common::config::get_profile_env();
    for (key, value) in env_vars {
        cmd.env(key, value);
    }

    cmd
}

/// Test `triton images` lists images (table output)
#[test]
#[ignore = "requires API access - run with make triton-test-api"]
fn test_images_list_table() {
    common::config::require_integration_config();

    let output = triton_with_profile()
        .args(["images"])
        .output()
        .expect("Failed to run command");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "Command should succeed.\nstdout: {}\nstderr: {}",
        stdout,
        stderr
    );

    // Table output should have column headers
    assert!(
        stdout.contains("SHORTID") || stdout.contains("ID") || stdout.contains("NAME"),
        "Should show ID or NAME column. Got:\n{}",
        stdout
    );
}

/// Test `triton image list` lists images
#[test]
#[ignore = "requires API access - run with make triton-test-api"]
fn test_image_list() {
    common::config::require_integration_config();

    let output = triton_with_profile()
        .args(["image", "list"])
        .output()
        .expect("Failed to run command");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "Command should succeed");
    assert!(
        stdout.contains("SHORTID") || stdout.contains("ID") || stdout.contains("NAME"),
        "Should show image columns"
    );
}

/// Test `triton images -j` returns JSON
///
/// Similar to Node.js api-images.test.js:
/// ```js
/// client.listImages(function (err, images) {
///     t.ok(Array.isArray(images), 'images');
///     t.ok(common.isUUID(img.id), 'img.id is a UUID');
///     t.ok(img.name, 'img.name');
///     t.ok(img.version, 'img.version');
/// });
/// ```
#[test]
#[ignore = "requires API access - run with make triton-test-api"]
fn test_images_json() {
    common::config::require_integration_config();

    let output = triton_with_profile()
        .args(["images", "-j"])
        .output()
        .expect("Failed to run command");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "Command should succeed.\nstdout: {}\nstderr: {}",
        stdout,
        stderr
    );

    // Parse JSON stream output
    let images: Vec<Value> = common::json_stream_parse(&stdout);

    assert!(
        !images.is_empty(),
        "Should have at least one image. Got stdout:\n{}",
        stdout
    );

    // First image should have id, name, and version fields
    let first = &images[0];
    let first_id = first["id"].as_str().expect("Image should have id field");
    common::assert_valid_uuid(first_id, "Image id");
    assert!(first["name"].is_string(), "Image should have name field");
    assert!(
        first["version"].is_string(),
        "Image should have version field"
    );
}

/// Test `triton image get ID` returns image details
///
/// Similar to Node.js api-images.test.js:
/// ```js
/// client.getImage(img.id, function (err, image) {
///     t.equal(image.id, img.id);
/// });
/// ```
#[test]
#[ignore = "requires API access - run with make triton-test-api"]
fn test_image_get_by_id() {
    common::config::require_integration_config();

    // First, get a list of images to find one to get
    let list_output = triton_with_profile()
        .args(["images", "-j"])
        .output()
        .expect("Failed to list images");

    let stdout = String::from_utf8_lossy(&list_output.stdout);
    let images: Vec<Value> = common::json_stream_parse(&stdout);

    if images.is_empty() {
        eprintln!("Skipping: no images available");
        return;
    }

    let image_id = images[0]["id"].as_str().expect("Image should have id");

    // Now get that specific image
    let get_output = triton_with_profile()
        .args(["image", "get", image_id])
        .output()
        .expect("Failed to get image");

    let get_stdout = String::from_utf8_lossy(&get_output.stdout);
    let get_stderr = String::from_utf8_lossy(&get_output.stderr);

    assert!(
        get_output.status.success(),
        "image get should succeed.\nstdout: {}\nstderr: {}",
        get_stdout,
        get_stderr
    );

    let image: Value = serde_json::from_str(&get_stdout).expect("Should return valid JSON");
    assert_eq!(
        image["id"].as_str(),
        Some(image_id),
        "Returned image should match requested ID"
    );
}

/// Test `triton image get SHORTID` returns image details
///
/// Similar to Node.js api-images.test.js:
/// ```js
/// var shortId = img.id.split('-')[0];
/// client.getImage(shortId, function (err, image) {
///     t.equal(image.id, img.id);
/// });
/// ```
#[test]
#[ignore = "requires API access - run with make triton-test-api"]
fn test_image_get_by_shortid() {
    common::config::require_integration_config();

    // First, get a list of images
    let list_output = triton_with_profile()
        .args(["images", "-j"])
        .output()
        .expect("Failed to list images");

    let stdout = String::from_utf8_lossy(&list_output.stdout);
    let images: Vec<Value> = common::json_stream_parse(&stdout);

    if images.is_empty() {
        eprintln!("Skipping: no images available");
        return;
    }

    let full_id = images[0]["id"].as_str().expect("Image should have id");
    let short_id = full_id.split('-').next().expect("ID should have parts");

    // Get by short ID
    let get_output = triton_with_profile()
        .args(["image", "get", short_id])
        .output()
        .expect("Failed to get image");

    let get_stdout = String::from_utf8_lossy(&get_output.stdout);

    assert!(
        get_output.status.success(),
        "image get by shortid should succeed"
    );

    let image: Value = serde_json::from_str(&get_stdout).expect("Should return valid JSON");
    assert_eq!(
        image["id"].as_str(),
        Some(full_id),
        "Returned image should match the full ID"
    );
}

/// Test `triton image get NAME` returns image details
///
/// Similar to Node.js api-images.test.js:
/// ```js
/// client.getImage(img.name, function (err, image) {
///     t.equal(image.name, img.name);
/// });
/// ```
#[test]
#[ignore = "requires API access - run with make triton-test-api"]
fn test_image_get_by_name() {
    common::config::require_integration_config();

    // First, get a list of images
    let list_output = triton_with_profile()
        .args(["images", "-j"])
        .output()
        .expect("Failed to list images");

    let stdout = String::from_utf8_lossy(&list_output.stdout);
    let images: Vec<Value> = common::json_stream_parse(&stdout);

    if images.is_empty() {
        eprintln!("Skipping: no images available");
        return;
    }

    let image_name = match images[0]["name"].as_str() {
        Some(name) => name,
        None => {
            eprintln!("Skipping: image has no name");
            return;
        }
    };

    // Get by name (note: may return a different version with same name)
    let get_output = triton_with_profile()
        .args(["image", "get", image_name])
        .output()
        .expect("Failed to get image");

    let get_stdout = String::from_utf8_lossy(&get_output.stdout);

    assert!(
        get_output.status.success(),
        "image get by name should succeed"
    );

    let image: Value = serde_json::from_str(&get_stdout).expect("Should return valid JSON");
    assert_eq!(
        image["name"].as_str(),
        Some(image_name),
        "Returned image should have the same name"
    );
}

// =============================================================================
// Filter verification tests - ensure list filters actually filter results
// =============================================================================

/// Test `triton image list --name <name>` filters by name
#[test]
#[ignore = "requires API access - run with make triton-test-api"]
fn test_image_list_filter_by_name() {
    use common::{json_stream_parse, safe_triton};

    common::config::require_integration_config();

    // List all images
    let stdout = safe_triton(["images", "-j"]);
    let all_images: Vec<Image> = json_stream_parse(&stdout);
    if all_images.is_empty() {
        eprintln!("Skipping: no images available");
        return;
    }

    let filter_name = &all_images[0].name;

    // List with --name filter
    let stdout = safe_triton(["images", "-j", "--name", filter_name]);
    let filtered: Vec<Image> = json_stream_parse(&stdout);

    assert!(
        filtered.len() <= all_images.len(),
        "filtered count ({}) should be <= total count ({})",
        filtered.len(),
        all_images.len()
    );
    for img in &filtered {
        assert_eq!(
            &img.name, filter_name,
            "every filtered image should have name '{}'",
            filter_name
        );
    }
}

/// Test `triton image list --type <type>` filters by type
#[test]
#[ignore = "requires API access - run with make triton-test-api"]
fn test_image_list_filter_by_type() {
    use common::{json_stream_parse, safe_triton};

    common::config::require_integration_config();

    let stdout = safe_triton(["images", "-j"]);
    let all_images: Vec<Image> = json_stream_parse(&stdout);
    if all_images.is_empty() {
        eprintln!("Skipping: no images available");
        return;
    }

    // Get the wire-format string for the first image's type
    let filter_type = &all_images[0].image_type;
    let type_str = serde_json::to_value(filter_type)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .expect("image type should serialize to a string");

    let stdout = safe_triton(["images", "-j", "--type", &type_str]);
    let filtered: Vec<Image> = json_stream_parse(&stdout);

    assert!(
        filtered.len() <= all_images.len(),
        "filtered count ({}) should be <= total count ({})",
        filtered.len(),
        all_images.len()
    );
    for img in &filtered {
        assert_eq!(
            img.image_type, *filter_type,
            "every filtered image should have type '{}'",
            type_str
        );
    }
}

/// Test `triton image list --os <os>` filters by OS
#[test]
#[ignore = "requires API access - run with make triton-test-api"]
fn test_image_list_filter_by_os() {
    use common::{json_stream_parse, safe_triton};

    common::config::require_integration_config();

    let stdout = safe_triton(["images", "-j"]);
    let all_images: Vec<Image> = json_stream_parse(&stdout);
    if all_images.is_empty() {
        eprintln!("Skipping: no images available");
        return;
    }

    let filter_os = &all_images[0].os;

    let stdout = safe_triton(["images", "-j", "--os", filter_os]);
    let filtered: Vec<Image> = json_stream_parse(&stdout);

    assert!(
        filtered.len() <= all_images.len(),
        "filtered count ({}) should be <= total count ({})",
        filtered.len(),
        all_images.len()
    );
    for img in &filtered {
        assert_eq!(
            &img.os, filter_os,
            "every filtered image should have os '{}'",
            filter_os
        );
    }
}

/// Test `triton image list --state active` filters by state
#[test]
#[ignore = "requires API access - run with make triton-test-api"]
fn test_image_list_filter_by_state() {
    use cloudapi_client::ImageState;
    use common::{json_stream_parse, safe_triton};

    common::config::require_integration_config();

    let stdout = safe_triton(["images", "-j"]);
    let all_images: Vec<Image> = json_stream_parse(&stdout);
    if all_images.is_empty() {
        eprintln!("Skipping: no images available");
        return;
    }

    let stdout = safe_triton(["images", "-j", "--state", "active"]);
    let filtered: Vec<Image> = json_stream_parse(&stdout);

    assert!(
        filtered.len() <= all_images.len(),
        "filtered count ({}) should be <= total count ({})",
        filtered.len(),
        all_images.len()
    );
    for img in &filtered {
        assert_eq!(
            img.state,
            Some(ImageState::Active),
            "every filtered image should have state 'active'"
        );
    }
}

/// Test `triton image list type=<type>` positional filter syntax
#[test]
#[ignore = "requires API access - run with make triton-test-api"]
fn test_image_list_positional_filter() {
    use common::{json_stream_parse, safe_triton};

    common::config::require_integration_config();

    let stdout = safe_triton(["images", "-j"]);
    let all_images: Vec<Image> = json_stream_parse(&stdout);
    if all_images.is_empty() {
        eprintln!("Skipping: no images available");
        return;
    }

    let filter_type = &all_images[0].image_type;
    let type_str = serde_json::to_value(filter_type)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .expect("image type should serialize to a string");
    let positional = format!("type={}", type_str);

    let stdout = safe_triton(["images", "-j", &positional]);
    let filtered: Vec<Image> = json_stream_parse(&stdout);

    assert!(
        filtered.len() <= all_images.len(),
        "filtered count ({}) should be <= total count ({})",
        filtered.len(),
        all_images.len()
    );
    for img in &filtered {
        assert_eq!(
            img.image_type, *filter_type,
            "every filtered image should have type '{}'",
            type_str
        );
    }
}
