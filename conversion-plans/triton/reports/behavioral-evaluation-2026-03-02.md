<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

Copyright 2026 Edgecast Cloud LLC.
-->

# Triton CLI Behavioral Evaluation Report — 2026-03-02

## Summary

| Category | Issues Found | P0 | P1 | P2 | P3 |
|----------|-------------|----|----|----|-----|
| Output format fidelity | 3 | 0 | 1 | 1 | 1 |
| Behavioral correctness | 10 | 1 | 4 | 3 | 2 |
| Test quality | 4 | 0 | 0 | 2 | 2 |
| Error handling | 3 | 0 | 1 | 1 | 1 |
| Wire format | 0 | 0 | 0 | 0 | 0 |
| **Total** | **20** | **1** | **6** | **7** | **6** |

**All 830 offline tests pass (68 skipped — require API access).** Wire format types are fully compliant — zero issues found. Anti-pattern scan found only one genuine issue (Debug format in user-facing output).

---

## Findings by Category

### Part 1: Output Format Fidelity

#### [P1] [output] Image list flags differ from node-triton — `monitor-reef-p10d`

**File(s):** `cli/triton-cli/src/commands/image.rs:467-480`
**Comparison:** node-triton computes 5 flag types: `I` (has origin), `P` (public), `X` (non-active state), `+` (shared by you), `S` (shared with you — account in ACL). Rust computes only 3: `I` (origin), `S` (any ACL present), `P` (public). Missing `X` and `+` flags; `S` conflates two distinct meanings.
**Impact:** Users relying on flags to distinguish "shared by me" (`+`) vs "shared with me" (`S`) cannot do so. Non-active images are not flagged.
**Suggested fix:** Add `X` flag for non-active state. Differentiate `+` vs `S` by comparing image owner to current account UUID.
**Test needed:** Yes — add fixture-based test for flag computation with various ACL/owner combinations.

#### [P2] [output] Debug format `{:?}` in user-facing HTTP headers — `monitor-reef-nrd2`

**File(s):** `cli/triton-cli/src/commands/cloudapi.rs:159`
**Comparison:** `println!("{:?} {}", response.version(), response.status())` uses Debug format for HTTP version in `--show-headers` output. Shows `HTTP/1.1` as `HTTP11` or similar Debug representation.
**Impact:** Minor cosmetic issue in a diagnostic flag. Users see Rust-internal format.
**Suggested fix:** Use `Display` trait or manual formatting for HTTP version.
**Test needed:** No — diagnostic output.

#### [P3] [output] Instance create output less detailed than node-triton — `monitor-reef-rkx0`

**File(s):** `cli/triton-cli/src/commands/instance/create.rs:304`
**Comparison:** node-triton prints `"Creating instance NAME (UUID, IMAGE@VERSION, PACKAGE)"`. Rust prints `"Creating instance NAME (SHORT_UUID)"` — omits resolved image and package names.
**Impact:** Less informative feedback during instance creation. Low priority.
**Suggested fix:** Include resolved image name@version and package name in output.
**Test needed:** No — cosmetic improvement.

---

### Part 2: Behavioral Correctness

#### [P0] [behavior] Multi-target commands exit on first error — `monitor-reef-qc0q`

**File(s):** `cli/triton-cli/src/commands/instance/lifecycle.rs:56-77` (start), `:80-101` (stop), `:104-126` (reboot)
**Comparison:** All lifecycle commands iterate targets with `for instance in &args.instances` and use `?` to propagate errors immediately. If `instance start inst1 inst2 inst3` fails on inst1, inst2 and inst3 are never attempted. node-triton attempts ALL targets and collects errors.
**Impact:** **Critical**. Partial failure silently skips remaining instances. Users expect all-or-report-all behavior.
**Suggested fix:** Collect errors in a Vec, attempt all targets, report all failures at the end, exit non-zero if any failed.
**Test needed:** Yes — test with 3 instances where the first fails (mock server returning 404 for first, 202 for others).

#### [P1] [behavior] Reboot `--wait` can return a false positive — `monitor-reef-0l7c`

**File(s):** `cli/triton-cli/src/commands/instance/wait.rs:73-99`
**Comparison:** Reboot `--wait` calls `wait_for_state(Running)`, which polls `getMachine` until state is `running`. If the reboot completes very quickly (before first poll), the first poll sees `running` and returns immediately — correctly. However, if the instance is already `running` before the reboot API call is processed (race condition), `wait_for_state` returns immediately even though the reboot hasn't happened yet. node-triton uses `waitForMachineAudit` to check the audit trail for a successful reboot action, which is definitive.
**Impact:** In fast-reboot scenarios, `triton instance reboot --wait` may return before the reboot actually completes.
**Suggested fix:** For reboot specifically, record a timestamp before the action and verify via audit trail or by checking that the machine transitioned through a non-running state.
**Test needed:** Yes — difficult to unit test but should document the race condition.

