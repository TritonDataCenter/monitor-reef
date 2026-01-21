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
- **Phase 3 (Cargo.toml Updates)**: ✅ COMPLETED
- **Phase 4 (Cleanup)**: ⚠️ PARTIALLY COMPLETED - legacy crates remain
- **Phase 5 (Crate Modernization)**: ✅ COMPLETED - all core crates modernized
- **Phase 6 (Qorb Migration)**: 🔴 REQUIRED - migrate moray to qorb, create qorb-manatee-resolver, delete cueball
- **Phase 7 (Dropshot Services)**: ⚠️ IN PROGRESS - See [rebalancer-review-findings.md](../../docs/design/rebalancer-review-findings.md)

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

## Phase 3: Post-Merge Cargo.toml Updates ✅ COMPLETED

### 3.1 Root Workspace Cargo.toml ✅ DONE

The root `Cargo.toml` has been updated with workspace members. Current state:

**Enabled members (modernized):**
- `libs/fast`
- `libs/quickcheck-helpers`
- `libs/cueball`
- `libs/cueball-static-resolver`
- `libs/cueball-tcp-stream-connection`
- `libs/libmanta`
- `libs/moray`
- `libs/sharkspotter`

**New Dropshot services:**
- `apis/rebalancer-agent-api`
- `apis/rebalancer-manager-api`
- `apis/rebalancer-types`
- `services/rebalancer-agent`
- `services/rebalancer-manager`
- `cli/rebalancer-adm`

**Commented out (to be deleted):**
- `libs/cueball-dns-resolver` - Legacy tokio 0.1
- `libs/cueball-manatee-primary-resolver` - Legacy tokio 0.1 + unmaintained deps
- `libs/cueball-postgres-connection` - Legacy
- `cli/manatee-echo-resolver` - Debug tool for old cueball
- `libs/rust-utils` - Inline into rebalancer-legacy if needed
- `libs/rebalancer-legacy/*` - Legacy Gotham implementation (reference only)

### 3.2 Path Dependencies ✅ DONE

Internal dependencies have been updated to use path references.

### 3.3 Verification ✅ DONE

Workspace builds and tests pass:
```bash
make build
make test
```

---

## Phase 4: Cleanup ⚠️ PARTIALLY COMPLETED

1. ~~**Fix dependency patches**~~: ✅ Resolved - patches removed, modern deps used
2. ~~**Enable commented crates**~~: ✅ Decision made - legacy crates to be deleted, not enabled
3. ✅ **Move rebalancer**: Relocated to `libs/rebalancer-legacy/`
4. ⚠️ **Update .gitignore**: Partially done
5. ⚠️ **Remove duplicate files**: Partially done

**Remaining cleanup:**
- Remove `libs/sharkspotter` from `arch-lint.toml` and `tarpaulin.toml` exclusion lists
- Delete legacy crates that will never be used:
  - `libs/cueball-dns-resolver`
  - `libs/cueball-postgres-connection`
  - `libs/cueball-manatee-primary-resolver`
  - `cli/manatee-echo-resolver`
  - `libs/rust-utils`

---

## Directory Structure (Current)

