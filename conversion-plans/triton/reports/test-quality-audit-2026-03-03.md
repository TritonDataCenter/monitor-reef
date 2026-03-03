<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

Copyright 2025 Edgecast Cloud LLC.
-->

# Test Quality Audit Report

**Date**: 2026-03-03
**Scope**: `cli/triton-cli` — 24 integration test files (653 tests), 16 source files with inline tests (118 tests), 20 fixture files

## Executive Summary

| Metric | Count | Percentage |
|--------|-------|-----------|
| Total tests audited | 771 | 100% |
| Strong (level 3-4) | 268 | 35% |
| Acceptable (level 2) | 462 | 60% |
| Weak (level 0-1) | 41 | 5% |
| Anti-patterns found | 56 | — |
| Tests to fix (P0-P1) | 65 | — |
| Tests to add | ~40 | — |

**Key findings**:
1. No `std::env::set_var` racing — this was fixed or never present in integration tests.
2. One critical fixture bug: `volume_list.json` uses wrong wire format (`ownerUuid` instead of `owner_uuid`).
3. The most widespread anti-pattern is silent test skipping via early `return` in `#[ignore]` API tests (affects ~35 API tests across all files).
4. Inline unit tests have a critical gap: `tag.rs` tests validate test-only helper code, not the production `parse_tag_value` function.
5. `format_age()` (output/mod.rs) — used in every list table's AGE column — has zero test coverage.
6. Zero fixture coverage for `#[serde(other)] Unknown` enum variants despite this being a documented type safety rule.

## Test Helper Issues (Task 1)

### `common/mod.rs` and `common/config.rs`

| Helper | Verdict | Issues |
|--------|---------|--------|
| `json_stream_parse()` | Good | Minor: JSON array parse failure falls through to NDJSON path with confusing error |
| `run_triton()` | Good | Captures stdout + stderr. No issues. |
| `run_triton_with_env()` | Good | Per-process env vars. No racing. |
| `safe_triton()` | **Doc mismatch** | Doc says "asserts empty stderr" but code does NOT check stderr |
| `make_resource_name()` | **Collision risk** | Format: `{prefix}-{hostname}`. No randomness — concurrent runs on same host collide |
| `create_test_instance()` | **Silent failure** | Returns `Option<Machine>` — conflates "no config" with "API failure" with "empty response" |
| `delete_test_instance()` | **Silent failure** | Two paths: `let _` discards deletion result; JSON parse failure silently skips cleanup |
| `load_config()` | **Error swallowing** | `.ok()` on lines 160-161 silently swallows file read and JSON parse errors |

### Top Recommendations

1. **P0**: `load_config()` — Replace `.ok()?` with `eprintln!` + `None` so developers know their config is broken.
2. **P1**: `delete_test_instance()` — Don't discard deletion result. Leaked instances cost money.
3. **P1**: `make_resource_name()` — Add PID or random suffix: `format!("{}-{}-{}", prefix, hostname, std::process::id())`.
4. **P2**: `safe_triton()` — Update doc comment to match behavior (stderr is not checked).

## Per-File Integration Test Audit (Task 2)

### Summary Table