#### [P1] [behavior] `triton env` missing `SDC_TESTING` export when `insecure=true` — `monitor-reef-1w47`

**File(s):** `cli/triton-cli/src/commands/env.rs:222` (bash), `:293` (fish), `:363` (powershell)
**Comparison:** node-triton conditionally exports `SDC_TESTING` based on `profile.insecure`. Rust unconditionally unsets `SDC_TESTING` in all three shell formats.
**Impact:** Profiles with `insecure: true` (self-signed certs) will break legacy `sdc-*` tools when using `eval $(triton env)`.
**Suggested fix:** Read profile insecure setting; conditionally emit `export SDC_TESTING=1` vs `unset SDC_TESTING`.
**Test needed:** Yes — test env output with insecure profile.

#### [P1] [behavior] `rbac apply` missing SSH key management — `monitor-reef-f2wt`

**File(s):** `cli/triton-cli/src/commands/rbac/apply.rs:58-67` (RbacConfigUser struct)
**Comparison:** node-triton's `rbac apply` handles per-user SSH key CRUD as part of the apply workflow (create-key, update-key, delete-key operations). Rust's `RbacConfigUser` has no `keys` field — key specifications in rbac.json are silently dropped by serde.
**Impact:** Users cannot manage SSH keys via `rbac apply`. Key rotation workflow is broken. Config files from node-triton lose key data silently.
**Suggested fix:** Add `keys` field to `RbacConfigUser`, implement key comparison and CRUD in the apply plan.
**Test needed:** Yes — test apply with user key configuration.

#### [P1] [behavior] `triton env --docker` silently empty when docker not set up — `monitor-reef-0s3k`

**File(s):** `cli/triton-cli/src/commands/env.rs:197-202`
**Comparison:** node-triton raises `ConfigError` when docker is explicitly requested (`-d`) but `setup.json` is missing, suggesting user run `triton profile docker-setup`. Rust silently outputs `# docker` with no variables and no error.
**Impact:** Users explicitly requesting docker env get no feedback about missing setup.
**Suggested fix:** When `--docker` is explicitly passed and setup.json is missing, return an error with setup instructions.
**Test needed:** Yes — test `triton env --docker` with no setup.json.

#### [P2] [behavior] UUID lookup not verified against server — `monitor-reef-61m4`

**File(s):** `cli/triton-cli/src/commands/instance/get.rs:60-63`
**Comparison:** Comment claims "matching node-triton's behavior" but node-triton actually calls `getMachine(uuid)` to verify existence and handles 404/410. Rust returns the parsed UUID without any API call.
**Impact:** Typos in UUIDs produce confusing delayed errors instead of clear "not found" errors at resolution time. The comment is misleading.
**Suggested fix:** For commands that resolve-then-act (lifecycle commands), the action itself will hit the server and fail, so this is medium priority. Fix the comment at minimum.
**Test needed:** No — existing error path tests cover the delayed error case.

#### [P2] [behavior] `rbac role-tags set/remove/clear` missing confirmation prompts — `monitor-reef-slvs`

**File(s):** `cli/triton-cli/src/commands/rbac/role_tags.rs:130` (set), `:272` (remove), `:294` (clear)
**Comparison:** node-triton prompts `"Set role tags on <type> \"<id>\"? [y/n]"` (skippable with `-y`). Rust executes immediately without confirmation.
**Impact:** Destructive role-tag operations (especially `clear` which removes all tags) have no safety net.
**Suggested fix:** Add `-y`/`--yes` flag; prompt for confirmation when not in non-interactive mode.
**Test needed:** No — interactive prompts are difficult to unit test.

#### [P2] [behavior] `triton env` drops null docker env values silently — `monitor-reef-n85d`

**File(s):** `cli/triton-cli/src/commands/env.rs:37`
**Comparison:** node-triton emits `unset KEY` for null docker env values. Rust's `filter_map` skips null values entirely — no `unset` command generated.
**Impact:** If docker setup.json has `"DOCKER_TLS_VERIFY": null`, the environment variable won't be cleaned up.
**Suggested fix:** Handle null values by emitting `unset` commands.
**Test needed:** Yes — test with fixture containing null docker env values.

#### [P3] [behavior] `triton info` instance state ordering is non-deterministic — `monitor-reef-omo1`

