<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

Copyright 2025 Edgecast Cloud LLC.
-->

# Triton CLI Conversion Plans

This directory contains documentation for the triton-cli Rust implementation project.

## Current Status

| Metric | Status |
|--------|--------|
| Command Coverage | 100% (107/107 commands) |
| Option Compatibility | ~95% |
| Functionality (node-smartdc) | 100% |

**Active Work:** [plan-100-percent-compatibility.md](plans/active/plan-100-percent-compatibility.md)

## Directory Structure

```
conversion-plans/triton/
├── prompts/          # Evergreen evaluation templates
├── reports/          # Generated dated outputs
├── plans/
│   ├── active/       # Current implementation plans
│   └── completed/    # Historical plans (reference)
├── reference/        # Technical documentation
└── README.md         # This file
```

## Document Types

| Type | Purpose | Location |
|------|---------|----------|
| **Prompts** | Reusable templates for running evaluations | `prompts/` |
| **Reports** | Dated outputs from running prompts | `reports/` |
| **Plans** | Implementation tracking with checkboxes | `plans/active/` or `plans/completed/` |
| **Reference** | Technical docs (constraints, decisions) | `reference/` |

## Workflow

1. **Run a prompt** to generate a new report
   - Use prompts from `prompts/` as input to Claude
   - Save output to `reports/` with date suffix

2. **Create a plan** from report findings
   - Extract actionable items into `plans/active/`
   - Prioritize (P1/P2/P3) based on user impact

3. **Implement** plan items
   - Mark checkboxes as items are completed
   - Run tests, update docs

4. **Re-run prompt** to verify
   - Generate new report to confirm completion
   - Update compatibility percentages

5. **Archive** when complete
   - Move finished plans to `plans/completed/`

## Prompts

| File | Purpose |
|------|---------|
| [triton-cli-evaluation-prompt.md](prompts/triton-cli-evaluation-prompt.md) | Comprehensive CLI evaluation (command coverage, option compatibility, new features) |

## Reports

| File | Date | Summary |
|------|------|---------|
| [validation-report-2025-12-16.md](reports/validation-report-2025-12-16.md) | 2025-12-16 | Comprehensive validation showing 100% command coverage |
| [compatibility-report-2025-12-16.md](reports/compatibility-report-2025-12-16.md) | 2025-12-16 | Detailed option analysis with TODOs |
| [validation-report-initial.md](reports/validation-report-initial.md) | Earlier | Initial validation report |

## Active Plans

| File | Status | Focus |
|------|--------|-------|
| [plan-100-percent-compatibility.md](plans/active/plan-100-percent-compatibility.md) | Active | 95% → 100% option compatibility |

## Completed Plans

| File | Summary |
|------|---------|
| [plan-overview.md](plans/completed/plan-overview.md) | Original project plan |
| [phase0-auth.md](plans/completed/phase0-auth.md) | Authentication foundation |
| [phase1-cli-foundation.md](plans/completed/phase1-cli-foundation.md) | CLI structure |
| [phase2-instance-commands.md](plans/completed/phase2-instance-commands.md) | Instance management |
| [phase3-resources.md](plans/completed/phase3-resources.md) | Resource commands |
| [phase4-rbac-polish.md](plans/completed/phase4-rbac-polish.md) | RBAC and polish |
| [plan-high-priority-2025-12-16.md](plans/completed/plan-high-priority-2025-12-16.md) | P1 features |
| [plan-low-priority-2025-12-16.md](plans/completed/plan-low-priority-2025-12-16.md) | P2/P3 features |
| [plan-vnc-proxy-2025-12-16.md](plans/completed/plan-vnc-proxy-2025-12-16.md) | VNC proxy implementation |
| [affinity-support-plan.md](plans/completed/affinity-support-plan.md) | Affinity rules |
| [plan-compatibility-2025-12-17.md](plans/completed/plan-compatibility-2025-12-17.md) | Initial compatibility work |
| [plan-remaining-gaps-2025-12-16.md](plans/completed/plan-remaining-gaps-2025-12-16.md) | Gap analysis (RBAC apply completed) |

## Reference

| File | Purpose |
|------|---------|
| [cli-option-compatibility.md](reference/cli-option-compatibility.md) | Clap constraints and option conflict resolution |