| File | Tests | L0-1 | L2 | L3-4 | Anti-Patterns | Quality |
|------|-------|------|-----|------|---------------|---------|
| `cli_env.rs` | 22 | 0 | 0 | 22 | 0 | Exemplary |
| `cli_instance_tag.rs` | 12 | 0 | 10 | 2 | 1 | Strong |
| `cli_deletion_protection.rs` | 10 | 0 | 8 | 2 | 1 | Strong |
| `cli_output_format.rs` | 28 | 0 | 5 | 23 | 2 | Strong |
| `cli_profiles.rs` | 18 | 1 | 4 | 13 | 2 | Good |
| `cli_snapshots.rs` | 12 | 0 | 10 | 2 | 1 | Good |
| `cli_vlans.rs` | 22 | 0 | 15 | 7 | 7 | Adequate |
| `cli_manage_workflow.rs` | 18 | 0 | 15 | 3 | 8 | Needs work |
| `cli_fwrules.rs` | 20 | 0 | 17 | 3 | 6 | Adequate |
| `cli_nics.rs` | 12 | 0 | 11 | 1 | 3 | Adequate |
| `cli_images.rs` | 17 | 0 | 13 | 4 | 6 | Needs work |
| `cli_networks.rs` | 16 | 0 | 11 | 5 | 5 | Adequate |
| `cli_packages.rs` | 14 | 0 | 9 | 5 | 4 | Adequate |
| `cli_ips.rs` | 13 | 0 | 8 | 5 | 3 | Adequate |
| `cli_volumes.rs` | 19 | 0 | 15 | 4 | 3 | Adequate |
| `cli_migrations.rs` | 23 | 0 | 18 | 5 | 5 | Needs work |
| `cli_keys.rs` | 15 | 0 | 12 | 3 | 2 | Adequate |
| `cli_account.rs` | 11 | 0 | 8 | 3 | 3 | Adequate |
| `cli_basics.rs` | 22 | 0 | 22 | 0 | 3 | Acceptable |
| `cli_subcommands.rs` | 218 | 0 | 164 | 54 | 2 | Good |
| `cli_error_paths.rs` | 37 | 15 | 22 | 0 | 5 | Needs work |
| `cli_image_create.rs` | 18 | 6 | 11 | 1 | 2 | Needs work |
| `cli_api_errors.rs` | 23 | 14 | 4 | 5 | 4 | Weakest |
| `cli_disks.rs` | 2 | 0 | 2 | 0 | 2 | Skeletal |

### Best Files (model for others)

- **`cli_env.rs`**: All 22 tests at Level 4. Zero anti-patterns. Validates exact output strings across bash/fish/powershell. Tests presence AND absence of expected strings.
- **`cli_instance_tag.rs`**: Workflow test is the most comprehensive in the audit — tests set/get/list/delete/replace-all, type coercion, file-based loading, and negative paths.
- **`cli_deletion_protection.rs`**: Tests both positive and negative paths with typed JSON parsing. Verifies deletion-protected instances cannot be deleted.

### Weakest Files (need attention)

- **`cli_api_errors.rs`**: 14/23 tests at Level 1 (exit-code only). Ironically, the error-handling test file does the least to verify error message content.
- **`cli_error_paths.rs`**: 15/37 tests at Level 1. The 11 "zero-args succeeds" tests don't verify empty stdout/stderr.
- **`cli_disks.rs`**: Only 2 tests. No API tests, no negative tests, no alias tests. Skeletal coverage.
- **`cli_image_create.rs`**: 6 `*_no_args` tests check `.failure()` only — no stderr validation.

### Cross-Cutting Anti-Patterns

#### 1. Silent Early Return in API Tests (~35 instances)

Every `#[ignore]` API test follows this pattern:
```rust
#[test]
#[ignore]
fn test_foo() {
    if !common::config::has_integration_config() { return; }  // silent pass
    // ... actual test
}
```

When un-ignored for API test runs, missing/broken config causes silent green passes. Should be `panic!("integration config required")` so failures are visible.

#### 2. Conditional Assertion Skipping (~15 instances)

```rust
if !items.is_empty() {
    assert!(items[0].id.contains('-'));  // never runs if list is empty
}
```

Empty API responses silently skip core assertions. Tests pass vacuously.

#### 3. Weak UUID Validation (~5 instances)

`first_id.contains('-')` used instead of `uuid::Uuid::parse_str()`. Matches any string with a hyphen.

#### 4. Overly Generic String Matching

`stderr.contains("required")` used in 22 error path tests without checking WHICH argument is required.

## Inline Unit Test Issues (Task 3)

