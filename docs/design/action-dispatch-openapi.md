<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

Copyright 2026 Edgecast Cloud LLC.
-->

# Action-Dispatch Endpoints and OpenAPI Schema Limitations

## Background

Several CloudAPI endpoints overload a single HTTP POST to handle multiple
unrelated operations, distinguished by an `action` parameter. This is a
Restify-era pattern: Node.js CloudAPI uses `mapParams: true` to merge query
string and request body parameters into a single `req.params` object, then
dispatches to different handler functions based on `req.params.action`.

## Affected Endpoints

| Endpoint | Actions | Body varies? |
|----------|---------|-------------|
| `POST /{account}/machines/{machine}` | start, stop, reboot, resize, rename, enable\_firewall, disable\_firewall, enable\_deletion\_protection, disable\_deletion\_protection | Yes (resize needs `package`, rename needs `name`, others are empty or have `origin`) |
| `POST /{account}/images/{dataset}` | update, export, clone, import-from-datacenter | Yes (update has metadata fields, export has `manta_path`, clone is empty, import uses query params) |
| `POST /{account}/images` | (none), import-from-datacenter | Yes (create has `machine`/`name`/`version`, import uses query params with empty body) |
| `POST /{account}/volumes/{id}` | update | No (single action) |
| `POST /{account}/disks/{id}` | resize | No (single action) |

## The Problem

These endpoints use `TypedBody<serde_json::Value>` in their Dropshot trait
signatures because each action expects a different set of fields in the request
body. This causes the generated OpenAPI spec to emit empty `{}` schemas for
these request bodies.

External code generators (oapi-codegen for Go, Progenitor for Rust) see the
empty schema and produce untyped request bodies (`interface{}`,
`serde_json::Value`). Consumers like the triton-go client library cannot
replace their hand-written input types with generated equivalents for these
endpoints.

## Decision: Keep `serde_json::Value`

We evaluated three alternatives and decided to keep the status quo:

**Option 1: Flat struct with all fields optional.** A single struct containing
every possible field across all actions, with everything optional. This gives
generators *something*, but the resulting types are semantically weak -- a bag
of optional fields where the valid combinations depend on which `action` query
parameter you send. This is arguably worse than a hand-written type with
proper required/optional semantics per action.

**Option 2: Internally-tagged enum (`#[serde(tag = "action")]`).** schemars
generates a proper `oneOf` discriminated union. However, the `action` parameter
is not always in the request body -- it can be a query parameter, and different
clients send it in different places (some in query, some in body, some in
both). An internally-tagged enum requires `action` to always be in the body,
which doesn't match the real wire protocol.

**Option 3: Split into separate Dropshot endpoints per action.** Cleanest for
OpenAPI but diverges from the actual HTTP API surface. CloudAPI has one POST
endpoint; splitting it into multiple would mean the spec no longer describes
what's actually deployed.

**Why none of these are worth it:** The action-dispatch pattern is a legacy
quirk of a small number of endpoints (5 out of 150+). Rust consumers already
have full type safety through `TypedClient` wrapper methods (e.g.,
`client.start_machine()`, `client.export_image()`). Go consumers need to
maintain hand-written input types for these endpoints, but this is a bounded
problem that doesn't grow. The cost of any fix (weaker types, protocol
mismatch, or spec divergence) outweighs the benefit.

## Typed Request Structs

The typed request structs for each action *do* exist in
`apis/cloudapi-api/src/types/` and are re-exported by `cloudapi-client`. They
are used by the `TypedClient` wrapper methods for serialization. They just
don't appear in the OpenAPI spec because the Dropshot trait endpoint signature
uses `serde_json::Value`.

| Type | Used by |
|------|---------|
| `StartMachineRequest`, `StopMachineRequest`, `ResizeMachineRequest`, ... | `TypedClient::start_machine()`, etc. |
| `CreateImageRequest`, `UpdateImageRequest`, `ExportImageRequest`, ... | `TypedClient::create_image()`, etc. |
| `UpdateVolumeRequest` | `TypedClient::update_volume()` |
| `ResizeDiskRequest` | `TypedClient::resize_disk()` |
