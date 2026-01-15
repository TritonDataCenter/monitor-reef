<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

Copyright 2025 Edgecast Cloud LLC.
-->

# History-Preserving Merge Plan

This document outlines the plan for merging Rust repositories into monitor-reef while preserving full git history.

## Current Status

- **Phase 1 (Directory Moves)**: ✅ COMPLETED
- **Phase 2 (Merges)**: ✅ COMPLETED
- **Phase 3 (Cargo.toml Updates)**: ⚠️ PARTIALLY COMPLETED
- **Phase 4 (Cleanup)**: 🔴 NOT STARTED

## Strategy Overview

The `monorepo` branch was created from `manta-rebalancer-master`, so the rebalancer code and history are already present at the root.

For each dependency repository:
1. **Checkout** the `-master` branch ✅
2. **Check** for existing move commits (some branches may already have them) ✅
3. **Move** all files into the target subdirectory with `git mv` (if not already done) ✅
4. **Commit** the move as a single commit ✅
5. **Merge** into the `monorepo` branch with `--allow-unrelated-histories` ✅

After all dependency merges complete:
6. **Update** Cargo.toml files to use path dependencies ⚠️ (in progress)
7. **Verify** the workspace builds 🔴 (blocked on dependency issues)
8. **Move** rebalancer to `libs/rebalancer-legacy/` (or directly to Dropshot structure) 🔴

This preserves complete git history - `git log --follow` will trace files back through the merge to their original commits.

---

## Source Branches

| Repository | Branch | Crates |
|------------|--------|--------|
| rust-fast | `rust-fast-master` | 1 |
| rust-cueball | `rust-cueball-master` | 7 (flatten) |
| rust-libmanta | `rust-libmanta-master` | 1 |
| rust-moray | `rust-moray-master` | 1 |
| rust-utils | `rust-utils-master` | 1 |
| rust-quickcheck-helpers | `rust-quickcheck-helpers-master` | 1 |
| rust-sharkspotter | `rust-sharkspotter-master` | 1 |
| manta-rebalancer | `manta-rebalancer-master` | 3 (single location) |

---

## Phase 1: Directory Move Commits

### 1.1 rust-fast ✅ COMPLETED

**Target**: `libs/fast/`

---

### 1.2 rust-cueball (flatten workspace) ✅ COMPLETED

**Target**: Each crate gets its own `libs/` directory (CLI tools go to `cli/`)

The rust-cueball repo contains a workspace with multiple crates. These were flattened by moving each crate to its own directory.

**Actual locations after merge:**
- `libs/cueball/`
- `libs/cueball-dns-resolver/`
- `libs/cueball-static-resolver/`
- `libs/cueball-tcp-stream-connection/`
- `libs/cueball-postgres-connection/`
- `libs/cueball-manatee-primary-resolver/`
- `cli/manatee-echo-resolver/` (CLI tool, not a library)

**Note**: The original workspace metadata files were not preserved in a separate directory (cleaner approach than originally planned).

---

### 1.3 rust-libmanta ✅ COMPLETED

**Target**: `libs/libmanta/`

---

### 1.4 rust-moray ✅ COMPLETED

**Target**: `libs/moray/`

---

### 1.5 rust-utils ✅ COMPLETED

**Target**: `libs/rust-utils/`

---

### 1.6 rust-quickcheck-helpers ✅ COMPLETED

**Target**: `libs/quickcheck-helpers/`

---

### 1.7 rust-sharkspotter ✅ COMPLETED

**Target**: `libs/sharkspotter/`

---

### 1.8 manta-rebalancer 🔴 PENDING

**Note**: manta-rebalancer is already in the `monorepo` branch (the branch was created from `manta-rebalancer-master`). No merge needed, but it should be moved to its target location after workspace is building.

**Current location**: Root (`agent/`, `manager/`, `rebalancer/`)
**Target**: `libs/rebalancer-legacy/` (or directly to Dropshot structure)

