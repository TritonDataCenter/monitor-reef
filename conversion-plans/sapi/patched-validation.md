# SAPI Patched OpenAPI Spec Validation Report

Comparison of `openapi-specs/patched/sapi-api.json` against
`target/sdc-sapi/lib/server/endpoints/` Node.js source.

## Summary

The patched spec contains **3 differences** from the generated spec
(all intentional patches to `/mode` and `/loglevel`). One of these patches
has a discrepancy with the Node.js source. Several create endpoints
also have a status code mismatch with Node.js behavior.

---

## Endpoint-by-Endpoint Validation

### Applications

#### POST /applications (CreateApplication)
- **Spec**: 201, returns `Application`
- **Node.js**: `res.send(app)` -- Restify default status is **200** when sending an object
- **DISCREPANCY**: Spec says 201 but Node.js sends 200. The handler does
  not set a status code explicitly, so Restify uses the default (200).
- **Request body fields**: Spec has `name`, `owner_uuid` (required), plus
  optional `params`, `metadata`, `metadata_schema`, `manifests`. Node.js
  also reads `uuid` and `master` from `req.params` -- these are missing
  from `CreateApplicationBody`.
- **DISCREPANCY**: `uuid` and `master` fields are missing from the create
  body schema.

#### GET /applications (ListApplications)
- **Spec**: 200, returns `Array_of_Application`. Query params: `include_master`, `name`, `owner_uuid`.
- **Node.js**: Matches. `res.send(apps)` returns 200 with array.
- **OK**

#### GET /applications/{uuid} (GetApplication)
- **Spec**: 200, returns `Application`.
- **Node.js**: `res.send(app)` -- matches.
- **OK**

#### PUT /applications/{uuid} (UpdateApplication)
- **Spec**: 200, returns `Application`. Body: `action`, `params`, `metadata`,
  `metadata_schema`, `manifests`, `owner_uuid`.
- **Node.js**: `res.send(app)` -- matches. Reads same fields from `req.params`.
- **OK**

#### DELETE /applications/{uuid} (DeleteApplication)
- **Spec**: 204 on success.
- **Node.js**: `res.send(204)` -- matches.
- **OK**

---

### Services

#### POST /services (CreateService)
- **Spec**: 201, returns `Service`.
- **Node.js**: `res.send(serialize(svc, ...))` -- Restify default is **200**.
- **DISCREPANCY**: Spec says 201 but Node.js sends 200.
- **Request body fields**: Spec has `name`, `application_uuid` (required),
  plus optional `params`, `metadata`, `manifests`, `type`. Node.js also
  reads `uuid` and `master` -- these are missing from `CreateServiceBody`.
- **DISCREPANCY**: `uuid` and `master` fields are missing from the create
  body schema.

#### GET /services (ListServices)
- **Spec**: 200, returns `Array_of_Service`. Query params: `application_uuid`,
  `include_master`, `name`, `type`.
- **Node.js**: Matches. Filters by name, application_uuid, type, include_master.
- **OK**

#### GET /services/{uuid} (GetService)
- **Spec**: 200, returns `Service`.
- **Node.js**: Matches.
- **OK**

#### PUT /services/{uuid} (UpdateService)
- **Spec**: 200, returns `Service`. Body: `action`, `params`, `metadata`, `manifests`.
- **Node.js**: Matches. Does NOT read `owner_uuid` (unlike application update).
- **OK** (correctly omits `owner_uuid` unlike UpdateApplicationBody)

#### DELETE /services/{uuid} (DeleteService)
- **Spec**: 204 on success.
- **Node.js**: `res.send(204)` -- matches.
- **OK**

---

### Instances

#### POST /instances (CreateInstance)
- **Spec**: 201, returns `Instance`. Query param: `async`.
- **Node.js**: `res.send(serialize(inst, ...))` -- Restify default is **200**.
- **DISCREPANCY**: Spec says 201 but Node.js sends 200.
- **Query param `async`**: Node.js reads `req.params.async` which with
  Restify's `mapParams` merges query and body. Spec puts it in query, which
  is reasonable.
- **Request body fields**: Spec has `service_uuid` (required), plus optional
  `uuid`, `params`, `metadata`, `manifests`. Node.js also reads `master` --
  this is missing from `CreateInstanceBody`.
- **DISCREPANCY**: `master` field is missing from the create body schema.

#### GET /instances (ListInstances)
- **Spec**: 200, returns `Array_of_Instance`. Query params: `include_master`,
  `service_uuid`, `type`.
- **Node.js**: Matches.
- **OK**

#### GET /instances/{uuid} (GetInstance)
- **Spec**: 200, returns `Instance`.
- **Node.js**: Matches.
- **OK**

#### GET /instances/{uuid}/payload (GetInstancePayload)
- **Spec**: 200, returns freeform JSON (empty schema `{}`).
- **Node.js**: `res.send(params)` returns freeform object. Also sends 404
  if params is null.
- **OK** (freeform schema is appropriate since the payload shape depends on
  the application/service/instance hierarchy)

#### PUT /instances/{uuid} (UpdateInstance)
- **Spec**: 200, returns `Instance`. Body: `action`, `params`, `metadata`, `manifests`.
- **Node.js**: Matches.
- **OK**

