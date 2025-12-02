// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

use std::path::Path;
use std::process::Command;

#[test]
fn test_openapi_spec_generation() {
    // Get the workspace root from CARGO_MANIFEST_DIR (which points to openapi-manager/)
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir.parent().expect("Failed to get workspace root");

    // Test that OpenAPI spec can be generated via openapi-manager (trait-based approach)
    // Uses default git-based blessed comparison (origin/main)
    let output = Command::new("cargo")
        .args(["run", "-p", "openapi-manager", "--", "generate"])
        .current_dir(workspace_root)
        .output()
        .expect("Failed to generate OpenAPI spec");

    assert!(
        output.status.success(),
        "OpenAPI generation failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Verify the generated spec file exists and is valid
    let spec_path = workspace_root.join("openapi-specs/generated/bugview-api.json");
    assert!(spec_path.exists(), "OpenAPI spec file not created");

    let spec_content = std::fs::read_to_string(&spec_path).expect("Failed to read spec file");
    assert!(
        spec_content.contains("\"openapi\":"),
        "Invalid OpenAPI spec"
    );
}
