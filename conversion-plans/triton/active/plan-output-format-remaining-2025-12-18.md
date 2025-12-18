# CLI Output Format - Remaining Work Plan

## Status Summary

**Completed in previous session:**
- JSON output format (compact NDJSON vs pretty arrays)
- triton_cns_enabled deserialization for account
- Table leading/trailing whitespace
- Services command headers (NAME/ENDPOINT)
- Column orders (keys, volumes, RBAC commands)
- AGE calculation (weeks instead of months)
- Missing table columns (FABRIC/VLAN for networks, GLOBAL/LOG for fwrules)

**Current results:** 10 matching, 37 different

---

## Remaining Tasks

### 1. Fix IMG Column to Show name@version (HIGH PRIORITY)

**Problem:** Instance list shows image UUID instead of `name@version` format.

**Current output:**
```
SHORTID   NAME               IMG       STATE    FLAGS  AGE
4d3025c3  nshalman-20250825  97219479  running  B      16w
```

**Expected output:**
```
SHORTID   NAME               IMG                    STATE    FLAGS  AGE
4d3025c3  nshalman-20250825  ubuntu-24.04@20250627  running  B      16w
```

**Files to modify:**
- `/cli/triton-cli/src/commands/instance/list.rs`

**Implementation:**
1. Fetch images list in parallel with machines list using `tokio::join!`
2. Build a HashMap mapping image UUID → `name@version` string
3. Update `get_field_value()` to use the image map for IMG field
4. Fall back to short UUID if image not found

**Reference:** node-triton `lib/do_instance/do_list.js` lines ~140-145

---

### 2. Fix Version Command Format (MEDIUM PRIORITY)

**Problem:** Version output doesn't match node-triton branding.

**Current output:**
```
triton 0.1.0
```

**Expected output:**
```
Triton CLI 7.17.0
https://github.com/TritonDataCenter/node-triton
```

**Files to modify:**
- `/cli/triton-cli/src/main.rs`

**Implementation:**
1. Add custom `Version` subcommand or override clap's `--version` handler
2. Output format:
   ```
   Triton CLI <version>
   https://github.com/TritonDataCenter/triton-rust-monorepo
   ```

---

### 3. Fix Info Command Restructuring (MEDIUM PRIORITY)

**Problem:** Info command output structure differs from node-triton.

**Current rust-triton:**
```
Account: nshalman
Email:   nshalman@parler.com

Instances:
  Total:   1
  Running: 1
  Stopped: 0

Resources:
  Memory:  32768 MB
  Disk:    819200 MB
```

**Expected node-triton:**
```
login: nshalman
name: Nahum Shalman
email: nshalman@parler.com
url: https://us-central-1.api.mnx.io/
totalDisk: 762.9 GiB
totalMemory: 30.5 GiB
instances: 1
    running: 1
```

**Files to modify:**
- `/cli/triton-cli/src/commands/info.rs`

**Implementation:**
1. Change to flat key-value format (no sections)
2. Add missing fields: `name`, `url`
3. Convert units: MB → GiB for display
4. Format instances with indented running count
5. Remove stopped count (node-triton doesn't show it)

**JSON changes:**
- Field names: `disk_used_mb` → `totalDisk`, `memory_used_mb` → `totalMemory`
- Units: MB → bytes (multiply by 1024*1024)
- Add `name` and `url` fields
- Structure `instances` object correctly

---

### 4. Show Command Aliases in Help Text (MEDIUM PRIORITY)

**Problem:** Help doesn't show command aliases like `list (ls)`.

**Current:**
```
Commands:
  list     List instances
  delete   Delete instance(s)
```

**Expected:**
```
Commands:
  list (ls)      List instances
  delete (rm)    Delete instance(s)
```

**Files to modify:**
- All command enum definitions across `/cli/triton-cli/src/commands/`

**Implementation:**
Use clap's `visible_alias` attribute:
```rust
#[derive(Subcommand)]
enum Commands {
    #[command(visible_alias = "ls")]
    List { ... },

    #[command(visible_alias = "rm")]
    Delete { ... },
}
```

**Commands to update:**
- instance: list→ls, delete→rm, get→info
- image: list→ls, delete→rm, copy→cp
- volume: list→ls, delete→rm
- key: list→ls, delete→rm
- fwrule: list→ls, delete→rm, instances→insts
- vlan: list→ls, delete→rm
- network: list→ls
- package: list→ls
- rbac subcommands

---

### 5. Add Missing JSON Fields (LOWER PRIORITY)

**Problem:** Some JSON outputs missing fields present in node-triton.

#### 5a. Network JSON missing fields:
- `description`
- `suffixes`
- `internet_nat`
- `provision_start_ip`
- `provision_end_ip`
- `vlan_id`

**Files to modify:**
- `/apis/cloudapi-api/src/types/network.rs` - Add missing fields to Network struct

#### 5b. Instance JSON missing augmented fields:
- `age`
- `img` (name@version)
- `shortid`
- `flags`

**Files to modify:**
- `/cli/triton-cli/src/commands/instance/list.rs` - Augment machine objects before JSON output

---

## Implementation Order

1. **IMG column fix** - Highest user impact, affects instance list which is heavily used
2. **Info command** - Commonly used command, noticeable difference
3. **Version command** - Quick fix, good for branding consistency
4. **Visible aliases** - Improves discoverability, many files but mechanical change
5. **Missing JSON fields** - Lower priority, mostly affects scripting

---

## Testing

After each change:
```bash
make format
make package-build PACKAGE=triton-cli
make package-test PACKAGE=triton-cli
./scripts/compare-cli-output.sh 2>&1 | grep -E "(✅|Matching|Different)"
```

Compare specific commands:
```bash
diff ./cli-output-comparison/<command>.node.txt ./cli-output-comparison/<command>.rust.txt
```

---

## Files Reference

Key files for this work:
- `/cli/triton-cli/src/commands/instance/list.rs` - Instance list, IMG column
- `/cli/triton-cli/src/commands/info.rs` - Info command
- `/cli/triton-cli/src/main.rs` - Version handling
- `/cli/triton-cli/src/commands/*.rs` - All command files for aliases
- `/apis/cloudapi-api/src/types/network.rs` - Network struct
