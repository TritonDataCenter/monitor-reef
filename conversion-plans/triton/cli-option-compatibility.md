# Triton CLI Option Compatibility Analysis

This document analyzes the differences between the Rust triton-cli and the original Node.js triton CLI regarding command-line option handling.

## Background

The original Node.js `triton` CLI uses a traditional argument parser that allows the same short option to be used at different levels of the command hierarchy. For example:

```bash
# Node.js triton allows:
triton -v instance create -v myvolume ...
#      ^^ verbose          ^^ volume
```

The Rust implementation uses [clap](https://docs.rs/clap), which has a different model for global vs. subcommand-specific arguments.

## Resolution: Top-Level Only Arguments

We resolved the short option conflicts by making global arguments **top-level only** (removing `global = true`). This means:

1. Top-level options (`-v`, `-j`, `-p`, `-a`, `-k`, `-U`) must come **before** the subcommand
2. Subcommands can freely use these short options for their own purposes
3. This matches the Node.js CLI behavior

```bash
# Correct usage:
triton -v instance list      # verbose mode
triton instance create -v myvol image pkg  # -v for volume

# No longer works:
triton instance list -v      # ERROR: -v not recognized
```

## Current Status: No Conflicts

| Short | Top-level Use | Subcommand Use | Status |
|-------|---------------|----------------|--------|
| `-v`  | `--verbose`   | `--volume` (create) | **Compatible** |
| `-k`  | `--key-id`    | `--key` (rbac key-add) | **Compatible** |
| `-a`  | `--account`   | `--affinity` (create) | **Compatible** |
| `-j`  | `--json`      | (none currently) | Available for subcommands |
| `-p`  | `--profile`   | (none currently) | Available for subcommands |

## Historical Context: Options Considered

When we initially implemented the CLI with `global = true` on top-level arguments, we faced conflicts. Here's what we considered:

### Option 1: Remove Global Short Options
Remove short options from global flags like `--verbose` and `--json`. Rejected because `-v` and `-j` are common conventions.

### Option 2: Different Short Options for Conflicts
Keep global options, remove conflicting short options from subcommands. This was our initial approach but reduced Node.js compatibility.

### Option 3: Top-Level Only Arguments (Chosen)
Remove `global = true` so top-level options don't propagate to subcommands. This allows subcommands to reuse short options like `-v`, `-k`, `-a`.

**Trade-off:** Options must come before the subcommand (`triton -v instance list` works, `triton instance list -v` doesn't). This matches Node.js CLI behavior.

## Current Compatibility Status

### Fully Compatible
All short options now work identically between Node.js and Rust CLIs:

| Command | Option | Status |
|---------|--------|--------|
| `triton instance create` | `-v` for volume | **Compatible** |
| `triton instance create` | `-a` for affinity | **Compatible** |
| `triton rbac key-add` | `-k` for key | **Compatible** |
| `triton instance list` | All options | **Compatible** |

### Aliases Adjusted

| Node.js | Rust | Reason |
|---------|------|--------|
| `triton rbac key-add` | `triton rbac key-add` or `add-key` | Clap doesn't allow alias = command name |
| `triton rbac key-delete` | `triton rbac key-delete` or `delete-key` | Same reason |

## Recommendations for Future Development

1. **Top-level options must come before subcommands** - This is now enforced. Document this in help text.

2. **Top-level short options (reserved for top-level only):**
   - `-v` → verbose (top-level)
   - `-j` → json (top-level)
   - `-h` → help (automatic)
   - `-V` → version (automatic)
   - `-p` → profile (top-level)
   - `-a` → account (top-level)
   - `-k` → key-id (top-level)
   - `-U` → url (top-level)

3. **Short options available for subcommands:**
   - `-v` → volume (in create)
   - `-a` → affinity (in create)
   - `-k` → key (in rbac key-add)
   - `-n` → name
   - `-f` → force
   - `-t` → tag, type, timeout
   - `-m` → memory, message, metadata
   - `-s` → size, state
   - `-w` → wait
   - `-y` → yes (skip confirmation)

4. **Document the ordering requirement** - Users must know that `-v` before subcommand means verbose, `-v` after means something else.

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
