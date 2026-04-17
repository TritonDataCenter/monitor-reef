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

    let imgapi_source = generated_dir.join("imgapi-api.json");
    if imgapi_source.exists() {
        let dest = patched_dir.join("imgapi-api.json");
        transform_imgapi_spec(&imgapi_source, &dest)
            .context("failed to transform imgapi-api.json")?;
    }

    // NAPI, PAPI, VMAPI all need the same error schema patch
    for api_name in ["napi-api", "papi-api", "vmapi-api"] {
        let source = generated_dir.join(format!("{api_name}.json"));
        if source.exists() {
            let dest = patched_dir.join(format!("{api_name}.json"));
            transform_with_error_patch(api_name, &source, &dest)
                .with_context(|| format!("failed to transform {api_name}.json"))?;
        }
    }

    let mahi_source = generated_dir.join("mahi-api.json");
    if mahi_source.exists() {
        let dest = patched_dir.join("mahi-api.json");
        transform_mahi_spec(&mahi_source, &dest).context("failed to transform mahi-api.json")?;
    }

    let mahi_sitter_source = generated_dir.join("mahi-sitter-api.json");
    if mahi_sitter_source.exists() {
        let dest = patched_dir.join("mahi-sitter-api.json");
        transform_mahi_sitter_spec(&mahi_sitter_source, &dest)
            .context("failed to transform mahi-sitter-api.json")?;
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

    all_fresh &= check_one_transform(
        generated_dir,
        patched_dir,
        "imgapi-api.json",
        apply_all_imgapi_patches,
    )?;

    for api_name in ["napi-api", "papi-api", "vmapi-api"] {
        all_fresh &= check_one_transform(
            generated_dir,
            patched_dir,
            &format!("{api_name}.json"),
            patch_node_triton_error_schema,
        )?;
    }

    all_fresh &= check_one_transform(
        generated_dir,
        patched_dir,
        "mahi-api.json",
        apply_all_mahi_patches,
    )?;

    all_fresh &= check_one_transform(
        generated_dir,
        patched_dir,
        "mahi-sitter-api.json",
        apply_all_mahi_sitter_patches,
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
    patch_node_triton_error_schema(spec)?;
    patch_empty_202_responses(spec)?;
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
    patch_node_triton_error_schema(spec)?;
    patch_sapi_get_mode(spec)?;
    patch_sapi_post_mode(spec)?;
    patch_sapi_post_loglevel(spec)?;
    patch_sapi_ping_500(spec)?;
    patch_sapi_create_status_codes(spec)?;
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

    // Replace SapiMode ref with a plain string
    *response_content = serde_json::json!({
        "type": "string"
    });

    Ok(())
}

/// POST /mode returns 204 with no body.
///
/// The trait now uses HttpResponseUpdatedNoContent (204) directly, so this
/// patch is a no-op. Kept for documentation and as a safety net.
fn patch_sapi_post_mode(_spec: &mut Value) -> Result<()> {
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

/// GET /ping: Node.js returns PingResponse with status 500 when storage is
/// unavailable (not an Error body). Progenitor can't handle multiple response
/// types for the same endpoint, so we leave the 5XX Error reference in place.
/// The client will receive an error on 500 — callers needing the PingResponse
/// body on failure should use the raw HTTP client directly.
///
/// This is a known limitation documented in the validation report.
fn patch_sapi_ping_500(_spec: &mut Value) -> Result<()> {
    // No-op: Progenitor doesn't support multiple success body types.
    // Keeping this function as documentation of the known discrepancy.
    Ok(())
}

/// Create endpoints return 200 in Node.js SAPI (Restify default), not 201.
///
/// The Rust trait uses HttpResponseOk (200) already, but this patch ensures the
/// OpenAPI spec doesn't have any leftover 201 status codes from prior generations.
fn patch_sapi_create_status_codes(spec: &mut Value) -> Result<()> {
    let create_endpoints: &[(&str, &str)] = &[
        ("/applications", "post"),
        ("/services", "post"),
        ("/instances", "post"),
        ("/manifests", "post"),
    ];

    let paths = spec
        .get_mut("paths")
        .ok_or_else(|| anyhow::anyhow!("paths not found in spec"))?;

    for (path, method) in create_endpoints {
        let responses = paths
            .get_mut(*path)
            .and_then(|p| p.get_mut(*method))
            .and_then(|m| m.get_mut("responses"));

        if let Some(resp_obj) = responses
            && let Some(obj) = resp_obj.as_object_mut()
        {
            // If there's a 201, move its content to 200
            if let Some(created) = obj.remove("201") {
                obj.insert("200".to_string(), created);
            }
        }
    }

    Ok(())
}

// --- Shared Node.js Triton error schema patch ---

/// Patch the Error schema to match Node.js Triton services' actual error format.
///
/// All Node.js Triton services (IMGAPI, NAPI, PAPI, VMAPI, etc.) return errors as
/// `{"code": "ResourceNotFound", "message": "..."}`. Dropshot generates an Error
/// schema with `error_code` and a required `request_id` which these services don't
/// include. This patch makes `request_id` optional and uses `code` instead.
fn patch_node_triton_error_schema(spec: &mut Value) -> Result<()> {
    let error_schema = spec
        .get_mut("components")
        .and_then(|c| c.get_mut("schemas"))
        .and_then(|s| s.get_mut("Error"));

    let error = error_schema.ok_or_else(|| {
        anyhow::anyhow!("Error schema not found in spec; expected components.schemas.Error")
    })?;

    *error = serde_json::json!({
        "description": "Error response from a Node.js Triton service",
        "type": "object",
        "properties": {
            "code": {
                "description": "Error code (e.g., \"ResourceNotFound\", \"InvalidArgument\")",
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

/// Transform a spec that only needs the Node.js error schema patch.
fn transform_with_error_patch(api_name: &str, source: &Utf8Path, dest: &Utf8Path) -> Result<()> {
    let content = std::fs::read_to_string(source).context("failed to read spec file")?;
    let mut spec: Value = serde_json::from_str(&content).context("failed to parse spec as JSON")?;

    patch_node_triton_error_schema(&mut spec)?;

    let output = serde_json::to_string_pretty(&spec).context("failed to serialize spec")?;
    std::fs::write(dest, output).context("failed to write patched spec file")?;

    eprintln!("Wrote patched {} spec to {}", api_name, dest);
    Ok(())
}

// --- IMGAPI transforms ---

/// Transform imgapi-api.json — currently only needs the error schema patch.
fn transform_imgapi_spec(source: &Utf8Path, dest: &Utf8Path) -> Result<()> {
    let content = std::fs::read_to_string(source).context("failed to read spec file")?;
    let mut spec: Value = serde_json::from_str(&content).context("failed to parse spec as JSON")?;

    apply_all_imgapi_patches(&mut spec)?;

    let output = serde_json::to_string_pretty(&spec).context("failed to serialize spec")?;
    std::fs::write(dest, output).context("failed to write patched spec file")?;

    eprintln!("Wrote patched IMGAPI spec to {}", dest);
    Ok(())
}

fn apply_all_imgapi_patches(spec: &mut Value) -> Result<()> {
    patch_node_triton_error_schema(spec)?;
    Ok(())
}

// --- Mahi transforms ---

/// Transform mahi-api.json for wire-format quirks the Rust trait can't express.
///
/// Three patches:
/// 1. `POST /sts/get-caller-identity` returns raw XML (`Content-Type: text/xml`).
/// 2. `GET /uuids?name=` accepts a repeated query parameter (`?name=a&name=b`).
/// 3. `GET /names?uuid=` accepts a repeated query parameter (`?uuid=x&uuid=y`).
fn transform_mahi_spec(source: &Utf8Path, dest: &Utf8Path) -> Result<()> {
    let content = std::fs::read_to_string(source).context("failed to read spec file")?;
    let mut spec: Value = serde_json::from_str(&content).context("failed to parse spec as JSON")?;

    apply_all_mahi_patches(&mut spec)?;

    let output = serde_json::to_string_pretty(&spec).context("failed to serialize spec")?;
    std::fs::write(dest, output).context("failed to write patched spec file")?;

    eprintln!("Wrote patched Mahi spec to {}", dest);
    Ok(())
}

fn apply_all_mahi_patches(spec: &mut Value) -> Result<()> {
    patch_mahi_sts_get_caller_identity_xml(spec)?;
    patch_mahi_repeated_query_param(
        spec,
        "/uuids",
        "name",
        "Names to resolve. Repeat the parameter, e.g. `?name=a&name=b`.",
    )?;
    patch_mahi_repeated_query_param(
        spec,
        "/names",
        "uuid",
        "UUIDs to resolve. Repeat the parameter, e.g. `?uuid=x&uuid=y`.",
    )?;
    Ok(())
}

/// Rewrite `POST /sts/get-caller-identity` 200 response to return raw XML.
///
/// The upstream Mahi service emits `Content-Type: text/xml` with a raw
/// `<GetCallerIdentityResponse>...</GetCallerIdentityResponse>` body. The
/// Rust trait uses `Result<Response<Body>, HttpError>` so the generated
/// spec only has a `default` response with `*/*`. Replace the `responses`
/// with a proper `200` that uses `text/xml` and a plain `string` schema.
fn patch_mahi_sts_get_caller_identity_xml(spec: &mut Value) -> Result<()> {
    let responses = spec
        .get_mut("paths")
        .and_then(|p| p.get_mut("/sts/get-caller-identity"))
        .and_then(|p| p.get_mut("post"))
        .and_then(|m| m.get_mut("responses"))
        .ok_or_else(|| {
            anyhow::anyhow!("POST /sts/get-caller-identity responses not found in spec")
        })?;

    *responses = serde_json::json!({
        "200": {
            "description": "XML body with Content-Type: text/xml",
            "content": {
                "text/xml": {
                    "schema": {
                        "type": "string"
                    }
                }
            }
        },
        "4XX": {
            "$ref": "#/components/responses/Error"
        },
        "5XX": {
            "$ref": "#/components/responses/Error"
        }
    });

    Ok(())
}

/// Rewrite a single query parameter on `GET {path}` to be a repeated (array)
/// parameter with `style: form, explode: true`.
///
/// Dropshot rejects `Vec<T>` in `Query<>`, so the Rust trait declares these
/// as `Option<String>` scalars. Real node-mahi clients send
/// `?name=a&name=b` / `?uuid=x&uuid=y`, so the spec must declare these as
/// arrays. The service layer splits on commas or accepts the raw repeated
/// form via the request context.
fn patch_mahi_repeated_query_param(
    spec: &mut Value,
    path: &str,
    param_name: &str,
    description: &str,
) -> Result<()> {
    let parameters = spec
        .get_mut("paths")
        .and_then(|p| p.get_mut(path))
        .and_then(|p| p.get_mut("get"))
        .and_then(|m| m.get_mut("parameters"))
        .and_then(|p| p.as_array_mut())
        .ok_or_else(|| anyhow::anyhow!("GET {} parameters not found in spec", path))?;

    let param = parameters
        .iter_mut()
        .find(|p| {
            p.get("in").and_then(|v| v.as_str()) == Some("query")
                && p.get("name").and_then(|v| v.as_str()) == Some(param_name)
        })
        .ok_or_else(|| {
            anyhow::anyhow!("query parameter `{}` not found on GET {}", param_name, path)
        })?;

    *param = serde_json::json!({
        "in": "query",
        "name": param_name,
        "description": description,
        "style": "form",
        "explode": true,
        "schema": {
            "type": "array",
            "items": {
                "type": "string"
            }
        }
    });

    Ok(())
}

/// Transform mahi-sitter-api.json — patch the `GET /snapshot` endpoint to
/// return a 201 with `application/octet-stream` streaming body.
fn transform_mahi_sitter_spec(source: &Utf8Path, dest: &Utf8Path) -> Result<()> {
    let content = std::fs::read_to_string(source).context("failed to read spec file")?;
    let mut spec: Value = serde_json::from_str(&content).context("failed to parse spec as JSON")?;

    apply_all_mahi_sitter_patches(&mut spec)?;

    let output = serde_json::to_string_pretty(&spec).context("failed to serialize spec")?;
    std::fs::write(dest, output).context("failed to write patched spec file")?;

    eprintln!("Wrote patched Mahi sitter spec to {}", dest);
    Ok(())
}

fn apply_all_mahi_sitter_patches(spec: &mut Value) -> Result<()> {
    patch_mahi_sitter_snapshot_binary(spec)?;
    Ok(())
}

/// Rewrite `GET /snapshot` to return `201 Created` with an
/// `application/octet-stream` binary streaming body.
///
/// Upstream Mahi sitter pipes the Redis `dump.rdb` file to the socket and
/// terminates the response with `res.send(201)`. The Rust trait returns
/// `Result<Response<Body>, HttpError>`, so the generated spec only has a
/// `default` response with `*/*`. Replace `responses` with a proper 201
/// that uses `application/octet-stream` and a binary schema.
fn patch_mahi_sitter_snapshot_binary(spec: &mut Value) -> Result<()> {
    let responses = spec
        .get_mut("paths")
        .and_then(|p| p.get_mut("/snapshot"))
        .and_then(|p| p.get_mut("get"))
        .and_then(|m| m.get_mut("responses"))
        .ok_or_else(|| anyhow::anyhow!("GET /snapshot responses not found in spec"))?;

    *responses = serde_json::json!({
        "201": {
            "description": "Streaming Redis dump.rdb body",
            "content": {
                "application/octet-stream": {
                    "schema": {
                        "type": "string",
                        "format": "binary"
                    }
                }
            }
        },
        "4XX": {
            "$ref": "#/components/responses/Error"
        },
        "5XX": {
            "$ref": "#/components/responses/Error"
        }
    });

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
    fn test_patch_node_triton_error_schema() {
        let mut spec = dropshot_error_spec();
        patch_node_triton_error_schema(&mut spec).unwrap();

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
        let result = patch_node_triton_error_schema(&mut spec);
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
