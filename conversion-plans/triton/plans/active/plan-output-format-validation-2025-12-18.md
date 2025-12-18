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

## Commands to Validate

### Resource Listing Commands

- [x] `triton package list` / `triton pkgs`
- [ ] `triton instance list` / `triton insts` / `triton ls`
- [ ] `triton image list` / `triton imgs`
- [ ] `triton network list` / `triton nets`
- [ ] `triton volume list` / `triton vols`
- [ ] `triton key list` / `triton keys`
- [ ] `triton fwrule list` / `triton fwrules`
- [ ] `triton vlan list` / `triton vlans`

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

- [ ] `triton account get`
- [ ] `triton info`
- [ ] `triton datacenters`
- [ ] `triton services`

### Instance Sub-resource Commands

- [ ] `triton instance tag list`
- [ ] `triton instance metadata list`
- [ ] `triton instance nic list`
- [ ] `triton instance snapshot list`
- [ ] `triton instance disk list`
- [ ] `triton instance audit`

### RBAC Commands

- [ ] `triton rbac users`
- [ ] `triton rbac roles`
- [ ] `triton rbac policies`
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

## Next Steps

1. Create test script to capture outputs from both CLIs
2. Build comparison table for each command
3. File issues or fix directly based on findings
