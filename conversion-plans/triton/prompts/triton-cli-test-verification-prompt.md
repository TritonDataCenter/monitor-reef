<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

Copyright 2025 Edgecast Cloud LLC.
-->

# Triton CLI Test Quality Audit Prompt

## Objective

Audit the quality and effectiveness of the triton-cli test suite. The initial test porting and 1:1 verification against node-triton is complete (Dec 2025). This prompt focuses on whether existing tests are **actually effective at catching bugs**.

The key question for every test: "If the code had a bug, would this test catch it?"

This audit should produce a report of weak tests, trivially-passing tests, missing assertions, and concrete fixes.

## Context: Why This Matters

The Dec 2025 test verification confirmed ~747 tests across 26 integration files and 14 source files with inline tests. The initial verification mapped Rust tests to Node.js tests 1:1. However, post-verification analysis found ~40 bugs that tests failed to catch, including:

- `json_stream_parse` silently dropping parse failures (test infrastructure bug)
- Env var manipulation in tests racing with parallel execution
- Tests that asserted success without checking output content
- Integration tests that didn't verify their transform pipelines
- Filters silently ignored (`image list --type`) with no test catching it

These are **test quality** problems, not coverage problems.

## Source Locations

| Component | Location |
|-----------|----------|
| Integration tests | `cli/triton-cli/tests/cli_*.rs` (26 files, ~636 tests) |
| Test helpers | `cli/triton-cli/tests/common/mod.rs` |
| Test config | `cli/triton-cli/tests/common/config.rs` |
| Fixtures | `cli/triton-cli/tests/fixtures/` (15 files) |
| Inline unit tests | `cli/triton-cli/src/**/*.rs` (~104 tests) |
| Node.js reference tests | `target/node-triton/test/` |
| Acceptable differences | `conversion-plans/triton/reference/acceptable-output-differences.md` |

## Audit Methodology

### For Each Test File

1. **Read every test function**
2. **Classify each test's assertion strength** (see scale below)
3. **Check for anti-patterns** (see checklist below)
4. **Verify the test would fail if the code was wrong** (the core question)
5. **Propose concrete improvements** for weak tests

### Assertion Strength Scale

| Level | Description | Example |
|-------|-------------|---------|
| **0 - None** | No assertion at all | `let _ = triton_cmd().arg("--help");` |
| **1 - Runs** | Only checks exit code | `.assert().success()` |
| **2 - Contains** | Checks output contains a string | `.stdout(predicate::str::contains("ID"))` |
| **3 - Structured** | Parses output and checks fields | `let v: Value = from_str(&stdout); assert!(v["id"].is_string())` |
| **4 - Precise** | Validates exact values or patterns | `assert_eq!(machine.state, MachineState::Running)` |

Tests at level 0-1 are **weak**. Level 2 is **acceptable** for help text. Level 3-4 is **strong** for data output.

### Anti-Pattern Checklist

For each test, check for these problems:

- [ ] **Trivially passing**: Test would pass even if the command was completely broken
- [ ] **Self-verifying**: Test constructs expected output from the same code it tests
- [ ] **Success-only**: Asserts `.success()` without checking output
- [ ] **Dead assertion**: Assertion is commented out or in unreachable code
- [ ] **Wrong target**: Test name suggests it tests X, but actually tests Y
- [ ] **Env var racing**: Uses `std::env::set_var` which races with parallel tests
- [ ] **Error swallowing**: Uses `unwrap_or_default()` or `.ok()` in test helpers
- [ ] **Hardcoded strings**: Uses string literals instead of typed enums for comparison
- [ ] **Missing negative**: Only tests happy path, never error path
- [ ] **Fixture mismatch**: Fixture data doesn't match current API response format

## Audit Tasks

### Task 1: Test Helper Audit

The test helpers in `common/mod.rs` are foundational — bugs here affect all tests.

**Verify:**

