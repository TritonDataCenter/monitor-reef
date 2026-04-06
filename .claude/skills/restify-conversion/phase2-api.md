<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

Copyright 2026 Edgecast Cloud LLC.
-->

# Phase 2: Generate API Trait

**Standalone skill for generating the Dropshot API trait crate.**

## Inputs

- **Service name**: Name of the service (e.g., "vmapi")
- **Plan file**: `conversion-plans/<service>/plan.md` (from Phase 1)

## Outputs

- **API crate**: `apis/<service>-api/`
- **OpenAPI spec**: `openapi-specs/generated/<service>-api.json`
- **Updated plan file** with Phase 2 status

## Prerequisites

Read and verify the plan file exists and Phase 1 is complete.

## Tasks

### 1. Create API Crate Directory

```bash
mkdir -p apis/<service>-api/src
```

### 2. Create Cargo.toml

```toml
[package]
name = "<service>-api"
version = "<version-from-plan>"
edition.workspace = true
description = "<Service> API trait definition"

[dependencies]
clap = { workspace = true }  # Needed if any enums derive clap::ValueEnum
dropshot = { workspace = true }
http = "1.1"
schemars = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }

[lints.clippy]
unused_async = "allow"
```

**Note:** Include `clap` only if enums in this crate will be used directly as CLI
`#[arg(value_enum)]` parameters (e.g., `MachineState`, `VolumeState`). If only the
Progenitor-generated copy needs `ValueEnum`, use `build.rs` patches instead (see Phase 3).

### 3. Create Type Modules

For each module in the plan, create type definitions.

**Read reference.md for detailed mapping rules.**

Key patterns:
- Response types: `#[derive(Debug, Serialize, Deserialize, JsonSchema)]`
- Request types: `#[derive(Debug, Deserialize, JsonSchema)]`
- Path params: `#[derive(Debug, Deserialize, JsonSchema)]`
- State/status enums: `#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]`
  - Include `PartialEq, Eq` so enums can be compared with `==` in CLI code
  - If the enum will be a CLI `--state`/`--sort` arg, also add `clap::ValueEnum`
- Use `#[serde(rename_all = "camelCase")]` **only if the Node.js translate() function outputs camelCase** — see reference.md "JSON Field Naming" for details
- Use `#[serde(default)]` for optional fields
- Use `#[serde(rename = "field-name")]` for fields with hyphens or non-standard casing

#### Forward-Compatible Enums

State and status enums **must** include a `#[serde(other)] Unknown` catch-all variant.
This prevents deserialization failures when the server adds new states:

```rust
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MachineState {
    Provisioning,
    Running,
    Stopped,
    Failed,
    // ... all known variants
    /// Catch-all for states added after this client was compiled
    #[serde(other)]
    Unknown,
}
```

This pattern is mandatory for any enum that represents server-side state. See
`apis/cloudapi-api/src/types/changefeed.rs` for the established example.

**Don't assume camelCase everywhere.** Check the plan's "Field Naming Exceptions" section for fields that use snake_case or other conventions in the actual API.

### 3b. Use Enums From Phase 1 Plan

Check the plan's "Enum Opportunities" section and create typed enums for every
identified field. Common patterns:

- Response fields with known value sets → enum on the response struct field
- Response fields that echo a request enum → reuse the same enum type
- `constructor.name` values → enum with variants matching the class names exactly
- Boolean-like string fields (`"proto"` / `"full"`) → enum

Every enum should have `#[serde(other)] Unknown` unless the set of values is
truly fixed and controlled by the client (like `UpdateAction`).

Cross-reference response structs against request enums. If `PingResponse` has a
`mode` field and `SetModeBody` uses a `SapiMode` enum, the response should use the
same `SapiMode` type — not `String`.

### 3c. Match Restify Response Patterns to Dropshot Types

Check the plan's "Patch Requirements" section and choose the right Dropshot return
type for each endpoint:

| Plan says | Dropshot return type | Needs patch? |
|-----------|---------------------|-------------|
| `res.send(obj)` (default 200) | `HttpResponseOk<T>` | No |
| `res.send(201, obj)` | `HttpResponseCreated<T>` | No |
| `res.send(204)` | `HttpResponseUpdatedNoContent` | No |
| `res.send()` (empty 200) | `HttpResponseOk<T>` | Yes: remove content from 200 |
| `res.send('string')` (bare text) | `HttpResponseOk<SomeEnum>` | Yes: replace schema with `{"type":"string"}` |
| `res.send(cond ? 200 : 500, obj)` | `HttpResponseOk<T>` | Document limitation |

Restify's default status for `res.send(obj)` is 200, not 201. Use `HttpResponseOk`,
not `HttpResponseCreated`, for create endpoints unless the Node.js code explicitly
sets status 201.

Don't create wrapper types just for Dropshot. If an endpoint returns a bare string
that maps to an existing enum, use the enum directly as the return type and patch the
spec to a string afterward. This avoids dead types like `ModeResponse`.

### 3d. Create Action-Specific Request Types (CRITICAL)

For action dispatch endpoints, create a **separate typed struct for each action**:

1. **Read the plan's action dispatch table** from Phase 1
2. **For each action**, create a struct with:
   - All required fields as non-Option types
   - All optional fields as `Option<T>` with `#[serde(default)]`
   - Doc comments explaining each field's purpose and defaults
3. **Place in a dedicated module** (e.g., `src/vms.rs` alongside the VmAction enum)

**DO NOT** just use `serde_json::Value` or skip "simple" actions - even start/stop have `idempotent` options.

