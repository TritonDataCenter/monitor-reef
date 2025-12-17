<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

Copyright 2025 Edgecast Cloud LLC.
-->

# Triton CLI Test Porting Plan

## Executive Summary

| Metric | Count |
|--------|-------|
| Node.js Unit Tests | 6 files (~1,196 lines) |
| Node.js Integration Tests | 28 files (~5,842 lines) |
| Tests to Port | ~30 files |
| Tests to Skip | 2 files (handled by Rust/Clap differently) |
| Estimated Rust Test Code | ~2,300 lines |

## Phase 1: Test Infrastructure Setup

### 1.1 Add dev-dependencies to Cargo.toml

**File:** `cli/triton-cli/Cargo.toml`

Add after line 45:
```toml
[dev-dependencies]
assert_cmd = "2.0"        # CLI testing with command assertions
predicates = "3.0"        # Fluent assertions for stdout/stderr
test-case = "3.3"         # Parameterized tests
pretty_assertions = "1.4" # Better diff output for test failures
```

### 1.2 Create Test Directory Structure

```
cli/triton-cli/tests/
  common/
    mod.rs              # Main test helpers
    config.rs           # Test configuration loading
  fixtures/
    metadata.json       # Copy from target/node-triton/test/unit/corpus/
    metadata.kv
    metadata-invalid-json.json
    metadata-illegal-types.json
    user-script.sh
    cloud.cfg
    tags.json           # Copy from target/node-triton/test/integration/data/
    tags.kv
    id_rsa.pub
  cli_basics.rs         # Help, version tests
  cli_profiles.rs       # Profile management tests
  cli_networks.rs       # Network listing tests
```

### 1.3 Implement Test Helpers

**File:** `cli/triton-cli/tests/common/mod.rs`

Key functions to implement (Rust equivalents of Node.js helpers.js):

| Node.js Function | Rust Equivalent |
|------------------|-----------------|
| `triton(args, cb)` | `fn triton(args: &[&str]) -> assert_cmd::Command` |
| `safeTriton(t, opts, cb)` | `fn safe_triton(args: &[&str]) -> String` |
| `getTestImg(t, cb)` | `async fn get_test_image() -> String` (cached) |
| `getTestPkg(t, cb)` | `async fn get_test_package() -> String` (cached) |
| `createTestInst(...)` | `struct TestInstance` with RAII Drop cleanup |
| `jsonStreamParse(...)` | `fn json_stream_parse<T>(output: &str) -> Vec<T>` |
| `makeResourceName(...)` | `fn make_resource_name(prefix: &str) -> String` |

### 1.4 Test Configuration

**File:** `cli/triton-cli/tests/common/config.rs`

Support same options as Node.js test/config.json:
- `profile_name` or inline `profile` object
- `allow_write_actions` (default: false)
- `allow_image_create` (default: false)
- `allow_volumes_tests` (default: true)
- `skip_affinity_tests`, `skip_kvm_tests`, `skip_flex_disk_tests`

Load from `TRITON_TEST_CONFIG` env var or `tests/config.json`.

---

## Phase 2: Unit Tests for Parsing Logic (P1 - Critical)

### 2.1 Metadata Parsing Tests

**Location:** `cli/triton-cli/src/commands/instance/create.rs` (inline `#[cfg(test)]` module)

Port from `target/node-triton/test/unit/metadataFromOpts.test.js` (247 lines):

- [ ] Simple key=value parsing (`-m foo=bar`)
- [ ] Multiple metadata flags
- [ ] File loading with `@filename` syntax (JSON and KV formats)
- [ ] User script loading (`--script`)
- [ ] Metadata file loading (`-M key=filepath`)
- [ ] Invalid JSON error handling
- [ ] Illegal types error handling (arrays/objects rejected)

### 2.2 Tag Parsing Tests

**Location:** `cli/triton-cli/src/commands/instance/tag.rs` (inline tests)

Port from:
- `target/node-triton/test/unit/tagsFromCreateOpts.test.js` (199 lines)
- `target/node-triton/test/unit/tagsFromSetArgs.test.js` (199 lines)

- [ ] Simple key=value tag parsing
- [ ] Multiple tags
- [ ] File loading (@filename.json, @filename.kv)
- [ ] Error cases

### 2.3 Volume Size Parsing Tests

**Location:** `cli/triton-cli/src/commands/volume.rs` (inline tests)

Port from `target/node-triton/test/unit/parseVolumeSize.test.js` (90 lines):

- [ ] Valid sizes: "42G", "100M", "1024"
- [ ] Invalid: "foo", "0", "-42", "", "042g"

### 2.4 Tests to Skip

| Test File | Reason |
|-----------|--------|
| `argvFromLine.test.js` | Clap handles argument parsing |
| `common.test.js` | Tests Node.js-specific utilities |

---

## Phase 3: CLI Integration Tests - Read-Only (P1 - Critical)