1. `json_stream_parse()` — Does it report parse failures or silently drop them?
2. `run_triton()` / `run_triton_with_env()` — Do they capture both stdout and stderr?
3. `safe_triton()` — Does it check stderr is empty, or just exit code?
4. `make_resource_name()` — Is it unique enough to avoid collisions?
5. `create_test_instance()` / `delete_test_instance()` — Do they handle failures?
6. Environment variable handling — Are any set globally (racing with parallel tests)?

**Known issue to verify fix:** `json_stream_parse` was reported to silently drop parse failures. Check if this has been fixed. If not, flag it as P1.

### Task 2: Integration Test File Audit

For each test file, produce a quality assessment:

#### Template per file:

```markdown
### cli_<name>.rs

| Metric | Value |
|--------|-------|
| Total tests | X |
| Level 0-1 (weak) | X |
| Level 2 (acceptable) | X |
| Level 3-4 (strong) | X |
| Anti-patterns found | X |

**Weak tests:**
- `test_foo()` — Only asserts .success(), should check output
- `test_bar()` — Contains string but wrong string

**Anti-patterns:**
- Uses env::set_var on line X (races)
- Hardcoded "running" instead of MachineState::Running on line Y

**Improvements needed:**
- [ ] Upgrade `test_foo()` to parse JSON and check fields
- [ ] Remove env::set_var, pass via Command::env
```

**Priority order for file audit:**

| Priority | File | Why |
|----------|------|-----|
| 1 | `cli_output_format.rs` | Core output correctness |
| 2 | `cli_manage_workflow.rs` | Complex workflow, many steps |
| 3 | `cli_error_paths.rs` | Error handling correctness |
| 4 | `cli_subcommands.rs` | Large file (218 tests), likely many weak |
| 5 | `cli_images.rs` | Known past bugs with type filter |
| 6 | `cli_fwrules.rs` | Workflow test quality |
| 7 | `cli_vlans.rs` | Workflow test quality |
| 8 | `cli_profiles.rs` | Config handling |
| 9 | `cli_networks.rs` | Filter tests |
| 10 | `cli_basics.rs` | Foundation |
| 11-26 | Remaining files | Standard audit |

### Task 3: Inline Unit Test Audit

For unit tests in `cli/triton-cli/src/`, focus on:

#### High-value inline tests to audit:

| File | Tests | What to Check |
|------|-------|---------------|
| `commands/volume.rs` | 20 | Volume size parsing edge cases |
| `commands/instance/create.rs` | 19 | Metadata/user-data handling |
| `output/table.rs` | 18 | Table rendering correctness |
| `commands/instance/tag.rs` | 11 | Tag parsing edge cases |
| `commands/instance/nic.rs` | 8 | NIC config validation |
| `config/paths.rs` | 8 | Path resolution correctness |
| `output/mod.rs` | 7 | Output formatting utilities |

For each:
1. Does the test cover edge cases (empty input, special chars, overflow)?
2. Does the test verify error cases, not just happy paths?
3. Could the test pass with a completely wrong implementation?

### Task 4: Fixture Quality Audit

Review `cli/triton-cli/tests/fixtures/`:

| Check | Description |
|-------|-------------|
| **Realistic data** | Do fixtures have valid UUIDs, realistic timestamps, proper field values? |
| **Edge cases** | Are there fixtures for empty arrays, null fields, missing optional fields? |
| **Wire format accuracy** | Do JSON field names match actual CloudAPI responses? |
| **snake_case exceptions** | Do Machine fixtures include `dns_names`, `free_space`, `delegate_dataset`? |
| **Enum coverage** | Do fixtures include all known enum variants (states, brands, types)? |
| **Unknown variant handling** | Are there fixtures with unknown/future enum values? |
| **Completeness** | Are there fixtures for all resource types? (Check for missing: audit log, snapshots, migrations, RBAC objects) |

### Task 5: Coverage Gap Analysis

Cross-reference commands against test assertions (not just test existence):

```markdown
| Command | Help Test | Output Test | Error Test | Edge Case | Total Score |
|---------|:---------:|:-----------:|:----------:|:---------:|:-----------:|
| instance list | ✅ | ✅ | ❌ | ❌ | 2/4 |
| instance get | ✅ | ✅ | ✅ | ❌ | 3/4 |
| instance create | ✅ | ⚠️ | ❌ | ❌ | 1.5/4 |
| ... | | | | | |
```