```bash
git checkout monorepo
mkdir -p libs/rebalancer-legacy
git mv agent libs/rebalancer-legacy/
git mv manager libs/rebalancer-legacy/
git mv rebalancer libs/rebalancer-legacy/
# Move other root files as needed
git commit -m "Move manta-rebalancer to libs/rebalancer-legacy/

Relocate rebalancer crates to legacy directory. The Dropshot rewrite
will use the target locations (apis/, services/, cli/)."
```

---

## Phase 2: Merge into Monorepo ✅ COMPLETED

All dependency repositories have been merged in dependency order:

| Order | Repository | Commit | Status |
|-------|------------|--------|--------|
| 1 | rust-fast | `f0cf732` | ✅ |
| 2 | rust-cueball | `0c41229` | ✅ |
| 3 | rust-libmanta | `94b4172` | ✅ |
| 4 | rust-moray | `56d3820` | ✅ |
| 5 | rust-utils | `6c9f92e` | ✅ |
| 6 | rust-quickcheck-helpers | `bfb6e71` | ✅ |
| 7 | rust-sharkspotter | `58a83f2` | ✅ |

manta-rebalancer was already in the branch (no merge needed).

---

## Phase 3: Post-Merge Cargo.toml Updates ⚠️ IN PROGRESS

### 3.1 Root Workspace Cargo.toml ⚠️ PARTIALLY DONE

The root `Cargo.toml` has been updated with workspace members. Current state:

**Enabled members:**
- `agent`, `manager`, `rebalancer` (manta-rebalancer at root)
- `libs/fast`
- `libs/cueball`
- `libs/cueball-static-resolver`
- `libs/cueball-tcp-stream-connection`
- `libs/libmanta`
- `libs/moray`
- `libs/quickcheck-helpers`
- `libs/rust-utils`
- `libs/sharkspotter`

**Commented out (need investigation):**
- `libs/cueball-dns-resolver`
- `libs/cueball-manatee-primary-resolver`
- `libs/cueball-postgres-connection`
- `cli/manatee-echo-resolver`

### 3.2 Path Dependencies ✅ DONE

Internal dependencies have been updated to use path references (commit `e22cc72`).

### 3.3 Verification 🔴 BLOCKED

**Current blocker:** Build fails due to `async-trait` patch pointing to unavailable git tag:

```
error: failed to load source for dependency `async-trait`
Caused by: revision 89923af3 not found (tag 0.1.36)
```

**Remaining verification steps:**
```bash
# After fixing dependencies:
cargo check --workspace
cargo build --workspace
cargo test --workspace
```

---

## Phase 4: Cleanup 🔴 NOT STARTED

1. **Fix dependency patches**: Update or remove `async-trait` and other patches in root `Cargo.toml`
2. **Enable commented crates**: Investigate and enable the 4 commented-out workspace members
3. **Move rebalancer**: Relocate `agent/`, `manager/`, `rebalancer/` to `libs/rebalancer-legacy/`
4. **Update .gitignore**: Consolidate ignore rules from merged repos
5. **Remove duplicate files**: LICENSE, CI configs that are now redundant

---

## Directory Structure (Current)

```
monitor-reef/
├── agent/                             # manta-rebalancer (to be moved)
├── manager/                           # manta-rebalancer (to be moved)
├── rebalancer/                        # manta-rebalancer (to be moved)
├── cli/
│   └── manatee-echo-resolver/         # rust-cueball CLI tool
├── libs/
│   ├── fast/                          # rust-fast
│   ├── cueball/                       # rust-cueball (core)
│   ├── cueball-dns-resolver/          # rust-cueball (commented out)
│   ├── cueball-static-resolver/       # rust-cueball
│   ├── cueball-tcp-stream-connection/ # rust-cueball
│   ├── cueball-postgres-connection/   # rust-cueball (commented out)
│   ├── cueball-manatee-primary-resolver/ # rust-cueball (commented out)
│   ├── libmanta/                      # rust-libmanta
│   ├── moray/                         # rust-moray
│   ├── rust-utils/                    # rust-utils
│   ├── quickcheck-helpers/            # rust-quickcheck-helpers
│   └── sharkspotter/                  # rust-sharkspotter
├── boot/                              # manta-rebalancer boot scripts
├── docs/                              # manta-rebalancer docs
├── test/                              # manta-rebalancer tests
└── conversion-plans/                  # migration planning docs
```

