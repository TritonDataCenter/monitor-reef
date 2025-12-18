<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

Copyright 2025 Edgecast Cloud LLC.
-->

# Triton CLI Test Verification Prompt

## Objective

Verify that the ported Rust tests for triton-cli actually test the same behaviors as the original node-triton tests. This is a **verification and gap analysis** task, not a porting task.

The goal is to ensure test coverage equivalence by systematically comparing the original Node.js tests against the Rust implementations.

## Goals and Non-Goals

### Goals

- **Verify behavioral equivalence** - Confirm Rust tests check the same behaviors as Node.js tests
- **Identify coverage gaps** - Find any test cases from Node.js that are missing in Rust
- **Identify over-testing** - Find any Rust tests that don't correspond to original tests
- **Document deviations** - Record any intentional differences and their justifications
- **Fix gaps** - When Rust tests are missing coverage, add the missing test cases
- **Fix Rust code** - When tests fail because Rust output differs from node-triton, modify the Rust CLI to match

### Non-Goals

- Re-porting tests that already exist
- Changing test structure without justification
- Adding new tests beyond what the original suite covered

## Source Locations

| Component | Location |
|-----------|----------|
| Original Node.js tests | `target/node-triton/test/` |
| Node.js unit tests | `target/node-triton/test/unit/*.test.js` |
| Node.js integration tests | `target/node-triton/test/integration/*.test.js` |
| Node.js test helpers | `target/node-triton/test/integration/helpers.js` |
| Ported Rust tests | `cli/triton-cli/tests/` |
| Rust test helpers | `cli/triton-cli/tests/common/mod.rs` |
| Test porting plan | `conversion-plans/triton/plans/active/plan-test-porting-*.md` |

## Verification Methodology

### Step 1: Extract Test Cases from Node.js

For each Node.js test file, extract:

1. **Test names** - The string passed to `t.test()` or `test()`
2. **Assertions** - What each test is checking (t.equal, t.ok, t.match, etc.)
3. **Setup/teardown** - Any preconditions or cleanup
4. **Command invocations** - The exact CLI commands being tested
5. **Expected outputs** - Strings, patterns, or JSON structures being validated

Example extraction from `cli-basics.test.js`:

```javascript
// Original Node.js test
t.test(' triton --version', function (t2) {
    triton(['--version'], function (err, stdout, stderr) {
        t2.error(err, 'no error');
        t2.match(stdout.trim(), /^\d+\.\d+\.\d+/);  // Checks version format
        t2.end();
    });
});
```

Document as:

| Test Name | Command | Expected Behavior |
|-----------|---------|-------------------|
| `triton --version` | `triton --version` | Outputs version matching `^\d+\.\d+\.\d+` |

### Step 2: Map to Rust Tests

For each extracted Node.js test case, find the corresponding Rust test:

```rust
// Rust equivalent
#[test]
fn test_version() {
    Command::cargo_bin("triton")
        .unwrap()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::is_match(r"^\d+\.\d+\.\d+").unwrap());
}
```

### Step 3: Verify Behavioral Equivalence

For each test pair, verify:

1. **Same command** - Is the CLI invocation identical?
2. **Same assertions** - Are the same properties being checked?
3. **Same error cases** - Are error conditions tested the same way?
4. **Same output format** - Is the expected output format (text, JSON, table) the same?

### Step 4: Document Findings

Create a verification matrix for each test file:

```markdown
## cli-basics.test.js → cli_basics.rs

| Node.js Test | Rust Test | Status | Notes |
|--------------|-----------|--------|-------|
| `triton --version` | `test_version` | ✓ Equivalent | |
| `triton --help` | `test_help_short` | ✓ Equivalent | |
| `triton badcommand` | `test_invalid_subcommand` | ⚠ Different | Rust shows different error message |
| `triton completion bash` | MISSING | ❌ Gap | Need to add completion test |
```

### Step 5: Fix Gaps

When gaps are found:

1. **Missing test** - Add the test to the Rust test file
2. **Different behavior** - Update the Rust CLI code to match node-triton behavior
3. **Different output format** - Modify Rust output formatting to match node-triton
4. **Missing alias** - Add the alias to the Rust CLI

**Important**: When tests fail because Rust output doesn't match node-triton:
- The Rust code should be modified to produce the expected output
- Do NOT change the test expectations to match wrong Rust output
- The node-triton behavior is the specification

## Verification Checklist

### Unit Tests

| File | Verified | Gaps Found | Gaps Fixed |
|------|----------|------------|------------|
| `metadataFromOpts.test.js` | [ ] | | |
| `tagsFromCreateOpts.test.js` | [ ] | | |
| `tagsFromSetArgs.test.js` | [ ] | | |
| `parseVolumeSize.test.js` | [ ] | | |

### Integration Tests - CLI

