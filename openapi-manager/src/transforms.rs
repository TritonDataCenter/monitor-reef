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
    std::fs::create_dir_all(patched_dir).context("failed to create patched specs directory")?;

    let cloudapi_source = generated_dir.join("cloudapi-api.json");
    if cloudapi_source.exists() {
        let dest = patched_dir.join("cloudapi-api.json");
        transform_cloudapi_spec(&cloudapi_source, &dest)
            .context("failed to transform cloudapi-api.json")?;
    }

    let sapi_source = generated_dir.join("sapi-api.json");
    if sapi_source.exists() {
        let dest = patched_dir.join("sapi-api.json");
        transform_sapi_spec(&sapi_source, &dest).context("failed to transform sapi-api.json")?;
    }

    Ok(())
}

/// Check that patched specs are up-to-date with their generated sources.
///
/// Returns true if everything is fresh, false if any patched spec is stale.
pub fn check_transforms(generated_dir: &Utf8Path, patched_dir: &Utf8Path) -> Result<bool> {
    let mut all_fresh = true;

    all_fresh &= check_one_transform(
        generated_dir,
        patched_dir,
        "cloudapi-api.json",
        apply_all_cloudapi_patches,
    )?;

    all_fresh &= check_one_transform(
        generated_dir,
        patched_dir,
        "sapi-api.json",
        apply_all_sapi_patches,
    )?;

    Ok(all_fresh)
}

