<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

Copyright 2025 Edgecast Cloud LLC.
-->

# History-Preserving Merge Plan

This document outlines the plan for merging Rust repositories into monitor-reef while preserving full git history.

## Strategy Overview

The `monorepo` branch was created from `manta-rebalancer-master`, so the rebalancer code and history are already present at the root.

For each dependency repository:
1. **Checkout** the `-master` branch
2. **Check** for existing move commits (some branches may already have them)
3. **Move** all files into the target subdirectory with `git mv` (if not already done)
4. **Commit** the move as a single commit
5. **Merge** into the `monorepo` branch with `--allow-unrelated-histories`

After all dependency merges complete:
6. **Update** Cargo.toml files to use path dependencies
7. **Verify** the workspace builds
8. **Move** rebalancer to `libs/rebalancer-legacy/` (or directly to Dropshot structure)

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

### 1.1 rust-fast

**Target**: `libs/fast/`

```bash
git checkout rust-fast-master
mkdir -p libs/fast
git mv -k * libs/fast/ 2>/dev/null || true
git mv .* libs/fast/ 2>/dev/null || true
git commit -m "Move rust-fast to libs/fast/ for monorepo merge

Relocate all files to target directory in preparation for
history-preserving merge into monitor-reef monorepo.

Source: https://github.com/TritonDataCenter/rust-fast"
```

---

### 1.2 rust-cueball (flatten workspace)

**Target**: Each crate gets its own `libs/` directory

The rust-cueball repo contains a workspace with multiple crates. We flatten this by moving each crate to its own directory.

```bash
git checkout rust-cueball-master
mkdir -p libs

# Move each crate to its own libs/ directory
git mv cueball libs/cueball
git mv cueball-dns-resolver libs/cueball-dns-resolver
git mv cueball-static-resolver libs/cueball-static-resolver
git mv cueball-tcp-stream-connection libs/cueball-tcp-stream-connection
git mv cueball-postgres-connection libs/cueball-postgres-connection
git mv manatee-primary-resolver libs/manatee-primary-resolver
git mv manatee-echo-resolver libs/manatee-echo-resolver

# Move root workspace files to a metadata location (or delete)
# These files won't be needed after flattening:
# - Cargo.toml (workspace definition)
# - Cargo.lock
# - .gitignore, LICENSE, etc.
mkdir -p libs/cueball-workspace-meta
git mv Cargo.toml libs/cueball-workspace-meta/ 2>/dev/null || true
git mv Cargo.lock libs/cueball-workspace-meta/ 2>/dev/null || true
git mv .gitignore libs/cueball-workspace-meta/ 2>/dev/null || true
git mv LICENSE libs/cueball-workspace-meta/ 2>/dev/null || true
git mv README.md libs/cueball-workspace-meta/ 2>/dev/null || true
# Move any remaining root files
git mv -k * libs/cueball-workspace-meta/ 2>/dev/null || true

git commit -m "Flatten rust-cueball workspace to libs/ for monorepo merge

Relocate each crate to its own libs/ directory:
- libs/cueball/
- libs/cueball-dns-resolver/
- libs/cueball-static-resolver/
- libs/cueball-tcp-stream-connection/
- libs/cueball-postgres-connection/
- libs/manatee-primary-resolver/
- libs/manatee-echo-resolver/

Original workspace metadata preserved in libs/cueball-workspace-meta/.

Source: https://github.com/TritonDataCenter/rust-cueball"
```

**Note**: After merge, we can delete `libs/cueball-workspace-meta/` as it's just the old workspace Cargo.toml.

---

### 1.3 rust-libmanta

**Target**: `libs/libmanta/`

```bash
git checkout rust-libmanta-master
mkdir -p libs/libmanta
git mv -k * libs/libmanta/ 2>/dev/null || true
git mv .* libs/libmanta/ 2>/dev/null || true
git commit -m "Move rust-libmanta to libs/libmanta/ for monorepo merge

Relocate all files to target directory in preparation for
history-preserving merge into monitor-reef monorepo.

Source: https://github.com/TritonDataCenter/rust-libmanta"
```

---

### 1.4 rust-moray

**Target**: `libs/moray/`

```bash
git checkout rust-moray-master
mkdir -p libs/moray
git mv -k * libs/moray/ 2>/dev/null || true
git mv .* libs/moray/ 2>/dev/null || true
git commit -m "Move rust-moray to libs/moray/ for monorepo merge

Relocate all files to target directory in preparation for
history-preserving merge into monitor-reef monorepo.

Source: https://github.com/TritonDataCenter/rust-moray"
```