| File | Tests | L4 | L3 | L2 | Critical Gap |
|------|-------|-----|-----|-----|-------------|
| `volume.rs` | 20 | 14 | 0 | 6 | `u64` overflow in `parse_volume_size`; `apply_positional_filters` untested |
| `create.rs` | 19 | 14 | 0 | 5 | `parse_size("0")` ambiguity; `parse_nic_spec` untested |
| `table.rs` | 18 | 5 | 8 | 5 | Empty table, right-alignment, column reordering untested |
| `tag.rs` | 11 | 9 | 0 | 2 | **Tests validate test-only helpers, not production `parse_tag_value`** |
| `nic.rs` | 8 | 8 | 0 | 0 | Non-contiguous netmask; `parse_nic_opts` untested |
| `paths.rs` | 8 | 0 | 2 | 6 | Unicode names; error message validation missing |
| `mod.rs` | 7 | 7 | 0 | 0 | **`format_age` (6+ branches) completely untested** |
| `image.rs` | 8 | 8 | 0 | 0 | Empty ACL edge case |
| `ssh.rs` | 6 | 6 | 0 | 0 | Multiple `@` signs; empty string |

### P0 Inline Test Issues

1. **`tag.rs` tests test-only code**: The test module defines its own `parse_tag` and `parse_tags_from_args` functions that do NOT perform the bool/number type coercion that the production `parse_tag_value` does. Production type coercion is completely untested.

2. **`format_age()` is untested**: Used in every table's AGE column. Has 6 branch conditions (years/weeks/days/hours/minutes/seconds) plus error and negative-duration paths. Zero tests.

3. **`parse_volume_size` overflow**: `gib * 1024` can overflow `u64` with valid-looking input like `"18014398509481984G"`. No guard or test.

## Fixture Issues (Task 4)

### Critical Bug

**`volume_list.json`** uses wrong wire format:
- Has `"ownerUuid"` — should be `"owner_uuid"` (struct has `#[serde(rename = "owner_uuid")]`)
- Has `"filesystemPath"` — should be `"filesystem_path"` (struct has `#[serde(rename = "filesystem_path")]`)
- The test `test_volume_fixture_camel_case_fields` validates the WRONG format — assertions are backwards.

Evidence: `cli/triton-cli/tests/comparison/fixtures/emit-payload-fakes.json` correctly uses `owner_uuid`.

### Missing snake_case Exception Fixtures

CLAUDE.md documents these as important exceptions. None have fixture coverage:
- `dns_names` — not in any machine fixture
- `free_space` — not in any machine fixture
- `delegate_dataset` — not in any machine fixture

### Enum Variant Coverage

| Enum | Variants Covered | Variants Missing |
|------|-----------------|-----------------|
| MachineState | `running`, `stopped` | `stopping`, `provisioning`, `failed`, `deleted`, `offline`, `ready` |
| MachineType | `smartmachine`, `virtualmachine` | `Unknown` |
| Brand | `lx`, `bhyve` | `joyent`, `joyent-minimal`, `kvm`, `builder` |
| ImageState | `active` | `unactivated`, `disabled`, `creating`, `failed` |
| ImageType | `zone-dataset`, `lx-dataset` | `zvol`, `docker`, `lxd`, `other` |
| VolumeState | `ready` | `creating`, `failed`, `deleting` |
| DiskState | `running` | `creating`, `resizing`, `failed`, `deleted` |
| NicState | `running` | `provisioning`, `stopped` |
| SnapshotState | `created` | `queued`, `creating`, `failed`, `deleted` |

### No Unknown Variant Fixtures

Zero fixtures test `#[serde(other)] Unknown` deserialization. Every enum in the type system has this variant for forward compatibility, but no test exercises it.

### Other Fixture Issues

- **Truncated SHA1**: `image_list.json` has `"sha1": "5dfeb0fba tried"` (not a valid SHA1)
- **CLI fields in API fixtures**: `instance_list.json` includes `age`, `img`, `shortid` (CLI display fields, not CloudAPI wire format)
- **No null optional field fixtures**: No Machine fixture with `memory: null`
- **No empty list fixtures**: No `[]` response for empty list handling
- **Missing resource types**: No fixtures for Account, User, AccessKey, FabricVlan, NetworkIp, Migration, AuditEntry, ErrorResponse