#### PUT /instances/{uuid}/upgrade (UpgradeInstance)
- **Spec**: 200, returns `Instance`. Body: `image_uuid` (required).
- **Node.js**: `res.send(serialize(inst, ...))` returns 200. Reads
  `req.params.image_uuid`.
- **OK**

#### DELETE /instances/{uuid} (DeleteInstance)
- **Spec**: 204 on success.
- **Node.js**: `res.send(204)` -- matches.
- **OK**

---

### Manifests

#### POST /manifests (CreateManifest)
- **Spec**: 201, returns `Manifest`.
- **Node.js**: `res.send(mfest)` -- Restify default is **200**.
- **DISCREPANCY**: Spec says 201 but Node.js sends 200.
- **Request body fields**: Spec has `name`, `path`, `template` (required),
  plus optional `post_cmd`, `post_cmd_linux`, `version`. Node.js also reads
  `uuid` and `master` -- these are missing from `CreateManifestBody`.
- **DISCREPANCY**: `uuid` and `master` fields are missing from the create
  body schema.

#### GET /manifests (ListManifests)
- **Spec**: 200, returns `Array_of_Manifest`. Query param: `include_master`.
- **Node.js**: Matches. Note: unlike other list endpoints, manifests does
  not take filter params (no name or other filters).
- **OK**

#### GET /manifests/{uuid} (GetManifest)
- **Spec**: 200, returns `Manifest`.
- **Node.js**: Matches.
- **OK**

#### DELETE /manifests/{uuid} (DeleteManifest)
- **Spec**: 204 on success.
- **Node.js**: `res.send(204)` -- matches.
- **OK**

---

### Mode (Patched Endpoints)

#### GET /mode (GetMode)
- **Spec (patched)**: 200, returns `{"type": "string"}` (bare JSON string).
- **Spec (generated)**: 200, returns `{"$ref": "#/components/schemas/ModeResponse"}`
  (a `{"mode": "proto"}` object).
- **Node.js**: `res.send(proto_mode ? 'proto' : 'full')` -- Restify
  `res.send(string)` sends the string as a bare JSON-quoted string
  (e.g., `"proto"`) with `content-type: application/json`.
- **Patch is CORRECT**: The patched spec correctly models this as a plain
  string response. The generated spec was wrong (wrapped in a
  `ModeResponse` object).

#### POST /mode (SetMode)
- **Spec (patched)**: 204 on success, no response body. Body: `SetModeBody`
  with `mode` field (SapiMode enum).
- **Spec (generated)**: 200 on success.
- **Node.js**: `res.send(204)` -- sends 204 with no body.
- **Patch is CORRECT**: The patched spec correctly uses 204.
- **Request body**: Node.js reads `req.params.mode`. The spec models this as
  `SetModeBody { mode: SapiMode }` which is correct.

---

### Loglevel (Patched + In index.js)

#### GET /loglevel (GetLogLevel)
- **Spec**: 200, returns `LogLevelResponse { level: string }`.
- **Node.js**: `res.send({ level: model.log.level() })` -- Bunyan's
  `log.level()` returns a **number** (10=trace, 20=debug, 30=info, etc.),
  not a string.
- **DISCREPANCY**: The `level` field in `LogLevelResponse` is typed as
  `string`, but Node.js actually returns a number (e.g., `{"level": 30}`).
  The schema should use `"type": "integer"` or have no type constraint.

#### POST /loglevel (SetLogLevel)
- **Spec (patched)**: 200, no response body. Body: `SetLogLevelBody { level: string }`.
- **Spec (generated)**: 200 with a response body content section.
- **Node.js**: `res.send()` -- Restify's `res.send()` with no arguments
  sends **200** with an empty body.
- **Patch is CORRECT**: The patched spec removes the response body content
  (since `res.send()` with no args sends an empty body). Status 200 is correct.
- **Request body**: Node.js reads `req.params.level` -- a string representing
  the Bunyan log level name (e.g., "debug", "info"). The spec models this
  correctly as a required string field.

---

### Cache

#### POST /cache (SyncCache)
- **Spec**: 204, no response body.
- **Node.js**: `res.send(204)` -- matches.
- **OK**

---

### Ping

#### GET /ping (Ping)
- **Spec**: 200, returns `PingResponse { mode: string, storType: string,
  storAvailable: boolean }`.
- **Node.js**: `res.send(storAvailable ? 200 : 500, { mode: ..., storType: ..., storAvailable: ... })`.
- **Note**: The Node.js handler can return **500** when storage is unavailable,
  but the spec only documents 200 (plus generic 4XX/5XX error responses).
  The 500 case returns the same `PingResponse` body shape (not an error
  object), so the 5XX error response schema does not cover this case.
- **MINOR DISCREPANCY**: When storage is unavailable, Node.js returns 500
  with a `PingResponse` body (not an `Error` body). The spec's `5XX` response
  references the `Error` schema, which would not match.

---

## Discrepancy Summary

### Status Code Mismatches (Create Endpoints)

