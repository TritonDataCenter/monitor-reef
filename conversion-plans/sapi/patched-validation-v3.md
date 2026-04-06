# SAPI Patched OpenAPI Spec Validation Report (v3)

Third validation pass of `openapi-specs/patched/sapi-api.json` against the
Node.js SAPI implementation in `target/sdc-sapi/`.

## Changes Since v2

Issues found and fixed in v1/v2 that were verified as resolved:

- GET /mode: patched to return bare string (not JSON object) -- confirmed
- POST /mode: natively 204 no content -- confirmed
- POST /loglevel: patched to return empty 200 -- confirmed
- Create endpoints: now return 200 (not 201) -- confirmed
- LogLevelResponse.level: now `serde_json::Value` (Bunyan returns integer) -- confirmed
- Added `uuid`/`master` fields to all create body types -- confirmed
- PingResponse.mode: now SapiMode enum -- confirmed
- PingResponse.stor_type: now StorageType enum -- confirmed
- Removed unused ModeResponse type -- confirmed (no longer in spec)
- GET /ping 500: documented Progenitor limitation -- confirmed

---

## Endpoint-by-Endpoint Audit

### Applications (`lib/server/endpoints/applications.js`)

| Node.js Route | Spec Path | Method | Status |
|---|---|---|---|
| `POST /applications` | `/applications` | POST | PASS |
| `GET /applications` | `/applications` | GET | PASS |
| `GET /applications/:uuid` | `/applications/{uuid}` | GET | PASS |
| `PUT /applications/:uuid` | `/applications/{uuid}` | PUT | PASS |
| `DELETE /applications/:uuid` | `/applications/{uuid}` | DELETE | PASS |

**CreateApplication (POST /applications)**
- Node.js body fields: `uuid`, `name`, `owner_uuid`, `params`, `metadata`, `metadata_schema`, `manifests`, `master`
- Spec `CreateApplicationBody`: `uuid`, `name`, `owner_uuid`, `params`, `metadata`, `metadata_schema`, `manifests`, `master`
- Required (Node.js `APPLICATION_KEYS`): `name`, `owner_uuid`
- Required (spec): `name`, `owner_uuid`
- Response: `res.send(app)` -> 200 + JSON body
- Spec: 200 + Application
- **PASS**

**ListApplications (GET /applications)**
- Query params (Node.js): `name`, `owner_uuid`, `include_master`
- Query params (spec): `name`, `owner_uuid`, `include_master`
- Response: 200 + array of Application
- **PASS**

**GetApplication (GET /applications/:uuid)**
- Path param: `uuid`; Response: 200 + Application
- **PASS**

**UpdateApplication (PUT /applications/:uuid)**
- Body fields (Node.js): `params`, `metadata`, `metadata_schema`, `manifests`, `owner_uuid`, `action`
- Spec `UpdateApplicationBody`: `params`, `metadata`, `metadata_schema`, `manifests`, `owner_uuid`, `action`
- Response: 200 + Application
- **PASS**

**DeleteApplication (DELETE /applications/:uuid)**
- Node.js: `res.send(204)` on success, `res.send(404)` on not found
- Spec: 204 on success, 4XX on error
- **PASS**

---

### Services (`lib/server/endpoints/services.js`)

| Node.js Route | Spec Path | Method | Status |
|---|---|---|---|
| `POST /services` | `/services` | POST | PASS |
| `GET /services` | `/services` | GET | PASS |
| `GET /services/:uuid` | `/services/{uuid}` | GET | PASS |
| `PUT /services/:uuid` | `/services/{uuid}` | PUT | PASS |
| `DELETE /services/:uuid` | `/services/{uuid}` | DELETE | PASS |

**CreateService (POST /services)**
- Node.js body fields: `uuid`, `name`, `application_uuid`, `params`, `metadata`, `manifests`, `master`, `type`
- Spec `CreateServiceBody`: `uuid`, `name`, `application_uuid`, `params`, `metadata`, `manifests`, `master`, `type`
- Required (Node.js `SERVICE_KEYS`): `name`, `application_uuid`
- Required (spec): `application_uuid`, `name`
- Response: 200 + serialized Service (via `serialize()`)
- Node.js `serialize()` returns: `uuid`, `name`, `application_uuid`, `params`, `metadata`, `manifests`, `master`, and conditionally `type` (v2+)
- Spec `Service` schema: all those fields, `type` nullable
- **PASS**

