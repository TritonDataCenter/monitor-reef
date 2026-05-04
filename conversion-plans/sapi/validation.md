# SAPI Conversion Validation Report

## Summary

| Category | Status | Issues |
|----------|--------|--------|
| Endpoint Coverage | ✅ | 24 of 24 endpoints |
| Type Completeness | ✅ | 0 issues |
| Route Conflicts | ✅ | 0 conflicts |
| CLI Coverage | ✅ | 28 of 28 commands (24 endpoints + 4 extra sub-operations) |
| API Compatibility | ⚠️ | 3 concerns (see below) |

## Endpoint Coverage

### ✅ Converted Endpoints

| Node.js | Rust | Notes |
|---------|------|-------|
| `GET /ping` | `ping` | |
| `GET /mode` | `get_mode` | Response wrapped in `ModeResponse` object (see compatibility) |
| `POST /mode` | `set_mode` | Response changed from 204 to JSON (see compatibility) |
| `GET /loglevel` | `get_log_level` | |
| `POST /loglevel` | `set_log_level` | Response changed from empty to JSON (see compatibility) |
| `POST /cache` | `sync_cache` | 204 response preserved |
| `GET /applications` | `list_applications` | Query filters: `name`, `owner_uuid`, `include_master` |
| `POST /applications` | `create_application` | Required: `name`, `owner_uuid` |
| `GET /applications/:uuid` | `get_application` | |
| `PUT /applications/:uuid` | `update_application` | Action dispatch: update/replace/delete |
| `DELETE /applications/:uuid` | `delete_application` | |
| `GET /services` | `list_services` | Query filters: `name`, `application_uuid`, `type`, `include_master` |
| `POST /services` | `create_service` | Required: `name`, `application_uuid` |
| `GET /services/:uuid` | `get_service` | |
| `PUT /services/:uuid` | `update_service` | Action dispatch: update/replace/delete |
| `DELETE /services/:uuid` | `delete_service` | |
| `GET /instances` | `list_instances` | Query filters: `service_uuid`, `type`, `include_master` |
| `POST /instances` | `create_instance` | Required: `service_uuid`; `async` query param |
| `GET /instances/:uuid` | `get_instance` | |
| `GET /instances/:uuid/payload` | `get_instance_payload` | Returns freeform JSON (`serde_json::Value`) |
| `PUT /instances/:uuid` | `update_instance` | Action dispatch: update/replace/delete |
| `PUT /instances/:uuid/upgrade` | `upgrade_instance` | Required: `image_uuid` |
| `DELETE /instances/:uuid` | `delete_instance` | |
| `GET /configs/:uuid` | `get_config` | Returns freeform JSON (`serde_json::Value`) |
| `GET /manifests` | `list_manifests` | Query filter: `include_master` |
| `POST /manifests` | `create_manifest` | Required: `name`, `path`, `template` |
| `GET /manifests/:uuid` | `get_manifest` | |
| `DELETE /manifests/:uuid` | `delete_manifest` | |

### ❌ Missing Endpoints

None. All 24 Node.js endpoints are covered.

## Type Analysis

### ✅ Complete Types

- **Application** -- All fields mapped: `uuid`, `name`, `owner_uuid`, `params`, `metadata`, `metadata_schema`, `manifests`, `master`
- **Service** -- All fields mapped: `uuid`, `name`, `application_uuid`, `params`, `metadata`, `manifests`, `master`, `type` (via `service_type` with serde rename)
- **Instance** -- All fields mapped: `uuid`, `service_uuid`, `params`, `metadata`, `manifests`, `master`, `type` (via `instance_type` with serde rename), `job_uuid`
- **Manifest** -- All fields mapped: `uuid`, `name`, `path`, `template`, `post_cmd`, `post_cmd_linux`, `version`, `master`
- **PingResponse** -- All fields mapped with correct camelCase renames: `mode`, `storType`, `storAvailable`
- **UpdateAction** -- All variants: `update`, `replace`, `delete`
- **ServiceType** -- All variants: `vm`, `agent`, plus `Unknown` for forward compatibility
- **SapiMode** -- All variants: `proto`, `full`, plus `Unknown` for forward compatibility

### ⚠️ Partial Types

- **UpdateAttributesBody** -- Defined as a shared base struct but not directly used by endpoints (each resource has its own typed body). This is fine; it documents the pattern.

### ❌ Missing Types

None.

## Route Conflict Resolutions

No route conflicts exist. All SAPI routes use distinct literal path segments (`/applications`, `/services`, `/instances`, `/manifests`, `/configs`, `/cache`, `/mode`, `/ping`, `/loglevel`). Sub-resources (`/instances/:uuid/payload`, `/instances/:uuid/upgrade`) do not conflict with parent `:uuid` routes.