All four create endpoints use `res.send(obj)` without an explicit status
code. Restify defaults to **200**. The spec declares **201** for all of them.

| Endpoint | Spec | Node.js Actual |
|----------|------|----------------|
| POST /applications | 201 | 200 |
| POST /services | 201 | 200 |
| POST /instances | 201 | 200 |
| POST /manifests | 201 | 200 |

**Recommendation**: Either change the spec to 200 to match Node.js, or
decide that the Rust implementation should "fix" this to return 201 (which
is more RESTful). If the latter, document this as an intentional behavioral
change.

### Missing Fields in Create Body Schemas

Several create endpoints accept `uuid` and/or `master` fields that are
not in the spec's request body schemas:

| Schema | Missing Fields |
|--------|---------------|
| CreateApplicationBody | `uuid`, `master` |
| CreateServiceBody | `uuid`, `master` |
| CreateInstanceBody | `master` |
| CreateManifestBody | `uuid`, `master` |

Note: `CreateInstanceBody` already has `uuid` but is missing `master`.

**Recommendation**: Add these optional fields. `uuid` allows the caller to
specify the UUID for the new resource (otherwise auto-generated). `master`
marks records as originating from a remote datacenter.

### GET /loglevel Response Type

The `LogLevelResponse.level` field is typed as `string` in the spec, but
Bunyan's `log.level()` returns a numeric level (integer). The wire format
is `{"level": 30}`, not `{"level": "info"}`.

**Recommendation**: Change the `level` field type to `integer`, or use a
typeless schema to accept either.

### GET /ping 500 Response Shape

When storage is unavailable, Node.js returns HTTP 500 with a `PingResponse`
body (not an `Error` body). The spec's `5XX` response references the `Error`
schema.

**Recommendation**: Add a separate 500 response that uses the `PingResponse`
schema, or document this behavior.

---

## Patch Validation (3 Patched Endpoints)

| Patch | Correct? | Notes |
|-------|----------|-------|
| GET /mode: changed from `ModeResponse` object to bare `string` | YES | Node.js sends a bare JSON string, not a wrapped object |
| POST /mode: changed from 200 to 204 | YES | Node.js uses `res.send(204)` |
| POST /loglevel: removed response body content | YES | Node.js uses `res.send()` (empty body) |

All three patches correctly align the spec with Node.js behavior.

---

## No Missing Endpoints

All routes registered in the Node.js source are present in the spec:

| Node.js Route Registration | Spec Path | Status |
|---------------------------|-----------|--------|
| `POST /loglevel` (index.js) | `/loglevel` POST | Present |
| `GET /loglevel` (index.js) | `/loglevel` GET | Present |
| `POST /applications` | `/applications` POST | Present |
| `GET /applications` | `/applications` GET | Present |
| `GET /applications/:uuid` | `/applications/{uuid}` GET | Present |
| `PUT /applications/:uuid` | `/applications/{uuid}` PUT | Present |
| `DEL /applications/:uuid` | `/applications/{uuid}` DELETE | Present |
| `POST /services` | `/services` POST | Present |
| `GET /services` | `/services` GET | Present |
| `GET /services/:uuid` | `/services/{uuid}` GET | Present |
| `PUT /services/:uuid` | `/services/{uuid}` PUT | Present |
| `DEL /services/:uuid` | `/services/{uuid}` DELETE | Present |
| `POST /instances` | `/instances` POST | Present |
| `GET /instances` | `/instances` GET | Present |
| `GET /instances/:uuid` | `/instances/{uuid}` GET | Present |
| `GET /instances/:uuid/payload` | `/instances/{uuid}/payload` GET | Present |
| `PUT /instances/:uuid` | `/instances/{uuid}` PUT | Present |
| `PUT /instances/:uuid/upgrade` | `/instances/{uuid}/upgrade` PUT | Present |
| `DEL /instances/:uuid` | `/instances/{uuid}` DELETE | Present |
| `POST /manifests` | `/manifests` POST | Present |
| `GET /manifests` | `/manifests` GET | Present |
| `GET /manifests/:uuid` | `/manifests/{uuid}` GET | Present |
| `DEL /manifests/:uuid` | `/manifests/{uuid}` DELETE | Present |
| `GET /configs/:uuid` | `/configs/{uuid}` GET | Present |
| `GET /mode` | `/mode` GET | Present |
| `POST /mode` | `/mode` POST | Present |
| `GET /ping` | `/ping` GET | Present |
| `POST /cache` | `/cache` POST | Present |

All 28 Node.js routes are accounted for in the spec. No endpoints are missing.

---

## Schema Notes (Non-Issues)

- **`ModeResponse` schema**: Still present in the patched spec's schema
  definitions even though GET /mode no longer references it. This is harmless
  but could be cleaned up.
- **`template` field (Manifest/CreateManifestBody)**: Has no `type` constraint
  in the spec, which correctly models Node.js behavior (templates can be
  strings or JSON objects).
- **`metadata_schema` on Service**: Not present in `UpdateServiceBody`, which
  matches Node.js (services don't have metadata_schema, only applications do).
- **Manifests have no update endpoint**: Correct -- Node.js does not register
  a PUT handler for manifests.
