# PAPI (Packages API) Conversion Plan

## Source
- Path: `./target/sdc-papi`
- Version: 7.2.5
- Package name: sdc-papi
- Description: Triton Packages API -- manages package definitions (RAM, CPU, disk, etc.) used by other services to provision VMs.

## Endpoints Summary
- Total: 8 (counting HEAD variants separately; 6 unique handler paths)
- By method: GET: 3, HEAD: 2, POST: 1, PUT: 1, DELETE: 1
- Source files: `lib/papi.js` (all routes defined in one file)

## Endpoints Detail

### Packages (from `lib/papi.js`)

| Method | Path | Handler | Request Body | Response | Notes |
|--------|------|---------|-------------|----------|-------|
| GET | `/packages` | `listPkgs` | -- | `200`: `Vec<Package>` (array) | Query params for filtering, sorting, pagination. Sets `x-resource-count` header. Returns `404` (numeric, no body!) if empty filter. |
| HEAD | `/packages` | `listPkgs` | -- | Same as GET (headers only) | |
| POST | `/packages` | `loadPkg`, `postPkg` | Package fields (schema-driven) | `201`: `Package` object | Sets `Location` header. Pre-checks via `loadPkg` if UUID already exists. |
| GET | `/packages/:uuid` | `loadPkg`, `getPkg` | -- | `200`: `Package` object | Optional `owner_uuids` query param for access filtering. |
| HEAD | `/packages/:uuid` | `loadPkg`, `getPkg` | -- | Same as GET (headers only) | |
| PUT | `/packages/:uuid` | `loadPkg`, `updatePkg` | Mutable package fields | `200`: `Package` object | Rejects immutable field changes unless `force=true`. |
| DELETE | `/packages/:uuid` | `loadPkg`, `deletePkg` | -- | `204`: No content | Requires `force=true` query param; otherwise returns `405 BadMethod`. |

### Ping (from `lib/papi.js`)

| Method | Path | Handler | Response | Notes |
|--------|------|---------|----------|-------|
| GET | `/ping` | `ping` | `200`: `PingResponse` object | Returns `{ pid, backend, backend_error? }` |

## Route Conflicts

None. All paths are distinct:
- `/packages` (collection)
- `/packages/:uuid` (single resource)
- `/ping` (health check)

## Action Dispatch Endpoints

None. PAPI does not use the action dispatch pattern.

## Package Schema (from production SAPI template)

The package object is defined by a JSON schema in `etc/config.json` (templated by SAPI). All fields use **snake_case** in the wire format. The `v` field is added server-side (hardcoded to `1`).

### Required Fields (immutable unless noted)

| Field | Type | Immutable | Notes |
|-------|------|-----------|-------|
| `uuid` | UUID | yes | Unique. Auto-generated if not provided on create. |
| `name` | string | yes | Must match `/^[a-zA-Z0-9]([a-zA-Z0-9_\-.]+)?[a-zA-Z0-9]$/`, no consecutive `_`/`-`/`.` |
| `version` | string | yes | Semver version string |
| `active` | boolean | no | Whether package can be used for provisioning |
| `cpu_cap` | number | yes | Sometimes optional (configurable via `IGNORE_CPU_CAP` SAPI metadata) |
| `max_lwps` | number | yes | Min: 250 |
| `max_physical_memory` | number | yes | Min: 64 (MiB) |
| `max_swap` | number | yes | Min: 128 (MiB), must be >= `max_physical_memory` |
| `quota` | number | yes | Min: 1024 (MiB), must be multiple of 1024 |
| `zfs_io_priority` | number | yes | Range: 0..16383 |

### Optional Fields

