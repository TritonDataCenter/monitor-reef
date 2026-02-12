<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

Copyright 2026 Edgecast Cloud LLC.
-->

# Type Safety Audit Skill

**Purpose:** Detect type-safety issues in CLI and client code and file them as Beads issues.

**Mode:** Read-only audit. No code changes. One `bd create` per finding.

## Audit Checks

Run each check below. For each finding, create a Beads issue (see Filing Issues section).

### Check 1: Hardcoded Enum String Literals

Search CLI code for string literals that match known enum variant wire names.

**How to find them:**
1. Read `apis/*/src/types/*.rs` to collect all enum variant wire names (accounting for `rename_all` and `rename` attributes)
2. Grep `cli/*/src/**/*.rs` for those string literals used in comparisons (`==`, `!=`, `contains`, `matches!`) or pushed into output rows

**Grep patterns:**
```
# Look for string comparisons that smell like enum variants
== "running"
== "stopped"
== "failed"
== "active"
== "destroyed"
```

**Why it matters:** String literals bypass the type system. If a variant is renamed or a new variant is added, the string comparison silently breaks.

### Check 2: Missing clap::ValueEnum

Find enums used as CLI `#[arg(value_enum)]` fields that lack the `ValueEnum` derive.

**How to find them:**
1. Grep `cli/*/src/**/*.rs` for `value_enum` in `#[arg(...)]` attributes
2. For each match, identify the type of the field
3. Check if that type has `clap::ValueEnum` derived — either:
   - On the API crate type in `apis/*/src/types/*.rs`
   - Via `with_patch` in the corresponding client's configuration in `client-generator/src/main.rs`

**A finding exists when:** The type has neither a `ValueEnum` derive on the source nor a `with_patch` in the client-generator config.

### Check 3: Missing client-generator Patches

Check that all enums used as CLI arguments have corresponding `with_patch` calls in the client-generator.

**How to find them:**
1. From Check 2, get the list of enum types used with `value_enum`
2. Read `client-generator/src/main.rs` to find each client's `configure_*` function
3. Compare: every enum used as a CLI `value_enum` arg should have a `with_patch` line (unless the API crate type itself has `ValueEnum`)

**A finding exists when:** An enum is used with `value_enum` in a CLI but has no `with_patch` in the client-generator config and no `ValueEnum` on the API type.

### Check 4: Duplicate Enum Definitions

Find enum definitions in CLI code that duplicate types from API crates or Progenitor.

**How to find them:**
1. Grep `cli/*/src/**/*.rs` for `enum ` definitions
2. For each enum, check if a type with the same name exists in:
   - `apis/*/src/types/*.rs`
   - The generated client types (check re-exports in `clients/internal/*/src/lib.rs`)

**A finding exists when:** A CLI defines an enum that already exists in the API or client crate.

### Check 5: Debug Format Anti-Pattern

Find `format!("{:?}", ...)` used on enum values for display or comparison.

**Grep pattern:**
```
format!("{:?}"
.to_lowercase()
```

**Look for the combination:** `format!("{:?}", some_enum).to_lowercase()` — this is the canonical anti-pattern. The correct approach is `enum_to_display()`.

### Check 6: Missing Forward-Compatibility Variants

Find state/status enums without `#[serde(other)]` catch-all variants.

**How to find them:**
1. Grep `apis/*/src/types/*.rs` for enums with "State" or "Status" in the name
2. Check if they have a `#[serde(other)]` variant

**A finding exists when:** A state/status enum lacks `#[serde(other)] Unknown`.

**Exception:** Enums that are only used for request input (not deserialized from server responses) don't need `#[serde(other)]`.

## Filing Issues

For each finding, run:

```bash
bd create \
  --title "<Short description of the issue>" \
  --description "<Details including:
- File path and line number(s)
- What the current code does wrong
- Suggested fix with code snippet
- Verification: how to confirm the fix>" \
  --priority P2 \
  --type task \
  --labels type-safety
```

**Title conventions:**
- Check 1: `Hardcoded enum string "<value>" in <file>`
- Check 2: `Missing ValueEnum derive on <EnumName>`
- Check 3: `Missing with_patch for <EnumName> in client-generator config for <client>`
- Check 4: `Duplicate enum <EnumName> in <cli-file> (exists in <api-crate>)`
- Check 5: `Debug format anti-pattern on <EnumName> in <file>`
- Check 6: `Missing #[serde(other)] on <EnumName>`

**Priority:**
- P1: Issues that cause runtime bugs (wrong comparisons, missing variants causing deserialization failures)
- P2: Issues that reduce type safety but work today (hardcoded strings matching current variants)
- P3: Style issues (Debug format when enum_to_display would be better but output happens to be correct)

## Output Summary

After filing all issues, print a summary:

```
Type Safety Audit Complete
==========================
Check 1 (hardcoded strings): N findings
Check 2 (missing ValueEnum): N findings
Check 3 (missing with_patch): N findings
Check 4 (duplicate enums):   N findings
Check 5 (Debug format):      N findings
Check 6 (missing serde(other)): N findings
Total: N findings filed as Beads issues

Run `bd ready` to see the work queue.
```
