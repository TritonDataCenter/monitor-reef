<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

Copyright 2026 Edgecast Cloud LLC.
-->

# Convert Node.js Restify API to Dropshot Trait

Convert a Node.js Restify API service into a Dropshot API trait definition for this Rust monorepo.

## Input

The user provides a path to a local checkout of a Restify-based service:
```
$ARGUMENTS
```

## Orchestration

This command orchestrates a multi-phase conversion by spawning separate sub-agents for each phase. This prevents running out of context on large APIs.

**Read the orchestrator instructions at:** `.claude/skills/restify-conversion/SKILL.md`

Follow those instructions to execute all 5 phases automatically:

1. **Phase 1** - Analyze the source and create a plan
2. **Phase 2** - Generate the API trait crate
3. **Phase 3** - Generate the client library
4. **Phase 4** - Generate the CLI
5. **Phase 5** - Validate against original Node.js

Each sub-agent reads its phase instructions from `.claude/skills/restify-conversion/phaseN-*.md`.

**IMPORTANT:** Execute all phases automatically without pausing between them (see "Autonomous Execution" in SKILL.md).

## Checkpoint Files

State is persisted in `conversion-plans/<service>/`:
- `plan.md` - Conversion plan and phase status
- `validation.md` - Final validation report

This allows resuming from any phase if interrupted.

## Reference Materials

- `.claude/skills/restify-conversion/reference.md` - Type mappings, patterns, examples
- `apis/bugview-api/` - Canonical API trait example
- `clients/internal/bugview-client/` - Canonical client example
- `cli/bugview-cli/` - Canonical CLI example

## Quick Start

To convert a service:

```
/convert-restify /path/to/sdc-vmapi
```

The orchestrator will:
1. Derive service name from path (e.g., "vmapi" from "sdc-vmapi")
2. Create checkpoint directory
3. Execute all 5 phases via sub-agents
4. Report final status and validation findings
