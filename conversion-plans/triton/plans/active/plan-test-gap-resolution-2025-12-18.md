# Plan: Test Gap Resolution

**Date:** 2025-12-18
**Status:** Active
**Reference:** [Test Verification Report](../../reports/test-verification-report-2025-12-18.md)

## Overview

The test verification report identified 5 minor gaps between the Node.js node-triton tests and the Rust triton-cli tests. This plan documents the resolution strategy for each gap.

## Gaps Analysis and Resolution

### Gap 1: Version Test Weakened

**Description:** The Rust version test only checks for "triton" in the output, while Node.js checks for:
- Semver pattern: `Triton CLI \d+\.\d+\.\d+`
- URL link to project

**Node.js Test:**
```javascript
test('triton --version', function (t) {
    h.triton('--version', function (err, stdout, stderr) {
        // ...
        t.ok(/^Triton CLI \d+\.\d+\.\d+/.test(stdout));
        t.ok(/https?:\/\//.test(stdout));
        // ...
    });
});
```

**Current Rust Test:**
```rust
#[test]
fn test_version() {
    triton_cmd()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains("triton"));
}
```

**Resolution:** ✅ **Fix - Strengthen the version test**

The version test should verify the actual version format. This is a simple fix.

**Action Items:**
- [x] Update `test_version()` in `cli_basics.rs` to check for semver pattern
- [ ] Optionally check for URL if the Rust CLI includes one (skipped - not included in Rust CLI)

---

### Gap 2: Error Message Format Differs

**Description:** Node.js uses custom error format `error (Usage):` while Rust uses Clap's standard format.

**Node.js:**
```
error (Usage): missing required arguments
```

**Rust (Clap):**
```
error: the following required arguments were not provided
```

**Resolution:** ❌ **No Fix - Intentional Design Decision**

**Rationale:**
1. **Ecosystem Consistency:** Using Clap's standard error format is consistent with other Rust CLI tools, making the triton CLI feel familiar to Rust users.
2. **Maintenance Burden:** Customizing Clap's error output to match Node.js format would require additional code and ongoing maintenance.
3. **User Experience:** Clap's error messages are well-tested and provide good user guidance including suggestions for similar commands.
4. **Tests Already Adapted:** The Rust tests already check for the keyword "required" which works with both formats, demonstrating the tests are correctly adapted.

**No action required.**

---

### Gap 3: Exit Codes Differ

**Description:** Some error exit codes differ between Node.js and Rust implementations.

| Scenario | Node.js | Rust |
|----------|---------|------|
| Missing args | Non-zero | Non-zero |
| Resource not found | 3 | 1 |
| InstanceDeleted (410) | 3 | 1 |

**Resolution:** ⚠️ **Partial Fix - Document and Consider**

**Rationale:**
1. **Unix Convention:** Exit code 1 for general errors is the Unix convention. Exit code 3 is non-standard.
2. **Scripting Impact:** Scripts relying on specific exit codes would need updates. However, checking for non-zero is more robust.
3. **API Compatibility:** The important behavior (success vs failure) is preserved.

**Action Items:**
- [ ] Document exit code behavior in CLI help or README (pending team discussion)
- [ ] Consider adding `--exit-code-compat` flag if backward compatibility is critical (pending team discussion)

**Note:** Exit code differences documented in `conversion-plans/triton/reference/exit-code-comparison.md` for team review.

---

### Gap 4: Test Resource Prefix Differs

**Description:** Test resources use different naming prefixes.

| Node.js | Rust |
|---------|------|
| `nodetritontest-*` | `tritontest-*` |

**Resolution:** ❌ **No Fix - Intentional Design Decision**

**Rationale:**
1. **Clarity:** The `tritontest-*` prefix clearly indicates these are test resources for the triton CLI, regardless of implementation language.
2. **Migration Friendly:** Using a different prefix means Rust tests won't conflict with Node.js tests if both are run against the same environment.
3. **Cleaner Naming:** Shorter prefix is cleaner and more readable.
4. **No User Impact:** Test resource names are internal and don't affect end users.

**No action required.**

---

### Gap 5: User-Script Option Not Explicitly Tested

**Description:** The Node.js manage workflow test includes `--script` option for user-scripts, but the Rust test doesn't explicitly verify this.

**Node.js Test:**
```javascript
var argv = [
    'create',
    '-wj',
    '-m', 'foo=bar',
    '--script', __dirname + '/script-log-boot.sh',
    '--tag', 'blah=bling',
    '-n', INST_ALIAS,
    imgId, pkgId
];
```

**Resolution:** ✅ **Fix - Add user-script test**

The `--script` option is an important feature for instance provisioning. It should be tested.

**Action Items:**
- [x] Create a simple test script file in `cli/triton-cli/tests/fixtures/` (already existed as `user-script.sh`)
- [x] Add `--script` option to the `test_instance_manage_workflow()` test
- [x] Verify the user-script metadata is set on the created instance

---

## Implementation Plan

### Phase 1: Version Test Fix (Priority: Low)

**File:** `cli/triton-cli/tests/cli_basics.rs`

```rust
/// Test `triton --version` output format
#[test]
fn test_version() {
    triton_cmd()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::is_match(r"triton \d+\.\d+\.\d+").unwrap());
}
```

**Estimated effort:** 5 minutes

### Phase 2: User-Script Test (Priority: Medium)

**Files:**
- Create: `cli/triton-cli/tests/fixtures/test-script.sh`
- Modify: `cli/triton-cli/tests/cli_manage_workflow.rs`

**Test Script (`test-script.sh`):**
```bash
#!/bin/bash
echo "Test script executed" > /var/log/test-script.log
```

**Test Modification:**
```rust
// In test_instance_manage_workflow()
let script_path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/test-script.sh");

let (stdout, stderr, success) = run_triton_with_profile([
    "create",
    "-wj",
    "-m", "foo=bar",
    "--script", script_path,
    "--tag", "blah=bling",
    "-n", &inst_alias,
    &img_id,
    &pkg_id,
]);

// Verify user-script metadata is set
if let Some(metadata) = &instance.metadata {
    assert!(
        metadata.get("user-script").is_some(),
        "user-script metadata should be set"
    );
}
```

**Estimated effort:** 15 minutes

### Phase 3: Exit Code Documentation (Priority: Low)

**File:** `cli/triton-cli/README.md` or help text

Add documentation noting:
- Exit code 0: Success
- Exit code 1: General error (missing args, API errors, resource not found)
- Exit code 2: Usage error (invalid command)

**Estimated effort:** 10 minutes

---

## Summary

| Gap | Resolution | Priority | Effort |
|-----|------------|----------|--------|
| Version test | Fix | Low | 5 min |
| Error format | No fix (by design) | N/A | N/A |
| Exit codes | Document | Low | 10 min |
| Resource prefix | No fix (by design) | N/A | N/A |
| User-script test | Fix | Medium | 15 min |

**Total estimated effort:** 30 minutes

---

## Acceptance Criteria

- [x] Version test checks for semver pattern
- [x] User-script option tested in workflow test
- [ ] Exit code behavior documented (pending team discussion, see reference/exit-code-comparison.md)
- [x] All tests pass: `make package-test PACKAGE=triton-cli`
