---
name: rust-modernization
description: Modernize legacy Rust crates to edition 2024 with updated dependencies. Use this skill when updating old Rust crates (tokio 0.1, bytes 0.4, quickcheck 0.8, etc.) to modern versions.
allowed-tools: Bash(git add:*), Bash(git commit:*), Bash(make:*), Bash(git status:*), Read, Glob, Grep, Write, Edit
---

<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

Copyright 2026 Edgecast Cloud LLC.
-->

# Rust Crate Modernization Skill

This skill modernizes legacy Rust crates to edition 2024 with modern dependencies.

## When to Use

Use this skill when you need to:
- Update a Rust crate from edition 2018 to 2024
- Migrate from tokio 0.1 to tokio 1.x
- Update bytes 0.4 to bytes 1.x
- Update quickcheck 0.8 to quickcheck 1.0
- Update other legacy dependencies

## Pre-flight Checks (CRITICAL)

Before starting any modernization, verify:

1. **Not on main/master branch:**
   ```bash
   git branch --show-current
   ```
   If on `main` or `master`, ask user to create/switch to a feature branch first.

2. **Working directory is clean:**
   ```bash
   git status --porcelain
   ```
   If there are uncommitted changes, ask user to commit or stash them first.

**Do not proceed until both checks pass.**

## Critical Rules

1. **ALWAYS use make targets instead of cargo commands:**
   - `make package-build PACKAGE=<name>` NOT `cargo build -p <name>`
   - `make package-test PACKAGE=<name>` NOT `cargo test -p <name>`
   - `make format` NOT `cargo fmt`

   The make targets ensure consistent toolchain configuration.

2. **NEVER call cargo directly** - even for quick checks. The Makefile wraps cargo with the correct environment variables and paths.

## Modernization Principles

### Delete Before Modernizing

**For library crates, deleting unused code is better than modernizing it.**

During analysis, identify which public functions/modules are actually used by other crates in the repo. Dead code should be deleted, not modernized. This reduces maintenance burden and keeps the API surface clean.

### All Crates Must Pass arch-lint (No Exceptions)

Every modernized crate must pass arch-lint with **no exclusions**. Common violations and fixes:

| Violation | Common Causes | Fix |
|-----------|---------------|-----|
| `no-panic-in-lib` | `unwrap()`, `expect()`, `panic!()` | Convert to `Result` with `?` operator |
| `no-sync-io` | Blocking I/O in async context | Use `tokio::fs` or `spawn_blocking` |

**Specific patterns to fix:**

```rust
// VIOLATION: unwrap() can panic
let data = serde_json::to_string(&msg).unwrap();

// FIX: Propagate error
let data = serde_json::to_string(&msg)
    .map_err(|e| Error::other(format!("serialize failed: {}", e)))?;
```

```rust
// VIOLATION: expect() on user input can panic (DoS vulnerability!)
let arr = value.as_array().expect("should be array");

// FIX: Return error
let arr = value.as_array()
    .ok_or_else(|| Error::other("Expected JSON array"))?;
```

**Phase 1 should identify these issues. Phase 2 must fix them.**

If fixing requires API changes (e.g., function returns `Result` instead of value), that's acceptable during modernization. Callers will be updated when their crates are modernized.

### Convert Panics to Results

Legacy code often uses `panic!()` for error handling. Modernized code should:

```rust
// Before (legacy)
pub fn do_thing(path: &str) -> String {
    let file = fs::File::open(path).unwrap(); // or panic!()
    // ...
}

// After (modern)
pub fn do_thing(path: &str) -> Result<String, std::io::Error> {
    let file = fs::File::open(path)?;
    // ...
}
```

Callers will need to be updated when their crates are modernized.

## Orchestration Flow

When asked to modernize a crate, execute this flow:

### Step 0: Pre-flight Checks

```bash
git branch --show-current
git status --porcelain
```

If either check fails, inform the user and stop.

### Step 1: Analyze Phase

Read `.claude/skills/rust-modernization/phase1-analyze.md` for instructions.

Analyze the crate to understand:
- Current dependencies and their versions
- Code patterns that need updating
- Complexity estimate

### Step 2: Update Phase

Read `.claude/skills/rust-modernization/phase2-update.md` for instructions.
Read `.claude/skills/rust-modernization/reference.md` for pattern mappings.

1. Update Cargo.toml with modern dependencies
2. Enable crate in workspace Cargo.toml
3. Build iteratively, fixing errors using reference patterns
4. Update examples if present