**File organization for many actions:**
```
apis/<service>-api/src/
├── lib.rs           # Trait definition, re-exports
├── types.rs         # Shared types (Vm, Nic, Disk, etc.)
├── vms.rs           # VM endpoint types including:
│   - VmAction enum
│   - VmActionQuery
│   - StartVmRequest
│   - StopVmRequest
│   - KillVmRequest (with signal field!)
│   - RebootVmRequest
│   - ReprovisionVmRequest
│   - UpdateVmRequest
│   - AddNicsRequest
│   - UpdateNicsRequest
│   - RemoveNicsRequest
│   - CreateSnapshotRequest
│   - RollbackSnapshotRequest
│   - DeleteSnapshotRequest
│   - CreateDiskRequest (size can be number OR "remaining")
│   - ResizeDiskRequest (with dangerous_allow_shrink!)
│   - DeleteDiskRequest
│   - MigrateVmRequest
├── migrations.rs    # Migration endpoint types
└── jobs.rs          # Job endpoint types
```

**Example action request types:**
```rust
/// Request body for `kill` action
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct KillVmRequest {
    /// Signal to send (default: SIGKILL). Examples: "SIGTERM", "SIGKILL"
    #[serde(default)]
    pub signal: Option<String>,
    /// If true, don't error if VM is already stopped
    #[serde(default)]
    pub idempotent: Option<bool>,
}

/// Request body for `create_disk` action (bhyve only)
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct CreateDiskRequest {
    /// Disk size in MB, or the literal "remaining" for remaining space
    pub size: serde_json::Value,  // Can be number or "remaining"
    /// PCI slot (optional, auto-assigned if not specified)
    #[serde(default)]
    pub pci_slot: Option<String>,
}
```

### 3c. Verify Field Types Against Actual Data

Before finalizing types, verify against test fixtures or existing clients:

- **String vs Enum**: If a field has a fixed set of values (e.g., `brand: "bhyve" | "kvm" | "lx"`), use an enum
- **Required vs Optional**: Check if fields are always present or sometimes missing
- **Primitive types**: `tags` might be `HashMap<String, Value>` not `HashMap<String, String>` if values can be booleans/numbers

### 4. Create lib.rs with API Trait

```rust
// SPDX-License-Identifier: MPL-2.0
// Copyright 2025 ...

//! <Service> API trait definition

pub mod types;
// pub mod <other modules>;

pub use types::*;

#[dropshot::api_description]
pub trait <Service>Api {
    type Context: Send + Sync + 'static;

    // Endpoints from plan...
}
```

**Apply route conflict resolutions** from the plan (treating literals as special values).

**Add WebSocket/channel endpoints** from the plan using `#[channel]` attribute:
```rust
#[channel {
    protocol = WEBSOCKETS,
    path = "/path/{id}/watch",
    tags = ["resource"],
}]
async fn watch_resource(
    rqctx: RequestContext<Self::Context>,
    path: Path<ResourcePath>,
    upgraded: WebsocketConnection,
) -> WebsocketChannelResult;
```

### 5. Add to Workspace

Edit root `Cargo.toml` to add the API crate to members.

### 6. Register with OpenAPI Manager

Edit `openapi-manager/Cargo.toml` to add dependency.

Edit `openapi-manager/src/main.rs` to register in `all_apis()`.

### 7. Build API Crate

```bash
make format package-build PACKAGE=<service>-api
```

**Fix all errors before proceeding.** Common issues:
- Missing imports (HashMap, etc.)
- Type mismatches
- Invalid derive combinations

### 8. Generate OpenAPI Spec

```bash
make openapi-generate
```

Verify spec created at `openapi-specs/generated/<service>-api.json`.

### 9. Update Plan File

Add to `conversion-plans/<service>/plan.md`:

```markdown
## Phase 2 Complete

- API crate: `apis/<service>-api/`
- OpenAPI spec: `openapi-specs/generated/<service>-api.json`
- Endpoint count: <N>
- Build status: SUCCESS

## Phase Status
- [x] Phase 1: Analyze - COMPLETE
- [x] Phase 2: Generate API - COMPLETE
- [ ] Phase 3: Generate Client
- [ ] Phase 4: Generate CLI
- [ ] Phase 5: Validate
```

## Success Criteria

Phase 2 is complete when:
- [ ] API crate structure created
- [ ] All type modules implemented
- [ ] Every enum from Phase 1 plan created (check "Enum Opportunities" section)
- [ ] Response structs use typed enums (not String) for fields with known value sets
- [ ] Dropshot return types match Restify patterns (200 not 201 for default `res.send(obj)`)
- [ ] No unnecessary wrapper types (don't create FooResponse just for Dropshot)
- [ ] **Every action has a dedicated typed request struct** (check plan's action dispatch table)
- [ ] Action-specific optional fields captured (idempotent, sync, signal, etc.)
- [ ] All create body fields included (including hidden optional fields like uuid, master)
- [ ] Field naming exceptions from plan applied (snake_case fields, hyphenated names)
- [ ] WebSocket/channel endpoints implemented (check plan)
- [ ] API trait with all endpoints implemented
- [ ] Route conflict resolutions applied
- [ ] Added to workspace Cargo.toml
- [ ] Registered in openapi-manager
- [ ] `make format package-build PACKAGE=<service>-api` succeeds
- [ ] `make openapi-generate` succeeds
- [ ] OpenAPI spec exists
- [ ] Plan file updated

## Error Handling

If build fails:
- Document specific errors in plan.md
- Set Phase 2 status to "FAILED: <reason>"
- Return error to orchestrator for user intervention

## After Phase Completion

The orchestrator will run:
```bash
make check
git add apis/<service>-api/ openapi-specs/generated/<service>-api.json conversion-plans/<service>/plan.md Cargo.toml Cargo.lock openapi-manager/
git commit -m "Add <service> API trait (Phase 2)"
```
