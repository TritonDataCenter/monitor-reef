<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

Copyright 2025 Edgecast Cloud LLC.
-->

# Plan: Triton CLI 100% Option Compatibility

**Created:** 2025-12-16
**Updated:** 2025-12-17
**Status:** In Progress
**Goal:** Achieve 100% option compatibility with node-triton

## Executive Summary

This plan details all remaining option gaps between the Rust `triton-cli` and `node-triton`. The analysis identified **~85 gaps** organized into actionable work items.

| Priority | Items | Effort | Impact |
|----------|-------|--------|--------|
| P1 - High | ~40 | Medium | Common use cases |
| P2 - Medium | ~25 | Low-Medium | Less common use cases |
| P3 - Low | ~20 | Low | Rare use cases |

**Key Insight:** Creating a shared table formatting utility would close ~30% of all gaps at once.

## Progress Summary

### ✅ Phase 1 Complete (2025-12-17)

**Shared Table Formatting:** Implemented `TableFormatArgs` struct and `TableBuilder` helper in `output/table.rs`. Added `-H`, `-o`, `-l`, `-s` options to 11 list commands.

**Short Flags:** Added `-s` to `image wait --state` and `-a` to `instance migration start --affinity`.

### ✅ Phase 2 Complete (2025-12-17)

**Wait/Timeout Support:** Added `--wait` and `--wait-timeout` options to async operations:
- `instance nic add` - wait for instance to return to running state
- `instance disk add` - wait for instance to return to running state
- `instance snapshot create` - wait for snapshot state to become "created"
- `instance tag set` - wait for instance to return to running state
- `instance metadata set` - wait for instance to return to running state
- `instance migration start` - added `--wait` and `--wait-timeout` to wait for migration completion
- `volume delete` - added `--wait-timeout` (already had `--wait`)

**File Input Support:** Added `-f/--file` option to read JSON data from file (or stdin with `-`):
- `instance tag set` - read tags from JSON file
- `instance metadata set` - read metadata from JSON file
- `network ip update` - read update data from JSON file
- `vlan update` - read update data from JSON file

See updated gap inventory below for current status.

---

## P1 - High Priority Work Items

### 1. Shared Table Formatting Options

**Problem:** 8+ list commands are missing standard table formatting options that node-triton provides via `common.getCliTableOptions()`.

**Missing Options:**
- `-H, --no-header` - Skip table header row
- `-o, --output COLUMNS` - Custom column selection
- `-l, --long` - Long/wider output format
- `-s, --sort-by FIELD` - Sort by field

**Commands Affected:**
- `profile list`
- `volume list`
- `volume sizes`
- `network list`
- `network ip list`
- `vlan list`
- `vlan networks`
- `fwrule list`
- `fwrule instances`
- `key list`
- `account limits`
- `rbac keys`

**Implementation:**

1. **Create shared table args struct** in `cli/triton-cli/src/output/table.rs`:

```rust
/// Common table formatting options matching node-triton's getCliTableOptions()
#[derive(Args, Clone, Default)]
pub struct TableFormatArgs {
    /// Skip table header row
    #[arg(short = 'H', long = "no-header")]
    pub no_header: bool,

    /// Specify columns to output (comma-separated)
    #[arg(short = 'o', long = "output", value_delimiter = ',')]
    pub columns: Option<Vec<String>>,

    /// Long/wider output format
    #[arg(short = 'l', long = "long")]
    pub long: bool,

    /// Sort by field (prefix with - for descending)
    #[arg(short = 's', long = "sort-by")]
    pub sort_by: Option<String>,
}
```

2. **Add to each list command's Args struct**:

```rust
#[derive(Args)]
pub struct ListArgs {
    #[command(flatten)]
    pub table: TableFormatArgs,
    // ... other args
}
```

3. **Update table printing logic** to respect these options.

