# Phase 2: Generate API Trait

**Standalone skill for generating the Dropshot API trait crate.**

## Inputs

- **Service name**: Name of the service (e.g., "vmapi")
- **Plan file**: `.claude/restify-conversion/<service>/plan.md` (from Phase 1)

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
dropshot = { workspace = true }
http = "1.1"
schemars = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }

[lints.clippy]
unused_async = "allow"
```

### 3. Create Type Modules

For each module in the plan, create type definitions.

**Read reference.md for detailed mapping rules.**

Key patterns:
- Response types: `#[derive(Debug, Serialize, Deserialize, JsonSchema)]`
- Request types: `#[derive(Debug, Deserialize, JsonSchema)]`
- Path params: `#[derive(Debug, Deserialize, JsonSchema)]`
- Use `#[serde(rename_all = "camelCase")]` for JSON compatibility
- Use `#[serde(default)]` for optional fields

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

### 5. Add to Workspace

Edit root `Cargo.toml` to add the API crate to members.

### 6. Register with OpenAPI Manager

Edit `openapi-manager/Cargo.toml` to add dependency.

Edit `openapi-manager/src/main.rs` to register in `all_apis()`.

### 7. Build API Crate

```bash
cargo build -p <service>-api
```

**Fix all errors before proceeding.** Common issues:
- Missing imports (HashMap, etc.)
- Type mismatches
- Invalid derive combinations

### 8. Generate OpenAPI Spec

```bash
cargo run -p openapi-manager -- generate
```

Verify spec created at `openapi-specs/generated/<service>-api.json`.

### 9. Update Plan File

Add to `.claude/restify-conversion/<service>/plan.md`:

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
- [ ] API trait with all endpoints implemented
- [ ] Route conflict resolutions applied
- [ ] Added to workspace Cargo.toml
- [ ] Registered in openapi-manager
- [ ] `cargo build -p <service>-api` succeeds
- [ ] `cargo run -p openapi-manager -- generate` succeeds
- [ ] OpenAPI spec exists
- [ ] Plan file updated

## Error Handling

If build fails:
- Document specific errors in plan.md
- Set Phase 2 status to "FAILED: <reason>"
- Return error to orchestrator for user intervention
