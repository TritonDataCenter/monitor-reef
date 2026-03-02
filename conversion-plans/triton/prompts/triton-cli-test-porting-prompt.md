<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

Copyright 2025 Edgecast Cloud LLC.
-->

# Triton CLI Test Development Prompt

## Objective

Develop new tests for triton-cli to fill quality gaps identified by the behavioral evaluation. The initial test porting from node-triton is complete (~747 tests exist). This prompt focuses on writing tests that catch **real bugs** — the kind that slipped past the initial porting round.

This prompt should produce new test code, improved fixtures, and strengthened existing assertions.

## Context: What Exists

### Current Test Infrastructure

| Component | Location | Status |
|-----------|----------|--------|
| Integration tests (26 files, ~636 tests) | `cli/triton-cli/tests/cli_*.rs` | Ported from node-triton |
| Test helpers | `cli/triton-cli/tests/common/mod.rs` | Complete |
| Test config | `cli/triton-cli/tests/common/config.rs` | Complete |
| Fixtures (15 files) | `cli/triton-cli/tests/fixtures/` | Basic coverage |
| Inline unit tests (~104 tests) | `cli/triton-cli/src/**/*.rs` | Good coverage |

### Dev Dependencies (from Cargo.toml)

- `assert_cmd` 2.0 — CLI execution and assertions
- `predicates` 3.0 — Fluent output matching
- `test-case` 3.3 — Parameterized tests
- `pretty_assertions` 1.4 — Better diff output
- `hostname` 0.4 — Unique test resource naming
- `regex` 1.10 — Pattern matching
- `rcgen` 0.14 — Certificate generation for TLS tests
- `tokio-rustls` 0.26 — TLS server for insecure-mode tests

### Test Helper Functions (from `common/mod.rs`)

| Function | Purpose |
|----------|---------|
| `triton_cmd()` | Get `assert_cmd::Command` for triton binary |
| `run_triton(args)` | Execute triton, capture output |
| `run_triton_with_env(args, env)` | Execute with environment variables |
| `json_stream_parse(stdout)` | Parse NDJSON or JSON array output |
| `make_resource_name(prefix)` | Generate unique resource names |
| `fixtures_dir()` / `fixture_path(name)` | Fixture file access |
| `safe_triton(args)` | Run and assert success |
| `create_test_instance(...)` | Create instance for testing (API) |
| `delete_test_instance(...)` | Clean up test instance (API) |
| `get_test_image()` | Find suitable test image (API) |
| `get_test_package()` | Find smallest test package (API) |
| `get_resize_test_package()` | Find resize-compatible package (API) |

### Known Acceptable Differences (do NOT test for these)

See `conversion-plans/triton/reference/acceptable-output-differences.md`:
- Clap-style help format vs node-triton custom format
- JSON key ordering
- Empty `[]` vs empty output for empty results
- Clap error message format vs `error (Type):` format
- Exit code 2 for usage errors (Clap) vs 1 (node-triton)

## Source Locations

| Component | Location |
|-----------|----------|
| Rust CLI source | `cli/triton-cli/src/` |
| Rust CLI tests | `cli/triton-cli/tests/` |
| Test helpers | `cli/triton-cli/tests/common/mod.rs` |
| Test config | `cli/triton-cli/tests/common/config.rs` |
| Test fixtures | `cli/triton-cli/tests/fixtures/` |
| API types | `apis/cloudapi-api/src/types/` |
| Node.js triton source | `target/node-triton/lib/` |
| Node.js triton tests | `target/node-triton/test/` |
| Acceptable differences | `conversion-plans/triton/reference/acceptable-output-differences.md` |
| Type safety rules | `CLAUDE.md` (Type Safety Rules section) |

## Goals and Non-Goals

### Goals

- **Strengthen weak assertions** — Upgrade tests from "runs without crashing" to "produces correct output"
- **Add edge case tests** — Boundary conditions, empty inputs, malformed data
- **Add error path tests** — Verify errors propagate correctly, not silently swallowed
- **Add output format tests** — Verify JSON fields, table columns, value formatting against fixtures
- **Add wire format tests** — Verify serde serialization/deserialization of API types
- **File issues for bugs found** — Use beads (`bd`) for anything that requires code changes

### Non-Goals

- Re-porting node-triton tests already ported
- Adding tests for features not yet implemented
- Testing Clap's argument parsing (Clap is well-tested)
- Testing help text content (acceptable difference)