## Coverage Gap Matrix (Task 5)

| Command | Help Test | Output Test | Error Test | Edge Case | Score |
|---------|:---------:|:-----------:|:----------:|:---------:|:-----:|
| instance list | L2 | L2 | L2 | -- | 2/4 |
| instance get | L2 | L3 | L2 | -- | 2.5/4 |
| instance create | L2 | L4 | L2 | L3 (metadata) | 3/4 |
| instance delete | L2 | L4 | L2 | -- | 2.5/4 |
| instance start/stop/reboot | L2 | L4 | L1 | -- | 2/4 |
| instance resize | L2 | L4 | L2 | -- | 2.5/4 |
| instance rename | L2 | L4 | L2 | -- | 2.5/4 |
| instance ssh | L2 | -- | L2 | -- | 1/4 |
| instance snapshot * | L2 | L4 | -- | -- | 2/4 |
| instance disk * | L2 | -- | -- | -- | 0.5/4 |
| instance nic * | L2 | L4 | -- | -- | 2/4 |
| instance tag * | L2 | L4 | L2 | L4 (coercion) | 3.5/4 |
| instance migration * | L2 | L3 | L2 | -- | 2/4 |
| instance deletion-protection | L2 | L4 | L4 | L4 (idempotent) | 4/4 |
| image list | L2 | L3 | -- | -- | 1.5/4 |
| image get | L2 | L3 | -- | -- | 1.5/4 |
| image create | L2 | L4 | L1 | -- | 2/4 |
| image share/unshare | L2 | L4 | L1 | -- | 2/4 |
| network list | L2 | L3 | -- | -- | 1.5/4 |
| network get | L2 | L4 | -- | -- | 2/4 |
| network ip * | L2 | L4 | L2 | -- | 2.5/4 |
| package list | L2 | L3 | -- | -- | 1.5/4 |
| package get | L2 | L4 | -- | -- | 2/4 |
| volume list | L2 | L3 | -- | -- | 1.5/4 |
| volume create/delete | L2 | L4 | L4 | L4 (filters) | 3.5/4 |
| volume sizes | L2 | L2 | -- | -- | 1/4 |
| vlan list | L2 | L3 | L2 | L4 (filter) | 3/4 |
| vlan get | L2 | L4 | -- | -- | 2/4 |
| vlan create/delete | L2 | L4 | -- | -- | 2/4 |
| key list | L2 | L3 | -- | -- | 1.5/4 |
| key get | L2 | L4 | -- | -- | 2/4 |
| fwrule list | L2 | L3 | L2 | -- | 2/4 |
| fwrule CRUD | L2 | L4 | -- | L3 (enable/disable) | 2.5/4 |
| account get | L2 | L3 | -- | -- | 1.5/4 |
| account limits | L2 | L3 | -- | -- | 1.5/4 |
| profile get/list/set | L2 | L4 | L2 | L4 (precedence) | 3.5/4 |
| env | L2 | L4 | L4 | L4 (shells) | 4/4 |
| completion | L2 | L2 | -- | -- | 1/4 |

**Biggest gaps**: `instance disk *` (0.5/4), `completion` (1/4), `volume sizes` (1/4), `instance ssh` (1/4).

## High-Value Mutations Not Caught (Task 6)

These are code changes that could be made without any test catching them:

### Image List Filtering

| Location | Mutation | Caught? |
|----------|----------|---------|
| `commands/image.rs` `list` | Remove `--type` filter entirely | No test verifies filtering |
| `commands/image.rs` `list` | Remove `--state` filter entirely | No test verifies filtering |
| `commands/image.rs` `list` | Remove `--all` flag | No test uses this flag |

### Table Output

| Location | Mutation | Caught? |
|----------|----------|---------|
| `output/table.rs` | Remove a default column | No integration test checks specific columns |
| `output/table.rs` | Break `--no-header` | Only `cli_env.rs` tests verify header/no-header |
| `output/table.rs` | Break sort order | Only unit tests check sort |

