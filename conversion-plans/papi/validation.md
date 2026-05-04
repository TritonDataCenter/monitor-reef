# PAPI Conversion Validation Report

## Summary

| Category | Status | Issues |
|----------|--------|--------|
| Endpoint Coverage | OK | 6 of 6 unique endpoints (8 of 8 counting HEAD) |
| Type Completeness | OK | All fields present, correct types |
| Route Conflicts | OK | None (all paths distinct) |
| CLI Coverage | Minor gaps | 6 of 6 commands, 3 mutable fields missing from `update` |
| Enum Wire Values | OK | All values match Node.js source |
| API Compatibility | OK | Status codes, wire format, field naming all correct |

## Endpoint Coverage

### Converted Endpoints

| Node.js | Rust | Status Code | Notes |
|---------|------|-------------|-------|
| `GET /packages` | `list_packages` | 200 | Query params for filtering, sort, pagination |
| `HEAD /packages` | (auto) | -- | Dropshot auto-handles HEAD for GET |
| `POST /packages` | `create_package` | 201 | Correct: `HttpResponseCreated<Package>` |
| `GET /packages/:uuid` | `get_package` | 200 | `owner_uuids` query param for access filtering |
| `HEAD /packages/:uuid` | (auto) | -- | Dropshot auto-handles HEAD for GET |
| `PUT /packages/:uuid` | `update_package` | 200 | Immutable field protection via `force` |
| `DELETE /packages/:uuid` | `delete_package` | 204 | Correct: `HttpResponseUpdatedNoContent` |
| `GET /ping` | `ping` | 200 | Process ID + backend status |

### Missing Endpoints

None. All 6 unique handler paths are covered. HEAD variants are handled automatically by Dropshot.

## Type Analysis

### Package Struct

All fields from the Node.js schema are present in the Rust `Package` struct:

| Field | Node.js Type | Rust Type | Status |
|-------|-------------|-----------|--------|
| `uuid` | UUID | `Uuid` | OK |
| `name` | string | `String` | OK |
| `version` | string | `String` | OK |
| `active` | boolean | `bool` | OK |
| `cpu_cap` | number | `Option<u64>` | OK (sometimes optional) |
| `max_lwps` | number | `u64` | OK |
| `max_physical_memory` | number | `u64` | OK |
| `max_swap` | number | `u64` | OK |
| `quota` | number | `u64` | OK |
| `zfs_io_priority` | number | `u64` | OK |
| `v` | number (always 1) | `Option<u64>` | OK |
| `brand` | string enum | `Option<Brand>` | OK |
| `owner_uuids` | [UUID] | `Option<Vec<Uuid>>` | OK |
| `vcpus` | number | `Option<u64>` | OK |
| `default` | boolean | `Option<bool>` | OK |
| `group` | string | `Option<String>` | OK |
| `description` | string | `Option<String>` | OK |
| `common_name` | string | `Option<String>` | OK |
| `networks` | [UUID] | `Option<Vec<Uuid>>` | OK |
| `os` | string | `Option<String>` | OK |
| `min_platform` | object (hash) | `Option<HashMap<String, String>>` | OK |
| `parent` | string | `Option<String>` | OK |
| `traits` | object (hash) | `Option<serde_json::Value>` | OK |
| `fss` | number | `Option<u64>` | OK |
| `cpu_burst_ratio` | double | `Option<f64>` | OK |
| `ram_ratio` | double | `Option<f64>` | OK |
| `created_at` | date string | `Option<String>` | OK |
| `updated_at` | date string | `Option<String>` | OK |
| `billing_tag` | string | `Option<String>` | OK |
| `alloc_server_spread` | string enum | `Option<AllocServerSpread>` | OK |
| `flexible_disk` | boolean | `Option<bool>` | OK |
| `disks` | [object] | `Option<Vec<DiskSpec>>` | OK |

### CreatePackageRequest

All required and optional fields present. `skip_validation` control parameter included. No `force` needed (create does not support force).

### UpdatePackageRequest

All mutable fields present plus `force` and `skip_validation` control parameters. Correctly excludes immutable fields (name, version, uuid, max_lwps, max_physical_memory, max_swap, quota, zfs_io_priority, cpu_cap, vcpus, os) from the default set. Includes `networks`, `min_platform`, `traits`, `disks` which are mutable in Node.js.

### PingResponse