## Directory Structure (Target)

```
monitor-reef/
├── cli/
│   └── manatee-echo-resolver/         # rust-cueball CLI tool
├── libs/
│   ├── fast/                          # rust-fast
│   ├── cueball/                       # rust-cueball (core)
│   ├── cueball-dns-resolver/          # rust-cueball
│   ├── cueball-static-resolver/       # rust-cueball
│   ├── cueball-tcp-stream-connection/ # rust-cueball
│   ├── cueball-postgres-connection/   # rust-cueball
│   ├── cueball-manatee-primary-resolver/ # rust-cueball
│   ├── libmanta/                      # rust-libmanta
│   ├── moray/                         # rust-moray
│   ├── rust-utils/                    # rust-utils
│   ├── quickcheck-helpers/            # rust-quickcheck-helpers
│   ├── sharkspotter/                  # rust-sharkspotter
│   └── rebalancer-legacy/             # manta-rebalancer (moved)
│       ├── rebalancer/                # shared library
│       ├── agent/                     # agent service
│       └── manager/                   # manager service
├── apis/                              # (future: Dropshot APIs)
├── services/                          # (future: Dropshot services)
└── clients/                           # (future: generated clients)
```

---

## Verification Commands

```bash
# Verify history is preserved (use original branch for clean history)
git log rust-fast-master -- src/lib.rs
git log --follow libs/fast/src/lib.rs

# Verify all crates build (after fixing blockers)
cargo build --workspace

# Verify tests pass
cargo test --workspace

# Check workspace members
cargo metadata --no-deps --format-version 1 | jq '.packages[].name'
```

---

## Risks and Mitigations

| Risk | Mitigation | Outcome |
|------|------------|---------|
| Merge conflicts | Each repo moves to unique directory | ✅ No conflicts |
| Broken dependencies | Update Cargo.toml in dependency order | ⚠️ Some crates commented out |
| Missing files from `git mv` | Use `-k` flag, verify file counts | ✅ All files moved |
| Old Rust editions/dependencies | Address in separate modernization phase | 🔴 Blocking build |

---

## Immediate Next Steps

1. **Fix build blockers**: Remove or update `async-trait` and other problematic patches
2. **Enable commented crates**: Investigate why 4 crates are disabled, fix and enable them
3. **Move rebalancer**: Execute 1.8 to relocate to `libs/rebalancer-legacy/`
4. **Verify workspace**: Run `cargo build --workspace` and `cargo test --workspace`

## Future Work (Post-Merge)

1. **Modernization**: Update Rust editions, dependency versions (separate commits)
   - As each legacy crate is modernized, fully enable it by:
     - `Cargo.toml` - uncomment from workspace members (if commented out)
     - `arch-lint.toml` - remove from `[analyzer].exclude` list
     - `tarpaulin.toml` - remove from `exclude-files` list
   - This ensures modernized crates build with the workspace and get the same quality checks as new code
   - **rust-utils**: Do NOT modernize separately. When modernizing rebalancer-legacy:
     - Inline `calculate_md5()` function directly into rebalancer (it's ~10 lines)
     - Delete the `net` module (never used by any crate)
     - Remove rust-utils dependency and delete the crate
2. **Dropshot Rewrite**: Implement new APIs in target locations (apis/, services/)
3. **Test Migration**: Port tests from rebalancer-legacy to new structure
4. **Cleanup**: Remove rebalancer-legacy after rewrite is complete