**ListServices (GET /services)**
- Query params (Node.js): `name`, `application_uuid`, `type`, `include_master`
- Query params (spec): `application_uuid`, `include_master`, `name`, `type`
- Response: 200 + array of Service
- **PASS**

**UpdateService (PUT /services/:uuid)**
- Body fields (Node.js): `params`, `metadata`, `manifests`, `action`
- Spec `UpdateServiceBody`: `params`, `metadata`, `manifests`, `action`
- Note: correctly omits `metadata_schema` (only applications have it)
- Response: 200 + Service
- **PASS**

**DeleteService (DELETE /services/:uuid)**
- Node.js: `res.send(204)` on success, `res.send(404)` on not found
- Spec: 204 on success, 4XX on error
- **PASS**

---

### Instances (`lib/server/endpoints/instances.js`)

| Node.js Route | Spec Path | Method | Status |
|---|---|---|---|
| `POST /instances` | `/instances` | POST | PASS |
| `GET /instances` | `/instances` | GET | PASS |
| `GET /instances/:uuid` | `/instances/{uuid}` | GET | PASS |
| `GET /instances/:uuid/payload` | `/instances/{uuid}/payload` | GET | PASS |
| `PUT /instances/:uuid` | `/instances/{uuid}` | PUT | PASS |
| `PUT /instances/:uuid/upgrade` | `/instances/{uuid}/upgrade` | PUT | PASS |
| `DELETE /instances/:uuid` | `/instances/{uuid}` | DELETE | PASS |

**CreateInstance (POST /instances)**
- Node.js body fields: `uuid`, `service_uuid`, `params`, `metadata`, `manifests`, `master`
- Spec `CreateInstanceBody`: `uuid`, `service_uuid`, `params`, `metadata`, `manifests`, `master`
- Note: correctly omits `type` (instances.js create does not read `req.params.type`, unlike services.js)
- Query param (Node.js): `async` (`req.params.async || false`)
- Query param (spec): `async` (nullable boolean)
- Required (Node.js `INSTANCE_KEYS`): `service_uuid`
- Required (spec): `service_uuid`
- Response: 200 + Instance (with optional `job_uuid` on async)
- **PASS**

**ListInstances (GET /instances)**
- Query params (Node.js): `service_uuid`, `type`, `include_master`
- Query params (spec): `include_master`, `service_uuid`, `type`
- **PASS**

**GetInstancePayload (GET /instances/:uuid/payload)**
- Response: 200 + freeform JSON, or 404 if null
- Spec: 200 + freeform `{}` schema, 4XX for errors
- **PASS**

**UpgradeInstance (PUT /instances/:uuid/upgrade)**
- Body fields (Node.js): `image_uuid` (required -- returns MissingParameterError if absent)
- Spec `UpgradeInstanceBody`: `image_uuid` (required)
- Response: 200 + Instance
- **PASS**

**UpdateInstance (PUT /instances/:uuid)**
- Body fields (Node.js): `params`, `metadata`, `manifests`, `action`
- Spec `UpdateInstanceBody`: `params`, `metadata`, `manifests`, `action`
- Response: 200 + Instance
- **PASS**

**DeleteInstance (DELETE /instances/:uuid)**
- Node.js: `res.send(204)` on success, `res.send(404)` on not found
- Spec: 204 on success, 4XX on error
- **PASS**

---

### Manifests (`lib/server/endpoints/manifests.js`)

| Node.js Route | Spec Path | Method | Status |
|---|---|---|---|
| `POST /manifests` | `/manifests` | POST | PASS |
| `GET /manifests` | `/manifests` | GET | PASS |
| `GET /manifests/:uuid` | `/manifests/{uuid}` | GET | PASS |
| `DELETE /manifests/:uuid` | `/manifests/{uuid}` | DELETE | PASS |

