// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Post-generation transforms for OpenAPI specs.
//!
//! Some APIs (like cloudapi-api) need modifications to their generated specs
//! to match the actual wire format of the services they describe. Dropshot
//! generates its own Error schema, but real CloudAPI returns a different
//! error format.

use anyhow::{Context, Result};
use camino::Utf8Path;
use serde_json::Value;

/// Apply post-generation transforms, writing patched specs to a separate directory.
///
/// Reads from `generated_dir` (managed by dropshot-api-manager, used by `openapi-check`)
/// and writes patched copies to `patched_dir` (checked into git, read by client build.rs).
/// This keeps the generated specs pristine so `openapi-check` continues to work.
pub fn apply_transforms(generated_dir: &Utf8Path, patched_dir: &Utf8Path) -> Result<()> {
    let source = generated_dir.join("cloudapi-api.json");
    if source.exists() {
        std::fs::create_dir_all(patched_dir).context("failed to create patched specs directory")?;
        let dest = patched_dir.join("cloudapi-api.json");
        transform_cloudapi_error_schema(&source, &dest)
            .context("failed to transform cloudapi-api.json")?;
    }

    Ok(())
}

/// Check that patched specs are up-to-date with their generated sources.
///
/// Returns true if everything is fresh, false if any patched spec is stale.
pub fn check_transforms(generated_dir: &Utf8Path, patched_dir: &Utf8Path) -> Result<bool> {
    let source = generated_dir.join("cloudapi-api.json");
    if !source.exists() {
        return Ok(true);
    }

    let dest = patched_dir.join("cloudapi-api.json");
    if !dest.exists() {
        eprintln!(
            "Patched spec missing: {}\n  fix: run `make openapi-generate`",
            dest
        );
        return Ok(false);
    }

    // Generate what the patched file should look like
    let content = std::fs::read_to_string(&source).context("failed to read source spec")?;
    let mut spec: Value = serde_json::from_str(&content).context("failed to parse spec")?;
    patch_cloudapi_error_schema(&mut spec);
    let expected = serde_json::to_string_pretty(&spec).context("failed to serialize")?;

    let actual = std::fs::read_to_string(&dest).context("failed to read patched spec")?;

    if expected != actual {
        eprintln!(
            "Patched spec is stale: {}\n  fix: run `make openapi-generate`",
            dest
        );
        Ok(false)
    } else {
        eprintln!("  Fresh patched spec: {}", dest);
        Ok(true)
    }
}

/// Transform the Error schema in cloudapi-api.json to match CloudAPI's actual format.
///
/// Dropshot generates:
/// ```json
/// {
///   "Error": {
///     "properties": {
///       "error_code": { "type": "string" },
///       "message": { "type": "string" },
///       "request_id": { "type": "string" }
///     },
///     "required": ["message", "request_id"]
///   }
/// }
/// ```
///
/// But CloudAPI actually returns:
/// ```json
/// {
///   "code": "ResourceNotFound",
///   "message": "network not found"
/// }
/// ```
///
/// This function reads a source spec, patches the Error schema, and writes to a destination.
fn transform_cloudapi_error_schema(source: &Utf8Path, dest: &Utf8Path) -> Result<()> {
    let content = std::fs::read_to_string(source).context("failed to read spec file")?;
    let mut spec: Value = serde_json::from_str(&content).context("failed to parse spec as JSON")?;

    patch_cloudapi_error_schema(&mut spec);

    let output = serde_json::to_string_pretty(&spec).context("failed to serialize spec")?;
    std::fs::write(dest, output).context("failed to write patched spec file")?;

    eprintln!("Wrote patched CloudAPI spec to {}", dest);
    Ok(())
}

/// Patch the Error schema in a parsed OpenAPI spec to match CloudAPI's wire format.
fn patch_cloudapi_error_schema(spec: &mut Value) {
    let error_schema = spec
        .get_mut("components")
        .and_then(|c| c.get_mut("schemas"))
        .and_then(|s| s.get_mut("Error"));

    if let Some(error) = error_schema {
        *error = serde_json::json!({
            "description": "CloudAPI error response",
            "type": "object",
            "properties": {
                "code": {
                    "description": "Error code (e.g., \"InvalidCredentials\", \"ResourceNotFound\")",
                    "type": "string"
                },
                "message": {
                    "description": "Human-readable error message",
                    "type": "string"
                },
                "request_id": {
                    "description": "Request ID for tracing (optional, not always present)",
                    "type": "string"
                }
            },
            "required": ["code"]
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn dropshot_error_spec() -> Value {
        serde_json::json!({
            "openapi": "3.0.3",
            "info": { "title": "Test", "version": "1.0.0" },
            "paths": {},
            "components": {
                "schemas": {
                    "Error": {
                        "type": "object",
                        "properties": {
                            "error_code": { "type": "string" },
                            "message": { "type": "string" },
                            "request_id": { "type": "string" }
                        },
                        "required": ["message", "request_id"]
                    }
                }
            }
        })
    }

    fn write_spec(path: &Utf8Path, spec: &Value) {
        std::fs::write(path, serde_json::to_string_pretty(spec).unwrap()).unwrap();
    }

    fn read_spec(path: &Utf8Path) -> Value {
        let content = std::fs::read_to_string(path).unwrap();
        serde_json::from_str(&content).unwrap()
    }

    #[test]
    fn test_patch_cloudapi_error_schema() {
        let mut spec = dropshot_error_spec();
        patch_cloudapi_error_schema(&mut spec);

        let error = &spec["components"]["schemas"]["Error"];

        // Should have "code" field, not "error_code"
        assert!(error["properties"]["code"].is_object());
        assert!(error["properties"]["error_code"].is_null());

        // "code" should be required, not "message" and "request_id"
        let required = error["required"].as_array().unwrap();
        assert_eq!(required.len(), 1);
        assert_eq!(required[0], "code");
    }

    #[test]
    fn test_apply_transforms_writes_to_patched_dir() {
        let temp_dir = TempDir::new().unwrap();
        let root = Utf8Path::from_path(temp_dir.path()).unwrap();
        let generated = root.join("generated");
        let patched = root.join("patched");
        std::fs::create_dir_all(&generated).unwrap();

        write_spec(&generated.join("cloudapi-api.json"), &dropshot_error_spec());

        apply_transforms(&generated, &patched).unwrap();

        // Generated file should be unchanged
        let gen_error =
            &read_spec(&generated.join("cloudapi-api.json"))["components"]["schemas"]["Error"];
        assert!(gen_error["properties"]["error_code"].is_object());

        // Patched file should have the transform applied
        let patched_error =
            &read_spec(&patched.join("cloudapi-api.json"))["components"]["schemas"]["Error"];
        assert!(patched_error["properties"]["code"].is_object());
        assert!(patched_error["properties"]["error_code"].is_null());
    }

    #[test]
    fn test_check_transforms_detects_stale() {
        let temp_dir = TempDir::new().unwrap();
        let root = Utf8Path::from_path(temp_dir.path()).unwrap();
        let generated = root.join("generated");
        let patched = root.join("patched");
        std::fs::create_dir_all(&generated).unwrap();
        std::fs::create_dir_all(&patched).unwrap();

        write_spec(&generated.join("cloudapi-api.json"), &dropshot_error_spec());

        // Write a stale patched file (just copy the unpatched version)
        write_spec(&patched.join("cloudapi-api.json"), &dropshot_error_spec());

        assert!(!check_transforms(&generated, &patched).unwrap());

        // Now generate the correct patched file
        apply_transforms(&generated, &patched).unwrap();
        assert!(check_transforms(&generated, &patched).unwrap());
    }
}