```
monitor-reef/
├── apis/                              # Dropshot API traits
│   ├── rebalancer-agent-api/          # Agent API definition
│   ├── rebalancer-manager-api/        # Manager API definition
│   └── rebalancer-types/              # Shared types
├── cli/
│   ├── rebalancer-adm/                # New Dropshot-based admin CLI
│   └── manatee-echo-resolver/         # Legacy (to be deleted)
├── clients/
│   └── internal/
│       └── rebalancer-manager-client/ # Generated manager client
├── services/
│   ├── rebalancer-agent/              # New Dropshot agent (~90% complete)
│   └── rebalancer-manager/            # New Dropshot manager (~70% complete)
├── libs/
│   ├── fast/                          # ✅ Modernized
│   ├── quickcheck-helpers/            # ✅ Modernized
│   ├── cueball/                       # ✅ Modernized (delete after qorb migration)
│   ├── cueball-static-resolver/       # ✅ Modernized (delete after qorb migration)
│   ├── cueball-tcp-stream-connection/ # ✅ Modernized (delete after qorb migration)
│   ├── cueball-dns-resolver/          # ❌ To be deleted
│   ├── cueball-postgres-connection/   # ❌ To be deleted
│   ├── cueball-manatee-primary-resolver/ # ❌ To be deleted
│   ├── libmanta/                      # ✅ Modernized
│   ├── moray/                         # ✅ Modernized
│   ├── sharkspotter/                  # ✅ Modernized (needs exclusion cleanup)
│   ├── rust-utils/                    # ❌ To be deleted
│   └── rebalancer-legacy/             # Legacy Gotham implementation (reference)
│       ├── agent/
│       ├── manager/
│       └── rebalancer/
├── docs/
│   └── design/
│       └── rebalancer-review-findings.md  # Migration gap analysis
└── conversion-plans/
    └── manta-rebalancer/
        ├── plan.md                    # This file
        └── cueball-to-qorb-migration.md
```

## Directory Structure (Target - After Cleanup)