## Development Tasks

### Phase 1: Strengthen Existing Assertions

Review each existing test file and upgrade weak assertions. The pattern to look for:

```rust
// BEFORE: Only checks exit code
triton_cmd()
    .args(["instance", "list", "--json"])
    .assert()
    .success();

// AFTER: Validates output structure
let output = triton_cmd()
    .args(["instance", "list", "--json"])
    .output()
    .expect("failed to run triton");
assert!(output.status.success());
let instances: Vec<serde_json::Value> = json_stream_parse(&String::from_utf8_lossy(&output.stdout));
assert!(!instances.is_empty(), "should return at least one instance");
for inst in &instances {
    assert!(inst.get("id").is_some(), "instance should have id field");
    assert!(inst.get("name").is_some(), "instance should have name field");
    assert!(inst.get("state").is_some(), "instance should have state field");
}
```

**Priority files to review** (sorted by likely weakness):

| File | Test Count | Likely Issue |
|------|-----------|--------------|
| `cli_subcommands.rs` | 218 | Many may only test `--help` success |
| `cli_output_format.rs` | 26 | May not validate all JSON fields |
| `cli_manage_workflow.rs` | 18 | Complex workflow — check each step asserts |
| `cli_error_paths.rs` | 40 | Check error messages are specific |

### Phase 2: Add Fixture-Based Output Tests

Create comprehensive fixtures and tests that validate exact output format without requiring API access.

#### 2a. Expand Fixtures

Current fixtures cover basic cases. Add fixtures for:

| Fixture | Purpose | File |
|---------|---------|------|
| Machine with all optional fields | Tests null handling | `fixtures/machine/instance_full.json` |
| Machine with snake_case exceptions | Tests `dns_names`, `free_space`, `delegate_dataset` | `fixtures/machine/instance_snake_case.json` |
| Machine with unknown brand/state | Tests `#[serde(other)]` handling | `fixtures/machine/instance_unknown_enum.json` |
| Image with all types | Tests ImageType variants | `fixtures/image/image_types.json` |
| Empty list responses | Tests `[]` output | `fixtures/empty_list.json` |
| Audit log entries | Tests audit trail format | `fixtures/machine/audit_log.json` |
| NIC list response | Tests MAC address formatting | `fixtures/machine/nic_list.json` |
| Snapshot list response | Tests snapshot fields | `fixtures/machine/snapshot_list.json` |
| RBAC user/role/policy | Tests RBAC object format | `fixtures/rbac/` |

#### 2b. Write Deserialization Round-Trip Tests

For each major API type, verify serialization matches the wire format:

```rust
#[test]
fn test_machine_deserialize_snake_case_fields() {
    let json = r#"{
        "id": "b6c0e147-96c7-4899-a3a5-e21d1fa1c6ad",
        "name": "test-machine",
        "state": "running",
        "dns_names": ["test-machine.inst.us-east-1.triton.zone"],
        "free_space": 10240
    }"#;
    let machine: Machine = serde_json::from_str(json).expect("should deserialize");
    assert_eq!(machine.dns_names.unwrap(), vec!["test-machine.inst.us-east-1.triton.zone"]);
    assert_eq!(machine.free_space.unwrap(), 10240);

    // Round-trip: verify field names in serialized output
    let reserialized = serde_json::to_string(&machine).unwrap();
    assert!(reserialized.contains("\"dns_names\""), "should serialize as dns_names, not dnsNames");
    assert!(reserialized.contains("\"free_space\""), "should serialize as free_space, not freeSpace");
}

#[test]
fn test_machine_unknown_state() {
    let json = r#"{"id": "...", "name": "test", "state": "totally_new_state"}"#;
    let machine: Machine = serde_json::from_str(json).expect("should handle unknown state");
    // Should not panic, should deserialize to Unknown variant
}
```

#### 2c. Write Table Output Tests

For each `list` command, verify default table columns against node-triton:

