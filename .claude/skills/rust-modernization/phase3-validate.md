# Phase 3: Validate and Commit

## Objective

Verify the modernized crate works correctly and commit the changes.

## Steps

### 3.1 Format Code

```bash
make format
```

This ensures consistent formatting across the codebase.

### 3.2 Run Tests

```bash
make package-test PACKAGE=<crate-name>
```

All tests should pass. If tests fail:
- Check if test code needs the same modernization patterns
- quickcheck tests especially need `Arbitrary` trait updates
- Async tests need tokio runtime

### 3.3 Enable in arch-lint.toml

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

### 3.4 Enable in tarpaulin.toml

Edit `tarpaulin.toml` and remove the crate from exclude-files:

```toml
exclude-files = [
    # ...
    # Remove this line:
    # "libs/<your-crate>/*",
    # ...
]
```

### 3.5 Run Full Validation Suite (Required)

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

### 3.6 Commit

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

### 3.7 Verify Commit

```bash
git status
git log --oneline -1
```

Working tree should be clean.

## Validation Checklist

Before marking complete, verify:

- [ ] `make package-build PACKAGE=<name>` succeeds
- [ ] `make package-test PACKAGE=<name>` succeeds
- [ ] Crate removed from arch-lint.toml exclude list
- [ ] Crate removed from tarpaulin.toml exclude-files list
- [ ] Crate added to "Modernized" section in workspace Cargo.toml
- [ ] Crate removed from "To be modernized" comments
- [ ] `make format check coverage` passes
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

Either fix the issues or document why they're acceptable.

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
