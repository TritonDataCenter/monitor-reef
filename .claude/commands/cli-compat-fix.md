<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

Copyright 2026 Edgecast Cloud LLC.
-->

# CLI Compatibility Fix

Work through compatibility differences between a Rust CLI and its Node.js predecessor.

## Input

The user provides the CLI name and optionally a specific test ID:
```
$ARGUMENTS
```

Examples:
- `/cli-compat-fix triton` — pick the next unfixed diff from known-diffs.txt
- `/cli-compat-fix triton profile-get` — work on a specific test ID
- `/cli-compat-fix triton --triage` — run comparison and help triage NEW diffs

## Behavior

**Read the skill instructions at:** `.claude/skills/cli-compat-fix/SKILL.md`

Follow those instructions to:

1. **Discover** the comparison framework for the given CLI
2. **Select** a test ID to work on (from args or known-diffs.txt)
3. **Analyze** the diff and trace it to source code
4. **Propose** a fix plan (enter plan mode for user approval)
5. **Implement** the fix after approval
6. **Test** — add a Rust integration test and re-run comparison
7. **Verify** — `make format && make lint`, confirm test passes

## Triage Mode

When invoked with `--triage`, the skill runs the comparison, identifies any
`DIFF (NEW)` results, and for each one presents analysis to the user for
a decision: file a bead (bug) or add to ignored-diffs.txt (intentional).
The agent does NOT decide triage outcomes — the human always decides.

## Convention

The comparison framework lives at `cli/<cli-name>/tests/comparison/` with:
- `<cli-name>-compare.sh` — the comparison script
- `known-diffs.txt` — test IDs mapped to bead IDs
- `ignored-diffs.txt` — intentional divergences
- `README.md` — workflow documentation
