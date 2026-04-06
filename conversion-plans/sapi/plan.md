# SAPI API Conversion Plan

## Source
- Path: `./target/sdc-sapi`
- Version: 2.1.3
- Package name: sapi
- Description: Triton Services and Configuration API

## API Versioning

SAPI supports two API versions via the `Accept-Version` header:
- **~1** (default): Legacy version. Services/instances with `type=agent` are hidden; the `type` field is omitted from responses.
- **~2** (2.0.0+): Includes `type` field in Service and Instance responses. Lists include `type=agent` objects.

The Rust API trait should handle this via a version header or query parameter, or expose all fields unconditionally and let clients filter. Recommend: always include `type` in responses (v2 behavior) since all modern clients use v2.

## Endpoints Summary
- Total: 24
- By method: GET: 11, POST: 5, PUT: 4, DELETE: 4
- Source files:
  - `lib/server/endpoints/index.js` (loglevel endpoints)
  - `lib/server/endpoints/applications.js`
  - `lib/server/endpoints/services.js`
  - `lib/server/endpoints/instances.js`
  - `lib/server/endpoints/manifests.js`
  - `lib/server/endpoints/configs.js`
  - `lib/server/endpoints/cache.js`
  - `lib/server/endpoints/mode.js`
  - `lib/server/endpoints/ping.js`

## Endpoints Detail

### Loglevel (from index.js)
| Method | Path | Handler | Notes |
|--------|------|---------|-------|
| POST | /loglevel | (inline) | Sets log level; body: `{ level }` |
| GET | /loglevel | (inline) | Returns `{ level }` |

### Applications (from applications.js)
| Method | Path | Handler | Notes |
|--------|------|---------|-------|
| POST | /applications | CreateApplication | Required: `name`, `owner_uuid` |
| GET | /applications | ListApplications | Filter: `name`, `owner_uuid`, `include_master` |
| GET | /applications/:uuid | GetApplication | |
| PUT | /applications/:uuid | UpdateApplication | Action dispatch: `update`/`replace`/`delete` |
| DELETE | /applications/:uuid | DeleteApplication | Returns 204 or 404 |

### Services (from services.js)
| Method | Path | Handler | Notes |
|--------|------|---------|-------|
| POST | /services | CreateService | Required: `name`, `application_uuid` |
| GET | /services | ListServices | Filter: `name`, `application_uuid`, `type`, `include_master` |
| GET | /services/:uuid | GetService | |
| PUT | /services/:uuid | UpdateService | Action dispatch: `update`/`replace`/`delete` |
| DELETE | /services/:uuid | DeleteService | Returns 204 or 404 |

### Instances (from instances.js)
| Method | Path | Handler | Notes |
|--------|------|---------|-------|
| POST | /instances | CreateInstance | Required: `service_uuid`; optional: `async` |
| GET | /instances | ListInstances | Filter: `service_uuid`, `type`, `include_master` |
| GET | /instances/:uuid | GetInstance | |
| GET | /instances/:uuid/payload | GetInstancePayload | Returns assembled zone params |
| PUT | /instances/:uuid | UpdateInstance | Action dispatch: `update`/`replace`/`delete` |
| PUT | /instances/:uuid/upgrade | UpgradeInstance | Required: `image_uuid` |
| DELETE | /instances/:uuid | DeleteInstance | Returns 204 or 404 |

### Manifests (from manifests.js)
| Method | Path | Handler | Notes |
|--------|------|---------|-------|
| POST | /manifests | CreateManifest | Required: `name`, `path`, `template` |
| GET | /manifests | ListManifests | Filter: `include_master` |
| GET | /manifests/:uuid | GetManifest | |
| DELETE | /manifests/:uuid | DeleteManifest | Returns 204 or 404 |

### Configs (from configs.js)
| Method | Path | Handler | Notes |
|--------|------|---------|-------|
| GET | /configs/:uuid | GetConfigs | Returns config with ETag; supports conditional requests |

### Cache (from cache.js)
| Method | Path | Handler | Notes |
|--------|------|---------|-------|
| POST | /cache | SyncCache | Returns 204 |

### Mode (from mode.js)
| Method | Path | Handler | Notes |
|--------|------|---------|-------|
| GET | /mode | GetMode | Returns string: `"proto"` or `"full"` |
| POST | /mode | SetMode | Required: `mode` (only `"full"` accepted) |

### Ping (from ping.js)
| Method | Path | Handler | Notes |
|--------|------|---------|-------|
| GET | /ping | Ping | Returns `{ mode, storType, storAvailable }` |

## Route Conflicts

No route conflicts detected. All routes use distinct literal path segments:
- `/applications`, `/services`, `/instances`, `/manifests`, `/configs`, `/cache`, `/mode`, `/ping`, `/loglevel`
- Sub-resources (`/instances/:uuid/payload`, `/instances/:uuid/upgrade`) do not conflict with each other or the parent `:uuid` routes.