```rust
#[test]
fn test_instance_list_table_columns() {
    // Test against fixture data
    let output = triton_cmd()
        .args(["instance", "list"])
        .env("TRITON_TEST_FIXTURE", fixture_path("machine/instance_list_multi.json"))
        .output()
        .expect("should run");
    let stdout = String::from_utf8_lossy(&output.stdout);

    // Verify column headers match node-triton
    let first_line = stdout.lines().next().expect("should have output");
    assert!(first_line.contains("SHORTID"), "should have SHORTID column");
    assert!(first_line.contains("NAME"), "should have NAME column");
    assert!(first_line.contains("IMG"), "should have IMG column");
    assert!(first_line.contains("STATE"), "should have STATE column");
    assert!(first_line.contains("AGE"), "should have AGE column");
}
```

### Phase 3: Add Error Path Tests

#### 3a. API Error Handling

Test how each command handles various HTTP error responses:

```rust
#[test]
fn test_instance_get_not_found() {
    // Should produce a clear error, not panic or silently succeed
    let output = triton_cmd()
        .args(["instance", "get", "nonexistent-instance-12345"])
        .output()
        .expect("should run");
    assert!(!output.status.success(), "should fail for nonexistent instance");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.is_empty(), "should have error output on stderr");
}
```

#### 3b. Input Validation

Test boundary conditions for user input:

| Test | Input | Expected |
|------|-------|----------|
| Empty instance name | `instance get ""` | Error, not crash |
| Very long name | `instance get <256 chars>` | Error, not crash |
| Special characters | `instance get "foo;rm -rf /"` | Error, no command injection |
| Invalid UUID format | `instance get "not-a-uuid"` | Treated as name, not panic |
| Negative limit | `instance list --limit -1` | Error, not wrap to max |
| Zero limit | `instance list --limit 0` | Error or empty result |
| Huge limit | `instance list --limit 999999999999` | Handled gracefully |

#### 3c. Silent Failure Detection

Write tests that verify errors are NOT silently swallowed:

```rust
#[test]
fn test_rbac_apply_reports_failures() {
    // If any operation fails during rbac apply, the command
    // should return non-zero exit code
    let output = triton_cmd()
        .args(["rbac", "apply", "-f", fixture_path("rbac/invalid_config.json")])
        .output()
        .expect("should run");
    assert!(
        !output.status.success(),
        "rbac apply with invalid config should fail, not silently succeed"
    );
}
```

### Phase 4: Add Missing Command Tests

Cross-reference the command list against test files to find untested commands:

| Command | Test File | Status | What to Add |
|---------|-----------|--------|-------------|
| `instance audit` | `cli_manage_workflow.rs` | Partial | Dedicated audit output format test |
| `instance migration *` | `cli_migrations.rs` | Help-only | Add workflow test (requires API) |
| `changefeed` | None | Missing | Add help/args test at minimum |
| `cloudapi` (raw) | None | Missing | Add basic request test |
| `profile create/edit` | `cli_profiles.rs` | Help-only | Add workflow test (offline possible with temp dir) |
| `profile docker-setup` | None | Missing | Add help test |
| `profile cmon-certgen` | None | Missing | Add help test |
| `rbac apply/reset` | None | Missing | Add help tests, fixture-based validation |
| `rbac role-tags *` | None | Missing | Add help tests |
| `image tag *` | None | Missing | Add help tests |

### Phase 5: Test Infrastructure Improvements

#### 5a. Fix Env Var Racing

Tests that set environment variables can race with parallel test execution. Use per-test isolated approaches:

```rust
// WRONG: Races with other tests
std::env::set_var("TRITON_PROFILE", "test");

// RIGHT: Pass env through Command
triton_cmd()
    .env("TRITON_PROFILE", "test")
    .args(["instance", "list"])
    // ...
```

Audit all tests in `cli/triton-cli/tests/` for direct `std::env::set_var` calls.

#### 5b. Improve json_stream_parse

The current `json_stream_parse` silently drops parse failures. Improve it to:
- Return `Result` instead of silently dropping errors
- Provide context on which line failed to parse
- Distinguish between NDJSON and JSON array format

#### 5c. Add Test Categorization

Tests should be clearly categorized so they can be run selectively:

```rust
// Offline tests: no API access needed, run in CI always
#[test]
fn test_instance_list_help() { ... }

// Fixture tests: use fixture data, no API access
#[test]
fn test_instance_list_json_format() { ... }

// API read tests: require TRITON_PROFILE, read-only
#[test]
#[ignore] // cargo test -- --ignored
fn test_instance_list_api() { ... }

// API write tests: require allowWriteActions
#[test]
#[ignore]
fn test_instance_create_workflow() { ... }
```

## Patterns and Anti-Patterns

