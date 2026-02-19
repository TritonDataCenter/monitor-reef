---
name: cli-compat-fix
description: Work through compatibility differences between a Rust CLI and its Node.js predecessor. Analyzes comparison diffs, proposes fixes, implements them, and adds regression tests.
allowed-tools: Bash(make:*), Bash(git status:*), Bash(git diff:*), Bash(git log:*), Bash(git add:*), Bash(git commit:*), Bash(bd:*), Bash(cargo test:*), Read, Glob, Grep, Write, Edit, EnterPlanMode, ExitPlanMode, AskUserQuestion, Task
---

<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

Copyright 2026 Edgecast Cloud LLC.
-->

# CLI Compatibility Fix Skill

**Purpose:** Systematically fix compatibility differences between a Rust CLI and
its Node.js predecessor, guided by a comparison framework and tracked via Beads.

**Mode:** Plan-first. Analyze and propose a fix for user approval before writing code.

## Arguments

Parse `$ARGUMENTS` as:
```
<cli-name> [test-id | --triage]
```

- `cli-name` (required): e.g., `triton` — maps to `cli/triton-cli/`
- `test-id` (optional): e.g., `profile-get` — work on this specific diff
- `--triage` (optional): run comparison and help triage NEW diffs

If neither `test-id` nor `--triage` is given, pick the next unfixed entry
from `known-diffs.txt` (first entry that isn't in `ignored-diffs.txt`).

## Directory Convention

The comparison framework for a CLI named `<name>` lives at:

```
cli/<name>-cli/tests/comparison/
    <name>-compare.sh         # Comparison script (may also be triton-compare.sh etc.)
    known-diffs.txt            # test-id  bead-id  description
    ignored-diffs.txt          # test-id  reason
    README.md                  # Workflow docs
```

The CLI source lives at:
```
cli/<name>-cli/src/            # CLI source code
cli/<name>-cli/tests/          # Rust integration tests
```

**Discovery:** Glob for `cli/<name>-cli/tests/comparison/*-compare.sh` to find
the comparison script. If not found, tell the user no comparison framework
exists for that CLI.

## Phase 1: Select and Analyze

### 1a. Run the comparison

Use `--output-dir` under `./target/` so the results are readable without
extra permissions (target/ is gitignored, and mktemp avoids collisions):

```bash
mkdir -p ./target
OUTPUT_DIR=$(mktemp -d ./target/triton-compare.XXXXXX)
cli/<name>-cli/tests/comparison/<name>-compare.sh --output-dir "$OUTPUT_DIR"
```

The comparison script auto-cleans the directory on all-PASS. On DIFF, it
persists so you can read the diff files. Since it's under target/, leftover
dirs are harmless and get cleaned by `make clean`.

This produces a report with PASS/DIFF/SKIP/NEW annotations and saves
diffs and raw outputs to `$OUTPUT_DIR/`.

### 1b. Select a test ID

- If a test ID was given as an argument, use that
- If `--triage` was given, go to the Triage Mode section below
- Otherwise, read `known-diffs.txt` and pick the first entry

### 1c. Get the diff

Read the diff file from the comparison output directory:
```
$OUTPUT_DIR/diffs/<test-id>.diff
```

Also read the raw outputs for both CLIs:
```
$OUTPUT_DIR/node/<test-id>.out    # What Node.js produces
$OUTPUT_DIR/rust/<test-id>.out    # What Rust currently produces
```

### 1d. Trace to source code

Based on the test ID and the command being tested, identify the relevant
Rust source files. Use these patterns:

| Test ID pattern | Likely source file |
|---|---|
| `profile-*` | `cli/<name>-cli/src/commands/profile.rs` |
| `env-*` | `cli/<name>-cli/src/commands/env.rs` |
| `help-<subcmd>` | `cli/<name>-cli/src/commands/<subcmd>.rs` or `<subcmd>/mod.rs` |
| `completion-*` | `cli/<name>-cli/src/commands/completion.rs` |
| `<resource>-list` | `cli/<name>-cli/src/commands/<resource>.rs` |
| `<resource>-get` | `cli/<name>-cli/src/commands/<resource>.rs` |

Also check the API types if the diff involves JSON field differences:
```
apis/<name>api-api/src/types/
```

### 1e. Understand the difference

Read the Node.js output and the Rust output. Categorize:

- **Output format** — different labels, padding, field order (fix: change Rust formatting)
- **Missing fields** — Rust JSON missing fields Node has, or vice versa (fix: update struct/serialization)
- **Different values** — same field, different representation (fix: match Node's format)
- **Missing functionality** — Rust doesn't implement something Node does (fix: add feature)
- **Exit code** — different exit codes for same scenario (fix: match Node's exit code)
- **Error message** — different error text (fix: update error formatting)

## Phase 2: Propose a Fix

**Enter plan mode** and present your analysis:

1. **What the diff shows** — quote the key differences
2. **Root cause** — which source file(s) and what they currently do
3. **Proposed fix** — specific code changes needed
4. **Test plan** — what Rust integration test to add and what it asserts
5. **Which existing test file** the new test goes in (see Test Patterns below)

Wait for user approval before proceeding.

## Phase 3: Implement

After approval:

### 3a. Fix the Rust code

Make the minimum changes needed to resolve the diff. Follow existing patterns
in the codebase. Refer to CLAUDE.md for type safety rules, field naming
conventions, etc.

### 3b. Add a Rust integration test

Add a test in the appropriate existing test file. **Do not create new test
files** unless no suitable file exists.

#### Test Patterns

Existing tests use `assert_cmd` with environment isolation:

```rust
use assert_cmd::Command;
use serde_json::Value;

fn triton_cmd() -> Command {
    Command::cargo_bin("triton").expect("Failed to find triton binary")
}

#[test]
fn test_profile_get_format() {
    let output = triton_cmd()
        .args(["profile", "get", "env"])
        .env("TRITON_URL", "https://cloudapi.test.example.com")
        .env("TRITON_ACCOUNT", "test-account")
        .env("TRITON_KEY_ID", "00:11:22:33:44:55:66:77:88:99:aa:bb:cc:dd:ee:ff")
        .env("HOME", "/nonexistent")
        .output()
        .expect("Failed to run command");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Assert specific format expectations
    assert!(stdout.contains("name: env"));
}
```

Key patterns:
- Use `Command::cargo_bin("<cli-name>")` to find the binary
- Use `.env("HOME", "/nonexistent")` and `.env("TRITON_CONFIG_DIR", ...)` for isolation
- For JSON tests, parse with `serde_json::from_str::<Value>` and assert fields
- For table/text tests, assert specific substrings or line patterns
- Tests for offline commands don't need `#[ignore]`
- Tests for API commands use `#[ignore]` and read from `tests/config.json`

#### Test file mapping

| Command area | Test file |
|---|---|
| `profile` | `tests/cli_profiles.rs` |
| `env` | `tests/cli_env.rs` (create if needed) |
| `instance` | `tests/cli_instances.rs` |
| `image` | `tests/cli_images.rs` |
| `package` | `tests/cli_packages.rs` |
| `network` | `tests/cli_networks.rs` |
| `volume` | `tests/cli_volumes.rs` |
| `key` | `tests/cli_keys.rs` |
| `fwrule` | `tests/cli_fwrules.rs` |
| `vlan` | `tests/cli_vlans.rs` |
| `account` | `tests/cli_account.rs` |
| `completion` | `tests/cli_completion.rs` (create if needed) |

If a test file doesn't exist yet, check with the user before creating it.

### 3c. Verify

Run these in order:

```bash
# 1. Format and lint
make format && make lint

# 2. Run the specific test
cargo test -p <name>-cli --test <test_file> -- <test_function_name>

# 3. Re-run comparison to confirm the diff is gone
cli/<name>-cli/tests/comparison/<name>-compare.sh --tier offline
```

The test ID should now show PASS (possibly with "fixed? bead XXX" annotation).

### 3d. Update tracking

1. Remove the entry from `known-diffs.txt` (or update the bead ID if filing new)
2. If there's a bead, run `bd close <bead-id>`
3. Report the results to the user

**Do not commit** — leave that to the user (they may want to review first or
batch multiple fixes into one commit).

## Triage Mode

When `--triage` is specified:

1. Run the comparison
2. For each `DIFF (NEW)` result:
   a. Show the user the diff content
   b. Show which source file is likely responsible
   c. Ask the user: **Bug** (file a bead) or **Intentional** (add to ignored-diffs.txt)?
   d. If bug: file a bead with `bd create`, add to `known-diffs.txt`
   e. If intentional: ask whether to add normalization or add to `ignored-diffs.txt`

**The agent never decides triage outcomes.** Always present the diff and ask.

When filing a bead in triage mode, use this template:

```bash
bd create \
  --title "triton <command> output differs from Node.js" \
  --description "<Details including:
- Comparison test ID: <test-id>
- What Node.js produces vs what Rust produces
- Likely source file: cli/<name>-cli/src/commands/<file>.rs
- Rust test to add in: cli/<name>-cli/tests/<test_file>.rs
- Diff:
<paste key lines from the diff>
>" \
  --priority P3 \
  --type task \
  --labels compatibility
```

## Error Handling

- If the comparison script doesn't exist, tell the user and stop
- If `known-diffs.txt` is empty (no more known diffs), congratulate and suggest
  running with `--triage` to check for any remaining NEW diffs
- If the fix causes other tests to break, stop and ask the user
- If `make lint` fails, fix the lint issues before proceeding
- If the comparison still shows DIFF after the fix, investigate — maybe the
  normalization needs updating, or the fix was incomplete
