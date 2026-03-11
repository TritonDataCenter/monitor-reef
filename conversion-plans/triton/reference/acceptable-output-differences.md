# Acceptable CLI Output Differences

This document tracks output differences between rust-triton and node-triton that have been reviewed and deemed acceptable. These are intentional design decisions or framework-imposed differences that do not break compatibility.

## Overview

After comparing outputs across 47 commands, we identified several categories of differences. This document covers the **low priority** differences that are acceptable as-is.

---

## 1. Help Text Format (Clap Framework Style)

**Status:** Acceptable - Framework difference

### Description
Rust-triton uses [Clap](https://docs.rs/clap)'s auto-generated help format, which differs structurally from node-triton's custom help format.

### Differences

| Aspect | node-triton | rust-triton |
|--------|-------------|-------------|
| Command grouping | Semantic sections (Instances, Images, etc.) | Flat alphabetical list |
| Options display | Grouped into "Options" and "CloudAPI Options" | Flat list |
| Aliases | Shown inline: `list (ls)` | Listed separately or via `visible_alias` |
| Description style | More verbose | More concise |

### Examples

**node-triton:**
```
Usage:
    triton [OPTIONS] COMMAND [ARGS...]
    triton help COMMAND

Options:
    -h, --help          Show this help message and exit.
    -V, --version       Print version and exit.
    ...

CloudAPI Options:
    -a, --account ACCOUNT   ...

Commands:
    Instances:
        instance        List and manage Triton instances.
        ...
```

**rust-triton:**
```
Usage: triton [OPTIONS] <COMMAND>

Commands:
  account    Manage account settings
  image      Manage images
  instance   Manage instances
  ...

Options:
  -a, --account <ACCOUNT>  [env: TRITON_ACCOUNT=nshalman]
  ...
```

### Why Acceptable

1. **Industry standard:** Clap's format is widely recognized in the Rust ecosystem
2. **Functionality preserved:** All commands and options are documented
3. **Better environment variable display:** Rust shows current values, which is more helpful
4. **Maintainability:** Custom help formatting would require significant effort to maintain

### Note
The following help-related improvements are tracked separately as medium priority:
- Adding `visible_alias` to show aliases like `list (ls)`
- Adding exit status documentation to main help

---

## 2. Environment Variable Display in Help

**Status:** Improvement - Keep as-is

### Description
Rust-triton shows current environment variable values in help output.

### Example

**node-triton:**
```
-a, --account ACCOUNT
    Triton account (login name). Environment: TRITON_ACCOUNT.
```

**rust-triton:**
```
-a, --account <ACCOUNT>  [env: TRITON_ACCOUNT=nshalman]
```

### Why Acceptable (Actually Better)
- Shows the **current value**, not just the variable name
- Helps users debug configuration issues
- Standard Clap behavior

---

## 3. JSON Key Ordering

**Status:** Acceptable - JSON spec compliant

### Description
JSON object key ordering may differ between implementations.

### Example

**node-triton:**
```json
{"cmon":"...","docker":"...","manta":"..."}
```

**rust-triton:**
```json
{"docker":"...","manta":"...","cmon":"..."}
```

### Why Acceptable
1. **JSON specification:** Key order in objects is explicitly not guaranteed
2. **Semantic equivalence:** Both outputs parse to identical data structures
3. **Best practice:** Scripts should not depend on key order

### Note
For deterministic testing/diffing, we could optionally sort keys, but this is not a compatibility requirement.

---

## 4. Additional Command Shortcuts

**Status:** Improvement - Keep as-is

### Description
Rust-triton includes additional top-level shortcuts that node-triton doesn't have.

### Additional shortcuts in rust-triton
- `nets` (shortcut for `network list`) - node-triton errors on this
- `vlans` (shortcut for `vlan list`) - node-triton errors on this

### Why Acceptable (Actually Better)
- Follows the pattern established by existing shortcuts (`insts`, `imgs`, `pkgs`, `vols`, `keys`)
- Improves consistency and user experience
- Backwards compatible (doesn't break existing commands)

---

## 5. Terse vs Verbose Descriptions

**Status:** Acceptable - Style preference

### Description
Some command descriptions are more concise in rust-triton.

### Examples

| Command | node-triton | rust-triton |
|---------|-------------|-------------|
| `instance ssh` | "SSH to the primary IP of an instance" | "SSH to an instance" |
| `fwrule get` | "Show a specific firewall rule." | "Get firewall rule details" |
| `key delete` | "Remove an SSH key from an account." | "Delete SSH key(s)" |

### Why Acceptable
1. **Meaning preserved:** Both descriptions communicate the same functionality
2. **Consistency:** Rust descriptions follow a consistent pattern
3. **Plural support:** Rust often indicates when commands support multiple items

---

## 6. RBAC Experimental Warning

**Status:** Intentional removal

### Description
node-triton shows an experimental warning for RBAC commands; rust-triton does not.

### node-triton:
```
Warning: RBAC support is experimental.

See <https://docs.tritondatacenter.com/...> for more information.
```

### rust-triton:
(No warning)

### Why Acceptable
RBAC is a mature feature at this point. The warning was historical and can be removed.

---

## 7. Empty JSON Array for Empty Results

**Status:** Improvement - Keep as-is

### Description
When listing resources with no results, JSON output format differs.

### Example (empty firewall rules)

**node-triton:**
```
(empty output - 0 bytes)
```

**rust-triton:**
```json
[]
```

### Why Acceptable (Actually Better)
- `[]` is valid JSON representing an empty array
- Easier to parse programmatically
- Scripts don't need special handling for empty vs. non-empty results

---

## 8. Reset Command in RBAC

**Status:** Enhancement - Document

### Description
Rust-triton includes a `triton rbac reset` command that doesn't exist in node-triton.

### Why Acceptable
- New functionality added during migration
- Doesn't break existing commands
- Useful for testing and development

---

## Summary Table

| Difference | Category | Decision |
|------------|----------|----------|
| Clap-style help format | Framework | Acceptable |
| Env var values in help | Framework | Keep (improvement) |
| JSON key ordering | Spec-compliant | Acceptable |
| Extra shortcuts (nets, vlans) | Enhancement | Keep (improvement) |
| Terse descriptions | Style | Acceptable |
| No RBAC warning | Intentional | Acceptable |
| Empty array for empty results | Correctness | Keep (improvement) |
| RBAC reset command | Enhancement | Keep (document) |

---

## Related Documents

- [Exit Code Comparison](./exit-code-comparison.md) - Differences in exit codes
- [Error Format Comparison](./error-format-comparison.md) - Differences in error messages
- [CLI Option Compatibility](./cli-option-compatibility.md) - Short option handling

---

## Changelog

- **2025-12-18:** Initial document created after comprehensive output comparison
