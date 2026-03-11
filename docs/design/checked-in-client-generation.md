<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

Copyright 2026 Edgecast Cloud LLC.
-->

# Checked-in Client Code Generation

## Background

This repo uses Progenitor to generate Rust client libraries from OpenAPI specs.
Previously, each client crate had a `build.rs` that ran Progenitor at build
time, writing unformatted code to `target/debug/build/<crate>-<hash>/out/client.rs`.

This created several problems:

- **Invisible code**: Generated types were hidden in build artifacts, invisible
  to grep, IDE navigation, and code review.
- **Slow builds**: Each client's build.rs re-ran Progenitor on every build,
  even when the spec hadn't changed.
- **Duplicated config**: Generation settings (patches, derives, hooks) were
  spread across 5 separate build.rs files.
- **Hard to debug**: When generated code had issues, finding the actual output
  required digging through target directories.

## Design

We replaced per-client build.rs scripts with a centralized `client-generator`
tool that produces formatted `src/generated.rs` files checked into git.

### Architecture

```
openapi-specs/generated/*.json   (checked in)
        │
        ▼
client-generator/src/main.rs     (centralized config + generation)
        │
        ▼
clients/internal/*/src/generated.rs  (checked in, formatted)
        │
        ▼
clients/internal/*/src/lib.rs    (mod generated; pub use generated::*)
```

### How it works

1. **OpenAPI specs** are generated from API traits by `openapi-manager` and
   checked into `openapi-specs/generated/`.

2. **client-generator** reads each spec, applies per-client settings (patches,
   derives, inner_type, pre_hook_async), generates code with Progenitor,
   formats it with rustfmt, and writes `src/generated.rs`.

3. **Client crates** use `mod generated; pub use generated::*;` to include the
   checked-in code. Hand-written code (TypedClient wrappers, auth modules,
   From impls, re-exports) stays in `lib.rs` alongside the generated module.

### Client configuration

Each client's generation settings are defined as a `ClientConfig` entry in
`client-generator/src/main.rs`. This includes:

- **spec_path**: Path to the OpenAPI spec (relative to repo root)
- **output_path**: Path to the generated file
- **configure**: Function that sets up `GenerationSettings` (interface style,
  tag style, derives, patches, inner_type, hooks)

### Commands

| Command | Purpose |
|---------|---------|
| `make clients-generate` | Generate all `src/generated.rs` files |
| `make clients-check` | Verify generated code matches disk (for CI) |
| `make clients-list` | List managed clients |
| `make regen-clients` | Regenerate OpenAPI specs + client code |

## Developer workflow

### Adding a new client

1. Create the client crate directory with `Cargo.toml` and `src/lib.rs`
2. Add a `ClientConfig` entry to `client-generator/src/main.rs`
3. Run `make clients-generate`
4. Add to workspace `Cargo.toml` members
5. Commit the generated `src/generated.rs`

### Modifying an API

1. Change the API trait in `apis/*/src/`
2. Run `make openapi-generate` to update OpenAPI specs
3. Run `make clients-generate` to regenerate client code
4. Review the diff in `src/generated.rs`
5. Commit spec and client changes together

### Adding a new ValueEnum patch

1. Edit the client's `configure_*` function in `client-generator/src/main.rs`
2. Run `make clients-generate`
3. Verify the enum now has `clap::ValueEnum` in `src/generated.rs`

## Relationship to OpenAPI specs

The generation pipeline has two stages, each producing checked-in artifacts:

```
API traits → openapi-manager → openapi-specs/generated/*.json (stage 1)
OpenAPI specs → client-generator → src/generated.rs (stage 2)
```

Both stages have `generate` and `check` commands. CI runs both checks to
ensure checked-in artifacts match the source of truth.

## Why check in generated code?

The same reasons we check in OpenAPI specs:

- **Visible in PRs**: API changes show up as diffs in generated code
- **Always available**: Builds work without running generators first
- **Searchable**: Types are visible to grep and IDE navigation
- **Debuggable**: Generated code is formatted and readable
- **Fast builds**: No build.rs overhead; code compiles directly
