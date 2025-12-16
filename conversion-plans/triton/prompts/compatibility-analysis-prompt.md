# Task: Triton CLI Option/Argument Compatibility Analysis

**Objective:** Systematically compare the Rust `triton-cli` implementation against the Node.js `triton` CLI to identify all differences in option and argument handling, and determine which gaps can be closed.

## Context

- **Rust CLI location:** `cli/triton-cli/`
- **Node.js CLI location:** `./target/node-triton/`
- **Known constraints:** Documented in `conversion-plans/triton/cli-option-compatibility.md`
  - Global `-v` (verbose) conflicts with `-v` for `--volume` in `instance create`
  - Global `-k` (key-id) conflicts with `-k` for `--key` in `rbac key-add`
  - Clap's global argument propagation prevents short option shadowing

## Analysis Tasks

### 1. Extract all commands and subcommands from both CLIs

- Node.js: Parse `lib/do_*.js` files and `lib/*/do_*.js` for command definitions
- Rust: Parse `cli/triton-cli/src/` for clap command definitions
- Create a comparison matrix of available commands

### 2. For each command, compare:

- Available options (short and long forms)
- Required vs optional arguments
- Positional argument handling
- Default values
- Argument types and validation

### 3. Identify categories of differences:

- **Unavoidable conflicts:** Short options that conflict with globals (already documented)
- **Missing short options:** Where Rust could add a short form but hasn't
- **Missing commands:** Commands in Node.js not yet implemented in Rust
- **Behavioral differences:** Same options but different behavior
- **Extra options:** Options in Rust not in Node.js

### 4. For each difference, assess:

- Can it be fixed? (Yes/No/Partial)
- What would it take to fix?
- Priority (High/Medium/Low based on user impact)

## Deliverables

1. **Command coverage report:** Table showing which Node.js commands exist in Rust
2. **Option compatibility matrix:** Per-command comparison of all options
3. **Actionable fixes list:** Specific changes that can improve compatibility
4. **Fundamental limitations:** Differences that cannot be resolved due to clap constraints

## Approach Hints

- Node.js uses `cmdln` library - look for `options` arrays in command files
- Check `triton --help` and `triton <command> --help` outputs from both versions
- The Node.js `triton` entry point is likely in `bin/triton` or `lib/cli.js`
- Focus on commonly-used commands first: `instance`, `image`, `network`, `volume`

## Output Format

Create or update a markdown document with:
- Executive summary of compatibility percentage
- Detailed per-command comparison tables
- Recommended changes with implementation notes
- Known limitations section