**CreateManifest (POST /manifests)**
- Node.js body fields: `uuid`, `name`, `path`, `template`, `post_cmd`, `post_cmd_linux`, `version`, `master`
- Spec `CreateManifestBody`: `uuid`, `name`, `path`, `template`, `post_cmd`, `post_cmd_linux`, `version`, `master`
- Required (Node.js `MANIFEST_KEYS`): `name`, `path`, `template`
- Required (spec): `name`, `path`, `template`
- Response: 200 + Manifest
- **PASS**

**ListManifests (GET /manifests)**
- Query param (Node.js): `include_master`
- Query param (spec): `include_master`
- Note: correctly has no name/uuid filter (unlike applications/services)
- Response: 200 + array of Manifest
- **PASS**

**Note**: No `PUT /manifests/:uuid` exists in Node.js. Spec correctly omits it.

---

### Configs (`lib/server/endpoints/configs.js`)

| Node.js Route | Spec Path | Method | Status |
|---|---|---|---|
| `GET /configs/:uuid` | `/configs/{uuid}` | GET | PASS |

- Response: 200 + freeform JSON with ETag header, or 404 if not found
- Spec: 200 + freeform `{}` schema, 4XX for errors
- **PASS**

---

### Mode (`lib/server/endpoints/mode.js`)

| Node.js Route | Spec Path | Method | Status |
|---|---|---|---|
| `GET /mode` | `/mode` | GET | PASS |
| `POST /mode` | `/mode` | POST | PASS |

**GetMode (GET /mode)**
- Node.js: `res.send(proto_mode ? 'proto' : 'full')` -> bare JSON string
- Spec: 200 + `{ "type": "string" }` schema
- **PASS**

**SetMode (POST /mode)**
- Node.js: reads `req.params.mode`, validates `=== 'full'`, returns `res.send(204)`
- Spec: `SetModeBody` with required `mode` (SapiMode enum), response 204
- **PASS**

---

### Ping (`lib/server/endpoints/ping.js`)

| Node.js Route | Spec Path | Method | Status |
|---|---|---|---|
| `GET /ping` | `/ping` | GET | PASS (see notes) |

- Node.js: `res.send(storAvailable ? 200 : 500, { mode, storType, storAvailable })`
- Spec: 200 + PingResponse, 5XX error ref
- **PASS** (500-with-PingResponse-body is a documented limitation)

---

### Cache (`lib/server/endpoints/cache.js`)

| Node.js Route | Spec Path | Method | Status |
|---|---|---|---|
| `POST /cache` | `/cache` | POST | PASS |

- No request body; Node.js: `res.send(204)`
- Spec: 204 response, no body
- **PASS**

---

### Log Level (inline in `lib/server/endpoints/index.js`)

| Node.js Route | Spec Path | Method | Status |
|---|---|---|---|
| `GET /loglevel` | `/loglevel` | GET | PASS |
| `POST /loglevel` | `/loglevel` | POST | PASS |

**GET /loglevel**
- Node.js: `res.send({ level: model.log.level() })` -- Bunyan returns integer
- Spec: 200 + `LogLevelResponse` with typeless `level` field
- **PASS**

**POST /loglevel**
- Node.js: `res.send()` -- empty 200
- Spec: 200 with no body content
- **PASS**

---

## Endpoint Completeness

From `index.js` `attachTo()` and individual `attachTo` functions:

| Source | Endpoints | In Spec? |
|---|---|---|
| `POST /loglevel`, `GET /loglevel` (inline) | 2 | Yes |
| `applications.attachTo()` | 5 | Yes |
| `cache.attachTo()` | 1 | Yes |
| `configs.attachTo()` | 1 | Yes |
| `instances.attachTo()` | 7 | Yes |
| `manifests.attachTo()` | 4 | Yes |
| `mode.attachTo()` | 2 | Yes |
| `ping.attachTo()` | 1 | Yes |
| `services.attachTo()` | 5 | Yes |