**File(s):** `cli/triton-cli/src/commands/info.rs:46-49`
**Comparison:** Uses `HashMap` for state counts, which has random iteration order. node-triton uses insertion order.
**Impact:** The instance state breakdown varies between runs. Minor cosmetic issue.
**Suggested fix:** Use `BTreeMap` for sorted, deterministic output.
**Test needed:** No — cosmetic.

#### [P3] [behavior] Short ID resolution doesn't normalize docker-style IDs — `monitor-reef-gdgm`

**File(s):** `cli/triton-cli/src/commands/instance/get.rs:68-72`
**Comparison:** node-triton's `normShortId` reformats docker-style container IDs (inserting dashes to form UUID prefix). Rust does simple `starts_with` on the raw string.
**Impact:** Docker users accustomed to `triton instance get abc123def456` format may get "not found" in Rust CLI. Very low usage.
**Suggested fix:** Low priority — document the difference.
**Test needed:** No.

---

### Part 3: Test Quality Audit

**636 tests across 24 test files. 99%+ have content assertions.** The test suite is high quality overall.

#### [P2] [testing] cli_subcommands.rs tests only check non-empty output — `monitor-reef-8gj3`

**File(s):** `cli/triton-cli/tests/cli_subcommands.rs`
**Impact:** 218 tests (~34% of all tests) only assert `.stdout(predicate::str::is_empty().not())` — verifies something was printed but not correctness. These pass even if help output is garbage.
**Suggested fix:** Add `predicate::str::contains("Usage:")` to help-output tests.
**Test needed:** N/A — this IS about tests.

#### [P2] [testing] Missing fixture files for several resource types — `monitor-reef-st21`

**File(s):** `cli/triton-cli/tests/fixtures/`
**Impact:** No JSON fixtures for: disks, NICs, snapshots, SSH keys, firewall rules. These resource types can only be tested with API access (integration tests).
**Suggested fix:** Add JSON fixture files for each missing resource type.
**Test needed:** N/A — this IS about tests.

#### [P3] [testing] cli_disks.rs has only 2 tests (help only) — `monitor-reef-ajba`

**File(s):** `cli/triton-cli/tests/cli_disks.rs`
**Impact:** Disk commands (list, get, resize, delete) have zero offline behavioral tests.
**Suggested fix:** Add fixture data and offline tests for disk operations.
**Test needed:** N/A.

#### [P3] [testing] No table header/column validation tests — `monitor-reef-3iqc`

**Impact:** Output format tests focus on JSON structure. No tests verify table column headers, column ordering, or `--long` column sets match node-triton's expected output.
**Suggested fix:** Add table output tests that verify column headers for key list commands.
**Test needed:** N/A.

---

### Part 4: Error Handling and Edge Cases

#### [P1] [error] `rbac info --all` silently swallows key fetch errors — `monitor-reef-y9a5`

**File(s):** `cli/triton-cli/src/commands/rbac/apply.rs:237-239`
**Comparison:** `if let Ok(keys) = keys_result` silently discards failures. A user who had keys but whose key fetch failed appears to have 0 keys — no warning.
**Impact:** Operators may make incorrect security decisions based on misleading "0 keys" display.
**Suggested fix:** Log a warning when key fetch fails, or include an error indicator in the output.
**Test needed:** Yes — mock server returning 500 for one user's keys.

#### [P2] [error] `rbac apply` dev-mode generates ed25519 keys (vs node-triton RSA-4096) — `monitor-reef-pcfq`

**File(s):** `cli/triton-cli/src/commands/rbac/apply.rs:1142-1143`
**Comparison:** node-triton generates RSA-4096 keys; Rust generates ed25519. Modern and better, but older SmartOS/CloudAPI may not support ed25519.
**Impact:** Dev-mode key generation may fail on legacy infrastructure.
**Suggested fix:** Low priority — ed25519 is reasonable modernization. Document the change.
**Test needed:** No.

#### [P3] [error] `unwrap_or_default()` usage is appropriate throughout

**File(s):** 17 instances across `commands/rbac/role_tags.rs`, `commands/accesskey.rs`, `commands/image.rs`, `commands/instance/nic.rs`
**Impact:** None — all uses are for genuinely optional fields (role tags, descriptions, expiration dates) where empty string is the correct default. **Not a bug.**

---

### Part 5: Wire Format Deep Dive

**Status: FULLY COMPLIANT — Zero issues found.**

All major structs verified:

| Struct | `rename_all` | Exception Fields | `#[serde(other)]` | Status |
|--------|:------------:|:----------------:|:------------------:|:------:|
| Machine | camelCase | dns_names, free_space, delegate_dataset, type, role-tag | N/A | ✅ |
| Volume | camelCase | type, owner_uuid, filesystem_path | N/A | ✅ |
| Image | camelCase | type, published_at, image_size, role-tag | N/A | ✅ |
| Package | camelCase | flexible_disk, role-tag | N/A | ✅ |
| MachineState | lowercase | — | ✅ Unknown | ✅ |
| Brand/MachineType | lowercase | — | ✅ Unknown | ✅ |
| ImageState | lowercase | — | ✅ Unknown | ✅ |
| ImageType | explicit renames | zone-dataset, lx-dataset | ✅ Unknown | ✅ |
| ImageAction | kebab-case | — | ✅ Unknown | ✅ |
| MachineAction | snake_case | — | ✅ Unknown | ✅ |
| VolumeType | lowercase | — | ✅ Unknown | ✅ |
| VolumeState | lowercase | — | ✅ Unknown | ✅ |
| DiskState | lowercase | — | ✅ Unknown | ✅ |
| MigrationState | lowercase | — | ✅ Unknown | ✅ |
| ChangefeedResource | lowercase | — | ✅ Unknown | ✅ |

All enums have proper forward-compatible `#[serde(other)]` catch-all variants.

---

## Anti-Pattern Scan Results

| Pattern | Found | In Production Code |
|---------|------:|:-----------------:|
| `{:?}` in user-facing output | 4 | 1 (cloudapi.rs:159) |
| `.unwrap()` in non-test code | 0 | 0 |
| `unwrap_or_default()` | 17 | All appropriate |
| TODO/FIXME/unimplemented/todo! | 0 | 0 |
| Hardcoded enum strings | 3 | 0 (all in tests) |

---

## Test Coverage Matrix

| Command Area | Offline Tests | Assertion Quality | Fixtures | Edge Cases | Error Paths | Score |
|-------------|:------------:|:-----------------:|:--------:|:----------:|:-----------:|:-----:|
| instance create | ✅ | ✅ | ✅ | ⚠️ | ✅ | 4/5 |
| instance list | ✅ | ✅ | ✅ | ✅ | ✅ | 5/5 |
| instance get | ✅ | ✅ | ✅ | ✅ | ✅ | 5/5 |
| instance lifecycle | ✅ | ✅ | N/A | ⚠️ | ⚠️ | 3/5 |
| instance delete | ✅ | ✅ | N/A | ⚠️ | ✅ | 4/5 |
| instance tag | ✅ | ✅ | ✅ | ✅ | ✅ | 5/5 |
| instance snapshot | ✅ | ✅ | ❌ | ⚠️ | ✅ | 3/5 |
| instance disk | ⚠️ | ⚠️ | ❌ | ❌ | ❌ | 1/5 |
| instance nic | ✅ | ✅ | ❌ | ⚠️ | ✅ | 3/5 |
| instance migration | ✅ | ✅ | N/A | ✅ | ✅ | 5/5 |
| deletion protection | ✅ | ✅ | N/A | ✅ | ✅ | 5/5 |
| image list/get | ✅ | ✅ | ✅ | ✅ | ✅ | 5/5 |
| image create | ✅ | ✅ | N/A | ✅ | ✅ | 5/5 |
| network | ✅ | ✅ | ✅ | ✅ | ✅ | 5/5 |
| vlan | ✅ | ✅ | N/A | ✅ | ✅ | 5/5 |
| volume | ✅ | ✅ | ✅ | ✅ | ✅ | 5/5 |
| package | ✅ | ✅ | ✅ | ✅ | ✅ | 5/5 |
| key | ✅ | ✅ | ❌ | ⚠️ | ✅ | 3/5 |
| fwrule | ✅ | ✅ | ❌ | ✅ | ✅ | 4/5 |
| account | ✅ | ✅ | N/A | ✅ | ✅ | 5/5 |
| profile | ✅ | ✅ | ✅ | ✅ | ✅ | 5/5 |
| env | ✅ | ✅ | N/A | ⚠️ | ⚠️ | 3/5 |
| output format | ✅ | ✅ | ✅ | ✅ | ✅ | 5/5 |
| error paths | ✅ | ✅ | N/A | ✅ | ✅ | 5/5 |
| api errors | ✅ | ✅ | N/A | ✅ | ✅ | 5/5 |

---

## Action Items

### P0 — Critical (Data loss, security, crashes)

- [x] `monitor-reef-qc0q` — **Multi-target early exit** — `cli/triton-cli/src/commands/instance/lifecycle.rs:56-77` — `?` operator causes exit on first error; remaining instances silently skipped

### P1 — Important (Wrong behavior visible to users)

