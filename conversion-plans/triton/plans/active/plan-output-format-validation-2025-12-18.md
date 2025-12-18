# Plan: Output Format Validation

**Date:** 2025-12-18
**Status:** Active
**Purpose:** Systematically compare CLI output formats between node-triton and triton-cli (Rust)

## Overview

The Rust CLI output formats need to match node-triton for user familiarity and script compatibility. This plan documents the validation approach and tracks discrepancies.

## Known Discrepancies

### `triton package list`

| Aspect | node-triton | triton-cli (Rust) | Status |
|--------|-------------|-------------------|--------|
| SWAP column | Present | Present | ✅ Fixed |
| Memory format | Human-readable (4G) | Human-readable (4G) | ✅ Fixed |
| Swap format | Human-readable (8G) | Human-readable (8G) | ✅ Fixed |
| Disk format | Human-readable (50G) | Human-readable (50G) | ✅ Fixed |
| Sort order | Group prefix, then memory | Group prefix, then memory | ✅ Fixed |
| Column alignment | Right-align numeric | Right-align numeric | ✅ Fixed |
| Cell padding | No leading/trailing spaces | No leading/trailing spaces | ✅ Fixed |

### `triton image list`

| Aspect | node-triton | triton-cli (Rust) | Status |
|--------|-------------|-------------------|--------|
| FLAGS column | I=incremental, S=shared, P=public | I=incremental, S=shared, P=public | ✅ Fixed |
| Sort order | By published date | By published date | ✅ Matches |
| Column format | SHORTID, NAME, VERSION, FLAGS, OS, TYPE, PUBDATE | Same | ✅ Matches |

### `triton network list`

| Aspect | node-triton | triton-cli (Rust) | Status |
|--------|-------------|-------------------|--------|
| Sort order | Public first, then by name | Public first, then by name | ✅ Fixed |
| Column format | SHORTID, NAME, SUBNET, GATEWAY, FABRIC, VLAN, PUBLIC | Same | ✅ Matches |

## Commands to Validate

### Resource Listing Commands

- [x] `triton package list` / `triton pkgs`
- [x] `triton instance list` / `triton insts` / `triton ls`
- [x] `triton image list` / `triton imgs`
- [x] `triton network list` / `triton nets`
- [x] `triton volume list` / `triton vols`
- [x] `triton key list` / `triton keys`
- [x] `triton fwrule list` / `triton fwrules`
- [x] `triton vlan list` / `triton vlans`

### Resource Detail Commands

- [ ] `triton package get`
- [ ] `triton instance get`
- [ ] `triton image get`
- [ ] `triton network get`
- [ ] `triton volume get`
- [ ] `triton key get`
- [ ] `triton fwrule get`
- [ ] `triton vlan get`

### Account/Info Commands

- [x] `triton account get`
- [ ] `triton info`
- [x] `triton datacenters`
- [x] `triton services`

### Instance Sub-resource Commands

- [ ] `triton instance tag list`
- [ ] `triton instance metadata list`
- [ ] `triton instance nic list`
- [ ] `triton instance snapshot list`
- [ ] `triton instance disk list`
- [ ] `triton instance audit`

### RBAC Commands

- [x] `triton rbac users`
- [x] `triton rbac roles`
- [x] `triton rbac policies`
- [ ] `triton rbac role-tags`

## Validation Approach

**Note:** User will run commands (requires Triton auth). Claude will analyze outputs and implement fixes.

For each command:

1. User runs: `triton <command>` (node-triton)
2. User runs: `./target/release/triton <command>` (Rust)
3. User shares both outputs
4. Claude compares:
   - Column headers (names, order, presence)
   - Data formatting (dates, sizes, UUIDs)
   - Sort order
   - Alignment/spacing
5. Claude documents discrepancies and implements fixes
6. User validates fixes

## Common Formatting Patterns to Check

### Size Formatting

node-triton uses human-readable sizes:
- Memory: `512M`, `1G`, `4G`, `16G`
- Disk: `25G`, `100G`, `1T`
- Swap: `1G`, `8G`, `32G`

### Date Formatting

Check timestamp formats:
- ISO 8601 vs human-readable
- Timezone handling

### UUID Display

- Full UUID vs short ID
- When each is used

### Boolean Display

- `true`/`false` vs `yes`/`no` vs checkmarks

## Priority

1. **High**: List commands (most commonly used)
2. **Medium**: Get commands
3. **Low**: RBAC and less common commands

## Current Status

**Last Updated:** 2025-12-18

Based on automated comparison testing (`./scripts/compare-cli-output.sh`):

| Category | Matching | Different | Notes |
|----------|----------|-----------|-------|
| Table outputs | 24 | - | All list commands match |
| JSON outputs | 7 | 9 | JSON field ordering/formatting differs |
| Help outputs | 0 | 12 | Expected - different CLI frameworks |
| Total | 24 | 23 | - |

### Matching Commands (Table Output)
- `package list`, `pkgs`
- `instance list`, `insts`
- `image list`, `imgs`
- `network list`
- `volume list`, `vols`
- `key list`, `keys`
- `fwrule list`, `fwrules`
- `vlan list`, `vlans`
- `account get`
- `datacenters`
- `services`
- `rbac users`, `rbac roles`, `rbac policies`

### Remaining Differences

1. **JSON outputs** - Field ordering, null handling, numeric precision differ between implementations. This is expected and generally acceptable for machine-parseable output.

2. **Help text** - Different CLI frameworks (Clap vs Node.js) produce different help formats. This is by design.

3. **`info` command** - Output format differs (needs investigation if critical).

## Next Steps

1. ~~Create test script to capture outputs from both CLIs~~ ✅ Done (`scripts/compare-cli-output.sh`)
2. ~~Build comparison table for each command~~ ✅ Done (automated)
3. Investigate `info` command differences if needed
4. Consider if JSON output normalization is needed for specific use cases