---

### 1.5 rust-utils

**Target**: `libs/rust-utils/`

```bash
git checkout rust-utils-master
mkdir -p libs/rust-utils
git mv -k * libs/rust-utils/ 2>/dev/null || true
git mv .* libs/rust-utils/ 2>/dev/null || true
git commit -m "Move rust-utils to libs/rust-utils/ for monorepo merge

Relocate all files to target directory in preparation for
history-preserving merge into monitor-reef monorepo.

Source: https://github.com/TritonDataCenter/rust-utils"
```

---

### 1.6 rust-quickcheck-helpers

**Target**: `libs/quickcheck-helpers/`

```bash
git checkout rust-quickcheck-helpers-master
mkdir -p libs/quickcheck-helpers
git mv -k * libs/quickcheck-helpers/ 2>/dev/null || true
git mv .* libs/quickcheck-helpers/ 2>/dev/null || true
git commit -m "Move quickcheck-helpers to libs/quickcheck-helpers/ for monorepo merge

Relocate all files to target directory in preparation for
history-preserving merge into monitor-reef monorepo.

Source: https://github.com/TritonDataCenter/rust-quickcheck-helpers"
```

---

### 1.7 rust-sharkspotter

**Target**: `libs/sharkspotter/`

```bash
git checkout rust-sharkspotter-master
mkdir -p libs/sharkspotter
git mv -k * libs/sharkspotter/ 2>/dev/null || true
git mv .* libs/sharkspotter/ 2>/dev/null || true
git commit -m "Move rust-sharkspotter to libs/sharkspotter/ for monorepo merge

Relocate all files to target directory in preparation for
history-preserving merge into monitor-reef monorepo.

Source: https://github.com/TritonDataCenter/rust-sharkspotter"
```

---

### 1.8 manta-rebalancer

**Note**: manta-rebalancer is already in the `monorepo` branch (the branch was created from `manta-rebalancer-master`). No merge needed.

After all dependency merges and Cargo.toml updates are complete, we will move the rebalancer to its target location:

**Target**: `libs/rebalancer-legacy/` (or directly to Dropshot structure)

```bash
git checkout monorepo
mkdir -p libs/rebalancer-legacy
git mv agent libs/rebalancer-legacy/
git mv manager libs/rebalancer-legacy/
git mv rebalancer libs/rebalancer-legacy/
git mv Cargo.toml libs/rebalancer-legacy/
# Move other root files as needed
git commit -m "Move manta-rebalancer to libs/rebalancer-legacy/

Relocate rebalancer crates to legacy directory. The Dropshot rewrite
will use the target locations (apis/, services/, cli/)."
```

---

## Phase 2: Merge into Monorepo

Merge in dependency order. The `monorepo` branch already contains manta-rebalancer, so we only merge the dependency repos.

```bash
git checkout monorepo

# 1. fast (no dependencies)
git merge rust-fast-master --allow-unrelated-histories -m "Merge rust-fast history into monorepo"

# 2. cueball (no triton dependencies)
git merge rust-cueball-master --allow-unrelated-histories -m "Merge rust-cueball history into monorepo"

# 3. libmanta (no triton dependencies)
git merge rust-libmanta-master --allow-unrelated-histories -m "Merge rust-libmanta history into monorepo"

# 4. moray (depends on fast, cueball, libmanta)
git merge rust-moray-master --allow-unrelated-histories -m "Merge rust-moray history into monorepo"

# 5. rust-utils (no triton dependencies)
git merge rust-utils-master --allow-unrelated-histories -m "Merge rust-utils history into monorepo"

# 6. quickcheck-helpers (no triton dependencies)
git merge rust-quickcheck-helpers-master --allow-unrelated-histories -m "Merge rust-quickcheck-helpers history into monorepo"

# 7. sharkspotter (depends on moray, libmanta)
git merge rust-sharkspotter-master --allow-unrelated-histories -m "Merge rust-sharkspotter history into monorepo"

# manta-rebalancer is already in the branch - no merge needed
```

---

## Phase 3: Post-Merge Cargo.toml Updates

After all merges, update dependencies to use path references. This is done incrementally, testing after each change.

### 3.1 Create/Update Root Workspace Cargo.toml

