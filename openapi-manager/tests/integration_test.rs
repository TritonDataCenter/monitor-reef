// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

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
    let spec_path = workspace_root.join("openapi-specs/generated/cloudapi-api.json");
    assert!(spec_path.exists(), "OpenAPI spec file not created");

    let spec_content = std::fs::read_to_string(&spec_path).expect("Failed to read spec file");
    assert!(
        spec_content.contains("\"openapi\":"),
        "Invalid OpenAPI spec"
    );

    // Verify the transform pipeline produced a patched spec
    let patched_path = workspace_root.join("openapi-specs/patched/cloudapi-api.json");
    assert!(
        patched_path.exists(),
        "Patched spec not created — transform pipeline did not run"
    );

    let patched_content: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&patched_path).expect("Failed to read patched spec"),
    )
    .expect("Patched spec is not valid JSON");

    // The transform should have replaced "error_code" with "code" in the Error schema
    let error_schema = &patched_content["components"]["schemas"]["Error"];
    assert!(
        error_schema["properties"]["code"].is_object(),
        "Transform not applied: Error schema should have 'code' property, got: {}",
        serde_json::to_string_pretty(error_schema).unwrap_or_default()
    );
    assert!(
        error_schema["properties"]["error_code"].is_null(),
        "Transform not applied: Error schema should not have 'error_code' property"
    );

    // "code" should be the only required field
    let required = error_schema["required"]
        .as_array()
        .expect("Error schema should have 'required' array");
    assert_eq!(
        required.len(),
        1,
        "Error schema should have exactly one required field"
    );
    assert_eq!(required[0], "code", "Only required field should be 'code'");

    // The generated (unpatched) spec should still have the original Dropshot error schema
    let generated_content: serde_json::Value =
        serde_json::from_str(&spec_content).expect("Generated spec is not valid JSON");
    let gen_error = &generated_content["components"]["schemas"]["Error"];
    assert!(
        gen_error["properties"]["error_code"].is_object(),
        "Generated spec should retain original Dropshot 'error_code' property"
    );
}
