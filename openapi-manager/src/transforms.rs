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
use serde_json::{Map, Value, json};

/// Filename of the merged gateway spec produced by [`apply_gateway_merge`].
const GATEWAY_SPEC_NAME: &str = "triton-gateway-api.json";

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

    // Build the merged gateway spec. This must run after the cloudapi
    // patch above, because the gateway merge reads the *patched*
    // cloudapi spec (accurate cloudapi wire format) as one of its two
    // sources.
    apply_gateway_merge(generated_dir, patched_dir)
        .context("failed to build triton-gateway-api.json")?;

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

    all_fresh &= check_gateway_merge(generated_dir, patched_dir)?;

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

// --- Gateway merge (triton-gateway-api.json) ---
//
// The gateway merge combines the patched cloudapi-api.json with the
// generated triton-api.json to produce a single external OpenAPI spec
// covering every endpoint the triton-gateway service exposes:
//
//   * tritonapi native `/v1/*` endpoints (auth / ping / jwks)
//   * cloudapi proxied `/{account}/*` endpoints (the gateway re-signs
//     these with an operator SSH key on its way out)
//
// Downstream TypeScript (web) and Go (Terraform provider) clients
// consume this merged spec instead of stitching two specs themselves.
// Phase 2 of the tritonapi profile plan also uses this spec as the
// input to a Rust `triton-gateway-client` crate.
//
// This is a port of `mariana-trench/openapi-manager/src/transforms.rs`
// with four deliberate changes:
//
//   1. Both source specs are in-repo (no cargo-metadata gymnastics).
//   2. Paths are NOT rewritten. Mariana-trench maps
//      `/{account}/...` → `/api/v1/...` because user-portal is a
//      web frontend with its own URL conventions. The gateway is the
//      canonical public surface, so we keep `/{account}/...` verbatim
//      to preserve method-signature parity with cloudapi-client.
//      (`/my/...` is a separate alias the gateway resolves at runtime.)
//   3. No `x-datacenter` header injection. User-portal fronts multiple
//      DCs; triton-gateway is per-DC.
//   4. `components.schemas.Error` collision is resolved in favour of
//      tritonapi's shape (cloudapi's variant is explicitly dropped).
//      This is correct *because* the gateway translates cloudapi error
//      bodies to the tritonapi shape at runtime — see
//      `services/triton-gateway/src/error_translate.rs`. Without that
//      translation this merge would be lying about the wire format.

/// Build `patched/triton-gateway-api.json` by merging the patched
/// cloudapi spec with the generated tritonapi spec.
fn apply_gateway_merge(generated_dir: &Utf8Path, patched_dir: &Utf8Path) -> Result<()> {
    let Some((cloudapi_spec, tritonapi_spec)) = load_gateway_sources(generated_dir, patched_dir)?
    else {
        return Ok(());
    };

    let merged = merge_gateway_specs(&cloudapi_spec, &tritonapi_spec)?;

    let dest = patched_dir.join(GATEWAY_SPEC_NAME);
    let output =
        serde_json::to_string_pretty(&merged).context("failed to serialize merged gateway spec")?;
    std::fs::write(&dest, output).with_context(|| format!("failed to write {}", dest))?;

    eprintln!("Wrote merged gateway spec to {}", dest);
    Ok(())
}

