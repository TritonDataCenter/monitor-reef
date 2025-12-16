# Triton CLI Option Compatibility Analysis

This document analyzes the differences between the Rust triton-cli and the original Node.js triton CLI regarding command-line option handling, particularly around short option conflicts.

## Background

The original Node.js `triton` CLI uses a traditional argument parser that allows the same short option to be used at different levels of the command hierarchy. For example:

```bash
# Node.js triton allows:
triton -v instance create -v myvolume ...
#      ^^ verbose          ^^ volume
```

The Rust implementation uses [clap](https://docs.rs/clap), which has a different model for global vs. subcommand-specific arguments.

## The Problem: Global Argument Propagation in Clap

In clap, when an argument is marked as `global = true`, it propagates to ALL subcommands. This means:

1. The argument can be specified anywhere on the command line
2. Short options must be unique across the entire command tree
3. There's no way to "shadow" or "override" a global short option in a subcommand

### Example Conflict

Our Rust CLI has:
- Global `-v` for `--verbose` (used everywhere)
- Global `-j` for `--json` (used everywhere)
- Global `-k` for `--key-id` (authentication)
- Global `-p` for `--profile`
- Global `-a` for `--account`

The Node.js CLI uses `-v` for `--volume` in `triton instance create`. This conflicts with our global `-v` for `--verbose`.

## Affected Options

| Short | Global Use (Rust) | Node.js Subcommand Use | Resolution |
|-------|-------------------|------------------------|------------|
| `-v`  | `--verbose`       | `--volume` (create)    | Removed `-v` from `--volume` |
| `-k`  | `--key-id`        | `--key` (rbac key-add) | Removed `-k` from `--key` |
| `-n`  | (available)       | `--name` (various)     | Can use `-n` for name |
| `-f`  | (available)       | `--force` (various)    | Can use `-f` for force |

## Options Considered

### Option 1: Remove Global Short Options (Rejected)

Remove short options from global flags like `--verbose` and `--json`.

**Pros:**
- Frees up `-v` and `-j` for subcommand use
- More compatible with Node.js CLI

**Cons:**
- `-v` for verbose and `-j` for JSON are very common conventions
- Users expect these to work globally
- Would be a usability regression

### Option 2: Don't Use Global Arguments (Rejected)

Define `-v`/`--verbose` separately in each subcommand instead of globally.

**Pros:**
- Each subcommand controls its own options
- Could match Node.js behavior exactly

**Cons:**
- Massive code duplication
- `triton -v instance list` wouldn't work (must be `triton instance list -v`)
- Inconsistent with modern CLI conventions

### Option 3: Different Short Options for Conflicts (Chosen)

Keep global options as-is, remove conflicting short options from subcommand-specific arguments.

**Pros:**
- Global options work intuitively (`triton -v` for verbose anywhere)
- No code duplication
- Clear, predictable behavior

**Cons:**
- Some subcommand options lose their short forms
- Not 100% compatible with Node.js CLI

### Option 4: Use `args_conflicts_with_subcommands` (Rejected)

This clap setting prevents mixing parent args with subcommands entirely.

**Pros:**
- Clear separation between levels

**Cons:**
- Can't do `triton -j instance list` (must be `triton instance list -j`)
- Major usability regression

## Current Compatibility Status

### Fully Compatible
Most options work identically between Node.js and Rust CLIs:
- `triton instance list`
- `triton instance get <id>`
- `triton -j instance list` (JSON output)
- `triton image list --name <pattern>`
- etc.

### Changed Short Options

| Command | Node.js | Rust | Notes |
|---------|---------|------|-------|
| `instance create` | `-v` for volume | `--volume` only | Use long form |
| `rbac key-add` | `-k` for key | `--key` only | Use long form |

### Aliases Adjusted

| Node.js | Rust | Reason |
|---------|------|--------|
| `triton rbac key-add` | `triton rbac key-add` or `add-key` | Clap doesn't allow alias = command name |
| `triton rbac key-delete` | `triton rbac key-delete` or `delete-key` | Same reason |

## Recommendations for Future Development

1. **Prefer long options for new subcommand-specific arguments** - Avoids potential conflicts with global options.

2. **Reserve common short options for global use:**
   - `-v` → verbose
   - `-j` → json
   - `-h` → help (automatic)
   - `-V` → version (automatic)
   - `-p` → profile
   - `-a` → account
   - `-k` → key-id
   - `-U` → url

3. **Safe short options for subcommands:**
   - `-n` → name
   - `-f` → force
   - `-t` → tag, type, timeout (context-dependent)
   - `-m` → memory, message
   - `-s` → size, state
   - `-w` → wait
   - `-y` → yes (skip confirmation)

4. **Document differences** - Maintain this document as new conflicts are discovered.

## Testing for Conflicts

The Rust CLI includes a test that validates the entire CLI structure at test time:

```rust
#[test]
fn verify_cli_structure() {
    Cli::command().debug_assert();
}
```

This test catches:
- Duplicate short options
- Duplicate long options
- Duplicate aliases
- Invalid argument configurations

Run with `make package-test PACKAGE=triton-cli` to verify.

## References

- [clap Arg documentation](https://docs.rs/clap/latest/clap/struct.Arg.html)
- [clap global argument issue #5690](https://github.com/clap-rs/clap/issues/5690)
- [clap conflicts_with on global options #5899](https://github.com/clap-rs/clap/issues/5899)
- [Rain's Rust CLI recommendations](https://rust-cli-recommendations.sunshowers.io/handling-arguments.html)
