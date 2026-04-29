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
        transform_cloudapi_spec(&source, &dest).context("failed to transform cloudapi-api.json")?;
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
    apply_all_cloudapi_patches(&mut spec)?;
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
    patch_inject_action_request_schemas(spec)?;
    Ok(())
}

/// Inject per-action request schemas into components.schemas.
///
/// The action-dispatch endpoints (`POST /{account}/machines/{machine}`,
/// `.../images/{dataset}`, `.../volumes/{id}`, `.../disks/{id}`) use
/// `TypedBody<serde_json::Value>` at the Dropshot trait level because each
/// action expects a different body shape. That erases the per-action body
/// types from the OpenAPI spec, leaving downstream code generators
/// (oapi-codegen for Go, Progenitor for Rust) with `interface{}` /
/// `serde_json::Value` bodies.
///
/// The per-action structs are defined in `apis/cloudapi-api/src/types/` and
/// carry `JsonSchema` derives. This patch uses schemars directly to emit
/// their schemas into `components.schemas` so generators can produce named
/// types. The endpoint signatures still take a free-form body — this patch
/// only makes the shapes *nameable*, not *enforced*.
///
/// See `docs/design/action-dispatch-openapi.md`.
fn patch_inject_action_request_schemas(spec: &mut Value) -> Result<()> {
    let injected: Vec<(&'static str, Value, serde_json::Map<String, Value>)> = vec![
        // Machine action bodies.
        action_schema::<cloudapi_api::StartMachineRequest>("StartMachineRequest")?,
        action_schema::<cloudapi_api::StopMachineRequest>("StopMachineRequest")?,
        action_schema::<cloudapi_api::RebootMachineRequest>("RebootMachineRequest")?,
        action_schema::<cloudapi_api::ResizeMachineRequest>("ResizeMachineRequest")?,
        action_schema::<cloudapi_api::RenameMachineRequest>("RenameMachineRequest")?,
        action_schema::<cloudapi_api::EnableFirewallRequest>("EnableFirewallRequest")?,
        action_schema::<cloudapi_api::DisableFirewallRequest>("DisableFirewallRequest")?,
        action_schema::<cloudapi_api::EnableDeletionProtectionRequest>(
            "EnableDeletionProtectionRequest",
        )?,
        action_schema::<cloudapi_api::DisableDeletionProtectionRequest>(
            "DisableDeletionProtectionRequest",
        )?,
        // Image action bodies.
        action_schema::<cloudapi_api::UpdateImageRequest>("UpdateImageRequest")?,
        action_schema::<cloudapi_api::ExportImageRequest>("ExportImageRequest")?,
        action_schema::<cloudapi_api::CloneImageRequest>("CloneImageRequest")?,
        action_schema::<cloudapi_api::ImportImageRequest>("ImportImageRequest")?,
        // Volume / disk action bodies.
        action_schema::<cloudapi_api::UpdateVolumeRequest>("UpdateVolumeRequest")?,
        action_schema::<cloudapi_api::ResizeDiskRequest>("ResizeDiskRequest")?,
    ];

    let schemas = spec
        .pointer_mut("/components/schemas")
        .and_then(|s| s.as_object_mut())
        .ok_or_else(|| anyhow::anyhow!("components.schemas not found in spec"))?;

    for (name, schema, definitions) in injected {
        schemas.insert(name.to_string(), schema);
        // Merge any subschemas discovered while generating the root schema.
        // `insert` overwrites if the name already exists — which is fine
        // because schemars produces the same shape for a given type
        // regardless of which root generated it.
        for (def_name, def_value) in definitions {
            schemas.insert(def_name, def_value);
        }
    }

    Ok(())
}

/// Generate an OpenAPI-3 schema for the given type.
///
/// Uses `inline_subschemas = false` so that named referenced types
/// (e.g. `Tags`, `MetadataObject`, `Uuid`) emit as `$ref`s to existing
/// `#/components/schemas/` entries rather than inlined copies. That
/// keeps code-generated clients seeing the same named type as every
/// other spec consumer. Any `definitions` discovered along the way
/// are merged into `components.schemas` by the caller.
fn action_schema<T: schemars::JsonSchema>(
    name: &'static str,
) -> Result<(&'static str, Value, serde_json::Map<String, Value>)> {
    let settings = schemars::r#gen::SchemaSettings::openapi3();
    let mut generator = schemars::r#gen::SchemaGenerator::new(settings);
    let root = generator.root_schema_for::<T>();
    let schema_value = serde_json::to_value(&root.schema)
        .with_context(|| format!("failed to serialize schema for {}", name))?;
    let mut definitions = serde_json::Map::new();
    for (def_name, def_schema) in &root.definitions {
        let def_value = serde_json::to_value(def_schema)
            .with_context(|| format!("failed to serialize definition {}", def_name))?;
        definitions.insert(def_name.clone(), def_value);
    }
    Ok((name, schema_value, definitions))
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