### 3.1 CLI Basics Tests

**File:** `cli/triton-cli/tests/cli_basics.rs`

Port from `target/node-triton/test/integration/cli-basics.test.js` (74 lines):

- [ ] `triton --version` outputs version
- [ ] `triton --help` shows usage
- [ ] `triton help <subcommand>` works
- [ ] Invalid subcommand shows error

### 3.2 Profile Tests (Read-Only)

**File:** `cli/triton-cli/tests/cli_profiles.rs`

Port read-only parts from `cli-profiles.test.js`:

- [ ] `triton profile get env` reads from environment
- [ ] `triton profile list` lists profiles
- [ ] Profile validation errors

### 3.3 Network Tests (Read-Only)

**File:** `cli/triton-cli/tests/cli_networks.rs`

Port from `cli-networks.test.js`:

- [ ] `triton networks -h` shows help
- [ ] `triton networks -j` returns JSON array
- [ ] `triton network get <id>` returns network details

### 3.4 Other Read-Only Tests

- [ ] `cli-account.test.js` - `triton account get`
- [ ] Package listing tests
- [ ] Image listing tests

---

## Phase 4: CLI Integration Tests - Write Operations (P2 - Important)

**Requires:** `allow_write_actions: true` in test config

### 4.1 Instance Tag Tests

**File:** `cli/triton-cli/tests/cli_instance_tag.rs`

Port from `cli-instance-tag.test.js` (243 lines):

- [ ] Create test instance with initial tag
- [ ] `triton inst tag ls` lists tags
- [ ] `triton inst tag set` adds/updates tags
- [ ] `triton inst tag get <key>` gets single tag
- [ ] `triton inst tag rm <key>` removes tag
- [ ] `triton inst tag replace-all` replaces all tags
- [ ] Cleanup test instance

### 4.2 Snapshot Tests

**File:** `cli/triton-cli/tests/cli_snapshots.rs`

- [ ] Create snapshot
- [ ] List snapshots
- [ ] Get snapshot
- [ ] Delete snapshot

### 4.3 Volume Tests

**File:** `cli/triton-cli/tests/cli_volumes.rs`

Port from `cli-volumes.test.js` and `cli-volumes-size.test.js`:

- [ ] Create volume
- [ ] List volumes
- [ ] Get volume
- [ ] Delete volume
- [ ] Size parsing edge cases

### 4.4 Firewall Rules Tests

**File:** `cli/triton-cli/tests/cli_fwrules.rs`

- [ ] Create rule
- [ ] List rules
- [ ] Update rule
- [ ] Delete rule

---

## Phase 5: Advanced Integration Tests (P3 - Nice-to-Have)

### Tests to Port (if time permits)

- [ ] `cli-manage-workflow.test.js` - Full instance lifecycle
- [ ] `cli-migrations.test.js` - Instance migrations
- [ ] `cli-deletion-protection.test.js` - Deletion protection
- [ ] `cli-nics.test.js` - NIC management
- [ ] `cli-vlans.test.js` - VLAN management
- [ ] `cli-ips.test.js` - IP management

### Tests to Skip (Special Infrastructure Required)

| Test File | Reason |
|-----------|--------|
| `cli-affinity.test.js` | Requires multi-CN setup |
| `cli-image-create-kvm.test.js` | Requires KVM support |
| `cli-disks.test.js` | Requires flex disk support |

---

## Critical Files to Modify

| File | Changes |
|------|---------|
| `cli/triton-cli/Cargo.toml` | Add dev-dependencies |
| `cli/triton-cli/src/commands/instance/create.rs` | Add unit tests |
| `cli/triton-cli/src/commands/instance/tag.rs` | Add unit tests |
| `cli/triton-cli/src/commands/volume.rs` | Add unit tests |

## Files to Create

| File | Purpose |
|------|---------|
| `cli/triton-cli/tests/common/mod.rs` | Test helpers |
| `cli/triton-cli/tests/common/config.rs` | Test configuration |
| `cli/triton-cli/tests/fixtures/*` | Test fixtures |
| `cli/triton-cli/tests/cli_basics.rs` | Basic CLI tests |
| `cli/triton-cli/tests/cli_profiles.rs` | Profile tests |
| `cli/triton-cli/tests/cli_networks.rs` | Network tests |
| `cli/triton-cli/tests/cli_instance_tag.rs` | Tag tests |

## Source Files to Reference

| Source | Purpose |
|--------|---------|
| `target/node-triton/test/integration/helpers.js` | Test helper patterns |
| `target/node-triton/test/unit/metadataFromOpts.test.js` | Metadata test cases |
| `target/node-triton/test/unit/tagsFromCreateOpts.test.js` | Tag test cases |
| `target/node-triton/test/unit/corpus/` | Unit test fixtures |
| `target/node-triton/test/integration/data/` | Integration fixtures |
