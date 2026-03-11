# Error Message Format Comparison: node-triton vs triton-cli (Rust)

**Date:** 2025-12-18
**Purpose:** Document error message format differences to inform compatibility decision

## Summary

The Node.js `triton` CLI and the Rust `triton-cli` produce differently formatted error messages. This document captures the observed formats to facilitate discussion about whether the Rust implementation should match Node.js for consistency.

## Error Format Patterns

### Node.js (node-triton v7.17.0)

Node.js triton uses a consistent format across all error types:

```
<command>: error (<ErrorType>): <message>
```

Examples:

```
triton: error (UnknownCommand): unknown command: "badcommand"
triton: error (Option): unknown option: "--badoption"
triton instance create: error (Usage): incorrect number of args
triton instance get: error (ResourceNotFound): no instance with name or short id "foo" was found
```

**Characteristics:**
- Prefixed with the command path that failed
- Error type in parentheses (UnknownCommand, Option, Usage, ResourceNotFound, etc.)
- Human-readable message follows
- All output goes to stderr

### Rust (triton-cli via Clap)

Rust/Clap uses its own format for CLI errors:

```
error: <message>

Usage: <usage>

For more information, try '--help'.
```

Examples:

```
error: unrecognized subcommand 'badcommand'

Usage: triton [OPTIONS] <COMMAND>

For more information, try '--help'.
```

```
error: the following required arguments were not provided:
  <INSTANCE>

Usage: triton instance get <INSTANCE>

For more information, try '--help'.
```

```
error: unexpected argument '--badoption' found

Usage: triton [OPTIONS] <COMMAND>

For more information, try '--help'.
```

**Characteristics:**
- Starts with `error:` (lowercase)
- More detailed: tells you *which* arguments are missing
- Includes usage hint inline
- Suggests `--help` for more info
- All output goes to stderr

## Side-by-Side Comparison

### Unknown Command

| CLI | Output |
|-----|--------|
| Node.js | `triton: error (UnknownCommand): unknown command: "badcommand"` |
| Rust | `error: unrecognized subcommand 'badcommand'` + usage hint |

### Unknown Option

| CLI | Output |
|-----|--------|
| Node.js | `triton: error (Option): unknown option: "--badoption"` |
| Rust | `error: unexpected argument '--badoption' found` + usage hint |

### Missing Required Arguments

| CLI | Output |
|-----|--------|
| Node.js | `triton instance create: error (Usage): incorrect number of args` |
| Rust | `error: the following required arguments were not provided:` + list of args + usage |

### API Error (Resource Not Found)

| CLI | Output |
|-----|--------|
| Node.js | `triton instance get: error (ResourceNotFound): no instance with name or short id "foo" was found` |
| Rust | Depends on how error is formatted in the CLI (currently uses anyhow/Display) |

## Analysis

### Advantages of Node.js Format

1. **Parseable error type** - The `(ErrorType)` is easy to extract programmatically
2. **Consistent structure** - Same format for all errors
3. **Command context** - Shows which subcommand failed

### Advantages of Rust/Clap Format

1. **More informative** - Lists exactly which arguments are missing
2. **Actionable** - Includes usage and --help suggestion
3. **Ecosystem consistency** - Matches other Rust CLI tools
4. **No custom code** - Uses Clap defaults

## Impact Analysis

### Scripts That Parse Error Messages

Scripts that parse error output (not recommended but sometimes done):

```bash
# This pattern would break
output=$(triton inst get "$INSTANCE" 2>&1)
if echo "$output" | grep -q "error (ResourceNotFound)"; then
    echo "Not found"
fi
```

### Interactive Users

Interactive users would see different messages but both are clear about what went wrong. The Rust format is arguably more helpful for CLI usage errors.

## Options for Resolution

### Option A: Match Node.js Format

Customize all error output to match the `command: error (Type): message` format.

**Pros:**
- Consistent with existing tooling
- Scripts parsing error messages still work

**Cons:**
- Significant custom code to override Clap's error formatting
- Loses Clap's helpful usage hints
- Ongoing maintenance burden
- Error message parsing is fragile anyway

### Option B: Keep Rust/Clap Format

Use Clap defaults for CLI errors, standard Rust error formatting for API errors.

**Pros:**
- More informative error messages
- Consistent with Rust ecosystem
- No custom code needed
- Easier to maintain

**Cons:**
- Different from node-triton
- Any scripts parsing error messages would break

### Option C: Hybrid - Match Format for API Errors Only

Keep Clap format for CLI errors, but format API errors to match Node.js style.

**Pros:**
- CLI errors get Clap's better formatting
- API errors maintain some consistency

**Cons:**
- Inconsistent within the same tool
- Partial custom code needed

## Recommendation

**Option B (Keep Rust/Clap Format)** is recommended because:

1. **Error message parsing is fragile** - Scripts should check exit codes, not parse stderr
2. **Clap's format is better** - More actionable for users
3. **Lower maintenance** - No custom error formatting code
4. **Ecosystem consistency** - Matches other modern CLI tools

If exit codes are made compatible (see `exit-code-comparison.md`), scripts can reliably detect error types without parsing messages.

## Test Commands to Reproduce

```bash
# Node.js triton
triton badcommand 2>&1
triton --badoption 2>&1
triton instance create 2>&1
triton instance get nonexistent 2>&1

# Rust triton-cli
./target/debug/triton badcommand 2>&1
./target/debug/triton --badoption 2>&1
./target/debug/triton instance create 2>&1
```

## Questions for Discussion

1. **Are there scripts that parse error messages rather than checking exit codes?**

2. **Is the error type (ResourceNotFound, Usage, etc.) used programmatically anywhere?**

3. **Do users have muscle memory or documentation that references the exact error format?**

## Related Documents

- [Exit Code Comparison](./exit-code-comparison.md) - Documents exit code differences