| Field | Type | Immutable | Notes |
|-------|------|-----------|-------|
| `brand` | string | -- | One of: `bhyve`, `joyent`, `joyent-minimal`, `kvm`, `lx` |
| `owner_uuids` | `[UUID]` | no | Array of owner UUIDs; absent = universal package |
| `vcpus` | number | yes | Range: 1..64 |
| `default` | boolean | no | Deprecated (SDC 6.5 compat) |
| `group` | string | no | Package grouping |
| `description` | string | no | Human-readable description |
| `common_name` | string | no | Display name in Portal |
| `networks` | `[UUID]` | no | Array of network UUIDs |
| `os` | string | yes | Operating system |
| `min_platform` | object (hash) | no | `{ "sdc_version": "platform_date" }` -- freeform JSON object |
| `parent` | string | no | Name/UUID of parent package |
| `traits` | object (hash) | no | Freeform JSON object for DAPI traits |
| `fss` | number | no | CPU shares (aka `cpu_shares`) |
| `cpu_burst_ratio` | double (f64) | no | Float, stored as string in Moray |
| `ram_ratio` | double (f64) | no | Float, stored as string in Moray |
| `created_at` | date (ISO 8601 string) | -- | Set automatically by Moray pre-trigger |
| `updated_at` | date (ISO 8601 string) | -- | Set automatically by Moray pre-trigger |
| `billing_tag` | string | no | Opaque billing string |
| `alloc_server_spread` | string | no | One of: `min-ram`, `random`, `min-owner` |
| `flexible_disk` | boolean | no | Enables flexible disk mode (bhyve) |
| `disks` | `[object]` | no | Array of `{ size: number | "remaining" }` objects; only valid when `flexible_disk=true` |

### Server-Added Field

| Field | Type | Notes |
|-------|------|-------|
| `v` | number | Always `1`, added by handler before response |

### Special Create Parameters (not part of schema)

| Param | Type | Notes |
|-------|------|-------|
| `skip_validation` | boolean | Skips validation step on create/update |
| `force` | boolean | On PUT: allows modifying immutable fields. On DELETE: allows deletion. |
| `filter` | string | On GET `/packages`: raw LDAP filter string, bypasses param-based filter |

### List Query Parameters

| Param | Type | Notes |
|-------|------|-------|
| `limit` | number | Limit result count |
| `offset` | number | Skip N results |
| `sort` | string | Field name to sort by |
| `order` | string | `ASC` or `DESC` (default: `ASC`) |
| `filter` | string | Raw LDAP search filter |
| Any schema field | varies | Used to build LDAP equality filter |
| `owner_uuids` | string/array | Special: also returns packages with no `owner_uuids` |

## Restify Response Patterns

| Endpoint | Pattern | Wire Behavior | Dropshot Mapping |
|----------|---------|---------------|------------------|
| `GET /packages` | `res.send(200, packages)` | 200 + JSON array | `HttpResponseOk<Vec<Package>>` |
| `GET /packages` (empty filter) | `res.send(404)` | 404 numeric, no JSON body | Error response (needs special handling) |
| `POST /packages` | `res.send(201, p)` | 201 + JSON object | `HttpResponseCreated<Package>` |
| `GET /packages/:uuid` | `res.send(200, req.pkg)` | 200 + JSON object | `HttpResponseOk<Package>` |
| `PUT /packages/:uuid` | `res.send(200, savedPkg)` | 200 + JSON object | `HttpResponseOk<Package>` |
| `DELETE /packages/:uuid` | `res.send(204)` | 204 no content | `HttpResponseUpdatedNoContent` |
| `GET /ping` | `res.send(data)` | 200 + JSON object | `HttpResponseOk<PingResponse>` |

## Patch Requirements

- **`GET /packages` 404 on empty filter**: The handler calls `res.send(404)` with a bare numeric status code (no JSON body) when the search filter is `undefined`. This is unusual -- it returns a 404 without a body for a list endpoint. In the Dropshot API, this edge case could be handled as an error response or by returning an empty array. Recommend returning an empty array for consistency (this case only triggers with an empty `[]` JSON array parameter).
- **`x-resource-count` header**: List endpoint sets a custom `x-resource-count` response header with the total count. Dropshot supports custom headers but they need explicit definition in the OpenAPI spec.

## Enum Opportunities

### `Brand` enum
- **Field**: `brand` on Package
- **Values**: `bhyve`, `joyent`, `joyent-minimal`, `kvm`, `lx`
- **Source**: `VALID_BRANDS` in `lib/validations.js`
- **Forward compat**: Yes, needs `#[serde(other)] Unknown` since new brands could be added
- **Notes**: Values `joyent-minimal` needs `#[serde(rename = "joyent-minimal")]`

