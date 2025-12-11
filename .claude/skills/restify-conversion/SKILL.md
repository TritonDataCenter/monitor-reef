---
name: restify-conversion
description: Orchestrate the conversion of Node.js Restify API services to Rust Dropshot API traits. Use this skill when migrating Node.js services to Rust. This orchestrator spawns separate sub-agents for each phase to manage context.
---

# Restify to Dropshot Conversion Orchestrator

This skill orchestrates the multi-phase conversion of Node.js Restify APIs to Rust Dropshot traits by spawning separate sub-agents for each phase.

## Why Separate Sub-agents?

Each phase can be context-intensive (reading many source files, generating code). Running all phases in a single context risks running out of context mid-conversion. By spawning separate sub-agents:

1. Each phase gets fresh context
2. Checkpoints allow resumption if interrupted
3. Summaries pass essential information between phases
4. User can review between phases if desired

## Orchestration Flow

When asked to convert a Restify service, execute this flow:

### Step 1: Spawn Phase 1 (Analyze)

```
Use the Task tool with subagent_type="general-purpose" to run Phase 1:

Prompt: "Execute the restify conversion Phase 1: Analyze.
Source path: <user-provided-path>
Read .claude/skills/restify-conversion/phase1-analyze.md for instructions.
Output a summary to .claude/restify-conversion/<service>/plan.md"
```

Wait for completion. Read the generated plan.md to get:
- Service name
- Version
- Endpoint count
- Route conflicts and resolutions
- File structure plan

### Step 2: Spawn Phase 2 (Generate API)

```
Use the Task tool with subagent_type="general-purpose" to run Phase 2:

Prompt: "Execute the restify conversion Phase 2: Generate API Trait.
Service name: <service>
Read the plan at .claude/restify-conversion/<service>/plan.md
Read .claude/skills/restify-conversion/phase2-api.md for instructions.
Read .claude/skills/restify-conversion/reference.md for mapping rules.
Update plan.md with Phase 2 results."
```

Wait for completion. Verify:
- `cargo build -p <service>-api` succeeds
- OpenAPI spec exists at `openapi-specs/generated/<service>-api.json`

### Step 3: Spawn Phase 3 (Generate Client)

```
Use the Task tool with subagent_type="general-purpose" to run Phase 3:

Prompt: "Execute the restify conversion Phase 3: Generate Client.
Service name: <service>
Read the plan at .claude/restify-conversion/<service>/plan.md
Read .claude/skills/restify-conversion/phase3-client.md for instructions.
Update plan.md with Phase 3 results."
```

Wait for completion. Verify:
- `cargo build -p <service>-client` succeeds

### Step 4: Spawn Phase 4 (Generate CLI)

```
Use the Task tool with subagent_type="general-purpose" to run Phase 4:

Prompt: "Execute the restify conversion Phase 4: Generate CLI.
Service name: <service>
Read the plan at .claude/restify-conversion/<service>/plan.md
Read .claude/skills/restify-conversion/phase4-cli.md for instructions.
Update plan.md with Phase 4 results."
```

Wait for completion. Verify:
- `cargo build -p <service>-cli` succeeds
- `cargo build --workspace` succeeds

### Step 5: Spawn Phase 5 (Validate)

```
Use the Task tool with subagent_type="general-purpose" to run Phase 5:

Prompt: "Execute the restify conversion Phase 5: Validate.
Service name: <service>
Source path: <original-source-path>
Read the plan at .claude/restify-conversion/<service>/plan.md
Read .claude/skills/restify-conversion/phase5-validate.md for instructions.
Create a validation report at .claude/restify-conversion/<service>/validation.md"
```

## Checkpoint Files

All state is persisted in `.claude/restify-conversion/<service>/`:

- `plan.md` - Conversion plan and phase completion status
- `validation.md` - Phase 5 validation report

This allows:
- Resuming from any phase if interrupted
- User review between phases
- Audit trail of the conversion

## Error Handling

If any phase fails:
1. The sub-agent should document the error in plan.md
2. The orchestrator reports the failure to the user
3. User can fix issues and resume from the failed phase

## Usage

When the user asks to convert a service:

```
Convert the Restify API at /path/to/sdc-vmapi to Dropshot
```

1. Confirm the source path exists
2. Derive service name from the path (e.g., "vmapi" from "sdc-vmapi")
3. Create `.claude/restify-conversion/<service>/` directory
4. Execute phases 1-5 in sequence, spawning sub-agents
5. Report final status and any validation findings