fn check_one_transform(
    generated_dir: &Utf8Path,
    patched_dir: &Utf8Path,
    filename: &str,
    apply_patches: fn(&mut Value) -> Result<()>,
) -> Result<bool> {
    let source = generated_dir.join(filename);
    if !source.exists() {
        return Ok(true);
    }

    let dest = patched_dir.join(filename);
    if !dest.exists() {
        eprintln!(
            "Patched spec missing: {}\n  fix: run `make openapi-generate`",
            dest
        );
        return Ok(false);
    }

    let content = std::fs::read_to_string(&source).context("failed to read source spec")?;
    let mut spec: Value = serde_json::from_str(&content).context("failed to parse spec")?;
    apply_patches(&mut spec)?;
    let expected = serde_json::to_string_pretty(&spec).context("failed to serialize")?;

    let actual = std::fs::read_to_string(&dest).context("failed to read patched spec")?;

    if expected.trim_end() != actual.trim_end() {
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
/// This function reads a source spec, applies all patches, and writes to a destination.
fn transform_cloudapi_spec(source: &Utf8Path, dest: &Utf8Path) -> Result<()> {
    let content = std::fs::read_to_string(source).context("failed to read spec file")?;
    let mut spec: Value = serde_json::from_str(&content).context("failed to parse spec as JSON")?;

    apply_all_cloudapi_patches(&mut spec)?;

    let output = serde_json::to_string_pretty(&spec).context("failed to serialize spec")?;
    std::fs::write(dest, output).context("failed to write patched spec file")?;

    eprintln!("Wrote patched CloudAPI spec to {}", dest);
    Ok(())
}

/// Apply all CloudAPI patches to a parsed spec.
fn apply_all_cloudapi_patches(spec: &mut Value) -> Result<()> {
    patch_cloudapi_error_schema(spec)?;
    patch_empty_202_responses(spec)?;
    Ok(())
}

/// Patch the Error schema in a parsed OpenAPI spec to match CloudAPI's wire format.
fn patch_cloudapi_error_schema(spec: &mut Value) -> Result<()> {
    let error_schema = spec
        .get_mut("components")
        .and_then(|c| c.get_mut("schemas"))
        .and_then(|s| s.get_mut("Error"));

    let error = error_schema.ok_or_else(|| {
        anyhow::anyhow!("Error schema not found in spec; expected components.schemas.Error")
    })?;

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

    Ok(())
}

/// Remove `content` from 202 responses that should have empty bodies.
///
/// Dropshot generates a 202 response with `application/json` content for
/// `HttpResponseAccepted<()>`, but the real CloudAPI returns 202 with an
/// empty body. Progenitor's `ResponseValue::from_response` tries to parse
/// the body as JSON, which fails on empty input. Removing the `content`
/// field makes Progenitor generate code that doesn't attempt JSON parsing.
fn patch_empty_202_responses(spec: &mut Value) -> Result<()> {
    let endpoints: &[(&str, &str)] = &[
        ("/{account}/machines/{machine}", "post"),
        ("/{account}/machines/{machine}/snapshots/{name}", "post"),
    ];

    let paths = spec
        .get_mut("paths")
        .ok_or_else(|| anyhow::anyhow!("paths not found in spec"))?;

    for (path, method) in endpoints {
        let response = paths
            .get_mut(*path)
            .and_then(|p| p.get_mut(*method))
            .and_then(|m| m.get_mut("responses"))
            .and_then(|r| r.get_mut("202"));

        if let Some(resp) = response
            && let Some(obj) = resp.as_object_mut()
        {
            obj.remove("content");
        }
    }

    Ok(())
}

// --- SAPI transforms ---

/// Transform sapi-api.json to match Node.js SAPI's actual wire format.
///
/// Three differences from Dropshot-generated spec:
/// - GET /mode returns a bare string ("proto"/"full"), not a JSON object
/// - POST /mode returns 204 with no body, not 200 with JSON
/// - POST /loglevel returns an empty 200, not a JSON body
fn transform_sapi_spec(source: &Utf8Path, dest: &Utf8Path) -> Result<()> {
    let content = std::fs::read_to_string(source).context("failed to read spec file")?;
    let mut spec: Value = serde_json::from_str(&content).context("failed to parse spec as JSON")?;

    apply_all_sapi_patches(&mut spec)?;

    let output = serde_json::to_string_pretty(&spec).context("failed to serialize spec")?;
    std::fs::write(dest, output).context("failed to write patched spec file")?;

    eprintln!("Wrote patched SAPI spec to {}", dest);
    Ok(())
}

fn apply_all_sapi_patches(spec: &mut Value) -> Result<()> {
    patch_sapi_get_mode(spec)?;
    patch_sapi_post_mode(spec)?;
    patch_sapi_post_loglevel(spec)?;
    Ok(())
}

/// GET /mode returns a bare string ("proto" or "full"), not a ModeResponse JSON object.
///
/// We keep the content-type as application/json with a plain string schema so that
/// Progenitor generates `ResponseValue<String>` (easy to use in CLIs) rather than
/// `ResponseValue<ByteStream>` (which requires stream collection).
fn patch_sapi_get_mode(spec: &mut Value) -> Result<()> {
    let response_content = spec
        .get_mut("paths")
        .and_then(|p| p.get_mut("/mode"))
        .and_then(|p| p.get_mut("get"))
        .and_then(|m| m.get_mut("responses"))
        .and_then(|r| r.get_mut("200"))
        .and_then(|r| r.get_mut("content"))
        .and_then(|c| c.get_mut("application/json"))
        .and_then(|aj| aj.get_mut("schema"))
        .ok_or_else(|| anyhow::anyhow!("GET /mode 200 response schema not found"))?;

    // Replace ModeResponse ref with a plain string
    *response_content = serde_json::json!({
        "type": "string"
    });

    Ok(())
}

/// POST /mode returns 204 with no body, not 200 with JSON.
fn patch_sapi_post_mode(spec: &mut Value) -> Result<()> {
    let responses = spec
        .get_mut("paths")
        .and_then(|p| p.get_mut("/mode"))
        .and_then(|p| p.get_mut("post"))
        .and_then(|m| m.get_mut("responses"))
        .ok_or_else(|| anyhow::anyhow!("POST /mode responses not found"))?;

    let responses_obj = responses
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("POST /mode responses is not an object"))?;

    // Remove 200, add 204 with no content
    responses_obj.remove("200");
    responses_obj.insert(
        "204".to_string(),
        serde_json::json!({
            "description": "successful operation"
        }),
    );

    Ok(())
}

/// POST /loglevel returns an empty 200, not a JSON body.
fn patch_sapi_post_loglevel(spec: &mut Value) -> Result<()> {
    let response = spec
        .get_mut("paths")
        .and_then(|p| p.get_mut("/loglevel"))
        .and_then(|p| p.get_mut("post"))
        .and_then(|m| m.get_mut("responses"))
        .and_then(|r| r.get_mut("200"));

    if let Some(resp) = response
        && let Some(obj) = resp.as_object_mut()
    {
        obj.remove("content");
    }

    Ok(())
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
        patch_cloudapi_error_schema(&mut spec).unwrap();

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
    fn test_patch_errors_on_missing_error_schema() {
        let mut spec = serde_json::json!({
            "openapi": "3.0.3",
            "info": { "title": "Test", "version": "1.0.0" },
            "paths": {},
            "components": {
                "schemas": {}
            }
        });
        let result = patch_cloudapi_error_schema(&mut spec);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Error schema not found"),
        );
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