Legend:
- ✅ = Strong assertion (level 3-4)
- ⚠️ = Weak assertion (level 1-2)
- ❌ = No test for this aspect

### Task 6: Mutation Testing Analysis

Without actually running a mutation testing tool, manually identify high-value mutations:

For each command implementation, identify code that could be wrong without any test catching it:

```markdown
### instance list (commands/instance/list.rs)

| Line | Code | Mutation | Would tests catch it? |
|------|------|----------|--------------------|
| 42 | `columns.push("SHORTID")` | Remove this line | ❌ No table column test |
| 67 | `if state_filter.is_some()` | Change to `is_none()` | ❌ No filter test |
| 89 | `serde_json::to_string(&instances)` | Return empty string | ⚠️ Only success check |
```

This identifies the highest-value tests to add.

## Output Format

### Audit Report

Create report at `conversion-plans/triton/reports/test-quality-audit-YYYY-MM-DD.md`:

```markdown
# Test Quality Audit Report

## Executive Summary

| Metric | Count | Percentage |
|--------|-------|-----------|
| Total tests audited | X | 100% |
| Strong (level 3-4) | X | X% |
| Acceptable (level 2) | X | X% |
| Weak (level 0-1) | X | X% |
| Anti-patterns found | X | - |
| Tests to fix | X | - |
| Tests to add | X | - |

## Test Helper Issues

[Findings from Task 1]

## Per-File Audit

[Findings from Task 2, one section per file]

## Inline Test Issues

[Findings from Task 3]

## Fixture Issues

[Findings from Task 4]

## Coverage Gap Matrix

[Table from Task 5]

## High-Value Mutations Not Caught

[Findings from Task 6]

## Prioritized Action Items

### P0 — Test infrastructure bugs
- [ ] Fix json_stream_parse error handling
- [ ] Fix env var racing in tests

### P1 — Tests that give false confidence
- [ ] Strengthen instance list output assertions
- [ ] Add filter verification tests
- [ ] ...

### P2 — Missing test categories
- [ ] Add error path tests for X
- [ ] Add edge case tests for Y
- [ ] ...

### P3 — Minor improvements
- [ ] Replace hardcoded strings with typed enums
- [ ] Add fixtures for missing resource types
- [ ] ...
```

### Issue Filing

For each P0/P1 finding, file a beads issue:

```bash
bd create --title "Short description" \
  --description "Detailed description with file:line references and suggested fix" \
  --labels testing
```

## Validation Steps

After completing the audit:

1. **Verify all existing tests still pass:**
   ```bash
   make package-test PACKAGE=triton-cli
   ```

2. **Count weak tests found:**
   ```bash
   # Tests with only .success() assertion
   grep -B2 '\.success();$' cli/triton-cli/tests/*.rs | grep '#\[test\]' | wc -l

   # Tests with no assertion at all
   # (Manual review required)
   ```

3. **Check for env var racing:**
   ```bash
   grep -rn 'std::env::set_var\|std::env::remove_var' cli/triton-cli/tests/ --include='*.rs'
   ```

4. **Check for silent error drops in helpers:**
   ```bash
   grep -rn 'unwrap_or_default\|\.ok()\.' cli/triton-cli/tests/common/ --include='*.rs'
   ```

5. **Verify fixture accuracy against API types:**
   ```bash
   # Check that fixture field names match struct definitions
   # Manual: compare fixture JSON keys against serde-annotated struct fields
   ```

## References

- [Evaluation prompt](./triton-cli-evaluation-prompt.md) — Identifies behavioral gaps
- [Test development prompt](./triton-cli-test-porting-prompt.md) — Writes new tests for gaps
- [Dec 2025 test verification](../reports/test-verification-report-2025-12-18.md) — Initial 1:1 mapping
- [Acceptable differences](../reference/acceptable-output-differences.md) — Don't flag these as test gaps
- [Type Safety Rules](../../../CLAUDE.md) — Patterns tests should enforce
