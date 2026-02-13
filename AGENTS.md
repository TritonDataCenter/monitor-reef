<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

Copyright 2025 Edgecast Cloud LLC.
-->

# Triton Rust Monorepo

<!-- Note: CLAUDE.md is a symlink to this file. Edit AGENTS.md directly, not CLAUDE.md. -->

Trait-based OpenAPI-driven migration of Node.js services to Rust. API traits (Dropshot) → OpenAPI specs → client libraries (Progenitor) → CLIs.

**Tooling**: The `/restify-conversion` skill automates Node.js Restify → Rust Dropshot conversion. Migration plans are in `conversion-plans/`.

## Architecture

- **`apis/`** — API trait definitions (fast to compile): `cloudapi-api`, `vmapi-api`, `bugview-api`, `jira-api`
- **`services/`** — Trait implementations: `bugview-service`, `jira-stub-server`
- **`clients/internal/`** — Progenitor-generated clients: `cloudapi-client`, `vmapi-client`, `bugview-client`, `jira-client`
- **`cli/`** — CLIs: `triton-cli`, `vmapi-cli`, `bugview-cli`, `manatee-echo-resolver`
- **`libs/`** — Shared crates: `cueball*`, `fast`, `libmanta`, `moray`, `rust-utils`, `triton-auth`
- **`client-generator/`** — Progenitor-based code generator
- **`openapi-manager/`** — Spec management (dropshot-api-manager)
- **`openapi-specs/generated/`** — Generated specs (checked into git)
- **`openapi-specs/patched/`** — Post-generation patched specs

## Core Technologies