## Action Dispatch Endpoints

### PUT /applications/:uuid (UpdateApplication)

The `action` parameter controls how attributes (`params`, `metadata`, `metadata_schema`, `manifests`, `owner_uuid`) are modified. Default action is `update`.

| Action | Behavior | Notes |
|--------|----------|-------|
| update | Merge changes into existing attributes | Default if `action` not specified |
| replace | Replace entire attribute sections | Overwrites `params`, `metadata`, etc. wholesale |
| delete | Delete specified keys from attributes | Removes listed keys from each section |

**Body fields:**
- `params` (optional): Object of zone parameters
- `metadata` (optional): Object of key-value metadata
- `metadata_schema` (optional): Object defining a JSON schema for metadata
- `manifests` (optional): Object of manifest UUID mappings
- `owner_uuid` (optional): New owner UUID (applications only)
- `action` (optional): One of `update`, `replace`, `delete` (default: `update`)

### PUT /services/:uuid (UpdateService)

Same action dispatch pattern as applications.

| Action | Behavior | Notes |
|--------|----------|-------|
| update | Merge changes into existing attributes | Default |
| replace | Replace entire attribute sections | |
| delete | Delete specified keys from attributes | |

**Body fields:**
- `params` (optional): Object
- `metadata` (optional): Object
- `manifests` (optional): Object
- `action` (optional): One of `update`, `replace`, `delete` (default: `update`)

### PUT /instances/:uuid (UpdateInstance)

Same action dispatch pattern as applications and services.

| Action | Behavior | Notes |
|--------|----------|-------|
| update | Merge changes into existing attributes | Default |
| replace | Replace entire attribute sections | |
| delete | Delete specified keys from attributes | |

**Body fields:**
- `params` (optional): Object
- `metadata` (optional): Object
- `manifests` (optional): Object
- `action` (optional): One of `update`, `replace`, `delete` (default: `update`)

**Note:** Unlike CloudAPI's action dispatch (which uses query params), SAPI's action dispatch uses a body field (`action`). All three update endpoints share the same action enum and semantics via the shared `attributes.js` module.

## Types to Define

### Application
```
{
    uuid: Uuid,
    name: String,
    owner_uuid: Uuid,
    params: Option<HashMap<String, Value>>,
    metadata: Option<HashMap<String, Value>>,
    metadata_schema: Option<HashMap<String, Value>>,
    manifests: Option<HashMap<String, String>>,  // name -> manifest UUID
    master: Option<bool>,
}
```

### Service
```
{
    uuid: Uuid,
    name: String,
    application_uuid: Uuid,
    params: Option<HashMap<String, Value>>,
    metadata: Option<HashMap<String, Value>>,
    manifests: Option<HashMap<String, String>>,
    master: Option<bool>,
    type: Option<ServiceType>,  // "vm" or "agent", v2+ only
}
```

### ServiceType (enum)
- `vm` (default)
- `agent`

### Instance
```
{
    uuid: Uuid,
    service_uuid: Uuid,
    params: Option<HashMap<String, Value>>,
    metadata: Option<HashMap<String, Value>>,
    manifests: Option<HashMap<String, String>>,
    master: Option<bool>,
    type: Option<ServiceType>,  // v2+ only
    job_uuid: Option<Uuid>,     // only present on async create
}
```

### Manifest
```
{
    uuid: Uuid,
    name: String,
    path: String,
    template: Value,  // can be string or object
    post_cmd: Option<String>,
    post_cmd_linux: Option<String>,
    version: Option<String>,  // semver, defaults to "1.0.0"
    master: Option<bool>,
}
```

### UpdateAction (enum)
- `update` (default)
- `replace`
- `delete`

### SapiMode (enum/string)
- `proto`
- `full`

### PingResponse
```
{
    mode: String,           // "proto" or "full"
    storType: String,       // e.g. "MorayLocalStorage"
    storAvailable: bool,
}
```

### Config (from configs.js)
The config endpoint returns a dynamically assembled object with `manifests` and `metadata`. The response type is `Value` (freeform JSON) since it depends on the instance's assembled configuration.

### InstancePayload (from instances.js GetInstancePayload)
Returns the assembled zone parameters for provisioning. This is also freeform JSON (`Value`) since it includes merged params from application, service, and instance.

## Field Naming Exceptions

All SAPI fields use **snake_case** in the JSON wire format. There is no camelCase convention in SAPI (unlike CloudAPI). The Rust structs should use `#[serde(rename_all = "snake_case")]` or simply have field names that match directly (since Rust conventions are also snake_case, no rename is needed for most fields).

Specific fields to note:
- `owner_uuid` -- snake_case
- `application_uuid` -- snake_case
- `service_uuid` -- snake_case
- `image_uuid` -- snake_case
- `metadata_schema` -- snake_case
- `post_cmd` -- snake_case
- `post_cmd_linux` -- snake_case
- `job_uuid` -- snake_case
- `include_master` -- snake_case (query parameter)
- `storType` -- **camelCase** (PingResponse only)
- `storAvailable` -- **camelCase** (PingResponse only)