**Files to modify:**
- `cli/triton-cli/src/output/table.rs` (new shared struct)
- `cli/triton-cli/src/commands/profile.rs` (list)
- `cli/triton-cli/src/commands/volume.rs` (list, sizes)
- `cli/triton-cli/src/commands/network.rs` (list, ip list)
- `cli/triton-cli/src/commands/vlan.rs` (list, networks)
- `cli/triton-cli/src/commands/fwrule.rs` (list, instances)
- `cli/triton-cli/src/commands/key.rs` (list)
- `cli/triton-cli/src/commands/account.rs` (limits)
- `cli/triton-cli/src/commands/rbac/keys.rs` (list)

**Estimated effort:** 3-4 hours

---

### 2. `--wait` and `--wait-timeout` for Async Operations

**Problem:** Several instance subcommands that perform async operations are missing wait functionality.

**Commands Missing `--wait`:**

| Command | File | Current State |
|---------|------|---------------|
| `instance nic add` | `instance/nic.rs` | No wait |
| `instance disk add` | `instance/disk.rs` | No wait |
| `instance snapshot create` | `instance/snapshot.rs` | No wait |
| `instance tag set` | `instance/tag.rs` | No wait |
| `instance metadata set` | `instance/metadata.rs` | No wait |
| `instance migration start` | `instance/migration.rs` | No wait |

**Commands Missing `--wait-timeout` (has `--wait`):**

| Command | File | Current State |
|---------|------|---------------|
| `volume delete` | `volume.rs` | Has `--wait`, missing `--wait-timeout` |

**Implementation Pattern:**

```rust
/// Wait for operation to complete
#[arg(short = 'w', long = "wait", action = ArgAction::Count)]
pub wait: u8,

/// Timeout for wait in seconds (default: 120)
#[arg(long = "wait-timeout", default_value = "120")]
pub wait_timeout: u64,
```

**Files to modify:**
- `cli/triton-cli/src/commands/instance/nic.rs`
- `cli/triton-cli/src/commands/instance/disk.rs`
- `cli/triton-cli/src/commands/instance/snapshot.rs`
- `cli/triton-cli/src/commands/instance/tag.rs`
- `cli/triton-cli/src/commands/instance/metadata.rs`
- `cli/triton-cli/src/commands/instance/migration.rs`
- `cli/triton-cli/src/commands/volume.rs`

**Estimated effort:** 2-3 hours

---

### 3. `--file` Input Support for Update Commands

**Problem:** Several commands that accept key=value pairs should also support reading from a JSON file.

**Commands Affected:**

| Command | File | Missing |
|---------|------|---------|
| `instance tag set` | `instance/tag.rs` | `-f, --file` |
| `instance metadata set` | `instance/metadata.rs` | `-f, --file` |
| `network ip update` | `network.rs` | `-f, --file` |
| `vlan update` | `vlan.rs` | `-f, --file` |

**Implementation:**

```rust
/// Read values from JSON file (use '-' for stdin)
#[arg(short = 'f', long = "file")]
pub file: Option<PathBuf>,
```

Then in the command handler:
```rust
let values = if let Some(file) = &args.file {
    if file.as_os_str() == "-" {
        serde_json::from_reader(std::io::stdin())?
    } else {
        serde_json::from_str(&std::fs::read_to_string(file)?)?
    }
} else {
    // Parse from command-line args
    parse_key_value_args(&args.values)?
};
```

**Files to modify:**
- `cli/triton-cli/src/commands/instance/tag.rs`
- `cli/triton-cli/src/commands/instance/metadata.rs`
- `cli/triton-cli/src/commands/network.rs`
- `cli/triton-cli/src/commands/vlan.rs`

**Estimated effort:** 1-2 hours

---

## P2 - Medium Priority Work Items

### 4. Missing Short Flag Aliases

**Problem:** Several commands have long options without their node-triton short equivalents.

| Command | Long Form | Missing Short | File |
|---------|-----------|---------------|------|
| `network create` | `--description` | `-D` | `network.rs` |
| `vlan create` | `--name` | `-n` | `vlan.rs` |
| `vlan create` | `--description` | `-D` | `vlan.rs` |
| `key add` | `--name` | `-n` | `key.rs` |
| `instance migration start` | `--affinity` | `-a` | `instance/migration.rs` |
| `image wait` | `--state` | `-s` | `image.rs` |