- **[Dropshot](https://github.com/oxidecomputer/dropshot)**: HTTP server framework with API trait support
- **[Dropshot API Manager](https://github.com/oxidecomputer/dropshot-api-manager)**: OpenAPI document management and versioning
- **[Progenitor](https://github.com/oxidecomputer/progenitor)**: OpenAPI client generator for Rust
- **[Oxide RFD 479](https://rfd.shared.oxide.computer/rfd/0479)**: Dropshot API Traits design documentation

## Atomic Commit Workflow

1. Implement a single, focused change
2. `make format`
3. `make package-test PACKAGE=<pkg>` and `make package-build PACKAGE=<pkg>`
4. `make audit` (check for vulnerabilities)
5. Commit only files related to this change — one commit = one logical change

**Known audit exceptions** (pre-existing, do not block commits): RUSTSEC-2023-0071 (rsa), RUSTSEC-2026-0009 (time), RUSTSEC-2024-0436 (paste), RUSTSEC-2025-0134 (rustls-pemfile).

## Common Make Targets

Run `make help` to see all available targets. Key commands:

| Target | Description |
|--------|-------------|
| `make build` | Build all crates |
| `make test` | Run all tests |
| `make check` | Run all validation (tests + OpenAPI check) |
| `make format` | Format all code |
| `make lint` | Run clippy linter |
| `make audit` | Security audit dependencies |
| `make list` | List all APIs, services, and clients |

### Package-specific commands

| Target | Description |
|--------|-------------|
| `make package-build PACKAGE=X` | Build specific package |
| `make package-test PACKAGE=X` | Test specific package |
| `make service-run SERVICE=X` | Run a service |

### OpenAPI commands

| Target | Description |
|--------|-------------|
| `make openapi-generate` | Generate specs from API traits |
| `make openapi-check` | Verify specs are up-to-date |
| `make openapi-list` | List managed APIs |

### Client generation commands

| Target | Description |
|--------|-------------|
| `make clients-generate` | Generate all client `src/generated.rs` files |
| `make clients-check` | Verify generated client code is up-to-date |
| `make clients-list` | List managed clients |
| `make regen-clients` | Regenerate OpenAPI specs + client code |

### Scaffolding commands

| Target | Description |
|--------|-------------|
| `make api-new API=X` | Create new API trait crate |
| `make service-new SERVICE=X API=Y` | Create new service |
| `make client-new CLIENT=X API=Y` | Create new client |

## Action-Dispatch Pattern

Several CloudAPI endpoints use `POST ...?action=<enum>` to dispatch multiple operations from one endpoint (mirrors Node.js Restify routes). Each has: an action enum, a query struct wrapping it, and per-action request body structs. The endpoint uses `TypedBody<serde_json::Value>` and deserializes based on the matched action.

**Endpoints**: `MachineAction` (start/stop/reboot/resize/rename/firewall/deletion-protection), `ImageAction` (update/export/clone/import/share/unshare), `DiskAction` (resize), `VolumeAction` (update).

See `apis/cloudapi-api/src/types/machine.rs` for the canonical example.

## WebSocket / Channel Endpoints

Dropshot supports WebSocket endpoints via `#[channel { protocol = WEBSOCKETS, ... }]`. Use `WebsocketConnection` as the last parameter, return `WebsocketChannelResult`. These are not covered by Progenitor-generated clients.

**Existing endpoints**: `/{account}/changefeed`, `/{account}/migrations/{machine}/watch`, `/{account}/machines/{machine}/vnc`. See `apis/cloudapi-api/src/types/changefeed.rs` for message types.

## Type Safety Rules

These rules prevent type-safety issues in CLI and client code. They are enforced by the `/type-safety-audit` skill (see `.claude/commands/type-safety-audit.md`) and should be followed in all new code. Violations found by the audit should be filed as beads issues with the `type-safety` label (see [Issue Tracking with Beads](#issue-tracking-with-beads)).

### 1. No Hardcoded Enum Strings

Never use string literals that match enum variant wire names. Use `enum_to_display()` or direct enum comparison.

```rust
// WRONG: hardcoded string matching an enum variant
if state_str == "running" { ... }

// RIGHT: compare typed enums directly
if machine.state == MachineState::Running { ... }

// RIGHT: use enum_to_display() when you need the wire-format string
println!("State: {}", enum_to_display(&machine.state));
```

### 2. ValueEnum on API Types

If an enum is used as a CLI argument (a `clap::Args` field), it **must** have `clap::ValueEnum` derived on the canonical API type definition in `apis/*/src/types/`. Progenitor generates separate types — the derive must be on the source type if the CLI imports via re-export.

### 3. client-generator Patch Consistency

Every enum used as a CLI argument must also have `with_patch(EnumName, &value_enum_patch)` in the corresponding client's configuration in `client-generator/src/main.rs`. This ensures the Progenitor-generated copy also gets `ValueEnum` for cases where the CLI uses `types::EnumName`.

### 4. No Duplicate Enum Definitions

Never reimplement enums that exist in API types or are generated by Progenitor. Import from `<service>_client::types::*` (Progenitor types) or the re-exported API types.

### 5. Forward Compatibility

Enums deserializing untrusted or evolving input (state fields, status fields) must include a `#[serde(other)] Unknown` catch-all variant. See `apis/cloudapi-api/src/types/changefeed.rs` for the established pattern.

### 6. Re-export Pattern

CLIs import types from `<service>_client` re-exports, not directly from API crates. The client crate re-exports canonical API types alongside Progenitor-generated types in `src/lib.rs`.

### 7. No Debug Format for User-Facing Output

Never use `{:?}` (Debug format) for values shown to users. Use `enum_to_display()` for serde enums, `.join(", ")` for collections, or implement `Display`. Debug format exposes Rust internals (e.g., `Brand::Bhyve` instead of `bhyve`).

```rust
// WRONG: Debug format in user-facing output
println!("  Brand: {:?}", brand);
println!("Waiting for {:?}", target_names);

// RIGHT: use enum_to_display() for serde enums
println!("  Brand: {}", enum_to_display(brand));

// RIGHT: use .join() for collections
println!("Waiting for {}", target_names.join(", "));
```

### 8. Field Naming Exceptions

Most CloudAPI response structs use `#[serde(rename_all = "camelCase")]` to match the JSON wire format. However, some fields from the original Node.js CloudAPI are returned in snake_case or other non-camelCase formats. These must use explicit `#[serde(rename = "...")]` overrides.

**Known exceptions in `Machine` (camelCase struct)**:
- `dns_names` -- returned as `"dns_names"` (snake_case) by CloudAPI despite other fields being camelCase
- `free_space` -- returned as `"free_space"` (snake_case) for bhyve flexible disk VMs
- `delegate_dataset` -- returned as `"delegate_dataset"` (snake_case)

**Other common rename patterns**:
- `type` fields -- Rust reserves `type` as a keyword, so fields like `machine_type` and `volume_type` use `#[serde(rename = "type")]`
- `role-tag` fields -- CloudAPI uses hyphenated `"role-tag"` in JSON, mapped to `role_tag` in Rust with `#[serde(rename = "role-tag")]`
- Enum variants with hyphens -- e.g., `#[serde(rename = "joyent-minimal")]`, `#[serde(rename = "zone-dataset")]`

**When adding new fields**: Always check the actual JSON wire format from the original Node.js service. If a field does not follow the struct-level `rename_all` convention, add an explicit `#[serde(rename = "...")]` with a comment explaining the exception.

```rust
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Machine {
    pub id: Uuid,
    pub name: String,
    // ...
    /// Note: CloudAPI returns this as snake_case despite other fields being camelCase
    #[serde(rename = "dns_names", default, skip_serializing_if = "Option::is_none")]
    pub dns_names: Option<Vec<String>>,
}
```

## UUID Handling Conventions

**Type alias**: API crates define `pub type Uuid = uuid::Uuid;` in their common types module (see `apis/cloudapi-api/src/types/common.rs`). Use this alias in all struct fields and function signatures rather than raw `uuid::Uuid` or `String`.

**Serialization**: The `uuid` crate's serde support handles serialization/deserialization as lowercase hyphenated strings (e.g., `"28faa36c-2031-4632-a819-f7defa1299a3"`). No custom serde logic is needed.

**Path parameters**: UUID path parameters (machine IDs, image IDs, etc.) are parsed automatically by Dropshot via the `Uuid` type in `Path<>` structs. Invalid UUIDs produce a 400 error.

**String UUIDs**: Some fields use `String` instead of `Uuid` when the upstream API may return non-UUID values or when the field serves double duty (e.g., `ChangefeedSubscription.vms` accepts UUIDs as strings). Prefer typed `Uuid` unless there is a specific reason for `String`.

**Testing**: When constructing test UUIDs, use `uuid::Uuid::parse_str("...")` or `uuid::Uuid::nil()` rather than placeholder strings.

## Issue Tracking with Beads

This repo uses [Beads](https://github.com/steveyegge/beads) (`bd` CLI) for lightweight issue tracking. Issues are stored in `.beads/` with JSONL export tracked in git.

```bash
bd ready                    # See work queue
bd update <id> --claim      # Claim an issue
bd show <id>                # View details
bd close <id>               # Close after fixing
bd close <id> -r "wontfix: reason"  # Close as won't-fix
bd create --title "..." --description "..." --add-label type-safety
```

**Session convention**: Check `bd ready` at session start. When finishing: add comments (`bd comments add <id> "..."`), close the issue, commit code + `.beads/issues.jsonl` together, and create new issues for follow-up work.

## Detailed Guides

Tutorial content for less-frequent tasks has been moved to dedicated files:

- **[API Workflow](docs/tutorials/api-workflow.md)** — Creating APIs, implementing services, managing OpenAPI specs, generating clients
- **[CLI Development](docs/tutorials/cli-development.md)** — Building CLI applications on generated clients
- **[External APIs](docs/tutorials/external-apis.md)** — Hand-writing minimal clients for legacy/external APIs
- **[Testing Guide](docs/tutorials/testing-guide.md)** — Test types, fixture management, doctests policy

## References

- [Oxide RFD 479: Dropshot API Traits](https://rfd.shared.oxide.computer/rfd/0479)
- [Dropshot Documentation](https://github.com/oxidecomputer/dropshot)
- [Dropshot API Manager](https://github.com/oxidecomputer/dropshot-api-manager)
- [Progenitor Documentation](https://github.com/oxidecomputer/progenitor)