| File | Rust File | Verified | Gaps Found | Gaps Fixed |
|------|-----------|----------|------------|------------|
| `cli-basics.test.js` | `cli_basics.rs` | [ ] | | |
| `cli-subcommands.test.js` | `cli_subcommands.rs` | [ ] | | |
| `cli-profiles.test.js` | `cli_profiles.rs` | [ ] | | |
| `cli-account.test.js` | `cli_account.rs` | [ ] | | |
| `cli-keys.test.js` | `cli_keys.rs` | [ ] | | |
| `cli-networks.test.js` | `cli_networks.rs` | [ ] | | |
| `cli-nics.test.js` | `cli_nics.rs` | [ ] | | |
| `cli-vlans.test.js` | `cli_vlans.rs` | [ ] | | |
| `cli-ips.test.js` | `cli_ips.rs` | [ ] | | |
| `cli-fwrules.test.js` | `cli_fwrules.rs` | [ ] | | |
| `cli-images.test.js` | `cli_images.rs` | [ ] | | |
| `cli-image-create.test.js` | `cli_image_create.rs` | [ ] | | |
| `cli-manage-workflow.test.js` | `cli_manage_workflow.rs` | [ ] | | |
| `cli-instance-tag.test.js` | `cli_instance_tag.rs` | [ ] | | |
| `cli-snapshots.test.js` | `cli_snapshots.rs` | [ ] | | |
| `cli-volumes.test.js` | `cli_volumes.rs` | [ ] | | |
| `cli-volumes-size.test.js` | `cli_volumes.rs` | [ ] | | |
| `cli-deletion-protection.test.js` | `cli_deletion_protection.rs` | [ ] | | |
| `cli-migrations.test.js` | `cli_migrations.rs` | [ ] | | |

### Skipped Tests (Verify Justification)

| File | Reason | Valid? |
|------|--------|--------|
| `cli-affinity.test.js` | Requires multi-CN setup | [ ] |
| `cli-image-create-kvm.test.js` | Requires KVM support | [ ] |
| `cli-disks.test.js` | Requires flex disk support | [ ] |

## Detailed Verification Tasks

### Task 1: Extract All Test Cases

For each Node.js test file:

1. Read the file and identify all `t.test()` blocks
2. For each test, extract:
   - Test name/description
   - CLI command(s) executed
   - Assertions made (what is being checked)
   - Expected values/patterns
3. Create a structured list of test cases

### Task 2: Map Tests to Rust

For each extracted test case:

1. Find the corresponding Rust test function
2. If not found, mark as MISSING
3. If found, compare:
   - Command arguments
   - Assertion type (success, failure, output content)
   - Expected output format

### Task 3: Verify Output Format Compatibility

Many tests check output format. Verify:

1. **Table output** - Column headers, spacing, alignment
2. **JSON output** - Field names, structure, arrays vs objects
3. **Error messages** - Error text format
4. **Help text** - Command descriptions, usage strings

Example verification:

```bash
# Node.js behavior
$ node-triton networks -j
[{"id":"...","name":"..."}]

# Rust behavior (should match)
$ triton networks -j
[{"id":"...","name":"..."}]
```

### Task 4: Verify Error Handling

Check that error conditions produce equivalent results:

1. Missing required arguments
2. Invalid argument values
3. Resource not found
4. Permission denied
5. Network errors

### Task 5: Document and Fix Gaps

For each gap found:

1. Create a test case in the Rust test file
2. If the Rust CLI produces different output, modify the Rust code to match
3. Add the test to the verification matrix as fixed

## Output Format

### Verification Report

Create report at `conversion-plans/triton/reports/test-verification-YYYY-MM-DD.md`:

```markdown
# Triton CLI Test Verification Report

## Summary

| Metric | Count |
|--------|-------|
| Node.js Test Files | X |
| Rust Test Files | X |
| Test Cases Verified | X |
| Gaps Found | X |
| Gaps Fixed | X |
| Intentional Differences | X |

## Verification Details

### cli-basics.test.js → cli_basics.rs

#### Test Case Mapping

| # | Node.js Test | Rust Test | Status |
|---|--------------|-----------|--------|
| 1 | `triton --version` | `test_version` | ✓ |
| 2 | `triton --help` | `test_help_short` | ✓ |
| 3 | ... | ... | ... |

#### Gaps Found

1. **Missing: completion tests**
   - Node.js tests shell completion for bash, zsh, fish
   - Action: Added `test_completion_bash`, `test_completion_zsh`, `test_completion_fish`

#### Intentional Differences

1. **Error message format**
   - Node.js: "triton: error: unknown command 'foo'"
   - Rust: "error: unrecognized subcommand 'foo'"
   - Justification: Clap provides error messages, not worth customizing

[Continue for each test file...]

## Fixes Applied

### Code Changes

1. `cli/triton-cli/src/commands/foo.rs:123` - Changed output format to match node-triton
2. ...

### Test Changes

1. `cli/triton-cli/tests/cli_foo.rs` - Added missing test for X
2. ...
```

## Validation Commands

Run these commands to validate the verification:

```bash
# Run all offline tests
make triton-test

# Run API tests (requires config)
make triton-test-api

# Run specific test file
make triton-test-file TEST=cli_basics

# Compare output formats manually
node target/node-triton/bin/triton networks -j | head -5
cargo run -p triton-cli -- networks -j | head -5
```

## Common Issues to Watch For

### Output Format Differences

1. **JSON spacing** - Node.js may use different indentation
2. **Field ordering** - JSON field order may differ
3. **Null handling** - `null` vs omitted fields
4. **Date formats** - ISO vs Unix timestamps
5. **Table alignment** - Column widths and padding

### Behavioral Differences

1. **Exit codes** - Different codes for same errors
2. **Stderr vs stdout** - Where errors are written
3. **Aliases** - Missing command aliases
4. **Short flags** - `-j` vs `--json`

### Test Infrastructure Differences

1. **Setup/teardown** - Different approaches to test instance creation
2. **Assertions** - Different assertion libraries
3. **Test isolation** - Parallel vs sequential execution

## References

- [Test porting plan](conversion-plans/triton/plans/active/plan-test-porting-*.md)
- [Node.js test suite](target/node-triton/test/)
- [Rust test suite](cli/triton-cli/tests/)
- [Node.js triton source](target/node-triton/lib/)
- [Rust triton-cli source](cli/triton-cli/src/)