**Implementation:** Add `short = 'X'` to clap attributes:

```rust
#[arg(short = 'D', long = "description")]
pub description: Option<String>,
```

**Estimated effort:** 30 minutes

---

### 5. `--quiet` Flag for Suppressing Output

**Problem:** Some commands dump updated state after changes; node-triton allows suppressing this.

| Command | File |
|---------|------|
| `instance tag set` | `instance/tag.rs` |
| `instance metadata set` | `instance/metadata.rs` |
| `instance migration start` | `instance/migration.rs` |

**Implementation:**

```rust
/// Suppress output after changes
#[arg(short = 'q', long = "quiet")]
pub quiet: bool,
```

**Estimated effort:** 30 minutes

---

### 6. Instance NIC `--primary` Flag

**Problem:** `instance nic add` is missing the `--primary` flag to make the new NIC primary.

**File:** `cli/triton-cli/src/commands/instance/nic.rs`

**Implementation:**

```rust
/// Make the new NIC the primary NIC
#[arg(short = 'p', long = "primary")]
pub primary: bool,
```

Note: Verify CloudAPI supports this via the `primary` field in the request body.

**Estimated effort:** 30 minutes

---

### 7. RBAC-Specific Options

| Command | Missing Option | Description |
|---------|----------------|-------------|
| `rbac info` | `--all, -a` | Include all info for full report |
| `rbac info` | `--no-color` | Disable ANSI color codes |
| `rbac reset` | `--dry-run, -n` | Show what would be deleted |
| `rbac user` | `--roles, -r` | Include roles in output |
| `rbac role-tags` | `--edit, -e` | Edit in $EDITOR |

**Files to modify:**
- `cli/triton-cli/src/commands/rbac/apply.rs` (info)
- `cli/triton-cli/src/commands/rbac/user.rs`
- `cli/triton-cli/src/commands/rbac/role_tags.rs`

**Estimated effort:** 1-2 hours

---

## P3 - Low Priority Work Items

### 8. Firewall Rule Options

| Command | Missing | Description |
|---------|---------|-------------|
| `fwrule create` | `--log, -l` | Enable TCP connection logging |
| `fwrule create` | `--disabled, -d` | Create rule in disabled state |

**File:** `cli/triton-cli/src/commands/fwrule.rs`

**Note:** Rust currently uses `--enabled` (opposite logic). Consider adding `--disabled` as well for compatibility.

**Estimated effort:** 30 minutes

---

### 9. Key Command Options

| Command | Missing | Description |
|---------|---------|-------------|
| `key list` | `--authorized-keys, -A` | Output in OpenSSH authorized_keys format |
| `key delete` | `--yes, -y` | Hidden auto-confirm flag |

**File:** `cli/triton-cli/src/commands/key.rs`

**Estimated effort:** 30 minutes

---

### 10. Image Command Options

| Command | Missing | Description |
|---------|---------|-------------|
| `image get` | `-a, --all` | Include inactive images |
| `image wait` | Multi-state | Support comma-separated states |
| `image export` | `--dry-run` | Show what would be exported |
| `image share` | `--dry-run` | Show what would be shared |
| `image unshare` | `--dry-run` | Show what would be unshared |
| `image update` | `--homepage` | Update homepage field |
| `image update` | `--eula` | Update EULA field |

**File:** `cli/triton-cli/src/commands/image.rs`

**Estimated effort:** 1 hour

---

### 11. SSH Command Options

| Command | Missing | Description |
|---------|---------|-------------|
| `instance ssh` | `--no-disable-mux` | Control SSH multiplexing |

**File:** `cli/triton-cli/src/commands/instance/ssh.rs`

**Estimated effort:** 15 minutes

---

## Implementation Order

### Phase 1: High-Impact Quick Wins
1. **Shared table formatting** (3-4 hours) - Fixes ~25 gaps
2. **Missing short flags** (30 min) - Simple attribute changes

