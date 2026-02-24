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
// Payload tests - verify field=value parsing (offline, uses --emit-payload)
// =============================================================================

/// Test `triton image update UUID name=val version=val` produces correct payload
#[test]
fn test_image_update_field_value_parsing() {
    let output = triton_cmd()
        .args([
            "--emit-payload",
            "image",
            "update",
            "00000000-0000-0000-0000-000000000001",
            "name=new-name",
            "version=2.0.0",
        ])
        .output()
        .expect("Failed to run command");

    assert!(output.status.success(), "command should succeed");
    let stdout = String::from_utf8_lossy(&output.stdout);

    // Parse the JSON envelopes (there may be a GET + POST)
    let envelopes: Vec<Value> = stdout
        .lines()
        .collect::<Vec<_>>()
        .join("\n")
        .split("\n}\n")
        .filter_map(|chunk| {
            let trimmed = chunk.trim();
            if trimmed.is_empty() {
                return None;
            }
            let json_str = if trimmed.ends_with('}') {
                trimmed.to_string()
            } else {
                format!("{trimmed}\n}}")
            };
            serde_json::from_str(&json_str).ok()
        })
        .collect();

    // Find the POST envelope (the actual update)
    let post = envelopes
        .iter()
        .find(|e| e["method"] == "POST")
        .expect("should have a POST envelope");

    assert_eq!(post["body"]["name"], "new-name");
    assert_eq!(post["body"]["version"], "2.0.0");
}

/// Test that --flag values take precedence over positional field=value
#[test]
fn test_image_update_flag_precedence() {
    let output = triton_cmd()
        .args([
            "--emit-payload",
            "image",
            "update",
            "00000000-0000-0000-0000-000000000001",
            "--name",
            "from-flag",
            "name=from-positional",
        ])
        .output()
        .expect("Failed to run command");

    assert!(output.status.success(), "command should succeed");
    let stdout = String::from_utf8_lossy(&output.stdout);

    let envelopes: Vec<Value> = stdout
        .lines()
        .collect::<Vec<_>>()
        .join("\n")
        .split("\n}\n")
        .filter_map(|chunk| {
            let trimmed = chunk.trim();
            if trimmed.is_empty() {
                return None;
            }
            let json_str = if trimmed.ends_with('}') {
                trimmed.to_string()
            } else {
                format!("{trimmed}\n}}")
            };
            serde_json::from_str(&json_str).ok()
        })
        .collect();

    let post = envelopes
        .iter()
        .find(|e| e["method"] == "POST")
        .expect("should have a POST envelope");

    // --name flag should win over name=from-positional
    assert_eq!(post["body"]["name"], "from-flag");
}

/// Test that unknown fields produce an error
#[test]
fn test_image_update_unknown_field() {
    triton_cmd()
        .args([
            "--emit-payload",
            "image",
            "update",
            "00000000-0000-0000-0000-000000000001",
            "badfield=value",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown field"));
}

/// Test `triton image delete UUID -f` emits only a DELETE (no preceding GET)
#[test]
fn test_image_delete_payload() {
    let output = triton_cmd()
        .args([
            "--emit-payload",
            "image",
            "delete",
            "00000000-0000-0000-0000-000000000001",
            "-f",
        ])
        .output()
        .expect("Failed to run command");

    assert!(output.status.success(), "command should succeed");
    let stdout = String::from_utf8_lossy(&output.stdout);

    let envelopes: Vec<Value> = stdout
        .lines()
        .collect::<Vec<_>>()
        .join("\n")
        .split("\n}\n")
        .filter_map(|chunk| {
            let trimmed = chunk.trim();
            if trimmed.is_empty() {
                return None;
            }
            let json_str = if trimmed.ends_with('}') {
                trimmed.to_string()
            } else {
                format!("{trimmed}\n}}")
            };
            serde_json::from_str(&json_str).ok()
        })
        .collect();

    // Should have a DELETE envelope
    let delete = envelopes
        .iter()
        .find(|e| e["method"] == "DELETE")
        .expect("should have a DELETE envelope");

    let path = delete["path"].as_str().expect("path should be a string");
    assert!(
        path.contains("/images/00000000-0000-0000-0000-000000000001"),
        "DELETE path should be for the image, got: {path}"
    );

    // Should NOT have a GET envelope (no verification before delete)
    let get = envelopes.iter().find(|e| e["method"] == "GET");
    assert!(
        get.is_none(),
        "should NOT have a GET envelope before delete, but found: {:?}",
        get
    );
}

/// Test `triton image export UUID /manta/path` accepts positional manta path
/// and serializes manta_path as snake_case in the wire format
#[test]
fn test_image_export_positional_manta_path() {
    let output = triton_cmd()
        .args([
            "--emit-payload",
            "image",
            "export",
            "00000000-0000-0000-0000-000000000001",
            "/user/stor/export",
        ])
        .output()
        .expect("Failed to run command");

    assert!(output.status.success(), "command should succeed");
    let stdout = String::from_utf8_lossy(&output.stdout);

    let envelopes: Vec<Value> = stdout
        .lines()
        .collect::<Vec<_>>()
        .join("\n")
        .split("\n}\n")
        .filter_map(|chunk| {
            let trimmed = chunk.trim();
            if trimmed.is_empty() {
                return None;
            }
            let json_str = if trimmed.ends_with('}') {
                trimmed.to_string()
            } else {
                format!("{trimmed}\n}}")
            };
            serde_json::from_str(&json_str).ok()
        })
        .collect();

    let post = envelopes
        .iter()
        .find(|e| e["method"] == "POST")
        .expect("should have a POST envelope");

    // Verify manta_path is snake_case (not camelCase "mantaPath")
    assert_eq!(
        post["body"]["manta_path"], "/user/stor/export",
        "body should contain 'manta_path' (snake_case), got: {}",
        post["body"]
    );
    assert!(
        post["body"]["mantaPath"].is_null(),
        "body should NOT contain 'mantaPath' (camelCase)"
    );
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
    if !common::config::has_integration_config() {
        eprintln!("Skipping: no test config found");
        return;
    }

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
    if !common::config::has_integration_config() {
        eprintln!("Skipping: no test config found");
        return;
    }

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
    if !common::config::has_integration_config() {
        eprintln!("Skipping: no test config found");
        return;
    }

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
    assert!(
        first_id.contains('-'),
        "Image id should be a UUID: {}",
        first_id
    );
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
    if !common::config::has_integration_config() {
        eprintln!("Skipping: no test config found");
        return;
    }

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
    if !common::config::has_integration_config() {
        eprintln!("Skipping: no test config found");
        return;
    }

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
    if !common::config::has_integration_config() {
        eprintln!("Skipping: no test config found");
        return;
    }

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