### Volume Wire Format

| Location | Mutation | Caught? |
|----------|----------|---------|
| `types/volume.rs` | Remove `#[serde(rename = "owner_uuid")]` | Fixture uses WRONG format — would not catch |
| `types/volume.rs` | Remove `#[serde(rename = "filesystem_path")]` | Same — fixture uses wrong format |

### Error Formatting

| Location | Mutation | Caught? |
|----------|----------|---------|
| `errors.rs` | Remove `"triton: error:"` prefix | Only 2 tests check for this prefix |
| `errors.rs` | Double the error prefix | Only `test_no_double_error_prefix` catches this |
| `errors.rs` | Panic instead of clean error | Most error tests only check `.failure()`, not absence of panic |

### Format Age

| Location | Mutation | Caught? |
|----------|----------|---------|
| `output/mod.rs` `format_age` | Return empty string always | No unit tests at all |
| `output/mod.rs` `format_age` | Swap hours/minutes labels | No unit tests at all |

## Prioritized Action Items

### P0 — Test infrastructure bugs

- [ ] Fix `volume_list.json` wire format: `ownerUuid` -> `owner_uuid`, `filesystemPath` -> `filesystem_path`
- [ ] Fix `test_volume_fixture_camel_case_fields` assertions (currently backwards)
- [ ] Fix `tag.rs` inline tests: test production `parse_tag_value` instead of test-only helpers

### P1 — Tests that give false confidence

- [ ] Add unit tests for `format_age()` (6+ branches, used in every list table)
- [ ] Add image list filter verification tests (verify `--type` actually filters)
- [ ] Replace silent `return` in API tests with `panic!` when config is missing
- [ ] Strengthen `cli_api_errors.rs`: 14 Level-1 tests should check stderr content
- [ ] Strengthen `cli_error_paths.rs`: 11 "zero-args succeeds" tests should verify empty stdout/stderr
- [ ] Strengthen 6 `cli_image_create.rs` `*_no_args` tests: add stderr checks
- [ ] Replace weak UUID validation (`contains('-')`) with `uuid::Uuid::parse_str()` across 5 test files
- [ ] Make conditional `if let Some(...)` assertions unconditional in `cli_manage_workflow.rs` (metadata/tag checks)
- [ ] Add guard for `parse_volume_size` `u64` overflow
- [ ] `load_config()` — report errors instead of `.ok()` swallowing

### P2 — Missing test categories

- [ ] Add `Unknown` variant fixtures for forward-compatibility testing
- [ ] Add fixtures for `dns_names`, `free_space`, `delegate_dataset` snake_case exceptions
- [ ] Add negative API tests for invalid UUIDs across `image get`, `network get`, `package get`, `key get`
- [ ] Add `cli_disks.rs` tests (currently only 2 help tests — no API tests at all)
- [ ] Add unit tests for `parse_nic_opts` and `parse_nic_spec`
- [ ] Add unit tests for `apply_positional_filters` (volume.rs)
- [ ] Add integration test for image list with `--type` filter
- [ ] Add multi-item list fixtures for Network, Volume, Package, Disk, NIC, Snapshot, Key, FwRule
- [ ] Add `ErrorResponse` fixture for error handling tests

### P3 — Minor improvements

- [ ] Fix truncated SHA1 in `image_list.json`: `"5dfeb0fba tried"` is not valid
- [ ] Remove CLI display fields (`age`, `img`, `shortid`) from API response fixtures
- [ ] Update `safe_triton()` doc comment to match behavior (stderr not checked)
- [ ] Add randomness to `make_resource_name()` for concurrent test safety
- [ ] `delete_test_instance()` — don't discard deletion result with `let _`
- [ ] Make error path tests check specific argument names, not just `"required"`
- [ ] Add null optional field fixtures (`memory: null` for LX zones)
- [ ] Add empty list response fixture (`[]`)
- [ ] Parameterize help text tests in `cli_basics.rs` to reduce boilerplate
- [ ] Remove unused `mod common;` from `cli_subcommands.rs`
