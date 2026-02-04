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

/// Apply post-generation transforms to specs in the given directory.
///
/// This should be called after `dropshot-api-manager generate` completes.
pub fn apply_transforms(openapi_dir: &Utf8Path) -> Result<()> {
    // Transform cloudapi-api.json to fix the Error schema
    let cloudapi_spec = openapi_dir.join("cloudapi-api.json");
    // arch-lint: allow(no-sync-io) reason="Build tool runs synchronously, not an async service"
    if cloudapi_spec.exists() {
        transform_cloudapi_error_schema(&cloudapi_spec)
            .context("failed to transform cloudapi-api.json")?;
    }

    Ok(())
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
/// This function patches the Error schema to match reality.
fn transform_cloudapi_error_schema(spec_path: &Utf8Path) -> Result<()> {
    // arch-lint: allow(no-sync-io) reason="Build tool runs synchronously, not an async service"
    let content = std::fs::read_to_string(spec_path).context("failed to read spec file")?;

    let mut spec: Value = serde_json::from_str(&content).context("failed to parse spec as JSON")?;

    // Navigate to components.schemas.Error
    let error_schema = spec
        .get_mut("components")
        .and_then(|c| c.get_mut("schemas"))
        .and_then(|s| s.get_mut("Error"));

    if let Some(error) = error_schema {
        // Replace with CloudAPI's actual error format
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

        // Write back with consistent formatting
        let output = serde_json::to_string_pretty(&spec).context("failed to serialize spec")?;
        // arch-lint: allow(no-sync-io) reason="Build tool runs synchronously, not an async service"
        std::fs::write(spec_path, output).context("failed to write spec file")?;

        eprintln!("Applied CloudAPI Error schema transform to {}", spec_path);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_transform_cloudapi_error_schema() {
        let temp_dir = TempDir::new().unwrap();
        let spec_path = Utf8Path::from_path(temp_dir.path())
            .unwrap()
            .join("cloudapi-api.json");

        // Write a minimal spec with Dropshot's default Error schema
        let initial_spec = serde_json::json!({
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
        });
        // arch-lint: allow(no-sync-io) reason="Test uses sync IO for simplicity"
        std::fs::write(
            &spec_path,
            serde_json::to_string_pretty(&initial_spec).unwrap(),
        )
        .unwrap();

        // Apply transform
        transform_cloudapi_error_schema(&spec_path).unwrap();

        // Verify the result
        // arch-lint: allow(no-sync-io) reason="Test uses sync IO for simplicity"
        let content = std::fs::read_to_string(&spec_path).unwrap();
        let result: Value = serde_json::from_str(&content).unwrap();

        let error = &result["components"]["schemas"]["Error"];

        // Should have "code" field, not "error_code"
        assert!(error["properties"]["code"].is_object());
        assert!(error["properties"]["error_code"].is_null());

        // "code" should be required, not "message" and "request_id"
        let required = error["required"].as_array().unwrap();
        assert_eq!(required.len(), 1);
        assert_eq!(required[0], "code");
    }
}
