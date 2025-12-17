<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

Copyright 2025 Edgecast Cloud LLC.
-->

# Triton CLI Test Porting Planning Prompt

## Objective

Create a comprehensive plan for porting the node-triton test suite to Rust tests for triton-cli. The goal is to achieve equivalent test coverage while leveraging Rust testing idioms and the existing Rust ecosystem.

This planning prompt should produce a prioritized implementation plan with clear work items.

## Goals and Non-Goals

### Goals

- **Equivalent coverage** - Port all meaningful tests from node-triton
- **Rust-idiomatic tests** - Use standard Rust testing patterns (not literal translations)
- **Test infrastructure** - Create reusable test helpers for CLI and API testing
- **CI/CD ready** - Tests should run in automated pipelines
- **Configurable** - Support the same test configuration options (allowWriteActions, skipKvmTests, etc.)

### Non-Goals

- Literal 1:1 translation of JavaScript test code to Rust
- Porting tests for deprecated or removed features
- Replicating TAP-specific output format (Rust test runner is sufficient)

## Source Locations

| Component | Location |
|-----------|----------|
| Node.js tests | `target/node-triton/test/` |
| Node.js unit tests | `target/node-triton/test/unit/*.test.js` |
| Node.js integration tests | `target/node-triton/test/integration/*.test.js` |
| Node.js test helpers | `target/node-triton/test/integration/helpers.js` |
| Node.js test common | `target/node-triton/test/lib/testcommon.js` |
| Test fixtures | `target/node-triton/test/unit/corpus/` and `target/node-triton/test/integration/data/` |
| Rust CLI | `cli/triton-cli/` |
| Existing Rust tests | `cli/triton-cli/src/main.rs` (verify_cli_structure), `cli/triton-cli/src/config/paths.rs` |

## Node.js Test Suite Overview

### Unit Tests (6 files, ~1,196 lines)

Located in `target/node-triton/test/unit/`:

| File | Purpose | Priority |
|------|---------|----------|
| `common.test.js` | Tests `lib/common.js` (objCopy, deepObjCopy) | P3 - Rust handles this differently |
| `metadataFromOpts.test.js` | Parsing metadata options for instance creation | P1 - Critical for `triton create` |
| `tagsFromCreateOpts.test.js` | Parsing tags from create command options | P1 - Critical for tagging |
| `tagsFromSetArgs.test.js` | Parsing tags from set command arguments | P1 - Critical for tagging |
| `argvFromLine.test.js` | Parsing command-line arguments | P3 - Clap handles this |
| `parseVolumeSize.test.js` | Parsing volume size specifications | P2 - Volume operations |

### Integration Tests (28 files, ~5,842 lines)

Located in `target/node-triton/test/integration/`:

**API Tests (7 files)** - Test TritonApi client methods directly:
- `api-images.test.js`, `api-instances.test.js`, `api-ips.test.js`
- `api-networks.test.js`, `api-nics.test.js`, `api-packages.test.js`, `api-vlans.test.js`

**CLI Tests (21 files)** - End-to-end CLI command testing:
- Basic: `cli-basics.test.js` (help, version), `cli-subcommands.test.js`
- Account: `cli-account.test.js`, `cli-profiles.test.js`, `cli-keys.test.js`
- Instance: `cli-manage-workflow.test.js`, `cli-instance-tag.test.js`, `cli-snapshots.test.js`
- Instance features: `cli-deletion-protection.test.js`, `cli-disks.test.js`, `cli-affinity.test.js`, `cli-migrations.test.js`
- Network: `cli-networks.test.js`, `cli-nics.test.js`, `cli-vlans.test.js`, `cli-ips.test.js`
- Firewall: `cli-fwrules.test.js`
- Images: `cli-image-create.test.js`, `cli-image-create-kvm.test.js`
- Volumes: `cli-volumes.test.js`, `cli-volumes-size.test.js`

### Test Infrastructure Components

**Test Configuration** (`test/config.json`):
```json
{
  "profileName": "test-profile",
  "allowWriteActions": false,
  "allowImageCreate": false,
  "allowVolumesTests": true,
  "skipAffinityTests": false,
  "skipKvmTests": false,
  "skipFlexDiskTests": false
}
```