**PingResponse** is the one exception: it uses camelCase for `storType` and `storAvailable` but snake_case-like `mode`. The struct should not use a blanket `rename_all`; instead use explicit `#[serde(rename)]` as needed, or define it as a one-off.

## WebSocket/Channel Endpoints

None. SAPI has no WebSocket or streaming endpoints.

## Planned File Structure

SAPI has a moderate number of endpoints (24) with clear resource groupings. The types are relatively uniform (applications, services, instances share the same attribute pattern). Recommended structure:

```
apis/sapi-api/src/
├── lib.rs          # API trait definition with all endpoints
└── types/
    ├── mod.rs      # Re-exports
    ├── common.rs   # Uuid alias, UpdateAction enum, shared attribute types
    ├── application.rs  # Application, CreateApplication, UpdateApplication
    ├── service.rs      # Service, ServiceType, CreateService, UpdateService
    ├── instance.rs     # Instance, CreateInstance, UpdateInstance, UpgradeInstance
    ├── manifest.rs     # Manifest, CreateManifest
    └── ops.rs          # PingResponse, SapiMode, LogLevel, Config types
```

## Additional Notes

### include_master Query Parameter
Several list endpoints (`ListApplications`, `ListServices`, `ListInstances`, `ListManifests`) accept an `include_master` query parameter. When set and a master SAPI host is configured, the list operation includes data from the master datacenter. This is middleware-level logic that can be modeled as an optional query parameter.

### Configs Endpoint Conditional Requests
`GET /configs/:uuid` computes a SHA-1 ETag from the flattened config and sets the `Etag` header. It uses Restify's `conditionalRequest()` middleware to support `If-None-Match` / `If-Modified-Since`. In Dropshot, this would need manual ETag header handling in the endpoint implementation.

### Create Instance Timeouts
`CreateInstance` sets a 1-hour HTTP timeout and `DeleteInstance` sets 10 minutes. These are server-side connection timeouts that don't map directly to Dropshot but should be documented for the service implementation.

### Async Instance Creation
`CreateInstance` accepts an `async` parameter. When true, the endpoint returns immediately with a `job_uuid` in the response. When false (default), it waits for the VM provisioning to complete before responding.

## Phase 2 Complete

- API crate: `apis/sapi-api/`
- OpenAPI spec: `openapi-specs/generated/sapi-api.json`
- Endpoint count: 28 (across 15 paths)
- Build status: SUCCESS

### Type modules created
- `types/common.rs` -- Uuid alias, UuidPath, UpdateAction enum, UpdateAttributesBody, ServiceType enum
- `types/application.rs` -- Application, CreateApplicationBody, UpdateApplicationBody, ListApplicationsQuery
- `types/service.rs` -- Service, CreateServiceBody, UpdateServiceBody, ListServicesQuery
- `types/instance.rs` -- Instance, CreateInstanceBody, CreateInstanceQuery, UpdateInstanceBody, UpgradeInstanceBody, ListInstancesQuery
- `types/manifest.rs` -- Manifest, CreateManifestBody, ListManifestsQuery
- `types/ops.rs` -- SapiMode, PingResponse, ModeResponse, SetModeBody, LogLevelResponse, SetLogLevelBody

### Design decisions
- Action dispatch for update endpoints uses typed `UpdateApplicationBody`/`UpdateServiceBody`/`UpdateInstanceBody` structs rather than `serde_json::Value`, since the action+fields pattern is uniform and well-defined
- `ServiceType` enum has `#[serde(other)] Unknown` variant for forward compatibility
- `SapiMode` enum has `#[serde(other)] Unknown` variant for forward compatibility
- PingResponse uses explicit `#[serde(rename)]` for camelCase `storType`/`storAvailable` fields
- Service and Instance `type` fields use `#[serde(rename = "type")]` since `type` is a Rust keyword
- `sync_cache` returns `HttpResponseUpdatedNoContent` (204) matching the original
- `get_instance_payload` and `get_config` return `serde_json::Value` since their responses are freeform JSON

## Phase 3 Complete

- Client crate: `clients/internal/sapi-client/`
- Build status: SUCCESS
- Typed wrappers: NO -- SAPI update endpoints use typed body structs with an `action` field directly (no action-dispatch pattern requiring TypedClient wrappers)
- Re-exports: All API crate types re-exported from `sapi_client` for CLI consumers
- Client-generator: registered in `client-generator/src/main.rs` with `configure_sapi` (Builder interface, Merged tags, schemars::JsonSchema derive)

## Phase Status
- [x] Phase 1: Analyze - COMPLETE
- [x] Phase 2: Generate API - COMPLETE
- [x] Phase 3: Generate Client - COMPLETE
- [ ] Phase 4: Generate CLI
- [ ] Phase 5: Validate
