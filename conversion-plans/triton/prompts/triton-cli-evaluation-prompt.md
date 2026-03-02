<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

Copyright 2025 Edgecast Cloud LLC.
-->

# Triton CLI Behavioral Evaluation Prompt

## Objective

Evaluate the Rust `triton-cli` for **behavioral correctness**, **test quality**, and **robustness** by comparing its actual behavior against node-triton. Command and option coverage is already at 100% (verified Dec 2025) — this evaluation focuses on whether the implemented commands **work correctly**.

This evaluation should produce a prioritized list of bugs, test gaps, and behavioral differences filed as beads issues.

## Context: What Previous Evaluations Found

The Dec 2025 evaluation confirmed 112/112 command coverage but subsequent deeper analysis found ~40 issues across these categories:

- **Security**: Shell injection in `triton env`, path traversal in profile names, insecure password generation, cleartext passphrase leaks via Debug
- **Type safety**: Missing `#[serde(rename)]` overrides causing silent data loss, Debug format `{:?}` in user-facing output, missing `#[serde(other)]` catch-all variants
- **Silent failures**: Swallowed API errors, masked config load errors, dropped malformed entries
- **Behavioral bugs**: Filters silently ignored, panics on edge input, incorrect error propagation

The lesson: "command exists" ≠ "command works correctly." This prompt targets the behavioral layer.

## Goals and Non-Goals

### Goals

- **Behavioral fidelity** — For each command, verify output matches node-triton's actual behavior
- **Test quality audit** — Verify tests actually assert meaningful properties, not just "runs without crashing"
- **Edge case coverage** — Test boundary conditions, empty inputs, invalid data, concurrent usage
- **Error path correctness** — Verify errors propagate properly, never silently swallowed
- **Wire format fidelity** — Verify JSON output field names, types, and structure match node-triton

### Non-Goals

- Re-checking command/option existence (already verified at 100%)
- Matching error message text exactly (Clap format is intentionally different — see `reference/error-format-comparison.md`)
- Matching exit codes exactly (documented difference — see `reference/exit-code-comparison.md`)
- Matching help text format (Clap style is intentionally different — see `reference/acceptable-output-differences.md`)

## Source Locations

| Component | Location |
|-----------|----------|
| Rust CLI source | `cli/triton-cli/src/` |
| Rust CLI tests | `cli/triton-cli/tests/` |
| Test helpers | `cli/triton-cli/tests/common/mod.rs` |
| Test fixtures | `cli/triton-cli/tests/fixtures/` |
| API type definitions | `apis/cloudapi-api/src/types/` |
| Node.js triton | `target/node-triton/` |
| Node.js triton lib | `target/node-triton/lib/` |
| Node.js triton tests | `target/node-triton/test/` |
| Acceptable differences | `conversion-plans/triton/reference/acceptable-output-differences.md` |
| Error format comparison | `conversion-plans/triton/reference/error-format-comparison.md` |
| Exit code comparison | `conversion-plans/triton/reference/exit-code-comparison.md` |
| Type safety rules | `CLAUDE.md` (Type Safety Rules section) |

## Known Acceptable Differences

Review `reference/acceptable-output-differences.md` before filing issues. These are **not bugs**:

1. Clap-style help format (vs node-triton's custom format)
2. Environment variable values shown in help output
3. JSON key ordering differences
4. Additional shortcuts (`nets`, `vlans`)
5. Terse vs verbose descriptions
6. No RBAC experimental warning
7. Empty `[]` for empty JSON results (vs empty output)
8. RBAC reset command (Rust-only addition)

## Evaluation Tasks

### Part 1: Output Format Fidelity

For each command category, compare actual JSON and table output between node-triton and triton-cli. Focus on **field names, field presence, value formats, and data types**.

**Method**: For commands that can be tested offline (using fixtures), build the CLI and test against fixture data. For commands requiring API access, compare the Rust source code's serialization against node-triton's `cloudapi2.js` response handling.

#### 1a. JSON Output Fields

For each `list` and `get` command, verify the JSON output includes the same fields with the same names as node-triton. Pay special attention to:

- **camelCase vs snake_case**: CloudAPI uses camelCase for most fields, but some are snake_case (see CLAUDE.md "Field Naming Exceptions")
- **Null handling**: Does Rust omit fields with `skip_serializing_if` where node-triton includes `null`?
- **Date formats**: Are timestamps in the same format?
- **Nested objects**: Are nested structures (metadata, tags, NICs) the same shape?

**Priority commands to check:**

| Command | Why It Matters |
|---------|---------------|
| `instance get` | Most complex response object (Machine struct) |
| `instance list` | Table columns and JSON fields must match |
| `image get` | Multiple type fields, requirements array |
| `network get` | Fabric vs non-fabric differences |
| `volume get` | Volume refs, filesystem_path |
| `account get` | Account fields used for auth |
| `fwrule get` | Rule string format |
| `instance audit` | Audit entry structure |

#### 1b. Table Output Columns

For each `list` command, verify the default table columns match node-triton. Check:

- Column names (headers)
- Column ordering
- Default vs `--long` column sets
- Value formatting (e.g., date display, ID truncation, size formatting)
- How `--short` mode works

#### 1c. Special Output Formats

- `triton env` — Verify shell variable export format for bash/fish/powershell
- `triton info` — Verify the summary format
- `triton datacenters` — Verify datacenter list format
- `triton services` — Verify services list format

### Part 2: Behavioral Correctness

For each command, verify it **does the right thing**, not just that it exists.

#### 2a. Filtering and Lookup

Many commands support looking up resources by name, short ID, or full UUID. Verify:

| Behavior | Commands to Check |
|----------|-------------------|
| Full UUID lookup | `instance get`, `image get`, `network get`, `volume get`, `fwrule get` |
| Short ID lookup (8-char prefix) | `instance get`, `image get` |
| Name lookup | `instance get`, `image get`, `network get`, `volume get` |
| `name@version` lookup | `image get` |
| Name filters on list commands | `instance list --name`, `image list --name` |
| State filters on list commands | `instance list --state`, `image list --state` |
| Type filters | `image list --type` (was previously silently ignored — verify fix) |

#### 2b. Action Dispatch Commands

These are critical because they use the action-dispatch pattern (single POST endpoint, different actions). Verify each action sends the correct request body:

| Command | Action | Key Fields to Verify |
|---------|--------|---------------------|
| `instance start` | `start` | action field in body |
| `instance stop` | `stop` | action field in body |
| `instance reboot` | `reboot` | action field in body |
| `instance resize` | `resize` | package field |
| `instance rename` | `rename` | name field |
| `instance enable-firewall` | `enable_firewall` | action field |
| `instance disable-firewall` | `disable_firewall` | action field |
| `instance enable-deletion-protection` | `enable_deletion_protection` | action field |
| `instance disable-deletion-protection` | `disable_deletion_protection` | action field |
| `image update` | `update` | fields being updated |
| `image export` | `export` | manta_path |
| `image clone` | `clone` | - |
| `image share` | `share` | account UUID |
| `image unshare` | `unshare` | account UUID |

#### 2c. Wait/Poll Behavior

Commands with `--wait` must poll correctly:

- Does `instance create --wait` actually wait for `running` state?
- Does `instance stop --wait` wait for `stopped`?
- Does `instance delete --wait` wait for deletion (handle 410 Gone)?
- Does `--wait-timeout` actually enforce the timeout?
- Does `volume create --wait` handle the async volume provisioning?

#### 2d. Multi-Target Commands

Several commands accept multiple targets. Verify:

- `instance start inst1 inst2 inst3` — starts all three
- `instance delete inst1 inst2` — deletes both
- Error handling when some targets succeed and others fail (partial failure)

#### 2e. RBAC Integration

RBAC has been a source of bugs. Verify:

- `rbac apply` reads and applies configuration correctly
- `rbac info` displays all users/roles/policies
- `rbac role-tags` set/add/remove work on actual resources
- Role headers are sent correctly on API calls when `--role` is used
- Sub-user key management (`rbac key`, `rbac keys`) works

### Part 3: Test Quality Audit

Existing tests (~747 total) need a quality review. For each test file, check:

#### 3a. Assertion Strength

Many tests may only assert `.success()` (exit code 0) without verifying output. For each test:

1. Does it assert on **output content**, not just success/failure?
2. For JSON output tests, does it parse the JSON and check field values?
3. For table output tests, does it check column headers and data rows?
4. For error tests, does it verify the specific error, not just "it failed"?

**Anti-patterns to flag:**

```rust
// WEAK: Only checks it doesn't crash
cmd.assert().success();

// BETTER: Checks output contains expected data
cmd.assert().success().stdout(predicate::str::contains("running"));

// BEST: Parses and validates structured output
let output: Vec<Instance> = serde_json::from_str(&stdout)?;
assert_eq!(output[0].state, "running");
```

#### 3b. Fixture Completeness

Review `cli/triton-cli/tests/fixtures/` against what tests need:

- Do fixtures cover all resource types?
- Do fixtures include edge cases (empty arrays, null fields, large data)?
- Are fixtures realistic (valid UUIDs, timestamps, field values)?
- Do any tests use hardcoded strings instead of fixtures?

#### 3c. Test Coverage Gaps

Cross-reference test files against command implementations:

| Test File | Commands Covered | Missing Coverage |
|-----------|-----------------|-----------------|
| `cli_manage_workflow.rs` | create, get, delete, start, stop, reboot, resize, rename | wait timeout, partial failure |
| `cli_snapshots.rs` | snapshot create/get/list/delete | snapshot boot |
| `cli_profiles.rs` | profile list/get | profile create/edit/delete (write ops) |
| `cli_images.rs` | image list/get | image create/update/export/share (write ops) |
| ... | ... | ... |

Fill in the complete matrix.

#### 3d. Tests That Test Themselves

Look for tests that may pass trivially:

- Tests where the expected value is derived from the same code being tested
- Tests that construct the expected output by calling the function under test
- Tests where assertions are commented out or use `#[allow(unused)]`
- Tests that only run setup but skip the actual assertion

### Part 4: Error Handling and Edge Cases

#### 4a. Error Propagation

For each command, trace the error path:

1. What happens when the API returns 404? 403? 500? 503?
2. Are errors displayed to the user or silently swallowed?
3. Does `anyhow` context get added at each layer?
4. Are there any `unwrap()` calls in non-test code that could panic?

**Known problem pattern**: `unwrap_or_default()` hiding errors. Search for:

```rust
// Potentially hides real errors
.unwrap_or_default()
.unwrap_or(Vec::new())
.ok().unwrap_or(...)
```

#### 4b. Input Validation

For commands that accept user input, verify:

- Empty string arguments
- Very long strings
- Special characters (quotes, newlines, shell metacharacters)
- Invalid UUIDs
- Non-existent resource names
- Numeric overflow (e.g., `--limit 999999999999`)

#### 4c. Concurrent Safety

For the test infrastructure:

- Do tests that manipulate environment variables (`TRITON_PROFILE`, etc.) race with parallel tests?
- Do tests that create temporary files clean up properly?
- Do workflow tests (create → use → delete) handle cleanup on failure?

### Part 5: Wire Format Deep Dive

Compare the actual structs in `apis/cloudapi-api/src/types/` against node-triton's `lib/cloudapi2.js` response handling:

#### 5a. Serde Configuration

For each major struct, verify:

- `#[serde(rename_all = "camelCase")]` is correct for the wire format
- Fields with non-standard naming have explicit `#[serde(rename = "...")]`
- Optional fields use `#[serde(default, skip_serializing_if = "Option::is_none")]`
- Enum variants with hyphens have explicit renames (e.g., `#[serde(rename = "joyent-minimal")]`)

**Known exception fields to verify** (from CLAUDE.md):

| Struct | Field | Expected Wire Name | Reason |
|--------|-------|-------------------|--------|
| Machine | dns_names | `"dns_names"` | snake_case exception |
| Machine | free_space | `"free_space"` | snake_case exception |
| Machine | delegate_dataset | `"delegate_dataset"` | snake_case exception |
| Machine | machine_type | `"type"` | Rust keyword |
| Volume | volume_type | `"type"` | Rust keyword |
| various | role_tag | `"role-tag"` | Hyphenated |

#### 5b. Enum Completeness

For enums that deserialize API responses, verify:

- All known variants are present (compare against node-triton's `VALID_*` constants)
- Forward-compatible enums have `#[serde(other)] Unknown` catch-all
- Variant wire names match the API (e.g., `"bhyve"` not `"Bhyve"`)

Priority enums:

| Enum | File | Check For |
|------|------|-----------|
| MachineState | `types/machine.rs` | All states from CloudAPI |
| Brand | `types/machine.rs` | `#[serde(other)]` variant |
| ImageState | `types/image.rs` | All states |
| ImageType | `types/image.rs` | Correct `#[serde(other)]` (not literal match) |
| MigrationType | `types/migration.rs` | All types |
| MigrationPhase | `types/migration.rs` | `#[serde(other)]` variant |

## Output Format

### Executive Summary

```markdown
## Summary

| Category | Issues Found | P0 | P1 | P2 | P3 |
|----------|-------------|----|----|----|----|
| Output format fidelity | X | | | | |
| Behavioral correctness | X | | | | |
| Test quality | X | | | | |
| Error handling | X | | | | |
| Wire format | X | | | | |
| **Total** | **X** | | | | |
```

### Findings by Category

For each finding:

```markdown
### [P1] [category] Short description

**File(s):** `path/to/file.rs:line`
**Comparison:** What node-triton does vs what triton-cli does
**Impact:** Who is affected and how
**Suggested fix:** Brief description of the fix
**Test needed:** Yes/No — description of test to add
```

### Prioritized Action List

```markdown
## Action Items

### P0 - Critical (Data loss, security, crashes)
- [ ] Item — file:line — brief description

### P1 - Important (Wrong behavior visible to users)
- [ ] Item — file:line — brief description

### P2 - Moderate (Cosmetic differences, weak tests)
- [ ] Item — file:line — brief description

### P3 - Low (Edge cases, minor improvements)
- [ ] Item — file:line — brief description
```

### Test Gaps Matrix

```markdown
## Test Coverage Matrix

| Command | Offline Tests | API Tests | Edge Cases | Error Paths | Score |
|---------|:------------:|:---------:|:----------:|:-----------:|:-----:|
| instance create | ✅ | ✅ | ⚠️ | ❌ | 3/4 |
| instance list | ✅ | ✅ | ✅ | ✅ | 4/4 |
| ... | | | | | |
```

## Methodology

### How to Compare Behavior

1. **Read the node-triton source** for each command (`target/node-triton/lib/do_*.js`)
2. **Read the Rust implementation** (`cli/triton-cli/src/commands/*.rs`)
3. **Compare the logic**, not just the interface — what API calls are made, how responses are processed, what output is produced
4. **Check the tests** — do they verify the behavior you just compared?

### How to Audit Tests

1. **Read each test function** in `cli/triton-cli/tests/`
2. For each assertion, ask: "Could this test pass even if the code is wrong?"
3. Check if the test has a corresponding node-triton test in `target/node-triton/test/`
4. If so, are they testing the same thing?

### How to File Issues

Use beads (`bd` CLI) to file issues found:

```bash
bd create --title "Short description" \
  --description "Detailed description with file:line references" \
  --labels <label>
```

Labels: `bug`, `type-safety`, `silent-failure`, `security`, `testing`, `compatibility`, `cli`

## Validation Steps

After analysis:

1. **Build the CLI:**
   ```bash
   make package-build PACKAGE=triton-cli
   ```

2. **Run existing tests (verify they pass):**
   ```bash
   make package-test PACKAGE=triton-cli
   ```

3. **Scan for common anti-patterns:**
   ```bash
   # Debug format in user-facing output
   grep -rn '{:?}' cli/triton-cli/src/ --include='*.rs' | grep -v '#\[derive' | grep -v '//.*{:?}' | grep -v test

   # unwrap() in non-test code
   grep -rn '\.unwrap()' cli/triton-cli/src/ --include='*.rs' | grep -v test | grep -v '#\[cfg(test)\]'

   # unwrap_or_default() hiding errors
   grep -rn 'unwrap_or_default\|unwrap_or(Vec' cli/triton-cli/src/ --include='*.rs'

   # TODO/FIXME markers
   grep -rn 'TODO\|FIXME\|unimplemented\|todo!' cli/triton-cli/src/ --include='*.rs'
   ```

4. **Check fixture coverage:**
   ```bash
   ls -la cli/triton-cli/tests/fixtures/
   ```

5. **Count test assertions vs test functions:**
   ```bash
   # Tests that might only assert success without checking output
   grep -A5 '#\[test\]' cli/triton-cli/tests/*.rs | grep -c 'assert().success()'
   grep -A10 '#\[test\]' cli/triton-cli/tests/*.rs | grep -c 'stdout(predicate'
   ```

## References

- [Acceptable output differences](../reference/acceptable-output-differences.md) — Known OK differences
- [Error format comparison](../reference/error-format-comparison.md) — Error message differences
- [Exit code comparison](../reference/exit-code-comparison.md) — Exit code differences
- [CLI option compatibility](../reference/cli-option-compatibility.md) — Short option handling
- [Previous evaluation report](../reports/evaluation-report-2025-12-17.md) — Dec 2025 command coverage
- [Test verification report](../reports/test-verification-report-2025-12-18.md) — Dec 2025 test mapping
- [Type Safety Rules](../../../CLAUDE.md) — CLAUDE.md type safety section
- [Node.js triton source](../../target/node-triton/)
- [Clap documentation](https://docs.rs/clap)