### Good Test Patterns

```rust
// Pattern: Parameterized tests for enum variants
#[test_case("running" ; "running state")]
#[test_case("stopped" ; "stopped state")]
#[test_case("provisioning" ; "provisioning state")]
fn test_machine_state_deserialization(state: &str) {
    let json = format!(r#"{{"state": "{}"}}"#, state);
    let machine: Machine = serde_json::from_str(&json).expect("should deserialize");
    // Verify Display output matches wire format
    assert_eq!(enum_to_display(&machine.state), state);
}

// Pattern: Fixture-based round-trip
#[test]
fn test_network_list_fixture_roundtrip() {
    let fixture = std::fs::read_to_string(fixture_path("network_list.json")).unwrap();
    let networks: Vec<Network> = serde_json::from_str(&fixture).unwrap();
    let reserialized = serde_json::to_string(&networks).unwrap();
    // Parse both and compare structurally (ignoring key order)
    let original: serde_json::Value = serde_json::from_str(&fixture).unwrap();
    let roundtripped: serde_json::Value = serde_json::from_str(&reserialized).unwrap();
    assert_eq!(original, roundtripped);
}
```

### Anti-Patterns to Avoid

```rust
// BAD: Test that always passes
#[test]
fn test_something() {
    let _ = triton_cmd().arg("--help");
    // No assertion!
}

// BAD: Assertion on wrong thing
#[test]
fn test_instance_list() {
    triton_cmd()
        .args(["instance", "list"])
        .assert()
        .success(); // Only checks exit code, not output correctness
}

// BAD: Test that tests its own setup
#[test]
fn test_json_parse() {
    let json = serde_json::to_string(&Machine { name: "test".into(), .. }).unwrap();
    let parsed: Machine = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.name, "test"); // Always passes — we just serialized it
}

// BAD: Hardcoded string matching an enum
#[test]
fn test_state_filter() {
    // Should use MachineState::Running, not "running"
    assert!(output.contains("running"));
}
```

## Output Format

### Plan Document

Create plan at `conversion-plans/triton/plans/active/plan-test-development-YYYY-MM-DD.md`:

```markdown
# Test Development Plan

## Summary

| Category | New Tests | Strengthened | Fixtures Added |
|----------|----------|-------------|----------------|
| Assertion upgrades | - | X | - |
| Output format | X | - | X |
| Error paths | X | - | X |
| Edge cases | X | - | - |
| Missing commands | X | - | - |
| Wire format | X | - | X |
| **Total** | **X** | **X** | **X** |

## Phase 1: Assertion Upgrades
- [ ] File: tests, description of upgrade

## Phase 2: Fixture Expansion
- [ ] Fixture file, purpose

[Continue for each phase...]
```

## Validation Steps

After writing new tests:

1. **Run all tests:**
   ```bash
   make package-test PACKAGE=triton-cli
   ```

2. **Run only new/modified tests:**
   ```bash
   cargo test -p triton-cli -- test_name_pattern
   ```

3. **Verify no test races:**
   ```bash
   # Run tests multiple times in parallel
   cargo test -p triton-cli -- --test-threads=8
   cargo test -p triton-cli -- --test-threads=8
   cargo test -p triton-cli -- --test-threads=8
   ```

4. **Check for env var racing:**
   ```bash
   grep -rn 'set_var\|remove_var' cli/triton-cli/tests/ --include='*.rs'
   ```

5. **Audit assertion strength:**
   ```bash
   # Count tests with only .success() assertions
   grep -c '\.success();$' cli/triton-cli/tests/*.rs

   # Count tests with output content assertions
   grep -c 'stdout(predicate\|\.stdout\|assert.*contains\|assert_eq' cli/triton-cli/tests/*.rs
   ```

## References

- [Evaluation prompt](./triton-cli-evaluation-prompt.md) — Identifies gaps to test
- [Test verification prompt](./triton-cli-test-verification-prompt.md) — Audits test quality
- [Acceptable differences](../reference/acceptable-output-differences.md) — Don't test for these
- [Type Safety Rules](../../../CLAUDE.md) — Patterns tests should enforce
- [assert_cmd crate](https://docs.rs/assert_cmd)
- [predicates crate](https://docs.rs/predicates)
- [test-case crate](https://docs.rs/test-case)
- [Node.js triton tests](../../target/node-triton/test/) — Reference for behavioral expectations
