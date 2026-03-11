<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

Copyright 2026 Edgecast Cloud LLC.
-->

# Triton CLI Comparison Framework

Compares Node.js `triton` and Rust `triton` CLI output side-by-side to track
compatibility as the Rust implementation matures.

## Quick Start

```bash
# Build the Rust CLI first
make build

# Run offline tests (no API access needed)
make triton-compare

# Run API tests (requires working profile)
make triton-compare-api PROFILE=demo

# Run everything
make triton-compare-all PROFILE=demo
```

## How It Works

The script runs the same commands against both CLIs, normalizes the output
(stripping version numbers, sorting JSON keys, collapsing whitespace), and
reports whether outputs match or differ.

### Test Tiers

- **Offline** — Commands that don't need API access: `--help`, `profile list`,
  `env`, `completion`. Uses an isolated temporary home directory with a
  deterministic test profile so both CLIs see identical configuration.

- **API** — Read-only commands that hit the live API: `instance list`,
  `image list`, `account get`, etc. Requires a `--profile` with working auth.

- **Payload** — Mutating operations (`instance create`, `start`, `stop`,
  `reboot`) compared by capturing the HTTP request payload each CLI *would*
  send, without actually sending it. Fully offline — uses dummy UUIDs so no
  name resolution or API contact is needed. Requires patching `node-triton`
  (see [Node-triton patch](#node-triton-patch) below).

### Output

```
RESULT   TEST ID                        DESCRIPTION
------   -------                        -----------
PASS     profile-list                   profile list
DIFF     profile-get                    profile get (bead TBD)
DIFF     env-bash                       env (bash output) (bead TBD)
SKIP     version                        --version output (intentional: different repo URLs)
PASS     profile-list-json              profile list -j (fixed? bead TBD)

=== Summary ===
  Pass: 2
  Diff: 2 (known: 2, new: 0)
  Skip: 1
  Fixed: 1 (known diffs now passing — close the bead!)
```

Results are annotated:
- **DIFF (bead XXX)** — known issue, tracked in beads
- **DIFF (NEW)** — untracked diff, needs triage
- **PASS (fixed? bead XXX)** — was a known diff, now passing — close the bead
- **SKIP (intentional: reason)** — intentional divergence, not a bug

For any `DIFF` result, a unified diff is saved to `$OUTPUT_DIR/diffs/<test-id>.diff`.

## Tracking Files

### `known-diffs.txt`

Maps test IDs to bead IDs for known bugs being worked on:

```
# test-id   bead-id  description
profile-get  TBD     profile get output format differs
env-bash     TBD     double vs single quotes
```

When a fix lands and the test flips to PASS, the script flags it so you know
to close the bead and remove the line.

### `ignored-diffs.txt`

Records intentional divergences that will never be "fixed":

```
# test-id  reason
help       clap vs node-triton help layout is by design
version    different repo URLs is intentional
```

These tests are skipped entirely. For differences that *can* be normalized
away (e.g., stripping version numbers), add normalization to the script
instead — no entry needed here.

## Workflow

### 1. Discovery — Run comparison, triage new diffs

```bash
make triton-compare
```

For each `DIFF (NEW)` result, decide:
- **Bug** → file a bead and add to `known-diffs.txt`
- **Intentional** → either add normalization to the script (so it becomes
  PASS) or add to `ignored-diffs.txt` (so it becomes SKIP)

### 2. File a bead

```bash
bd create \
    --title "triton profile get output format differs from Node.js" \
    --description "$(cat <<'EOF'
The Rust CLI outputs a different format for `profile get` than Node.js.

Comparison test ID: profile-get
Diff: see `make triton-compare --verbose`

Fix should go in: cli/triton-cli/src/commands/profile.rs
Rust test to add: cli/triton-cli/tests/cli_profiles.rs
EOF
)" \
    --labels compatibility
```

Then add to `known-diffs.txt`:
```
profile-get  monitor-reef-xxx  profile get output format differs
```

### 3. Fix the Rust code

1. `bd show <id>` → understand the issue
2. Fix the Rust CLI code
3. Re-run `make triton-compare` → confirm test flips to `PASS (fixed? bead XXX)`

### 4. Add a Rust integration test

Add a test in the appropriate existing test file (e.g., `cli/triton-cli/tests/cli_profiles.rs`)
that locks in the correct behavior. This ensures `cargo test` catches regressions
without needing Node.js triton installed.

### 5. Close out

1. Remove the line from `known-diffs.txt`
2. `bd close <id>`
3. Commit the code fix, test, and tracking file update together

## Adding Test Cases

Test cases are defined directly in `triton-compare.sh` inside
`run_offline_tests()` or `run_api_tests()`.

### Text output test

```bash
run_test "my-test-id" "description of what this tests" \
    isolated my-subcommand --flag
```

- `isolated` uses a temp home dir (for offline tests)
- `live` uses the real environment (for API tests, add `-p "$PROFILE"`)

### JSON output test

```bash
run_json_test "my-test-json" "description" \
    live -p "$PROFILE" my-subcommand list -j
```

JSON tests sort keys with `jq -S` and convert NDJSON to sorted arrays before
comparing.

## Options Reference

| Flag | Description | Default |
|------|-------------|---------|
| `--node-triton PATH` | Path to Node.js triton binary | `$(which triton)` |
| `--rust-triton PATH` | Path to Rust triton binary | `target/debug/triton` |
| `--tier TIER` | `offline`, `api`, `payload`, or `all` | `offline` |
| `--profile NAME` | Profile name for API tests | (required for api) |
| `--output-dir DIR` | Where to save diffs and raw output | `mktemp` |
| `--verbose` | Print each command before running | off |

## Node-triton Patch

The **payload** tier requires a small patch to `node-triton` so it can emit
request payloads without sending them. The patch is stored at
`patches/node-triton-emit-payload.patch`.

To apply it to your local `node-triton` checkout:

```bash
cd target/node-triton   # or wherever your node-triton lives
git apply /path/to/monitor-reef/cli/triton-cli/tests/comparison/patches/node-triton-emit-payload.patch
```

The patch adds a `TRITON_EMIT_PAYLOAD` env var check to
`CloudApi.prototype._request()` in `lib/cloudapi2.js`. When set, instead of
signing and sending the HTTP request, it prints a JSON envelope
(`{ method, path, body }`) to stdout and returns a synthetic 200 response.

The Rust CLI has the equivalent built in via the `--emit-payload` flag (also
settable via `TRITON_EMIT_PAYLOAD` env var).