Add all crates as workspace members:

```toml
[workspace]
members = [
    # Existing monitor-reef crates...

    # Migrated libraries
    "libs/fast",
    "libs/cueball",
    "libs/cueball-dns-resolver",
    "libs/cueball-static-resolver",
    "libs/cueball-tcp-stream-connection",
    "libs/cueball-postgres-connection",
    "libs/manatee-primary-resolver",
    "libs/manatee-echo-resolver",
    "libs/libmanta",
    "libs/moray",
    "libs/rust-utils",
    "libs/quickcheck-helpers",
    "libs/sharkspotter",
    "libs/rebalancer-legacy/*",  # Or list individual crates
]
```

### 3.2 Update Internal Dependencies

Update each crate's Cargo.toml to use path dependencies for internal crates:

**libs/cueball-*/Cargo.toml** (cueball derivatives):
```toml
[dependencies]
cueball = { path = "../cueball" }
```

**libs/moray/Cargo.toml**:
```toml
[dependencies]
cueball = { path = "../cueball" }
cueball-postgres-connection = { path = "../cueball-postgres-connection" }
cueball-static-resolver = { path = "../cueball-static-resolver" }
fast = { path = "../fast" }
libmanta = { path = "../libmanta" }
```

**libs/sharkspotter/Cargo.toml**:
```toml
[dependencies]
moray = { path = "../moray" }
libmanta = { path = "../libmanta" }
```

**libs/rebalancer-legacy/*/Cargo.toml** (as needed):
```toml
[dependencies]
moray = { path = "../../moray" }
libmanta = { path = "../../libmanta" }
sharkspotter = { path = "../../sharkspotter" }
rust-utils = { path = "../../rust-utils" }
```

### 3.3 Verification

After each update:
```bash
cargo check -p <crate-name>
```

After all updates:
```bash
cargo build --workspace
cargo test --workspace
```

---

## Phase 4: Cleanup

1. **Remove workspace metadata**: Delete `libs/cueball-workspace-meta/` if not needed
2. **Update .gitignore**: Consolidate ignore rules
3. **Remove duplicate files**: LICENSE, CI configs that are now redundant

---

## Directory Structure After Merge

```
monitor-reef/
├── libs/
│   ├── fast/                          # rust-fast
│   ├── cueball/                       # rust-cueball (core)
│   ├── cueball-dns-resolver/          # rust-cueball
│   ├── cueball-static-resolver/       # rust-cueball
│   ├── cueball-tcp-stream-connection/ # rust-cueball
│   ├── cueball-postgres-connection/   # rust-cueball
│   ├── manatee-primary-resolver/      # rust-cueball
│   ├── manatee-echo-resolver/         # rust-cueball
│   ├── cueball-workspace-meta/        # (to be deleted)
│   ├── libmanta/                      # rust-libmanta
│   ├── moray/                         # rust-moray
│   ├── rust-utils/                    # rust-utils
│   ├── quickcheck-helpers/            # rust-quickcheck-helpers
│   ├── sharkspotter/                  # rust-sharkspotter
│   └── rebalancer-legacy/             # manta-rebalancer (original)
│       ├── rebalancer/                # shared library
│       ├── agent/                     # agent service
│       └── manager/                   # manager service
├── apis/                              # (future: Dropshot APIs)
├── services/                          # (future: Dropshot services)
├── clients/                           # (future: generated clients)
└── cli/                               # (future: new CLI)
```

---

## Verification Commands

```bash
# Verify history is preserved
git log --follow libs/fast/src/lib.rs
git log --follow libs/moray/src/lib.rs

# Verify all crates build
cargo build --workspace

# Verify tests pass
cargo test --workspace
```

---

## Risks and Mitigations

| Risk | Mitigation |
|------|------------|
| Merge conflicts | Each repo moves to unique directory; conflicts unlikely |
| Broken dependencies | Update Cargo.toml in dependency order, test incrementally |
| Missing files from `git mv` | Use `-k` flag, verify file counts before/after |
| Old Rust editions/dependencies | Address in separate modernization phase |

---

## Next Steps After This Plan

1. **Modernization**: Update Rust editions, dependency versions (separate commits)
2. **Dropshot Rewrite**: Implement new APIs in target locations (apis/, services/)
3. **Test Migration**: Port tests from rebalancer-legacy to new structure
4. **Cleanup**: Remove rebalancer-legacy after rewrite is complete