### `AllocServerSpread` enum
- **Field**: `alloc_server_spread` on Package
- **Values**: `min-ram`, `random`, `min-owner`
- **Source**: `VALID_SPREADS` in `lib/validations.js`
- **Forward compat**: Yes, needs `#[serde(other)] Unknown`
- **Notes**: Values `min-ram` and `min-owner` need `#[serde(rename = "...")]`

### `BackendStatus` enum
- **Field**: `backend` on PingResponse
- **Values**: `up`, `down`
- **Source**: `ping()` handler in `lib/papi.js`
- **Forward compat**: Yes, needs `#[serde(other)] Unknown`

### `SortOrder` enum (query parameter)
- **Field**: `order` query parameter on ListPackages
- **Values**: `ASC`, `DESC`
- **Forward compat**: No (client-provided, fixed set)

### `DiskSize` enum/union
- **Field**: `size` within `disks[].size`
- **Values**: A positive integer (MiB) OR the string `"remaining"`
- **Source**: `lib/validations.js` disk validation
- **Notes**: This is a tagged union, not a simple enum. Needs `#[serde(untagged)]` with integer and string variants.

## Types to Define

### `Package` (response/request struct)
- All fields from schema above
- All fields use **snake_case** wire format (no `rename_all` needed, or use `rename_all = "snake_case"`)
- `v` field: server-added, always `1`
- `min_platform`: `Option<HashMap<String, String>>`
- `traits`: `Option<serde_json::Value>` (freeform JSON object)
- `disks`: `Option<Vec<DiskSpec>>`
- `cpu_burst_ratio` / `ram_ratio`: `Option<f64>`

### `DiskSpec`
- `size`: `Option<DiskSize>` -- either a number or `"remaining"`

### `PingResponse`
- `pid`: `u32` (process ID -- in Rust service, could be u32 or omitted)
- `backend`: `BackendStatus` enum
- `backend_error`: `Option<String>`

### `CreatePackageRequest` / `UpdatePackageRequest`
- Create: all schema fields (required + optional)
- Update: all mutable fields as `Option<T>`, plus `force` and `skip_validation`
- Both accept `skip_validation: Option<bool>`

### `ListPackagesQuery`
- `limit`: `Option<u64>`
- `offset`: `Option<u64>`
- `sort`: `Option<String>`
- `order`: `Option<SortOrder>`
- `filter`: `Option<String>`
- Plus all schema fields as optional query params for filtering

### `GetPackageQuery`
- `owner_uuids`: `Option<String>` (JSON-encoded array or single UUID)

### `DeletePackageQuery`
- `force`: `Option<bool>`

### `ValidationError` (error response body)
- `code`: `String`
- `message`: `String`
- `errors`: `Option<Vec<FieldError>>`

### `FieldError`
- `field`: `String`
- `code`: `String` (values: `Invalid`, `Missing`)
- `message`: `String`

## Field Naming Exceptions

All package fields use **snake_case** in the JSON wire format. There is no `translate()` function -- packages are stored in Moray and returned as-is (after decode which just handles type conversions for dates and doubles).

Key observations:
- All multi-word fields are already snake_case: `owner_uuids`, `cpu_cap`, `max_lwps`, `max_physical_memory`, `max_swap`, `common_name`, `min_platform`, `zfs_io_priority`, `cpu_burst_ratio`, `ram_ratio`, `created_at`, `updated_at`, `billing_tag`, `alloc_server_spread`, `flexible_disk`
- No camelCase fields exist in the wire format
- The struct should either use `#[serde(rename_all = "snake_case")]` or no `rename_all` (since Rust field names will naturally be snake_case)

## WebSocket/Channel Endpoints

None. PAPI is a pure REST API with no WebSocket or streaming endpoints.

## Planned File Structure

PAPI is a small API (6 unique endpoints). A single-module API crate with a types submodule is appropriate:

```
apis/papi-api/src/
    lib.rs          # API trait with 6 endpoints (list, get, create, update, delete packages + ping)
    types.rs        # Package, PingResponse, query/request types, enums
```