- [x] `monitor-reef-0l7c` — **Reboot --wait race condition** — `cli/triton-cli/src/commands/instance/wait.rs:73-99` — polls for `running` state without confirming reboot actually happened
- [x] `monitor-reef-1w47` — **SDC_TESTING missing for insecure profiles** — `cli/triton-cli/src/commands/env.rs:222,293,363` — breaks legacy sdc-* tool TLS
- [x] `monitor-reef-f2wt` — **RBAC apply ignores SSH keys** — `cli/triton-cli/src/commands/rbac/apply.rs:58-67` — key config silently dropped
- [x] `monitor-reef-0s3k` — **Docker env error suppressed** — `cli/triton-cli/src/commands/env.rs:197-202` — empty output instead of helpful error
- [x] `monitor-reef-p10d` — **Image list flags missing X and +** — `cli/triton-cli/src/commands/image.rs:467-480` — S flag conflates two meanings
- [x] `monitor-reef-y9a5` — **RBAC key fetch errors swallowed** — `cli/triton-cli/src/commands/rbac/apply.rs:237-239` — silent 0-key display on API failure (pre-existing)

### P2 — Moderate (Cosmetic differences, weak tests)

- [x] `monitor-reef-nrd2` — **Debug format in show-headers** — `cli/triton-cli/src/commands/cloudapi.rs:159` — wontfix: `http::Version`'s Debug output is already user-friendly ("HTTP/1.1" etc.); added comment documenting the allowed exception
- [x] `monitor-reef-61m4` — **UUID resolve comment misleading** — `cli/triton-cli/src/commands/instance/get.rs:61` — fixed comment to not claim node-triton parity
- [ ] `monitor-reef-slvs` — **RBAC role-tags no confirmation** — `cli/triton-cli/src/commands/rbac/role_tags.rs:130,272,294` — destructive ops lack prompts
- [x] `monitor-reef-n85d` — **Docker env null handling** — `cli/triton-cli/src/commands/env.rs:37` — null values now emit unset/set -e/Remove-Item commands
- [x] `monitor-reef-8gj3` — **Subcommand tests only check non-empty** — `cli/triton-cli/tests/cli_subcommands.rs` — assertions now check for "Usage:"
- [ ] `monitor-reef-st21` — **Missing fixture files** — `cli/triton-cli/tests/fixtures/` — no disk/NIC/snapshot/key/fwrule fixtures
- [ ] `monitor-reef-pcfq` — **RBAC dev-mode ed25519 vs RSA** — `cli/triton-cli/src/commands/rbac/apply.rs:1142` — compatibility concern

### P3 — Low (Edge cases, minor improvements)

- [ ] `monitor-reef-rkx0` — **Instance create output detail** — `cli/triton-cli/src/commands/instance/create.rs:304` — missing image/package names
- [ ] `monitor-reef-omo1` — **Info state order random** — `cli/triton-cli/src/commands/info.rs:46` — HashMap → BTreeMap
- [ ] `monitor-reef-gdgm` — **Short ID no docker normalization** — `cli/triton-cli/src/commands/instance/get.rs:68-72` — edge case
- [ ] `monitor-reef-ajba` — **Disk tests incomplete** — `cli/triton-cli/tests/cli_disks.rs` — only 2 help tests
- [ ] `monitor-reef-3iqc` — **No table header tests** — no tests verify table column names/ordering
- [ ] `monitor-reef-316i` — **Short ID fetches all machines** — `cli/triton-cli/src/commands/instance/get.rs:73-79` — performance concern at scale

---

## Methodology

1. **Anti-pattern scanning**: Automated grep for `{:?}`, `.unwrap()`, `unwrap_or_default`, TODO/FIXME, hardcoded enum strings
2. **Wire format audit**: Manual review of all types in `apis/cloudapi-api/src/types/` checking serde attributes
3. **Test quality audit**: Categorized all 636 tests by assertion strength, fixture usage, and coverage
4. **Behavioral comparison**: Side-by-side source review of node-triton (`target/node-triton/lib/`) vs Rust commands
5. **Build & test validation**: `make package-build PACKAGE=triton-cli` and `make package-test PACKAGE=triton-cli` — all 830 tests pass

## References

- [Acceptable output differences](../reference/acceptable-output-differences.md)
- [Error format comparison](../reference/error-format-comparison.md)
- [Exit code comparison](../reference/exit-code-comparison.md)
- [Previous evaluation report](../reports/evaluation-report-2025-12-17.md) — Dec 2025 command coverage
- [Test verification report](../reports/test-verification-report-2025-12-18.md) — Dec 2025 test mapping