### Phase 2: Async Operation Improvements
3. **`--wait`/`--wait-timeout`** (2-3 hours) - Pattern exists, copy it
4. **`--file` input support** (1-2 hours) - Pattern exists in RBAC

### Phase 3: Medium Priority
5. **`--quiet` flags** (30 min)
6. **Instance NIC `--primary`** (30 min)
7. **RBAC options** (1-2 hours)

### Phase 4: Polish
8. **Firewall options** (30 min)
9. **Key options** (30 min)
10. **Image options** (1 hour)
11. **SSH options** (15 min)

---

## Verification

After each phase, run:

```bash
# Build
make package-build PACKAGE=triton-cli

# Run tests
make package-test PACKAGE=triton-cli

# Verify help output matches node-triton
./target/debug/triton <command> --help
```

Compare with node-triton:
```bash
./target/node-triton/bin/triton <command> --help
```

---

## Total Effort Estimate

| Phase | Items | Effort |
|-------|-------|--------|
| Phase 1 | Table formatting, short flags | 4-5 hours |
| Phase 2 | Wait, file input | 3-5 hours |
| Phase 3 | Quiet, primary, RBAC | 2-3 hours |
| Phase 4 | Polish items | 2-3 hours |
| **Total** | **~85 gaps** | **11-16 hours** |

---

## Appendix: Complete Gap Inventory

### Table Formatting Gaps (25 items) - ✅ COMPLETE

| Command | `-H` | `-o` | `-l` | `-s` |
|---------|------|------|------|------|
| `profile list` | ✅ | ✅ | ✅ | ✅ |
| `volume list` | ✅ | ✅ | ✅ | ✅ |
| `volume sizes` | ✅ | ✅ | N/A | ✅ |
| `network list` | ✅ | ✅ | ✅ | ✅ |
| `network ip list` | ✅ | ✅ | ✅ | ✅ |
| `vlan list` | ✅ | ✅ | ✅ | ✅ |
| `vlan networks` | ✅ | ✅ | ✅ | ✅ |
| `fwrule list` | ✅ | ✅ | ✅ | ✅ |
| `fwrule instances` | ✅ | ✅ | ✅ | ✅ |
| `key list` | ✅ | ✅ | ✅ | ✅ |
| `account limits` | N/A | N/A | N/A | N/A |
| `rbac keys` | ✅ | ✅ | ✅ | ✅ |

Note: `account limits` uses a nested key-value format, not a table, so table formatting options don't apply.

### Wait/Timeout Gaps (10 items) - ✅ COMPLETE

| Command | `--wait` | `--wait-timeout` |
|---------|----------|------------------|
| `instance nic add` | ✅ | ✅ |
| `instance disk add` | ✅ | ✅ |
| `instance snapshot create` | ✅ | ✅ |
| `instance tag set` | ✅ | ✅ |
| `instance metadata set` | ✅ | ✅ |
| `instance migration start` | ✅ | ✅ |
| `volume delete` | ✅ | ✅ |

### File Input Gaps (4 items) - ✅ COMPLETE

| Command | `-f, --file` |
|---------|--------------|
| `instance tag set` | ✅ |
| `instance metadata set` | ✅ |
| `network ip update` | ✅ |
| `vlan update` | ✅ |

### Short Flag Gaps (6 items) - ✅ COMPLETE

| Command | Option | Short | Status |
|---------|--------|-------|--------|
| `network create` | `--description` | `-D` | ✅ Already had |
| `vlan create` | `--name` | `-n` | ✅ Already had |
| `vlan create` | `--description` | `-D` | ✅ Already had |
| `key add` | `--name` | `-n` | ✅ Already had |
| `migration start` | `--affinity` | `-a` | ✅ Added |
| `image wait` | `--state` | `-s` | ✅ Added |

### Miscellaneous Gaps (20+ items)

See individual sections above for complete details.

---

## References

- [Evaluation Report](../reports/evaluation-report-2025-12-16.md)
- [CLI Option Compatibility](../reference/cli-option-compatibility.md)
- [node-triton source](../../target/node-triton/)
- [Rust triton-cli source](../../cli/triton-cli/)
