# Phase 3: Validate and Commit

## Objective

Verify the modernized crate works correctly and commit the changes.

## Steps

### 3.1 Format Code

```bash
make format
```

This ensures consistent formatting across the codebase.

### 3.2 Run Tests (AND VERIFY THEY EXECUTE)

```bash
make package-test PACKAGE=<crate-name>
```

All tests should pass. If tests fail:
- Check if test code needs the same modernization patterns
- quickcheck tests especially need `Arbitrary` trait updates
- Async tests need tokio runtime

**CRITICAL: Verify tests actually ran:**
```bash
# Check test count - should be > 0 for crates with test files
make package-test PACKAGE=<crate-name> 2>&1 | grep "test result"
# Expected: "test result: ok. X passed; 0 failed"
# If X is 0 but test files exist, tests are orphaned!
```

If you see `0 passed` but test files exist:
1. Check if `src/tests/` files are declared in `lib.rs`
2. Move integration tests to `tests/` directory
3. Re-run and verify test count increases

### 3.3 Review Error Path Test Coverage

After modernization, verify tests exist for critical error paths:

**Check for coverage of:**
- Handler/callback error paths (errors returned to callers/clients)
- Connection/stream error handling (what happens when I/O fails)
- Malformed input rejection (invalid data should error, not panic)
- Async cancellation behavior (if applicable)

```bash
# Look for error-related tests
rg "Error|error|Err\(" libs/<crate>/tests/ --type rust
rg "#\[test\]" -A 10 libs/<crate>/ --type rust | grep -i error
```

**If critical error paths lack tests, consider adding basic coverage:**

```rust
#[test]
fn test_handler_error_returns_error_response() {
    // Test that when handler fails, proper error is returned
}

#[test]
fn test_malformed_input_rejected() {
    // Test that invalid input returns error, not panic
}
```

**Priority for test coverage:**
| Path | Priority | Reason |
|------|----------|--------|
| Handler errors → error responses | High | Clients depend on proper error format |
| Malformed protocol data | High | Security: shouldn't crash on bad input |
| Connection failures | Medium | Should fail gracefully |
| Edge cases (empty input, etc.) | Low | Nice to have |

### 3.4 Enable in arch-lint.toml

Edit `arch-lint.toml` and remove the crate from the exclude list:

```toml
[analyzer]
exclude = [
    # ...
    # Remove this line:
    # "libs/<your-crate>",
    # ...
]
```

### 3.5 Enable in tarpaulin.toml

Edit `tarpaulin.toml` and remove the crate from exclude-files:

```toml
exclude-files = [
    # ...
    # Remove this line:
    # "libs/<your-crate>/*",
    # ...
]
```

### 3.6 Run Full Validation Suite (Required)

Before committing, run the full validation suite:

```bash
make format check coverage
```

This runs:
- Code formatting (format)
- All workspace tests (check)
- OpenAPI spec validation (check)
- arch-lint (check)
- Code coverage with tarpaulin (coverage)

All checks must pass before proceeding to commit.

### 3.7 Final Error Handling Check

Before committing, verify no error context is being discarded:

```bash
# Check for patterns that discard error information
rg "map_err\(\|_\|" libs/<crate>/src/ --type rust
```

If any `map_err(|_|` patterns remain, fix them to preserve error context:

```rust
// Bad: discards original error
.map_err(|_| Error::other("parse failed"))

// Good: includes original error
.map_err(|e| Error::other(format!("parse failed: {}", e)))
```

### 3.8 Commit

Stage all changes:

```bash
git add \
    Cargo.toml \
    Cargo.lock \
    libs/<your-crate>/ \
    arch-lint.toml \
    tarpaulin.toml
```

Create commit with descriptive message:

```bash
git commit -m "$(cat <<'EOF'
Modernize <crate-name> crate to edition 2024

- Update to <dep> X.Y (from A.B)
- Update to <dep> X.Y (from A.B)
- <describe key API changes>
- Enable in arch-lint and tarpaulin

Co-Authored-By: Claude Opus 4.5 <noreply@anthropic.com>
EOF
)"
```

### 3.9 Verify Commit

```bash
git status
git log --oneline -1
```

Working tree should be clean.

## Validation Checklist

Before marking complete, verify:

- [ ] `make package-build PACKAGE=<name>` succeeds
- [ ] `make package-test PACKAGE=<name>` succeeds
- [ ] Error path test coverage reviewed (critical paths have tests)
- [ ] Crate removed from arch-lint.toml exclude list (must pass, no exclusions)
- [ ] Crate removed from tarpaulin.toml exclude-files list
- [ ] Crate added to "Modernized" section in workspace Cargo.toml
- [ ] Crate removed from "To be modernized" comments
- [ ] `make format check coverage` passes (includes arch-lint)
- [ ] No `map_err(|_|` patterns remain (error context preserved)
- [ ] Documentation updated to match code (versions, API names, typos fixed)
- [ ] Single atomic commit created

## Common Issues

### Tests fail with async runtime errors

If tests need a tokio runtime:
```rust
#[tokio::test]
async fn test_something() {
    // ...
}
```

### arch-lint fails

The crate may have patterns that arch-lint flags:
- `no-sync-io`: Sync I/O in async context
- `no-panic-in-lib`: Panics in library code

**These issues must be fixed.** Arch-lint exclusions are not allowed for modernized crates.

Common fixes:
- `panic!()` → Return `Result` type with `?` operator
- `unwrap()` → `?` or proper error handling
- Sync I/O in async code → Use `tokio::fs` or move to sync function

If fixing these requires API changes, that's acceptable during modernization. Callers will be updated when their crates are modernized.

### Coverage too low

If the crate has low test coverage, tarpaulin may fail.
Options:
1. Add tests
2. Keep crate in exclude-files temporarily
3. Adjust fail-under threshold (not recommended)

## Output

At the end of this phase:
- All validations pass
- Single commit created
- Ready for next crate or PR creation