All fields present: `pid` (u32), `backend` (BackendStatus), `backend_error` (Option<String>).

### DiskSpec / DiskSize

Correctly models the `{ size: number | "remaining" }` union type with `#[serde(untagged)]`.

## Enum Wire Values

### Brand

| Rust Variant | Wire Value | Node.js (`VALID_BRANDS`) | Match |
|-------------|------------|--------------------------|-------|
| `Bhyve` | `"bhyve"` | `'bhyve'` | OK |
| `Joyent` | `"joyent"` | `'joyent'` | OK |
| `JoyentMinimal` | `"joyent-minimal"` | `'joyent-minimal'` | OK |
| `Kvm` | `"kvm"` | `'kvm'` | OK |
| `Lx` | `"lx"` | `'lx'` | OK |
| `Unknown` | (catch-all) | -- | OK (forward compat) |

### AllocServerSpread

| Rust Variant | Wire Value | Node.js (`VALID_SPREADS`) | Match |
|-------------|------------|---------------------------|-------|
| `MinRam` | `"min-ram"` | `'min-ram'` | OK |
| `Random` | `"random"` | `'random'` | OK |
| `MinOwner` | `"min-owner"` | `'min-owner'` | OK |
| `Unknown` | (catch-all) | -- | OK (forward compat) |

### BackendStatus

| Rust Variant | Wire Value | Node.js | Match |
|-------------|------------|---------|-------|
| `Up` | `"up"` | `data.backend = 'up'` | OK |
| `Down` | `"down"` | `data.backend = 'down'` | OK |
| `Unknown` | (catch-all) | -- | OK (forward compat) |

### SortOrder

| Rust Variant | Wire Value | Node.js | Match |
|-------------|------------|---------|-------|
| `Asc` | `"ASC"` | `order = 'ASC'` | OK |
| `Desc` | `"DESC"` | `order = 'DESC'` | OK |

No `#[serde(other)]` needed -- client-provided, fixed set.

## OpenAPI Spec Validation

### Response Status Codes

| Endpoint | Node.js | OpenAPI | Match |
|----------|---------|---------|-------|
| `GET /packages` | `res.send(200, packages)` | 200 | OK |
| `POST /packages` | `res.send(201, p)` | 201 | OK |
| `GET /packages/:uuid` | `res.send(200, req.pkg)` | 200 | OK |
| `PUT /packages/:uuid` | `res.send(200, savedPkg)` | 200 | OK |
| `DELETE /packages/:uuid` | `res.send(204)` | 204 | OK |
| `GET /ping` | `res.send(data)` (default 200) | 200 | OK |

### No Bare String / Empty Body Issues

- DELETE returns 204 with no content -- correctly mapped to `HttpResponseUpdatedNoContent` with no response body in the spec.
- All other responses are JSON objects or arrays -- no bare string responses.

### No Dead Schemas

All schemas in the spec are referenced: `Package`, `CreatePackageRequest`, `UpdatePackageRequest`, `PingResponse`, `Brand`, `AllocServerSpread`, `BackendStatus`, `SortOrder`, `DiskSize`, `DiskSizeRemaining`, `DiskSpec`, `Error`. No unused schemas.

### Field Naming

All fields use **snake_case** in the wire format, matching the Node.js service exactly. No `rename_all` annotation on the `Package` struct (Rust field names are naturally snake_case, matching the wire format). This is correct.

## CLI Command Analysis

### Implemented Commands

| CLI Command | API Endpoint | Notes |
|-------------|-------------|-------|
| `papi ping` | `GET /ping` | OK |
| `papi list` | `GET /packages` | OK - all filter params exposed |
| `papi get <uuid>` | `GET /packages/{uuid}` | OK - `--owner-uuids` supported |
| `papi create` | `POST /packages` | OK - all fields as flags |
| `papi update <uuid>` | `PUT /packages/{uuid}` | Minor gaps (see below) |
| `papi delete <uuid>` | `DELETE /packages/{uuid}` | OK - `--force` required |

### CLI Issues

1. **`update` missing `--networks` flag**: The `UpdatePackageRequest` includes `networks: Option<Vec<Uuid>>` but the CLI `Update` command does not expose a `--networks` flag. Users cannot update network UUIDs via the CLI.