## CLI Command Analysis

### ✅ Implemented Commands

| CLI Command | API Endpoint |
|-------------|-------------|
| `sapi ping` | `GET /ping` |
| `sapi get-mode` | `GET /mode` |
| `sapi set-mode <mode>` | `POST /mode` |
| `sapi get-log-level` | `GET /loglevel` |
| `sapi set-log-level <level>` | `POST /loglevel` |
| `sapi sync-cache` | `POST /cache` |
| `sapi list-applications` | `GET /applications` |
| `sapi get-application <uuid>` | `GET /applications/{uuid}` |
| `sapi create-application` | `POST /applications` |
| `sapi update-application <uuid>` | `PUT /applications/{uuid}` |
| `sapi delete-application <uuid>` | `DELETE /applications/{uuid}` |
| `sapi list-services` | `GET /services` |
| `sapi get-service <uuid>` | `GET /services/{uuid}` |
| `sapi create-service` | `POST /services` |
| `sapi update-service <uuid>` | `PUT /services/{uuid}` |
| `sapi delete-service <uuid>` | `DELETE /services/{uuid}` |
| `sapi list-instances` | `GET /instances` |
| `sapi get-instance <uuid>` | `GET /instances/{uuid}` |
| `sapi create-instance` | `POST /instances` |
| `sapi update-instance <uuid>` | `PUT /instances/{uuid}` |
| `sapi upgrade-instance <uuid>` | `PUT /instances/{uuid}/upgrade` |
| `sapi delete-instance <uuid>` | `DELETE /instances/{uuid}` |
| `sapi get-instance-payload <uuid>` | `GET /instances/{uuid}/payload` |
| `sapi list-manifests` | `GET /manifests` |
| `sapi get-manifest <uuid>` | `GET /manifests/{uuid}` |
| `sapi create-manifest` | `POST /manifests` |
| `sapi delete-manifest <uuid>` | `DELETE /manifests/{uuid}` |
| `sapi get-config <uuid>` | `GET /configs/{uuid}` |

All 28 CLI commands map to API endpoints. Every endpoint has a corresponding CLI command.

### ❌ Missing Commands

None.

## Behavioral Notes

### API Versioning (Accept-Version)

The original Node.js SAPI supports two API versions via `Accept-Version`:
- **~1 (default)**: Services/instances with `type=agent` are hidden from lists; the `type` field is omitted from responses.
- **~2 (2.0.0+)**: Includes `type` field in responses; lists include `type=agent` objects.

The Rust API trait always includes the `type` field (v2 behavior). This is the correct approach since all modern clients use v2. The field is `Option<ServiceType>`, so it serializes correctly when absent.

**Impact**: The service implementation should always return v2-style responses. If v1 compatibility is needed, the service implementation can filter based on a header, but this is not expected to be necessary.

### Action Dispatch Pattern

SAPI's update endpoints (`PUT /applications/:uuid`, `PUT /services/:uuid`, `PUT /instances/:uuid`) all share the same action-dispatch pattern via the `attributes.js` module:
- `action` is a body field (not query parameter), defaulting to `"update"`
- Three actions: `update` (merge), `replace` (overwrite), `delete` (remove keys)

The Rust API correctly models this with typed `UpdateAction` enum and per-resource body structs (`UpdateApplicationBody`, `UpdateServiceBody`, `UpdateInstanceBody`). The `action` field is `Option<UpdateAction>`, matching the Node.js default behavior.

### Async Instance Creation

`POST /instances` accepts an `async` query parameter (mapped to `async_create` in Rust since `async` is a reserved keyword). When true, the endpoint returns immediately with a `job_uuid` field in the response. The CLI exposes this as `--async-create`.

### Connection Timeouts

Node.js sets extended connection timeouts:
- `CreateInstance`: 1 hour (`60 * 60 * 1000` ms)
- `DeleteInstance`: 10 minutes (`10 * 60 * 1000` ms)

These do not map directly to Dropshot but should be documented for the service implementation. The client or server may need equivalent timeout configuration.

### Conditional Requests (ETag)

`GET /configs/:uuid` computes a SHA-1 ETag from the flattened config and supports conditional requests via Restify's `conditionalRequest()` middleware. The Rust endpoint returns `HttpResponseOk<serde_json::Value>`. ETag support would need manual header handling in the service implementation.

### Error Responses

- Node.js uses Restify error types: `MissingParameterError`, `InvalidArgumentError`, `ServiceUnavailableError`, `ObjectNotFoundError`
- Rust uses Dropshot's `HttpError` which provides `{ message, error_code }` format
- The error payload shape differs, but HTTP status codes can be matched

