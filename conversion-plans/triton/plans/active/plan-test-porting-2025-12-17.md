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

## Implementation Status

| Phase | Status | Notes |
|-------|--------|-------|
| Phase 1: Test Infrastructure | **COMPLETE** | All dev-deps, helpers, fixtures in place |
| Phase 2: Unit Tests | **COMPLETE** | 51 unit tests passing |
| Phase 3.1: CLI Basics Tests | **COMPLETE** | 32 integration tests passing |
| Phase 3.2: Profile Tests | **COMPLETE** | 17 profile tests passing (env profile, list, help) |
| Phase 3.3: Network Tests | **COMPLETE** | 17 tests (8 offline + 9 API ignored) |
| Phase 3.4: Account Tests | **COMPLETE** | 18 tests (6 offline + 5 API ignored) |
| Phase 3.5: Package Tests | **COMPLETE** | 23 tests (10 offline + 6 API ignored) |
| Phase 3.6: Image Tests | **COMPLETE** | 25 tests (12 offline + 6 API ignored) |
| Phase 3.7: Instance Tag Tests | **COMPLETE** | 17 offline tests, tag commands fixed |
| Phase 3.8: Key Tests | **COMPLETE** | 21 tests (14 offline + 2 API ignored) |
| Phase 3.9: Fwrule Tests | **COMPLETE** | 26 tests (17 offline + 2 API ignored) |
| Phase 3.10: Volume Tests | **COMPLETE** | 25 tests (15 offline + 3 API ignored) |
| Phase 4: Write Operations | Not Started | Requires allow_write_actions |
| Phase 5: Advanced Tests | Not Started | P3 priority |

**Total Tests: 228 offline passing, 38 API tests (ignored by default)**

## Current Session Progress (2025-12-17)

### Fixes Applied This Session

1. **`account get` output** - Changed to match node-triton format:
   - Uses lowercase field names (`login:`, `email:`, `companyName:`, etc.)
   - Added `long_ago()` function for relative timestamps (`1d`, `41w`)
   - File: `cli/triton-cli/src/commands/account.rs`

2. **`account limits` output** - Changed to match node-triton format:
   - Shows table with `TYPE  USED  LIMIT` columns (was showing "Provisioning Limits:" header)
   - JSON output returns `[]` array (was returning `{}` object)
   - File: `cli/triton-cli/src/commands/account.rs`

3. **`image get` output** - Changed to output JSON by default (matching node-triton)
   - File: `cli/triton-cli/src/commands/image.rs` line 521-538

4. **`json_stream_parse()` test helper** - Updated to handle both formats:
   - NDJSON (node-triton style): one JSON object per line
   - JSON array (Rust CLI style): pretty-printed `[...]` array
   - File: `cli/triton-cli/tests/common/mod.rs`

5. **`image get` short ID resolution** - Fixed to match node-triton behavior:
   - Now lists ALL images (without name filter) when looking up by name or short ID
   - Short ID is first segment of UUID (before first dash), exact match required
   - Prefers name matches over short ID matches
   - Returns most recent image when multiple name matches exist
   - File: `cli/triton-cli/src/commands/image.rs` lines 860-929

6. **`package get` output** - Changed to output JSON by default (matching node-triton):
   - Without `-j`: pretty-printed JSON
   - With `-j`: compact JSON (single line)
   - File: `cli/triton-cli/src/commands/package.rs` lines 109-134

7. **Instance tag commands** - Major refactor to match node-triton behavior:
   - `tag list` / `tags`: Always outputs JSON (pretty-printed without `-j`, compact with `-j`)
   - `tag get`: Plain text output, `-j` flag for JSON-quoted values
   - `tag set`: Outputs resulting tags as JSON after modification
   - `tag delete`: Supports multiple keys, added `-a`/`--all` flag, `-w` wait flag
   - `tag replace-all`: Renamed from `replace`, outputs resulting tags as JSON
   - Added `inst tags INST` shortcut for `inst tag list INST`
   - Tag value type parsing: "true"/"false" -> bool, numeric -> number
   - File loading: Supports both JSON object and key=value .kv format files
   - File: `cli/triton-cli/src/commands/instance/tag.rs`

8. **`fwrule instances` alias** - Added `insts` alias for `fwrule instances` command:
   - File: `cli/triton-cli/src/commands/fwrule.rs`

### New Test Files Added

1. **Key tests** (`cli/triton-cli/tests/cli_keys.rs`):
   - 14 offline help tests for key commands
   - 2 API tests for key list

2. **Fwrule tests** (`cli/triton-cli/tests/cli_fwrules.rs`):
   - 17 offline help tests for fwrule commands
   - 2 API tests for fwrule list

3. **Volume tests** (`cli/triton-cli/tests/cli_volumes.rs`):
   - 15 offline help tests for volume commands
   - 3 API tests for volume list and sizes

### All API Tests Passing