**Total: 28 endpoints in Node.js, 28 endpoints in spec. None missing, none extra.**

---

## Schema Audit

### Dead/Unused Schemas

All 21 schemas in the spec are referenced at least once:

| Schema | Reference Count | Status |
|---|---|---|
| Application | 4 | Used |
| CreateApplicationBody | 1 | Used |
| CreateInstanceBody | 1 | Used |
| CreateManifestBody | 1 | Used |
| CreateServiceBody | 1 | Used |
| Error | 1 | Used |
| Instance | 5 | Used |
| LogLevelResponse | 1 | Used |
| Manifest | 3 | Used |
| PingResponse | 1 | Used |
| SapiMode | 2 | Used |
| Service | 4 | Used |
| ServiceType | 5 | Used |
| SetLogLevelBody | 1 | Used |
| SetModeBody | 1 | Used |
| StorageType | 1 | Used |
| UpdateAction | 3 | Used |
| UpdateApplicationBody | 1 | Used |
| UpdateInstanceBody | 1 | Used |
| UpdateServiceBody | 1 | Used |
| UpgradeInstanceBody | 1 | Used |

**No unused/dead schemas remain.** The previously unused `ModeResponse` has been removed.

### Enum Verification

**UpdateAction** (`#/components/schemas/UpdateAction`):
- Spec variants: `update`, `replace`, `delete`
- Node.js (applications.js, services.js, instances.js): validates `action !== 'update' && action !== 'replace' && action !== 'delete'`
- **PASS**: Exact match.

**ServiceType** (`#/components/schemas/ServiceType`):
- Spec variants: `vm`, `agent`, `unknown`
- Node.js (services.js): `type` field, default filter `filters.type = 'vm'` for v1 clients
- Rust: `#[serde(rename_all = "lowercase")]` on `ServiceType` enum -> `Vm` serializes as `"vm"`, `Agent` as `"agent"`
- **PASS**: Wire values match.

**SapiMode** (`#/components/schemas/SapiMode`):
- Spec variants: `proto`, `full`, `unknown`
- Node.js (mode.js): `proto_mode ? 'proto' : 'full'`
- Rust: `#[serde(rename_all = "lowercase")]` on `SapiMode` enum -> `Proto` serializes as `"proto"`, `Full` as `"full"`
- **PASS**: Wire values match.

**StorageType** (`#/components/schemas/StorageType`):
- Spec variants: `LocalStorage`, `MorayStorage`, `MorayLocalStorage`, `TransitionStorage`, `Unknown`
- Node.js (ping.js): `model.stor.constructor.name` returns the JavaScript constructor function name
- JavaScript constructor names (from `lib/server/stor/`):
  - `local.js`: `function LocalStorage(config)` -> `"LocalStorage"`
  - `moray.js`: `function MorayStorage(config, metricsManager)` -> `"MorayStorage"`
  - `moray_local.js`: `function MorayLocalStorage(opts)` -> `"MorayLocalStorage"`
  - `transition.js`: `function TransitionStorage(opts)` -> `"TransitionStorage"`
- Rust: No `rename_all` on `StorageType` enum, so variant names are used directly as wire values (PascalCase matches JS constructor names)
- **PASS**: All four constructor names match exactly.

---

## Re-export Validation (`clients/internal/sapi-client/src/lib.rs`)

All 24 re-exported types from `sapi_api` were verified to exist as `pub` items in `apis/sapi-api/src/types/`:

| Re-export | Source File | Exists? |
|---|---|---|
| `Application` | `types/application.rs` | Yes |
| `CreateApplicationBody` | `types/application.rs` | Yes |
| `CreateInstanceBody` | `types/instance.rs` | Yes |
| `CreateInstanceQuery` | `types/instance.rs` | Yes |
| `CreateManifestBody` | `types/manifest.rs` | Yes |
| `CreateServiceBody` | `types/service.rs` | Yes |
| `Instance` | `types/instance.rs` | Yes |
| `ListApplicationsQuery` | `types/application.rs` | Yes |
| `ListInstancesQuery` | `types/instance.rs` | Yes |
| `ListManifestsQuery` | `types/manifest.rs` | Yes |
| `ListServicesQuery` | `types/service.rs` | Yes |
| `LogLevelResponse` | `types/ops.rs` | Yes |
| `Manifest` | `types/manifest.rs` | Yes |
| `PingResponse` | `types/ops.rs` | Yes |
| `SapiMode` | `types/ops.rs` | Yes |
| `Service` | `types/service.rs` | Yes |
| `ServiceType` | `types/common.rs` | Yes |
| `SetLogLevelBody` | `types/ops.rs` | Yes |
| `SetModeBody` | `types/ops.rs` | Yes |
| `StorageType` | `types/ops.rs` | Yes |
| `UpdateAction` | `types/common.rs` | Yes |
| `UpdateApplicationBody` | `types/application.rs` | Yes |
| `UpdateInstanceBody` | `types/instance.rs` | Yes |
| `UpdateServiceBody` | `types/service.rs` | Yes |
| `UpgradeInstanceBody` | `types/instance.rs` | Yes |
| `Uuid` | `types/common.rs` | Yes |
| `UuidPath` | `types/common.rs` | Yes |

**All 27 re-exports are valid.** (27 actual items -- some grouped on same line in source.)

---

## Remaining Observations (Non-Blocking)

### 1. Ping 500 Response Body Shape (DOCUMENTED LIMITATION)

When storage is unavailable, Node.js sends HTTP 500 with a PingResponse body (not an Error body). The spec's 5XX response references the generic Error schema. A Progenitor-generated client would attempt to deserialize 500 responses as Error, which would fail for this case. This was documented in v1/v2 and remains a known limitation.

**Severity**: Low. Known behavioral oddity in the original Node.js service.

### 2. Bare 404 Responses on Delete/Config/Payload (MINOR)

Several endpoints return `res.send(404)` with no body:
- `DELETE /applications/:uuid` (ObjectNotFoundError)
- `DELETE /services/:uuid` (ObjectNotFoundError)
- `DELETE /instances/:uuid` (ObjectNotFoundError)
- `DELETE /manifests/:uuid` (ObjectNotFoundError)
- `GET /configs/:uuid` (no config found)
- `GET /instances/:uuid/payload` (no payload found)

The spec's 4XX error ref expects an Error JSON body. A strict client might fail deserializing a bodyless 404. This is a standard Restify pattern and affects many Triton APIs.

**Severity**: Low. Consistent pattern across the API.

### 3. `CreateInstance` `async` Parameter Source (INFORMATIONAL)

Node.js reads `req.params.async` which under `mapParams: true` merges query and body. The spec models it as query-only, which matches real client usage (e.g., `node-sdc-clients`).

**Severity**: None.

### 4. `Manifest.template` Typeless Field (INTENTIONAL)

Both `CreateManifestBody.template` and `Manifest.template` have no `type` constraint in the schema. This is intentional: Node.js accepts string templates or JSON objects.

**Severity**: None.

### 5. Unused `UpdateAttributesBody` in Rust Code (CODE-ONLY)

`apis/sapi-api/src/types/common.rs` defines `UpdateAttributesBody` which is never referenced anywhere in the codebase. This is dead Rust code, not a spec issue. It could be removed in a cleanup pass.

**Severity**: Cosmetic (code only, not spec).

---

## Summary

| Category | Result |
|---|---|
| Endpoints present | 28/28 -- all match |
| HTTP methods | All correct |
| Path parameters | All correct |
| Query parameters | All correct |
| Request body fields | All correct |
| Required fields | All correct |
| Response status codes | All correct |
| Response body shapes | All correct |
| Enum wire values | All correct |
| StorageType variants vs JS constructors | All 4 match exactly |
| Dead/unused schemas | None (ModeResponse removed) |
| Client re-exports | All 27 valid |
| Previously fixed issues | All confirmed resolved |

**Overall: The patched spec is a faithful representation of the Node.js SAPI API. No new discrepancies were found in this third pass. All issues from v1/v2 have been verified as resolved. The remaining observations are non-blocking known limitations or informational notes.**