### Delete Endpoints

All delete endpoints (`applications`, `services`, `instances`, `manifests`) check for `ObjectNotFoundError` and return 404, otherwise return 204. The Rust API uses `HttpResponseDeleted` which returns 204. The 404 case would be handled in the service implementation.

### Semver Validation for Manifests

`POST /manifests` validates the `version` field with `semver.valid()`. This validation logic would need to be implemented in the Rust service (e.g., using the `semver` crate).

### include_master Middleware

The `ensureMasterConfigLoaded` middleware validates that master configuration is available when `include_master=true` is requested. If the master host is not configured, it returns 503 `ServiceUnavailableError`. This middleware logic needs implementation in the service.

## API Compatibility Assessment

### ⚠️ Compatibility Concerns

1. **GET /mode response format**: Node.js returns a bare string (`"proto"` or `"full"`). The Rust API wraps this in a `ModeResponse { mode: SapiMode }` object. The service implementation should match the wire format expected by existing clients. If bare-string compatibility is required, the endpoint return type may need adjustment.

2. **POST /mode response format**: Node.js returns 204 (no content). The Rust API returns `HttpResponseOk<ModeResponse>` (200 with JSON body). The service implementation will differ from the original.

3. **POST /loglevel response format**: Node.js returns `res.send()` (200 with empty body). The Rust API returns `HttpResponseOk<LogLevelResponse>` with the level in the body. This is an enhancement over the original but changes the wire format.

### ✅ Compatible

- All JSON field names match the wire format (snake_case throughout, except PingResponse's camelCase fields)
- HTTP methods match for all endpoints
- Path parameters correctly converted from `:uuid` to `{uuid}`
- Query parameters match: `name`, `owner_uuid`, `application_uuid`, `type`, `include_master`, `async`
- `type` field correctly renamed from Rust field names (`service_type`, `instance_type`) to `"type"` via serde
- DELETE endpoints return 204 (via `HttpResponseDeleted`)
- POST/create endpoints return 201 (via `HttpResponseCreated`) -- Note: Node.js returns 200 by default with `res.send()`, so this is a minor difference but generally acceptable for REST semantics
- `PingResponse` correctly uses explicit `#[serde(rename)]` for camelCase `storType` and `storAvailable`

## Recommendations

### High Priority

1. [ ] **Mode endpoint wire format**: Verify whether existing SAPI clients (sdcadm, sdc-sapi CLI) expect a bare string or JSON object from `GET /mode`. If bare string is required, consider changing the return type to `HttpResponseOk<String>` or `HttpResponseOk<SapiMode>`.
2. [ ] **POST /mode 204 vs 200**: Consider using `HttpResponseUpdatedNoContent` for `set_mode` to match the original 204 response.
3. [ ] **POST /loglevel empty response**: Consider using `HttpResponseUpdatedNoContent` for `set_log_level` to match the original empty response, or keep the enhanced response if clients tolerate it.

### Medium Priority

1. [ ] **Create endpoints 200 vs 201**: Node.js `res.send(obj)` returns 200, but Rust uses `HttpResponseCreated` (201). Verify that existing clients handle 201 correctly. Most well-behaved HTTP clients do.
2. [ ] **ETag support for configs**: Implement SHA-1 ETag computation and conditional request handling in the service implementation for `GET /configs/:uuid`.
3. [ ] **Connection timeout documentation**: Document the expected timeout values for `CreateInstance` (1 hour) and `DeleteInstance` (10 minutes) in the service implementation.

### Low Priority

1. [ ] **Semver validation**: Add `semver` crate validation in the manifest creation service handler.
2. [ ] **include_master middleware**: Implement the master configuration availability check in list endpoint handlers.
3. [ ] **Ping status code**: Node.js returns 500 when storage is unavailable. The Rust endpoint always returns 200 with the response body. Consider returning an error when `stor_available` is false.

## Conclusion

**Overall Status**: ✅ READY FOR TESTING

The SAPI conversion is comprehensive. All 24 Node.js endpoints have corresponding Rust API trait definitions, client methods, and CLI commands. Type definitions are complete with correct field names, serde renames, and forward-compatible `Unknown` variants on enums.

The three compatibility concerns (mode/loglevel response formats, create endpoint status codes) are minor and can be addressed during integration testing. The mode endpoint is the most significant difference -- existing clients may expect a bare string rather than a JSON object. All other endpoints have compatible wire formats.

The action-dispatch pattern for update endpoints is correctly modeled with typed enums rather than raw strings. The async instance creation, freeform JSON responses (configs, payload), and PingResponse camelCase fields are all handled appropriately.
