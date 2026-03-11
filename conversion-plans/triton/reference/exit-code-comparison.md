# Exit Code Comparison: node-triton vs triton-cli (Rust)

**Date:** 2025-12-18
**Purpose:** Document exit code differences to inform compatibility decision

## Summary

The Node.js `triton` CLI and the Rust `triton-cli` use different exit code conventions. This document captures the observed behavior to facilitate discussion about whether the Rust implementation should match Node.js for backward compatibility.

## Observed Exit Codes

### Node.js (node-triton v7.17.0)

| Scenario | Exit Code | Error Format |
|----------|-----------|--------------|
| Unknown command | 1 | `triton: error (UnknownCommand): unknown command: "badcommand"` |
| Unknown option | 1 | `triton: error (Option): unknown option: "--badoption"` |
| Missing required args | 1 | `triton instance create: error (Usage): incorrect number of args` |
| Resource not found (404) | 3 | `triton instance get: error (ResourceNotFound): no instance with name or short id "foo" was found` |
| Instance deleted (410) | 3 | Similar format with ResourceNotFound or InstanceDeleted |

**Pattern:** Node.js uses exit code **1** for CLI/usage errors and exit code **3** for API errors.

### Rust (triton-cli via Clap)

| Scenario | Exit Code | Error Format |
|----------|-----------|--------------|
| Unknown command | 2 | `error: unrecognized subcommand 'badcommand'` |
| Unknown option | 2 | `error: unexpected argument '--badoption' found` |
| Missing required args | 2 | `error: the following required arguments were not provided:` |
| Resource not found (404) | 1 | Error message from API response |
| Instance deleted (410) | 1 | Error message from API response |

**Pattern:** Rust/Clap uses exit code **2** for CLI/usage errors (Clap default) and exit code **1** for all other errors (Rust's default for `Result::Err`).

## Side-by-Side Comparison

| Scenario | Node.js | Rust | Match? |
|----------|---------|------|--------|
| Unknown command | 1 | 2 | ❌ |
| Unknown option | 1 | 2 | ❌ |
| Missing required args | 1 | 2 | ❌ |
| API error (404) | 3 | 1 | ❌ |
| API error (410) | 3 | 1 | ❌ |
| Success | 0 | 0 | ✅ |

## Convention Context

### Unix/POSIX Conventions

- **0**: Success
- **1**: General errors
- **2**: Misuse of shell command (incorrect arguments, missing keywords, etc.)

### Clap (Rust CLI framework) Defaults

Clap follows the Unix convention of using exit code 2 for usage errors. This is intentional and consistent with tools like `grep`, `diff`, and many others.

### Node.js cmdln Library

The `cmdln` library used by node-triton uses exit code 1 for all errors by default. The exit code 3 for API errors appears to be a node-triton-specific convention.

## Impact Analysis

### Scripts That Would Break

Scripts checking for specific exit codes:

```bash
# This pattern would break
triton inst get "$INSTANCE"
if [ $? -eq 3 ]; then
    echo "Instance not found, creating..."
    triton create ...
fi
```

### Scripts That Would Work

Scripts checking for success/failure only:

```bash
# This pattern works with both implementations
if triton inst get "$INSTANCE"; then
    echo "Instance exists"
else
    echo "Error occurred"
fi
```

## Options for Resolution

### Option A: Full Node.js Compatibility

Override Clap defaults and match node-triton exactly:
- Exit 1 for CLI/usage errors
- Exit 3 for API errors (404, 410, etc.)

**Pros:**
- Drop-in replacement for existing scripts
- No user migration needed

**Cons:**
- Non-standard (exit 2 for usage errors is the Unix convention)
- Requires custom error handling to override Clap
- Ongoing maintenance burden

### Option B: Keep Rust/Clap Conventions

Use standard exit codes:
- Exit 2 for CLI/usage errors (Clap default)
- Exit 1 for API errors

**Pros:**
- Follows Unix conventions
- Consistent with other Rust CLI tools
- Simpler implementation (use defaults)

**Cons:**
- Scripts relying on exit code 3 would need updates
- Scripts relying on exit code 1 for usage errors would need updates

### Option C: Hybrid Approach

Keep Clap's exit 2 for usage errors, but use exit 3 for API errors:
- Exit 2 for CLI/usage errors
- Exit 3 for API errors (404, 410, etc.)

**Pros:**
- Preserves meaningful distinction between error types
- Scripts checking for API errors (exit 3) still work

**Cons:**
- Usage error scripts still break (1 → 2)
- Partially custom, partially standard

### Option D: Document and Move On

Keep Rust defaults, document the change:
- Exit 2 for CLI/usage errors
- Exit 1 for API errors

**Pros:**
- Simplest implementation
- Standard conventions

**Cons:**
- Breaking change for scripts

## Test Commands to Reproduce

```bash
# Node.js triton
which triton  # Should be node-triton

triton badcommand 2>&1; echo "Exit: $?"
triton --badoption 2>&1; echo "Exit: $?"
triton instance get 2>&1; echo "Exit: $?"
triton instance get nonexistent-instance-12345 2>&1; echo "Exit: $?"

# Rust triton-cli (assuming built to ./target/debug/triton)
./target/debug/triton badcommand 2>&1; echo "Exit: $?"
./target/debug/triton --badoption 2>&1; echo "Exit: $?"
./target/debug/triton instance get 2>&1; echo "Exit: $?"
# API test requires valid profile/auth
```

## Questions for Discussion

1. **Are there existing scripts that rely on exit code 3 for "not found" scenarios?**

2. **Is backward compatibility with node-triton a hard requirement or a nice-to-have?**

3. **If we match Node.js, should we also match the error message format (`error (Type): message`) or is exit code sufficient?**

4. **Is there a migration period where both CLIs will be available, allowing gradual script updates?**

## Recommendation

*[To be filled in after discussion]*