```
services/papi-service/src/
    lib.rs          # Trait implementation
    backend.rs      # Moray backend abstraction
```

```
clients/internal/papi-client/
    src/
        lib.rs          # Re-exports
        generated.rs    # Progenitor-generated client
```

## Additional Notes

### Restify `mapParams: true` Behavior
PAPI uses `bodyParser({ overrideParams: true, mapParams: true })`, which merges query params, URL params, and body into a single `req.params`. This means:
- For POST/PUT, body fields and query params are merged (body takes precedence via `overrideParams`)
- For GET/DELETE, query params are available via `req.params`
- The `force` and `skip_validation` flags can come from either query string or body

### Moray-Specific Details (not relevant to API surface)
- Reserved PostgreSQL column names `default` and `group` are stored as `_default` and `_group` in Moray but exposed as `default` and `group` in the API
- Doubles (`cpu_burst_ratio`, `ram_ratio`) are stored as strings in Moray but served as numbers
- Dates (`created_at`, `updated_at`) are stored as epoch milliseconds in Moray but served as ISO 8601 strings

### `owner_uuids` Special Search Behavior
When filtering by `owner_uuids`, PAPI also returns packages that have **no** `owner_uuids` set (universal packages). This is important semantics to preserve.

### HEAD Endpoints
Both `GET /packages` and `GET /packages/:uuid` have `HEAD` variants that use the same handler. Dropshot does not have explicit HEAD support -- it automatically handles HEAD requests for GET endpoints.

## Phase 2 Complete

- API crate: `apis/papi-api/`
- OpenAPI spec: `openapi-specs/generated/papi-api.json`
- Endpoint count: 6 (ping, list_packages, get_package, create_package, update_package, delete_package)
- Enums: Brand, AllocServerSpread, BackendStatus, SortOrder, DiskSize (with DiskSizeRemaining)
- Build status: SUCCESS
- OpenAPI generation: SUCCESS

## Phase 3 Complete

- Client crate: `clients/internal/papi-client/`
- Build status: SUCCESS
- Typed wrappers: NO (no action dispatch endpoints)
- ValueEnum patches: Brand, AllocServerSpread, BackendStatus, SortOrder
- Re-exports: All API crate types (enums, structs, Uuid)

## Phase 4 Complete

- CLI crate: `cli/papi-cli/`
- Binary name: `papi`
- Commands implemented: 6
- Build status: SUCCESS
- Full workspace build: SUCCESS
- OpenAPI check: SUCCESS

### CLI Commands
- `papi ping` - Health check endpoint
- `papi list` - List packages (with filters: --name, --version, --active, --brand, --owner-uuids, --group, --os, --flexible-disk, --filter, --sort, --order, --limit, --offset, --raw)
- `papi get <uuid>` - Get a package by UUID (--owner-uuids, --raw)
- `papi create` - Create a new package (all required and optional fields as flags, --raw)
- `papi update <uuid>` - Update a package (mutable fields as flags, --force, --skip-validation, --raw)
- `papi delete <uuid>` - Delete a package (--force required)

## Phase 5 Complete - CONVERSION VALIDATED

- Validation report: `conversion-plans/papi/validation.md`
- Overall status: READY FOR TESTING
- Endpoint coverage: 6/6 (100%)
- Issues found: 0 blocking, 3 minor (CLI missing --networks on create/update, complex-type fields not exposed as CLI flags)

## Phase Status
- [x] Phase 1: Analyze - COMPLETE
- [x] Phase 2: Generate API - COMPLETE
- [x] Phase 3: Generate Client - COMPLETE
- [x] Phase 4: Generate CLI - COMPLETE
- [x] Phase 5: Validate - COMPLETE

## Conversion Complete

The PAPI API has been converted to Rust. See validation.md for details.

### Generated Artifacts
- API crate: `apis/papi-api/`
- Client crate: `clients/internal/papi-client/`
- CLI crate: `cli/papi-cli/`
- OpenAPI spec: `openapi-specs/generated/papi-api.json`

### Next Steps
1. Run integration tests against live Node.js service
2. Add `--networks` flag to create/update CLI commands
3. Deploy Rust service for parallel testing