All API integration tests now pass:
- Account: 5 tests
- Images: 6 tests
- Networks: 9 tests
- Packages: 6 tests
- Keys: 2 tests
- Fwrules: 2 tests
- Volumes: 3 tests
- Profiles: 0 (all offline)
- Basics: 0 (all offline)

## Running Tests

### Offline Tests (No API Required)

Tests that don't require API access run with:

```bash
make triton-test
```

### API Integration Tests (Requires Config)

Tests requiring CloudAPI access are marked with `#[ignore]`. To run them:

1. **Create test configuration**:
   ```bash
   cp cli/triton-cli/tests/config.json.sample cli/triton-cli/tests/config.json
   # Edit config.json with your settings
   ```

2. **Run the API tests**:
   ```bash
   # Run only API tests (requires config.json)
   make triton-test-api

   # Run ALL tests (offline + API)
   make triton-test-all
   ```

3. **Configuration options** (in `config.json`):
   - `profileName`: Use existing profile from `~/.triton/profiles.d/` (e.g., `"env"`)
   - `profile`: Inline profile with `url`, `account`, `keyId`, `insecure`
   - `allowWriteActions`: Enable tests that create/modify resources (default: false)
   - `allowImageCreate`: Enable image creation tests (default: false)
   - `skipKvmTests`: Skip KVM-specific tests
   - `skipAffinityTests`: Skip multi-CN affinity tests

**Note:** The `TRITON_TEST_CONFIG` environment variable can point to an alternate config file location.

## Phase 1: Test Infrastructure Setup (COMPLETE)

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

## Phase 2: Unit Tests for Parsing Logic (COMPLETE)

### 2.1 Metadata Parsing Tests

**Location:** `cli/triton-cli/src/commands/instance/create.rs` (inline `#[cfg(test)]` module)

Port from `target/node-triton/test/unit/metadataFromOpts.test.js` (247 lines):

- [x] Simple key=value parsing (`-m foo=bar`)
- [x] Multiple metadata flags
- [x] Metadata file loading (`-M key=filepath`)
- [x] Volume spec parsing (NAME, NAME@MOUNTPOINT, NAME:MODE:MOUNTPOINT)
- [x] Disk spec parsing (SIZE, IMAGE:SIZE with G/M suffixes)
- [x] Brand parsing (bhyve, kvm, joyent, joyent-minimal, lx)

### 2.2 Tag Parsing Tests

**Location:** `cli/triton-cli/src/commands/instance/tag.rs` (inline tests)

- [x] Simple key=value tag parsing
- [x] Multiple tags
- [x] Empty values
- [x] Values with equals signs
- [x] Error cases (missing equals)

### 2.3 Volume Size Parsing Tests

**Location:** `cli/triton-cli/src/commands/volume.rs` (inline tests)

Port from `target/node-triton/test/unit/parseVolumeSize.test.js` (90 lines):

- [x] Valid sizes: "42G", "100G", "1024" (plain MB)
- [x] Invalid: "foo", "0", "-42", "", "042g" (leading zeros rejected)
- [x] Invalid prefix/suffix combinations
- [x] Tag parsing tests (boolean, numeric, float, string types)

### 2.4 Tests to Skip

| Test File | Reason |
|-----------|--------|
| `argvFromLine.test.js` | Clap handles argument parsing |
| `common.test.js` | Tests Node.js-specific utilities |

---

## Phase 3: CLI Integration Tests - Read-Only (P1 - Critical)

### 3.1 CLI Basics Tests (COMPLETE)

**File:** `cli/triton-cli/tests/cli_basics.rs`

Port from `target/node-triton/test/integration/cli-basics.test.js` (74 lines):

- [x] `triton --version` outputs version
- [x] `triton --help` shows usage (short and long forms)
- [x] `triton help <subcommand>` works
- [x] Invalid subcommand shows error
- [x] Shell completions (bash, zsh, fish)
- [x] Help for all subcommands (instance, volume, network, package, image, profile, key, fwrule, account, env)
- [x] Command aliases (inst, ls, pkg, img, net, vol)

### 3.2 Profile Tests (Read-Only) (COMPLETE)

**File:** `cli/triton-cli/tests/cli_profiles.rs`

Port read-only parts from `cli-profiles.test.js`:

- [x] `triton profile get env` reads from environment (TRITON_* and SDC_* vars)
- [x] `triton profile get env` with optional user field
- [x] `triton profile get env` with insecure flag
- [x] `triton profile get env` fails when required vars missing
- [x] `triton profile list` lists profiles (includes env profile)
- [x] `triton profile list` with empty config
- [x] `triton profile list -h` shows help
- [x] `triton profile get -h` shows help
- [x] `triton profile ls` alias works

**Bug Fixed:** `profile get env` now correctly uses `env_profile()` instead of trying to load from file.

### 3.3 Network Tests (Read-Only) (COMPLETE)

**File:** `cli/triton-cli/tests/cli_networks.rs`

Port from `cli-networks.test.js`:

**Offline tests (always run):**
- [x] `triton networks -h` shows help
- [x] `triton networks --help` shows help
- [x] `triton help networks` shows help
- [x] `triton network list -h` shows help
- [x] `triton network get -h` shows help
- [x] `triton network help get` shows help
- [x] `triton network get` without args shows error
- [x] `triton net` alias works
- [x] `triton net ls` alias works

**API tests (ignored by default):**
- [x] `triton networks` lists networks (table output)
- [x] `triton network list` lists networks
- [x] `triton networks -j` returns JSON array
- [x] `triton networks -l` shows long format
- [x] `triton network get ID` returns network details
- [x] `triton network get SHORTID` returns network details
- [x] `triton network get NAME` returns network details
- [x] `triton networks public=true` filters by public
- [x] `triton networks public=false` filters by non-public
- [x] `triton networks public=bogus` returns error

### 3.4 Account Tests (COMPLETE)

**File:** `cli/triton-cli/tests/cli_account.rs`

Port from `cli-account.test.js`:

**Offline tests (always run):**
- [x] `triton account -h` shows help
- [x] `triton account --help` shows help
- [x] `triton help account` shows help
- [x] `triton account get -h` shows help
- [x] `triton account limits -h` shows help
- [x] `triton account update -h` shows help

**API tests (ignored by default):**
- [x] `triton account get` returns account info
- [x] `triton account get -j` returns JSON
- [x] `triton account limits` returns limit info
- [x] `triton account limits -j` returns JSON array
- [x] `triton account update foo=bar` fails with invalid field

### 3.5 Package Tests (COMPLETE)

**File:** `cli/triton-cli/tests/cli_packages.rs`

**Offline tests (always run):**
- [x] `triton package -h` shows help
- [x] `triton package --help` shows help
- [x] `triton help package` shows help
- [x] `triton package list -h` shows help
- [x] `triton package get -h` shows help
- [x] `triton package help get` shows help
- [x] `triton package get` without args shows error
- [x] `triton pkg` alias works
- [x] `triton pkg ls` alias works
- [x] `triton pkgs` shortcut works

**API tests (ignored by default):**
- [x] `triton packages` lists packages (table output)
- [x] `triton package list` lists packages
- [x] `triton packages -j` returns JSON
- [x] `triton package get ID` returns package details
- [x] `triton package get SHORTID` returns package details
- [x] `triton package get NAME` returns package details

### 3.6 Image Tests (COMPLETE)

**File:** `cli/triton-cli/tests/cli_images.rs`

**Offline tests (always run):**
- [x] `triton image -h` shows help
- [x] `triton image --help` shows help
- [x] `triton help image` shows help
- [x] `triton image list -h` shows help
- [x] `triton image get -h` shows help
- [x] `triton image help get` shows help
- [x] `triton image get` without args shows error
- [x] `triton img` alias works
- [x] `triton img ls` alias works
- [x] `triton imgs` shortcut works
- [x] `triton image create -h` shows help
- [x] `triton image delete -h` shows help

**API tests (ignored by default):**
- [x] `triton images` lists images (table output)
- [x] `triton image list` lists images
- [x] `triton images -j` returns JSON with id, name, version
- [x] `triton image get ID` returns image details
- [x] `triton image get SHORTID` returns image details
- [x] `triton image get NAME` returns image details

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

| File | Purpose | Status |
|------|---------|--------|
| `cli/triton-cli/tests/common/mod.rs` | Test helpers | DONE |
| `cli/triton-cli/tests/common/config.rs` | Test configuration | DONE |
| `cli/triton-cli/tests/fixtures/*` | Test fixtures | DONE |
| `cli/triton-cli/tests/cli_basics.rs` | Basic CLI tests | DONE |
| `cli/triton-cli/tests/cli_profiles.rs` | Profile tests | DONE |
| `cli/triton-cli/tests/cli_networks.rs` | Network tests | DONE |
| `cli/triton-cli/tests/cli_account.rs` | Account tests | DONE |
| `cli/triton-cli/tests/cli_packages.rs` | Package tests | DONE |
| `cli/triton-cli/tests/cli_images.rs` | Image tests | DONE |
| `cli/triton-cli/tests/cli_instance_tag.rs` | Tag tests | DONE |
| `cli/triton-cli/tests/cli_keys.rs` | Key tests | DONE |
| `cli/triton-cli/tests/cli_fwrules.rs` | Firewall rule tests | DONE |
| `cli/triton-cli/tests/cli_volumes.rs` | Volume tests | DONE |

## Source Files to Reference

| Source | Purpose |
|--------|---------|
| `target/node-triton/test/integration/helpers.js` | Test helper patterns |
| `target/node-triton/test/unit/metadataFromOpts.test.js` | Metadata test cases |
| `target/node-triton/test/unit/tagsFromCreateOpts.test.js` | Tag test cases |
| `target/node-triton/test/unit/corpus/` | Unit test fixtures |
| `target/node-triton/test/integration/data/` | Integration fixtures |