### Step 3: Validate Phase

Read `.claude/skills/rust-modernization/phase3-validate.md` for instructions.

1. Run `make format`
2. Run `make package-test PACKAGE=<name>`
3. Enable crate in arch-lint.toml and tarpaulin.toml
4. Run `make format check coverage` (full validation suite)
5. Commit the changes

## One Commit Per Crate

Each crate modernization should be a single atomic commit containing:
- Cargo.toml updates
- Source code fixes
- Example updates
- Workspace Cargo.toml changes
- Lint/coverage config updates

## Commit Message Format

```
Modernize <crate-name> crate to edition 2024

- Update to <dep> X.Y (from A.B)
- <describe key API changes>
- <any breaking changes to the crate's public API>

Co-Authored-By: Claude Opus 4.5 <noreply@anthropic.com>
```

## Target Crates

### Completed Modernizations

| Crate | Status | Notes |
|-------|--------|-------|
| fast | ✅ Done | tokio 1.x, bytes 1.x, quickcheck 1.0 |
| quickcheck-helpers | ✅ Done | quickcheck 1.0 |
| cueball | ✅ Done | Core pool functionality (kept for now, qorb migration deferred) |
| cueball-static-resolver | ✅ Done | Static backend resolver |
| cueball-tcp-stream-connection | ✅ Done | TCP connector |
| libmanta | ✅ Done | Manta types and utilities |
| moray | ✅ Done | Moray client (still uses cueball, qorb migration deferred) |
| sharkspotter | ✅ Done | Fully integrated (no exclusions) |

### Remaining Crates to Modernize

| Crate | Key Dependencies | Complexity | Strategy |
|-------|------------------|------------|----------|
| rebalancer-legacy/* | Multiple | High | May not need modernization - see note below |

**Note on rebalancer-legacy:** New Dropshot-based services exist in `services/rebalancer-agent/` and `services/rebalancer-manager/`. The legacy code may only be needed as reference. See `docs/design/rebalancer-review-findings.md` for gaps in the new implementation.

### Crates to Delete (Not Modernize)

| Crate | Reason | Action |
|-------|--------|--------|
| cueball-dns-resolver | Legacy tokio 0.1, use qorb DnsResolver if needed | Delete (never enable) |
| cueball-postgres-connection | Legacy, use qorb DieselPgConnector if needed | Delete (never enable) |
| cueball-manatee-primary-resolver | Legacy tokio 0.1 + unmaintained tokio-zookeeper | Port to qorb or delete |
| cli/manatee-echo-resolver | Debug tool for old cueball | Delete |
| rust-utils | Only used by rebalancer-legacy | Inline `calculate_md5` if needed, then delete |

### Future: Qorb Migration

The cueball crates were modernized but qorb migration was deferred. When qorb migration is needed:
- See `conversion-plans/manta-rebalancer/cueball-to-qorb-migration.md` for details
- Replace cueball usage in moray with qorb equivalents
- Delete cueball crates after migration

## Dependency Order (Historical)

Modernization was completed in this order:
1. `fast` (no internal deps) ✅ DONE
2. `quickcheck-helpers` (no internal deps) ✅ DONE
3. `cueball` + resolvers/connectors ✅ DONE
4. `libmanta` ✅ DONE
5. `moray` (depends on fast, cueball) ✅ DONE
6. `sharkspotter` (depends on moray, libmanta) ✅ DONE

## New Dropshot Services

The new rebalancer implementation uses Dropshot instead of Gotham:

| Component | Location | Status |
|-----------|----------|--------|
| Agent API | `apis/rebalancer-agent-api/` | Complete |
| Manager API | `apis/rebalancer-manager-api/` | Complete |
| Shared Types | `apis/rebalancer-types/` | Complete |
| Agent Service | `services/rebalancer-agent/` | ~90% - needs testing |
| Manager Service | `services/rebalancer-manager/` | ~70% - missing critical integrations |
| Admin CLI | `cli/rebalancer-adm/` | Complete |

**Critical gaps in new services:** See `docs/design/rebalancer-review-findings.md` for:
- 8 critical issues (sharkspotter integration, moray client, error handling)
- 12 important issues (metrics, resume on startup, test coverage)
- Recommended priority order for fixes

## Error Handling

If any phase fails:
1. Document the specific error
2. Check reference.md for known patterns
3. If pattern not found, investigate and add to reference.md
4. Resume from the failed step