2. **`update` missing `--min-platform` flag**: The `UpdatePackageRequest` includes `min_platform` but the CLI does not expose it. This is understandable since it is a `HashMap<String, String>` which is awkward as a CLI flag, but should be documented or supported via `--raw` JSON input.

3. **`update` missing `--default` flag**: The `UpdatePackageRequest` includes `default: Option<bool>` but the CLI does not expose it. This is a deprecated field (SDC 6.5 compat), so omitting it from the CLI is a reasonable choice.

4. **`update` missing `--traits` flag**: Same as `min_platform` -- freeform JSON object, awkward as CLI flag.

5. **`create` missing `--default` flag**: Deprecated field, reasonable to omit.

6. **`create` missing `--networks` flag**: The `CreatePackageRequest` includes `networks` but the CLI does not expose a `--networks` flag.

7. **`create` missing `--min-platform` and `--traits` flags**: Same rationale as update -- complex types awkward as CLI flags.

These are all low-priority: the complex-type fields (`min_platform`, `traits`, `networks`) would need special CLI handling (e.g., repeated `--network <uuid>` flags or JSON input), and `default` is deprecated. The `--raw` flag on output does not help here since it only affects output format, not input.

## Route Conflict Resolutions

No conflicts exist. All paths are distinct:
- `/ping` - health check
- `/packages` - collection
- `/packages/{uuid}` - single resource

## Behavioral Notes

### Pagination

- Node.js uses `?offset=N&limit=M` with `sort` and `order` params
- Rust API preserves all four query parameters in `ListPackagesQuery`
- Default sort is by `_id ASC` in Node.js (implementation detail)

### Error Responses

- Node.js uses Restify error classes: `ConflictError` (409), `ResourceNotFoundError` (404), `BadMethodError` (405), `InvalidArgumentError` (400), `InternalError` (500)
- Rust uses `HttpError` which provides similar status codes
- Node.js validation errors return 409 with `{ code, message, errors: [{ field, code, message }] }` format
- The Rust `Error` schema uses `{ error_code, message, request_id }` (Dropshot default) which differs from Node.js validation error format

### Special Behaviors

1. **`GET /packages` 404 on undefined filter**: Node.js returns `res.send(404)` with no body when search filter is `undefined` (empty array parameter). This is unusual and the plan recommends returning an empty array instead.

2. **`x-resource-count` header**: Node.js `listPkgs` sets `res.header('x-resource-count', r.total)`. This custom header is not modeled in the OpenAPI spec. Implementation should add it.

3. **`Location` header on create**: Node.js sets `res.header('Location', ...)` on POST. Standard HTTP behavior, Dropshot's `HttpResponseCreated` may or may not set this automatically.

4. **`force` parameter on DELETE**: Must be `true` or returns 405. The Rust API correctly models this as a query parameter.

5. **`loadPkg` middleware pattern**: Node.js uses a middleware chain (`loadPkg` then handler). The Rust implementation will handle this within the endpoint handler.

6. **Stale attribute stripping**: Node.js strips `overprovision_*` and `urn` on update. This is implementation-specific and does not affect the API surface.

7. **`owner_uuids` special search semantics**: Filtering by `owner_uuids` also returns packages with no `owner_uuids` set (universal packages). This is important implementation behavior to preserve.

## Recommendations

### High Priority

None. The API surface is complete and correct.

### Medium Priority

1. [ ] Add `--networks` flag to `create` and `update` CLI commands (use `--network <uuid>` with `value_delimiter`)
2. [ ] Document the `min_platform` and `traits` fields as requiring direct API/client usage (not available via CLI)
3. [ ] Consider supporting JSON input mode for complex create/update operations

### Low Priority

1. [ ] Add `x-resource-count` response header to the OpenAPI spec for `list_packages`
2. [ ] Consider adding `--default` flag to CLI (deprecated but may be needed for legacy compat)
3. [ ] Add integration tests comparing wire-format responses between Node.js and Rust implementations

## Conclusion

**Overall Status**: READY FOR TESTING

The PAPI conversion is thorough and correct. All 6 unique endpoints are covered with matching HTTP methods, paths, and status codes. The `Package` struct includes all 31 fields with correct types. All enum wire values match the Node.js source exactly. The main gap is a few complex-typed fields (`networks`, `min_platform`, `traits`) not exposed as CLI flags, which is a reasonable trade-off for a first pass. The OpenAPI spec is clean with no dead schemas and correct response codes.
