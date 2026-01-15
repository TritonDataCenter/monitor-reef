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

### All Crates Must Pass arch-lint

Every modernized crate must pass arch-lint with **no exclusions**. This means:

- **No panic in library code** - Convert `panic!()` to `Result` types
- **No sync I/O in async context** - Use async I/O or justify sync usage
- **Proper error handling** - No swallowed errors

If fixing these issues requires API changes, that's acceptable during modernization.

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

Located in `libs/` and commented out in workspace Cargo.toml under "To be modernized":

| Crate | Key Dependencies | Complexity |
|-------|------------------|------------|
| fast | tokio 0.1, bytes 0.4, quickcheck 0.8 | High |
| quickcheck-helpers | quickcheck 0.8 | Low |
| cueball | tokio 0.1, futures 0.1 | Medium |
| cueball-* | cueball dependencies | Low-Medium |
| libmanta | TBD | Medium |
| moray | fast-rpc, tokio 0.1 | High |
| sharkspotter | TBD | TBD |
| rebalancer-legacy/* | Multiple | High |

### Crates to Inline (Not Modernize Separately)

| Crate | Reason | Action |
|-------|--------|--------|
| rust-utils | Only used by rebalancer-legacy, tiny crate | Inline `calculate_md5` into rebalancer-legacy when modernizing that crate, delete `net` module (unused) |

## Dependency Order

Some crates depend on others. Modernize in this order:
1. `fast` (no internal deps)
2. `quickcheck-helpers` (no internal deps)
3. `cueball` (no internal deps)
4. `cueball-*` resolvers/connections (depend on cueball)
5. `libmanta` (may depend on others)
6. `moray` (depends on fast-rpc)
7. `sharkspotter` (depends on moray, libmanta)
8. `rebalancer-legacy/*` (depends on many; inline rust-utils here)

## Error Handling

If any phase fails:
1. Document the specific error
2. Check reference.md for known patterns
3. If pattern not found, investigate and add to reference.md
4. Resume from the failed step
