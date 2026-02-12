<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

Copyright 2026 Edgecast Cloud LLC.
-->

# Type Safety Audit

Audit the codebase for type-safety issues in CLI and client code.

## Behavior

This is a **read-only audit** — it searches for issues but makes no code changes. Each finding is filed as a Beads issue via `bd create`.

**Read the audit instructions at:** `.claude/skills/type-safety-audit/SKILL.md`

Follow those instructions to:
1. Search for hardcoded enum string literals
2. Find enums missing `clap::ValueEnum` derives
3. Identify duplicate enum definitions
4. Check for missing `with_patch` calls in build.rs files
5. Find `format!("{:?}", ...)` anti-patterns on enums
6. Check for missing `#[serde(other)] Unknown` variants on state enums

Each finding becomes a Beads issue with category label `type-safety`.

## Output

- Summary of findings printed to console
- One `bd create` per finding with file locations and suggested fixes
- Run `bd ready` afterward to see the work queue
