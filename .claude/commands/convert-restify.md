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

Follow those instructions to:

1. **Spawn Phase 1 sub-agent** - Analyze the source and create a plan
2. **Review plan with user** - Check for route conflicts needing approval
3. **Spawn Phase 2 sub-agent** - Generate the API trait crate
4. **Spawn Phase 3 sub-agent** - Generate the client library
5. **Spawn Phase 4 sub-agent** - Generate the CLI
6. **Spawn Phase 5 sub-agent** - Validate against original Node.js

Each sub-agent reads its phase instructions from `.claude/skills/restify-conversion/phaseN-*.md`.

## Checkpoint Files

State is persisted in `.claude/restify-conversion/<service>/`:
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