**Key Helper Functions** (from `helpers.js`):
- `triton(args, opts, cb)` - Execute CLI with environment-based profile
- `safeTriton(t, opts, cb)` - Execute CLI with error/stderr validation
- `getTestImg(t, cb)` - Find compatible base/minimal image for provisioning
- `getTestKvmImg(t, cb)` - Find compatible KVM image
- `getTestPkg(t, cb)` - Find smallest available package
- `createTestInst(t, opts, cb)` - Create test instance with cleanup
- `deleteTestInst(t, opts, cb)` - Clean up test instance
- `createClient()` - Create TritonApi client for direct API testing
- `jsonStreamParse(stdout)` - Parse JSON stream output

## Evaluation Tasks

### Task 1: Analyze Existing Rust Test Infrastructure

1. Review current tests in `cli/triton-cli/src/`
2. Identify existing dev-dependencies in `Cargo.toml`
3. Document what test patterns are already established
4. Identify gaps in test infrastructure

### Task 2: Design Test Infrastructure

Design Rust equivalents for these key Node.js components:

**Test Helpers Module** (`tests/helpers.rs` or `tests/common/mod.rs`):

| Node.js Function | Rust Equivalent | Notes |
|------------------|-----------------|-------|
| `triton(args, cb)` | `run_triton(args: &[&str]) -> Result<Output>` | Use `std::process::Command` |
| `safeTriton(t, opts, cb)` | `run_triton_success(args: &[&str]) -> String` | Assert exit code 0, empty stderr |
| `getTestImg(t, cb)` | `get_test_image() -> String` | Find suitable image, cache result |
| `getTestPkg(t, cb)` | `get_test_package() -> String` | Find suitable package, cache result |
| `createTestInst(...)` | `TestInstance::create(...) -> TestInstance` | RAII cleanup with Drop trait |
| `jsonStreamParse(...)` | Parse via serde_json | Rust handles this naturally |

**Test Configuration** (`tests/config.rs`):
- Load from `TRITON_TEST_CONFIG` or `test/config.json`
- Support same config options as Node.js
- Provide `#[ignore]` annotations for conditional test execution

**Fixtures**:
- Port `test/unit/corpus/*` to `cli/triton-cli/tests/fixtures/`
- Port `test/integration/data/*` to appropriate location

### Task 3: Categorize Tests by Porting Strategy

**Category A: Direct Port** - Tests that translate naturally to Rust:
- Unit tests for parsing logic
- CLI output validation tests
- JSON response validation tests

**Category B: Redesign** - Tests that need Rust-specific approach:
- Tests heavily dependent on TAP assertions
- Tests using Node.js callback patterns
- Tests with complex async chains

**Category C: Skip/Defer** - Tests that may not be needed:
- Tests for features not in Rust CLI
- Tests for deprecated behaviors
- Tests covered by Rust's type system

### Task 4: Create Implementation Plan

For each test category, create prioritized work items:

**Phase 1: Test Infrastructure**
- [ ] Create `tests/` directory structure
- [ ] Implement test helpers module
- [ ] Create test configuration system
- [ ] Port test fixtures

**Phase 2: Unit Tests**
- [ ] Port metadata parsing tests
- [ ] Port tag parsing tests
- [ ] Port volume size parsing tests

**Phase 3: Integration Tests - Read-Only**
- [ ] Port `cli-basics.test.js` (help, version)
- [ ] Port `cli-account.test.js` (account get)
- [ ] Port `cli-profiles.test.js` (profile management)
- [ ] Port list commands (instances, images, networks, packages)

**Phase 4: Integration Tests - Write Operations**
- [ ] Port instance lifecycle tests
- [ ] Port snapshot tests
- [ ] Port tag operation tests
- [ ] Port image create tests

**Phase 5: API Tests (if applicable)**
- Determine if direct API testing is needed
- Consider if cloudapi-client tests provide sufficient coverage

## Output Format

### Plan Document Structure

Create plan at `conversion-plans/triton/plans/active/plan-test-porting-YYYY-MM-DD.md`:

```markdown
# Test Porting Plan

## Executive Summary

| Metric | Count |
|--------|-------|
| Node.js Unit Tests | X files |
| Node.js Integration Tests | X files |
| Tests to Port | X |
| Tests to Skip | X (with justification) |

## Phase 1: Test Infrastructure

### 1.1 Directory Structure
- [ ] Create `cli/triton-cli/tests/` directory
- [ ] Create `cli/triton-cli/tests/common/mod.rs` for helpers
- [ ] Create `cli/triton-cli/tests/fixtures/` for test data

### 1.2 Test Helpers
- [ ] Implement `run_triton()` CLI execution helper
- [ ] Implement `run_triton_success()` with assertions
- [ ] Implement test image/package discovery
- [ ] Implement `TestInstance` with RAII cleanup

### 1.3 Configuration
- [ ] Create test config loading from file/env
- [ ] Support `allowWriteActions` flag
- [ ] Support skip flags (KVM, affinity, etc.)

## Phase 2: Unit Tests

### 2.1 Metadata Parsing
- [ ] Port `metadataFromOpts.test.js` test cases
- [ ] Test `-m key=value` parsing
- [ ] Test `-m @file.json` parsing
- [ ] Test `--script` and `--cloud-config` handling

[Continue with detailed work items...]

## Deferred/Skipped Tests

| Test File | Reason |
|-----------|--------|
| `common.test.js` | Rust handles object copying differently |
| `argvFromLine.test.js` | Clap handles argument parsing |

## Dependencies

- `tempfile` (already in dev-dependencies)
- `assert_cmd` - CLI testing assertions
- `predicates` - Output matching
- Consider: `wiremock` for API mocking (if needed)
```

## Rust Testing Patterns to Use

### CLI Integration Tests

Use `assert_cmd` crate for CLI testing:

```rust
use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn test_triton_help() {
    Command::cargo_bin("triton")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("triton"));
}

#[test]
fn test_instance_list_json() {
    Command::cargo_bin("triton")
        .unwrap()
        .args(["instance", "list", "--json"])
        .env("TRITON_PROFILE", "test")
        .assert()
        .success();
}
```

### Test Instance Cleanup (RAII)

```rust
struct TestInstance {
    id: String,
}

impl TestInstance {
    fn create(name: &str) -> Result<Self> {
        // Create instance via CLI
        Ok(Self { id })
    }
}

impl Drop for TestInstance {
    fn drop(&mut self) {
        // Clean up instance
        let _ = Command::cargo_bin("triton")
            .args(["instance", "delete", "-w", &self.id])
            .output();
    }
}
```

### Conditional Test Execution

```rust
fn should_run_write_tests() -> bool {
    std::env::var("TRITON_TEST_ALLOW_WRITE")
        .map(|v| v == "true" || v == "1")
        .unwrap_or(false)
}

#[test]
#[ignore] // Run with: cargo test -- --ignored
fn test_instance_create() {
    if !should_run_write_tests() {
        return;
    }
    // Test implementation
}
```

## Validation Steps

After creating the plan:

1. **Verify test file inventory**:
   ```bash
   ls -la target/node-triton/test/unit/
   ls -la target/node-triton/test/integration/
   ```

2. **Count lines and complexity**:
   ```bash
   wc -l target/node-triton/test/unit/*.test.js
   wc -l target/node-triton/test/integration/*.test.js
   ```

3. **Check existing Rust tests**:
   ```bash
   grep -r "#\[test\]" cli/triton-cli/src/
   ```

4. **Review Cargo.toml dev-dependencies**:
   ```bash
   grep -A 10 "\[dev-dependencies\]" cli/triton-cli/Cargo.toml
   ```

## References

- [Rust Book: Testing](https://doc.rust-lang.org/book/ch11-00-testing.html)
- [assert_cmd crate](https://docs.rs/assert_cmd)
- [predicates crate](https://docs.rs/predicates)
- [TAP testing framework](https://testanything.org/) (Node.js source)
- [Node.js triton test suite](target/node-triton/test/)
- [Existing triton-cli code](cli/triton-cli/)
