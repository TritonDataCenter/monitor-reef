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
| `POST /{account}/machines/{machine}/disks/{disk}` | resize | No (single action) |

## The Problem

These endpoints use `TypedBody<serde_json::Value>` in their Dropshot trait
signatures because each action expects a different set of fields in the request
body. This causes the generated OpenAPI spec to emit empty `{}` schemas for
these request bodies.

External code generators (oapi-codegen for Go, Progenitor for Rust) see the
empty schema and produce untyped request bodies (`interface{}`,
`serde_json::Value`).

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

**Why none of these change the endpoint signature:** The action-dispatch pattern
is a legacy quirk of a small number of endpoints (5 out of 150+). Changing the
Dropshot trait signature would either weaken the types (Option 1), break wire
compatibility (Option 2), or misrepresent the HTTP surface (Option 3). Instead,
we inject the per-action schemas separately (see below).

## Solution: Schema Injection

The per-action request structs are defined in `apis/cloudapi-api/src/types/`
with `JsonSchema` derives. The `patch_inject_action_request_schemas()` transform
in `openapi-manager/src/transforms.rs` uses schemars to emit their schemas into
`components.schemas` of the patched OpenAPI spec. The endpoint signatures still
take a free-form body — the patch only makes the shapes *nameable*, not
*enforced* at the spec level.

This gives downstream code generators named, typed structs for each action
without changing the Dropshot trait or diverging from the wire protocol. The
generated spec (`openapi-specs/generated/`) retains the empty `{}` bodies; the
patched spec (`openapi-specs/patched/`) adds the schemas.

### Rust consumers

`cloudapi-client` provides `TypedClient` wrapper methods (e.g.,
`client.start_machine()`, `client.export_image()`) that accept the typed
request structs and serialize them via the `ActionBody` flatten pattern.

### Go consumers

The Go client (`clients/external/cloudapi-client/golang/`) uses oapi-codegen
with `generate-unused-schemas: true` (see `cloudapi.cfg.yaml`) to emit the
injected schemas as Go structs. A typed wrapper layer
(`clients/external/cloudapi-client/golang/typed/`) provides one method per
action, taking the generated request struct.

## Typed Request Structs

The per-action request structs are defined in `apis/cloudapi-api/src/types/`
and injected into the patched OpenAPI spec's `components.schemas`. They are
re-exported by `cloudapi-client` (Rust) and generated by oapi-codegen (Go).

| Type | Rust `TypedClient` method | Go `typed.Client` method |
|------|--------------------------|--------------------------|
| `StartMachineRequest` | `start_machine()` | `StartMachine()` |
| `StopMachineRequest` | `stop_machine()` | `StopMachine()` |
| `RebootMachineRequest` | `reboot_machine()` | `RebootMachine()` |
| `ResizeMachineRequest` | `resize_machine()` | `ResizeMachine()` |
| `RenameMachineRequest` | `rename_machine()` | `RenameMachine()` |
| `EnableFirewallRequest` | `enable_firewall()` | `EnableFirewall()` |
| `DisableFirewallRequest` | `disable_firewall()` | `DisableFirewall()` |
| `EnableDeletionProtectionRequest` | `enable_deletion_protection()` | `EnableDeletionProtection()` |
| `DisableDeletionProtectionRequest` | `disable_deletion_protection()` | `DisableDeletionProtection()` |
| `UpdateImageRequest` | `update_image_metadata()` | `UpdateImageMetadata()` |
| `ExportImageRequest` | `export_image()` | `ExportImage()` |
| `CloneImageRequest` | `clone_image()` | `CloneImage()` |
| `ImportImageRequest` | `import_image_from_datacenter()` | `ImportImage()` |
| `UpdateVolumeRequest` | `update_volume_name()` | `UpdateVolume()` |
| `ResizeDiskRequest` | `resize_disk()` | `ResizeDisk()` |