/// Verify `patched/triton-gateway-api.json` is up-to-date relative to its
/// two source specs. Returns `true` when fresh.
fn check_gateway_merge(generated_dir: &Utf8Path, patched_dir: &Utf8Path) -> Result<bool> {
    let Some((cloudapi_spec, tritonapi_spec)) = load_gateway_sources(generated_dir, patched_dir)?
    else {
        return Ok(true);
    };

    let expected = merge_gateway_specs(&cloudapi_spec, &tritonapi_spec)?;
    let expected_text =
        serde_json::to_string_pretty(&expected).context("failed to serialize expected spec")?;

    let dest = patched_dir.join(GATEWAY_SPEC_NAME);
    if !dest.exists() {
        eprintln!(
            "Patched spec missing: {}\n  fix: run `make openapi-generate`",
            dest
        );
        return Ok(false);
    }

    let actual = std::fs::read_to_string(&dest).context("failed to read merged gateway spec")?;
    if expected_text.trim_end() != actual.trim_end() {
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

/// Load the two source specs for the gateway merge, or return
/// `Ok(None)` if either is missing (treat as "nothing to do").
fn load_gateway_sources(
    generated_dir: &Utf8Path,
    patched_dir: &Utf8Path,
) -> Result<Option<(Value, Value)>> {
    let cloudapi_path = patched_dir.join("cloudapi-api.json");
    let tritonapi_path = generated_dir.join("triton-api.json");

    if !cloudapi_path.exists() || !tritonapi_path.exists() {
        return Ok(None);
    }

    let cloudapi_text = std::fs::read_to_string(&cloudapi_path)
        .with_context(|| format!("failed to read {}", cloudapi_path))?;
    let tritonapi_text = std::fs::read_to_string(&tritonapi_path)
        .with_context(|| format!("failed to read {}", tritonapi_path))?;

    let cloudapi_spec: Value = serde_json::from_str(&cloudapi_text)
        .with_context(|| format!("failed to parse {}", cloudapi_path))?;
    let tritonapi_spec: Value = serde_json::from_str(&tritonapi_text)
        .with_context(|| format!("failed to parse {}", tritonapi_path))?;

    Ok(Some((cloudapi_spec, tritonapi_spec)))
}

/// Merge the cloudapi and tritonapi specs into a single gateway spec.
///
/// The output takes tritonapi as the base (its paths, schemas, and
/// components are assumed to be "fresher" per the Phase 0 error
/// translation agreement) and overlays the cloudapi paths and schemas.
/// Known collisions (`Error`) are resolved in favour of tritonapi;
/// any other schema collisions are logged as a warning so they get
/// human review.
fn merge_gateway_specs(cloudapi_spec: &Value, tritonapi_spec: &Value) -> Result<Value> {
    let mut merged = Map::new();

    // Synthesize the `info` block. We draw the version from the
    // tritonapi crate (already embedded in its generated spec) to keep
    // this consistent with how openapi-manager sources versions for
    // every other API — see `crate_version` in main.rs.
    let tritonapi_version = tritonapi_spec
        .pointer("/info/version")
        .and_then(|v| v.as_str())
        .unwrap_or("0.1.0");
    merged.insert("openapi".to_string(), json!("3.0.3"));
    merged.insert(
        "info".to_string(),
        json!({
            "title": "Triton Gateway API",
            "description": "Merged public surface of the triton-gateway service: \
                tritonapi `/v1/*` endpoints plus the cloudapi `/{account}/*` \
                endpoints it proxies. This spec is the single source of truth \
                for downstream TypeScript (web) and Go (Terraform provider) \
                client generators, and for the Rust `triton-gateway-client` \
                crate.",
            "version": tritonapi_version,
        }),
    );

    // Merge paths. No collisions are expected (tritonapi is `/v1/*`,
    // cloudapi is `/{account}/*`) but we still detect and error on
    // them rather than silently clobbering.
    let mut paths = Map::new();
    copy_paths_into(
        &mut paths,
        tritonapi_spec.get("paths"),
        "triton-api.json",
        "cloudapi-api.json",
    )?;
    copy_paths_into(
        &mut paths,
        cloudapi_spec.get("paths"),
        "cloudapi-api.json",
        "triton-api.json",
    )?;
    merged.insert("paths".to_string(), Value::Object(paths));

    // Build components.
    let components = build_components(cloudapi_spec, tritonapi_spec)?;
    merged.insert("components".to_string(), Value::Object(components));

    // Global security scheme — every gateway request needs a Bearer
    // JWT. (The gateway also accepts HTTP Signature on the cloudapi
    // leg as a fallback during testing, but that's not surfaced in
    // the public spec.)
    merged.insert("security".to_string(), json!([{ "bearerAuth": [] }]));

    // Merge tags (sorted union).
    merged.insert(
        "tags".to_string(),
        Value::Array(merge_tag_lists(&[
            tritonapi_spec.get("tags"),
            cloudapi_spec.get("tags"),
        ])),
    );

    Ok(Value::Object(merged))
}

/// Copy every entry from `src` into `dst`, erroring on collision.
fn copy_paths_into(
    dst: &mut Map<String, Value>,
    src: Option<&Value>,
    src_name: &str,
    other_name: &str,
) -> Result<()> {
    let Some(obj) = src.and_then(|v| v.as_object()) else {
        return Ok(());
    };
    for (path, methods) in obj {
        if dst.contains_key(path) {
            anyhow::bail!(
                "path `{}` appears in both {} and {}; gateway merge cannot safely \
                 pick one. Resolve the collision in the source specs.",
                path,
                src_name,
                other_name,
            );
        }
        dst.insert(path.clone(), methods.clone());
    }
    Ok(())
}

/// Build the merged `components` object. Tritonapi wins for any schema
/// or response collision — today that's just `Error`, which is the
/// whole reason Phase 0 exists. Any other collision is logged as a
/// warning so a human can decide whether the gateway merge is still
/// safe.
fn build_components(cloudapi_spec: &Value, tritonapi_spec: &Value) -> Result<Map<String, Value>> {
    let mut components = Map::new();

    // Schemas: tritonapi wins on collision (see module comment above).
    let mut schemas = Map::new();
    merge_component_map(
        &mut schemas,
        tritonapi_spec.pointer("/components/schemas"),
        cloudapi_spec.pointer("/components/schemas"),
        "schema",
        &["Error"],
    );
    components.insert("schemas".to_string(), Value::Object(schemas));

    // Responses: same precedence rule. `Error` also lives here.
    let mut responses = Map::new();
    merge_component_map(
        &mut responses,
        tritonapi_spec.pointer("/components/responses"),
        cloudapi_spec.pointer("/components/responses"),
        "response",
        &["Error"],
    );
    components.insert("responses".to_string(), Value::Object(responses));

    // Fold in any other component buckets tritonapi happens to define
    // (`parameters`, `requestBodies`, `headers`, ...). Unlikely for
    // our current surface but cheap and correct.
    if let Some(tri_components) = tritonapi_spec.get("components").and_then(|v| v.as_object()) {
        for (key, value) in tri_components {
            if key == "schemas" || key == "responses" || key == "securitySchemes" {
                continue;
            }
            components.insert(key.clone(), value.clone());
        }
    }
    if let Some(cloud_components) = cloudapi_spec.get("components").and_then(|v| v.as_object()) {
        for (key, value) in cloud_components {
            if key == "schemas" || key == "responses" || key == "securitySchemes" {
                continue;
            }
            if let Some(existing) = components.get(key)
                && existing != value
            {
                eprintln!(
                    "  warning: gateway merge: components.{} differs between cloudapi \
                     and tritonapi specs; keeping tritonapi's copy",
                    key
                );
                continue;
            }
            components
                .entry(key.clone())
                .or_insert_with(|| value.clone());
        }
    }

    // Bearer security scheme (global) — identical to the mariana-trench
    // portal merge.
    let mut security_schemes = Map::new();
    security_schemes.insert(
        "bearerAuth".to_string(),
        json!({
            "type": "http",
            "scheme": "bearer",
            "bearerFormat": "JWT",
            "description": "JWT issued by the gateway's `/v1/auth/login` endpoint.",
        }),
    );
    components.insert(
        "securitySchemes".to_string(),
        Value::Object(security_schemes),
    );

    Ok(components)
}

/// Merge `primary` and `secondary` component maps into `dst`. Primary
/// wins on collision. Entries whose name appears in `expected_collisions`
/// are merged silently; any other collision is logged as a warning so
/// a human can audit the merge during review.
fn merge_component_map(
    dst: &mut Map<String, Value>,
    primary: Option<&Value>,
    secondary: Option<&Value>,
    kind: &str,
    expected_collisions: &[&str],
) {
    if let Some(obj) = primary.and_then(|v| v.as_object()) {
        for (name, value) in obj {
            dst.insert(name.clone(), value.clone());
        }
    }
    if let Some(obj) = secondary.and_then(|v| v.as_object()) {
        for (name, value) in obj {
            if dst.contains_key(name) {
                if !expected_collisions.contains(&name.as_str()) {
                    eprintln!(
                        "  warning: gateway merge: {} `{}` defined in both source \
                         specs; keeping tritonapi's copy",
                        kind, name
                    );
                }
                continue;
            }
            dst.insert(name.clone(), value.clone());
        }
    }
}

/// Merge a list of `tags` arrays (each a `Vec<{name, ...}>`) into a
/// sorted, deduplicated-by-name array. Using a `BTreeSet` keeps the
/// output deterministic across runs.
fn merge_tag_lists(tag_sources: &[Option<&Value>]) -> Vec<Value> {
    let mut by_name: std::collections::BTreeMap<String, Value> = std::collections::BTreeMap::new();
    for source in tag_sources {
        let Some(arr) = source.and_then(|v| v.as_array()) else {
            continue;
        };
        for tag in arr {
            if let Some(name) = tag.get("name").and_then(|v| v.as_str()) {
                // First occurrence wins, mostly so tritonapi's
                // descriptions (if any) don't get overwritten by
                // cloudapi's.
                by_name
                    .entry(name.to_string())
                    .or_insert_with(|| tag.clone());
            }
        }
    }
    by_name.into_values().collect()
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

    /// Minimal synthetic cloudapi-shaped spec for gateway-merge tests.
    fn cloudapi_fixture() -> Value {
        serde_json::json!({
            "openapi": "3.0.3",
            "info": { "title": "Triton CloudAPI", "version": "9.20.0" },
            "paths": {
                "/{account}/machines": {
                    "get": {
                        "tags": ["machines"],
                        "operationId": "list_machines",
                        "responses": { "200": { "description": "ok" } }
                    }
                }
            },
            "components": {
                "schemas": {
                    "Error": {
                        "type": "object",
                        "properties": {
                            "code": { "type": "string" },
                            "message": { "type": "string" }
                        },
                        "required": ["code"]
                    },
                    "Machine": {
                        "type": "object",
                        "properties": { "id": { "type": "string" } }
                    }
                },
                "responses": {
                    "Error": {
                        "description": "Error",
                        "content": {
                            "application/json": {
                                "schema": { "$ref": "#/components/schemas/Error" }
                            }
                        }
                    }
                }
            },
            "tags": [{ "name": "machines" }]
        })
    }

    /// Minimal synthetic tritonapi-shaped spec for gateway-merge tests.
    fn tritonapi_fixture() -> Value {
        serde_json::json!({
            "openapi": "3.0.3",
            "info": { "title": "Triton API", "version": "0.1.0" },
            "paths": {
                "/v1/auth/login": {
                    "post": {
                        "tags": ["auth"],
                        "operationId": "auth_login",
                        "responses": { "200": { "description": "ok" } }
                    }
                }
            },
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
                    },
                    "LoginRequest": {
                        "type": "object",
                        "properties": { "username": { "type": "string" } }
                    }
                },
                "responses": {
                    "Error": {
                        "description": "Error",
                        "content": {
                            "application/json": {
                                "schema": { "$ref": "#/components/schemas/Error" }
                            }
                        }
                    }
                }
            },
            "tags": [{ "name": "auth" }]
        })
    }

    #[test]
    fn test_gateway_merge_combines_paths_and_keeps_tritonapi_error() {
        let cloudapi = cloudapi_fixture();
        let tritonapi = tritonapi_fixture();

        let merged = merge_gateway_specs(&cloudapi, &tritonapi).unwrap();

        // Paths merged: tritonapi and cloudapi both present.
        assert!(merged["paths"]["/{account}/machines"].is_object());
        assert!(merged["paths"]["/v1/auth/login"].is_object());

        // Error schema: tritonapi's shape wins (has error_code, not code).
        let error = &merged["components"]["schemas"]["Error"];
        assert!(
            error["properties"]["error_code"].is_object(),
            "tritonapi Error shape should win (had error_code)"
        );
        assert!(
            error["properties"]["code"].is_null(),
            "cloudapi Error shape should be dropped (had code)"
        );
        assert_eq!(
            error["required"].as_array().unwrap(),
            &[
                serde_json::json!("message"),
                serde_json::json!("request_id")
            ]
        );

        // Non-colliding schemas from both specs are carried through.
        assert!(merged["components"]["schemas"]["Machine"].is_object());
        assert!(merged["components"]["schemas"]["LoginRequest"].is_object());

        // Bearer security scheme added + global security set.
        assert_eq!(
            merged["components"]["securitySchemes"]["bearerAuth"]["scheme"],
            "bearer"
        );
        assert_eq!(
            merged["security"][0]["bearerAuth"]
                .as_array()
                .unwrap()
                .len(),
            0
        );

        // Tags are the sorted union.
        let tag_names: Vec<&str> = merged["tags"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(tag_names, vec!["auth", "machines"]);

        // Info block is synthesized.
        assert_eq!(merged["info"]["title"], "Triton Gateway API");
        assert_eq!(merged["info"]["version"], "0.1.0");
    }

    #[test]
    fn test_gateway_merge_rejects_path_collision() {
        let mut cloudapi = cloudapi_fixture();
        // Make cloudapi claim a tritonapi-shaped path too.
        let login = cloudapi["paths"]["/{account}/machines"].clone();
        cloudapi["paths"]
            .as_object_mut()
            .unwrap()
            .insert("/v1/auth/login".to_string(), login);

        let tritonapi = tritonapi_fixture();
        let result = merge_gateway_specs(&cloudapi, &tritonapi);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("/v1/auth/login"));
    }

    #[test]
    fn test_gateway_merge_writes_and_checks() {
        let temp_dir = TempDir::new().unwrap();
        let root = Utf8Path::from_path(temp_dir.path()).unwrap();
        let generated = root.join("generated");
        let patched = root.join("patched");
        std::fs::create_dir_all(&generated).unwrap();
        std::fs::create_dir_all(&patched).unwrap();

        // Write the sources in the locations apply_gateway_merge reads.
        write_spec(&patched.join("cloudapi-api.json"), &cloudapi_fixture());
        write_spec(&generated.join("triton-api.json"), &tritonapi_fixture());

        apply_gateway_merge(&generated, &patched).unwrap();
        assert!(patched.join(GATEWAY_SPEC_NAME).exists());
        assert!(check_gateway_merge(&generated, &patched).unwrap());

        // Tamper with the merged file; check should now fail.
        let merged_path = patched.join(GATEWAY_SPEC_NAME);
        let original = std::fs::read_to_string(&merged_path).unwrap();
        std::fs::write(&merged_path, original.replacen("Gateway", "Tampered", 1)).unwrap();
        assert!(!check_gateway_merge(&generated, &patched).unwrap());
    }
}