```
monitor-reef/
├── apis/
│   ├── rebalancer-agent-api/
│   ├── rebalancer-manager-api/
│   └── rebalancer-types/
├── cli/
│   └── rebalancer-adm/
├── clients/
│   └── internal/
│       └── rebalancer-manager-client/
├── services/
│   ├── rebalancer-agent/              # Production ready
│   └── rebalancer-manager/            # Production ready
├── libs/
│   ├── fast/
│   ├── quickcheck-helpers/
│   ├── qorb-manatee-resolver/         # NEW: Manatee/ZooKeeper resolver for qorb
│   ├── libmanta/
│   ├── moray/                         # Migrated to use qorb
│   └── sharkspotter/
└── docs/

# DELETED (after qorb migration):
# - libs/cueball/
# - libs/cueball-static-resolver/
# - libs/cueball-tcp-stream-connection/
# - libs/cueball-dns-resolver/
# - libs/cueball-postgres-connection/
# - libs/cueball-manatee-primary-resolver/
# - cli/manatee-echo-resolver/
# - libs/rust-utils/
# - libs/rebalancer-legacy/            # After Dropshot services are production-ready
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
| Broken dependencies | Update Cargo.toml in dependency order | ✅ Resolved |
| Missing files from `git mv` | Use `-k` flag, verify file counts | ✅ All files moved |
| Old Rust editions/dependencies | Modernize crates to edition 2024 | ✅ Core crates modernized |
| Dropshot service gaps | Review against legacy, document findings | ⚠️ Review complete, fixes pending |

---

## Immediate Next Steps

1. **Complete Dropshot services** - See [rebalancer-review-findings.md](../../docs/design/rebalancer-review-findings.md):
   - CRIT-3: Create async Moray client
   - CRIT-1: Implement sharkspotter integration
   - CRIT-2: Implement metadata updates
   - CRIT-8: Port HTTP API tests from legacy
2. **Cleanup exclusions**: Remove `libs/sharkspotter` from arch-lint.toml and tarpaulin.toml
3. **Delete unused crates**: cueball-dns-resolver, cueball-postgres-connection, cueball-manatee-primary-resolver, manatee-echo-resolver, rust-utils

---

## Phase 5: Crate Modernization ✅ COMPLETED

All core library crates have been modernized to edition 2024 with modern dependencies:

| Crate | Status |
|-------|--------|
| fast | ✅ tokio 1.x, bytes 1.x, quickcheck 1.0 |
| quickcheck-helpers | ✅ quickcheck 1.0 |
| cueball | ✅ Modernized (temporary - delete after qorb migration) |
| cueball-static-resolver | ✅ Modernized (temporary - delete after qorb migration) |
| cueball-tcp-stream-connection | ✅ Modernized (temporary - delete after qorb migration) |
| libmanta | ✅ Modernized |
| moray | ✅ Modernized (qorb migration required) |
| sharkspotter | ✅ Modernized (needs exclusion cleanup) |

---

## Phase 6: Qorb Migration 🔴 REQUIRED

**Qorb migration is REQUIRED.** The cueball crates were modernized as a stepping stone, but they must be replaced with qorb and then deleted.

### Why Required

1. **Manatee support**: Production requires Manatee/ZooKeeper-based service discovery
2. **Modern async**: Qorb is native tokio 1.x; cueball is fundamentally synchronous
3. **Observability**: Qorb has 24 DTrace probes built-in
4. **Maintenance**: Cueball is legacy code with no upstream development

### Migration Steps

| Step | Description | Status |
|------|-------------|--------|
| 1 | Create `libs/qorb-manatee-resolver` | 🔴 TODO |
| 2 | Migrate `libs/moray` from cueball to qorb | 🔴 TODO |
| 3 | Delete `libs/cueball*` crates | 🔴 TODO |
| 4 | Delete legacy cueball crates | 🔴 TODO |

See [cueball-to-qorb-migration.md](cueball-to-qorb-migration.md) for full migration details.

---

## Phase 7: Dropshot Services ⚠️ IN PROGRESS

New Dropshot-based rebalancer services replace the legacy Gotham implementation.

### Components

| Component | Location | Status |
|-----------|----------|--------|
| Agent API | `apis/rebalancer-agent-api/` | ✅ Complete |
| Manager API | `apis/rebalancer-manager-api/` | ✅ Complete |
| Shared Types | `apis/rebalancer-types/` | ✅ Complete |
| Agent Service | `services/rebalancer-agent/` | ~90% - Testing/Staging |
| Manager Service | `services/rebalancer-manager/` | ~70% - Missing critical integrations |
| Admin CLI | `cli/rebalancer-adm/` | ✅ Complete |
| Manager Client | `clients/internal/rebalancer-manager-client/` | ✅ Complete |

### Critical Issues (Must Fix Before Production)

See [rebalancer-review-findings.md](../../docs/design/rebalancer-review-findings.md) for full details.

**Phase 1 - Critical (Before any testing):**
1. CRIT-3: Create Moray client
2. CRIT-1: Sharkspotter integration
3. CRIT-2: Metadata updates
4. CRIT-8: HTTP API tests

**Phase 2 - Error Handling (Before staging):**
5. CRIT-4: HTTP client fallback
6. CRIT-5: Corrupted file removal
7. CRIT-6: Skipped reason parse
8. CRIT-7: Discovery error propagation

**Phase 3 - Important (Before production):**
9. IMP-1: Max fill percentage
10. IMP-10: Configuration tests
11. IMP-8: Worker task results
12. IMP-2: Duplicate object tracking

---

## Future Work

1. **Complete qorb migration** (REQUIRED):
   - Create `libs/qorb-manatee-resolver` for Manatee/ZooKeeper service discovery
   - Migrate `libs/moray` from cueball to qorb
   - Delete all cueball crates after migration
2. **Complete Dropshot services**: Address all critical and important issues in review findings
3. **Delete rebalancer-legacy**: After Dropshot services are production-ready

---

## References

- **Review findings**: [rebalancer-review-findings.md](../../docs/design/rebalancer-review-findings.md) - Gap analysis between legacy and new implementation
- **Qorb migration**: [cueball-to-qorb-migration.md](cueball-to-qorb-migration.md) - Required migration plan
- **Modernization skill**: `.claude/skills/rust-modernization/SKILL.md` - Crate modernization process
- **Legacy code**: `libs/rebalancer-legacy/` - Reference implementation
- **New services**: `services/rebalancer-agent/`, `services/rebalancer-manager/`
