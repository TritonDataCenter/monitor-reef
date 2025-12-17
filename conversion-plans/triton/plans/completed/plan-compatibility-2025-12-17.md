<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

Copyright 2025 Edgecast Cloud LLC.
-->

# Triton CLI Compatibility Implementation Plan

**Date:** 2025-12-17
**Status:** Active
**Source:** reports/compatibility-report-2025-12-16.md
**Goal:** Achieve 100% option/argument compatibility with node-triton CLI

## Current State

| Metric | Current | Target |
|--------|---------|--------|
| Command Coverage | 100% (107/107) | 100% |
| Short Option Compatibility | ~90% | 100% |
| Long Option Compatibility | ~97% | 100% |
| Behavioral Parity | ~93% | 100% |

## Priority Legend

- **P1**: Blocks common user workflows
- **P2**: Improves compatibility for power users
- **P3**: Legacy compatibility / edge cases

---

## P1: Global Options

| Item | Description | Status |
|------|-------------|--------|
| Add `-u/--user` | RBAC user login name | [x] |
| Add `-r/--role` | RBAC role assumption | [x] |
| Add `-i/--insecure` | Skip TLS certificate validation | [x] |
| Add `--act-as` | Masquerade as another account | [x] |
| Add `--accept-version` | CloudAPI version header (hidden) | [x] |

**Files:** `cli/triton-cli/src/main.rs`, `libs/triton-auth/src/lib.rs`, `clients/internal/cloudapi-client/src/auth.rs`

---

## P1: File/Stdin Input Patterns

| Item | Description | Status |
|------|-------------|--------|
| `profile create -f FILE` | Create profile from JSON file | [x] |
| `profile create -f -` | Create profile from stdin | [x] |
| `account update -f FILE` | Update account from JSON file | [x] |
| `rbac apply -f FILE` | Apply RBAC config from file | [x] (pre-existing) |
| `FIELD=VALUE` syntax | For `account update email=foo@bar.com` | [x] |

**Files:** `cli/triton-cli/src/commands/profile.rs`, `cli/triton-cli/src/commands/account.rs`, `cli/triton-cli/src/commands/rbac.rs`

---

## P2: Instance List Enhancements

| Item | Description | Status |
|------|-------------|--------|
| Add `-o` short form | Output column selection (`-o field1,field2`) | [x] |
| Add `-s` short form | Sort field selection | [x] |
| Add `--brand` filter | Filter by instance brand | [x] |
| Add `--memory` filter | Filter by memory size | [x] |
| Add `--docker` filter | Filter by docker flag | [x] |
| Add `--credentials` | Include credentials in output | [x] |

**Files:** `cli/triton-cli/src/commands/instance/list.rs`, `apis/cloudapi-api/src/types/machine.rs`

---

## P2: Image Commands

| Item | Description | Status |
|------|-------------|--------|
| Add `-a/--all` | Include inactive images | [x] |
| Add `-l/--long` | Long format output | [x] |
| Add `-o COLUMNS` | Output columns | [x] |
| Add `-H` | No-header option | [x] |
| Add `-s FIELD` | Sort field | [x] |
| Add `--homepage` | Image homepage for create | [x] |
| Add `--eula` | Image EULA for create | [x] |
| Add `--acl` | Access control list for create | [x] |
| Add `-t/--tag` | Tags for create | [x] |
| Add `--dry-run` | Dry-run for create/copy/clone | [x] |
| Support positional `DATACENTER` | For `image copy IMAGE DC` | [x] |

**Files:** `cli/triton-cli/src/commands/image.rs`

---

## P2: Network/VLAN/Volume Short Forms

| Item | Description | Status |
|------|-------------|--------|
| Support positional VLAN_ID | For network/vlan create | [x] |
| Add `-D` short form | Description | [x] |
| Add `-s` short form | Subnet | [x] |
| Rename `--provision_start` → `--start-ip` | With `-S` short form | [x] |
| Rename `--provision_end` → `--end-ip` | With `-E` short form | [x] |
| Add `-g` short form | Gateway | [x] |
| Add `-R/--route` | Static routes | [x] |
| Change `--internet_nat` → `--no-nat` | With `-x` short form | [x] |
| Add `-n` short form | Name for volume/vlan | [x] |
| Add `-t` short form | Type for volume | [x] |
| Support GiB size format | "20G" instead of MB only | [x] |
| Add `-N` short form | Network for volume | [x] |
| Add `--tag` | Tags for volume create | [x] |
| Add `-a/--affinity` | Affinity rules for volume | [x] |
| Add `-w/--wait` | Wait for volume creation | [x] |
| Add `--wait-timeout` | Wait timeout for volume | [x] |

**Files:** `cli/triton-cli/src/commands/network.rs`, `cli/triton-cli/src/commands/vlan.rs`, `cli/triton-cli/src/commands/volume.rs`

---

## P2: Profile/Key/Account Commands

| Item | Description | Status |
|------|-------------|--------|
| Add `--copy PROFILE` | Copy from existing profile | [x] |
| Add `--no-docker` | Skip docker setup | [x] |
| Add `-y/--yes` | Non-interactive mode | [x] |
| Add `-n` short form | Name for key add | [x] |

**Files:** `cli/triton-cli/src/commands/profile.rs`, `cli/triton-cli/src/commands/key.rs`

---

## P3: RBAC Action Flags (Legacy Compat)

Node.js triton uses action flags (`-a`, `-e`, `-d`) instead of subcommands. The Rust CLI uses a modern subcommand pattern (`user create`, `user delete`) which is cleaner and more explicit.

| Item | Description | Status |
|------|-------------|--------|
| **User action flags** | | |
| Support `-a` action flag | Add user (alternative to `user create`) | [x] |
| Support `-e` action flag | Edit user in $EDITOR | [x] |
| Support `-d` action flag | Delete user (alternative to `user delete`) | [x] |
| Support `-k` flag on user get | Show keys inline | [x] |
| **Role action flags** | | |
| Support `-a` action flag on role | Add role from file/stdin/interactive | [x] |
| Support `-e` action flag on role | Edit role in $EDITOR | [x] |
| Support `-d` action flag on role | Delete role(s) | [x] |
| **Policy action flags** | | |
| Support `-a` action flag on policy | Add policy from file/stdin/interactive | [x] |
| Support `-e` action flag on policy | Edit policy in $EDITOR | [x] |
| Support `-d` action flag on policy | Delete policy(s) | [x] |
| **Key action flags** | | |
| Support `-a` action flag on key | Add key from file | [x] |
| Support `-d` action flag on key | Delete key(s) | [x] |
| Support `-n` flag on key add | Key name for add | [x] |
| **Common** | | |
| Add `-y/--yes` alias | For confirmation skipping | [x] |
| Add `--dev-create-keys-and-profiles` | Development mode for apply | [x] (hidden, not implemented) |
| Add plural list aliases | `users`, `roles`, `policies` commands | [x] |

**Notes:**
- `--dev-create-keys-and-profiles` flag is accepted but returns an error until SSH key generation is implemented

### Implemented: $EDITOR Integration for `-e` Flag

The `-e` flag launches the user's `$EDITOR` to edit RBAC objects (users, roles, policies) in commented YAML format.

**Implementation approach** (based on node-triton `lib/common.js:editInEditor`):

1. **Fetch** current object from CloudAPI
2. **Serialize** to commented YAML using template strings
3. **Write** to temp file: `triton-<pid>-edit-<account>-<type>-<name>.yaml`
4. **Spawn** `$EDITOR` (fallback: `/usr/bin/vi`) with `stdio: inherit`
5. **Read back** edited content after editor exits
6. **Parse** YAML with `serde_yaml` (comments are ignored)
7. **Validate** changes and detect if content actually changed
8. **Retry loop** on parse/validation errors (prompt user to re-edit)
9. **Update** via CloudAPI if changed

**Commented YAML format** (template-based, comments stripped on parse):

```yaml
# Role: admin-role
# ID: a1b2c3d4-e5f6-7890-abcd-ef1234567890
# Account: myaccount
# Edit below, save and quit to apply changes

# Role name (required, cannot change for built-in roles)
name: admin-role

# Users assigned to this role
members:
  - alice
  - bob

# Policies attached to this role
policies:
  - admin-policy

# Default members (automatically assigned to new users)
default_members: []
```

**Core implementation** in `cli/triton-cli/src/commands/rbac/editor.rs`:

```rust
use std::env;
use std::fs;
use std::io::{self, Write};
use std::process::{Command, Stdio};
use anyhow::{anyhow, Result};

/// Result of editing in $EDITOR
pub struct EditResult {
    pub content: String,
    pub changed: bool,
}

/// Launch $EDITOR to edit text, returns edited content and whether it changed
pub fn edit_in_editor(text: &str, filename: &str) -> Result<EditResult> {
    let tmp_dir = env::temp_dir();
    let tmp_path = tmp_dir.join(format!(
        "triton-{}-edit-{}",
        std::process::id(),
        filename
    ));

    fs::write(&tmp_path, text)?;

    let editor = env::var("EDITOR").unwrap_or_else(|_| "/usr/bin/vi".into());

    // Parse editor command (handles "code --wait", "vim", etc.)
    let mut parts = editor.split_whitespace();
    let program = parts.next().ok_or_else(|| anyhow!("Empty EDITOR"))?;
    let mut cmd = Command::new(program);
    for arg in parts {
        cmd.arg(arg);
    }

    let status = cmd
        .arg(&tmp_path)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()?;

    if !status.success() {
        fs::remove_file(&tmp_path).ok();
        return Err(anyhow!(
            "Editor exited with status: {}",
            status.code().unwrap_or(-1)
        ));
    }

    let after_text = fs::read_to_string(&tmp_path)?;
    fs::remove_file(&tmp_path).ok();

    Ok(EditResult {
        changed: after_text != text,
        content: after_text,
    })
}

/// Prompt user to retry editing after an error
pub fn prompt_retry() -> Result<bool> {
    eprint!("Press Enter to re-edit, Ctrl+C to abort: ");
    io::stderr().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    Ok(true)
}
```

**YAML serializers** (one per type, in respective command files):

```rust
// Example for Role in rbac/role.rs
fn role_to_commented_yaml(role: &Role, account: &str) -> String {
    let members = if role.members.is_empty() {
        "  []".to_string()
    } else {
        role.members.iter()
            .map(|m| format!("  - {}", m))
            .collect::<Vec<_>>()
            .join("\n")
    };

    let policies = if role.policies.is_empty() {
        "  []".to_string()
    } else {
        role.policies.iter()
            .map(|p| format!("  - {}", p.name))
            .collect::<Vec<_>>()
            .join("\n")
    };

    format!(r#"# Role: {name}
# ID: {id}
# Account: {account}
# Edit below, save and quit to apply changes

# Role name (required)
name: {name}

# Users assigned to this role
members:
{members}

# Policies attached to this role
policies:
{policies}

# Default members (automatically assigned)
default_members: []
"#,
        name = role.name,
        id = role.id,
        account = account,
        members = members,
        policies = policies,
    )
}

// Deserialize struct (comments ignored by serde_yaml)
#[derive(Deserialize)]
struct RoleEdit {
    name: String,
    #[serde(default)]
    members: Vec<String>,
    #[serde(default)]
    policies: Vec<String>,
    #[serde(default)]
    default_members: Vec<String>,
}
```

**Edit flow with retry loop**:

```rust
async fn edit_role(client: &TypedClient, role_id: &str) -> Result<()> {
    let role = client.get_role(role_id).await?;
    let account = client.auth_config().account.clone();
    let filename = format!("{}-role-{}.yaml", account, role.name);
    let original_yaml = role_to_commented_yaml(&role, &account);

    let mut current_yaml = original_yaml.clone();
    loop {
        let result = edit_in_editor(&current_yaml, &filename)?;

        if !result.changed {
            println!("No changes made");
            return Ok(());
        }

        match serde_yaml::from_str::<RoleEdit>(&result.content) {
            Ok(edited) => {
                // Validate and update
                client.update_role(&role.id, &edited).await?;
                println!("Updated role \"{}\"", edited.name);
                return Ok(());
            }
            Err(e) => {
                eprintln!("Error parsing YAML: {}", e);
                if !prompt_retry()? {
                    return Err(anyhow!("Aborted"));
                }
                current_yaml = result.content; // Keep user's edits for retry
            }
        }
    }
}
```

**Dependencies** (add to workspace `Cargo.toml`):

```toml
serde_yaml = "0.9"
```

**Files to modify:**
- `cli/triton-cli/src/commands/rbac/editor.rs` (new - core edit_in_editor function)
- `cli/triton-cli/src/commands/rbac/mod.rs` (add editor module)
- `cli/triton-cli/src/commands/rbac/user.rs` (add -e flag handling)
- `cli/triton-cli/src/commands/rbac/role.rs` (add -e flag handling)
- `cli/triton-cli/src/commands/rbac/policy.rs` (add -e flag handling)

### Implemented: Action Flag Implementation Approach

Clap supports commands that have both subcommands AND direct flags/arguments using `Option<Subcommand>`. This pattern has been applied to all RBAC commands (user, role, policy, key).

**Implementation pattern:**

1. Convert the command enum (e.g., `RbacUserCommand`) to an `Args` struct with:
   - `#[command(subcommand)] command: Option<Subcommand>` - optional subcommand
   - `-a/--add` flag (conflicts with `-d`)
   - `-d/--delete` flag (conflicts with `-a`)
   - Positional args for context-specific arguments
   - Additional flags as needed (`-k/--keys`, `-n/--name`, `-y/--yes`)

2. Dispatch logic in `run()`:
   - If subcommand present → delegate to subcommand (modern pattern)
   - If `-a` flag → add/create from file/stdin/interactive (legacy compat)
   - If `-d` flag → delete (legacy compat)
   - Otherwise → show (default action)

3. This allows both patterns to coexist:
   ```bash
   # Modern (subcommand) pattern - preferred for new scripts
   triton rbac user create LOGIN --email foo@bar.com
   triton rbac user delete USER
   triton rbac role create NAME --policy ...
   triton rbac policy create NAME --rule ...

   # Legacy (action flag) pattern - node-triton compatibility
   triton rbac user -a FILE        # add from file
   triton rbac user -d USER...     # delete
   triton rbac user USER           # show (default)
   triton rbac user -k USER        # show with keys
   triton rbac role -a FILE        # add role from file
   triton rbac role -d ROLE...     # delete role(s)
   triton rbac policy -a FILE      # add policy from file
   triton rbac policy -d POLICY... # delete policy(s)
   triton rbac key -a USER FILE    # add key from file
   triton rbac key -d USER KEY...  # delete key(s)
   ```

**Files:**
- `cli/triton-cli/src/commands/rbac/user.rs`
- `cli/triton-cli/src/commands/rbac/role.rs`
- `cli/triton-cli/src/commands/rbac/policy.rs`
- `cli/triton-cli/src/commands/rbac/keys.rs`
- `cli/triton-cli/src/commands/rbac/mod.rs`

---

## Technical Notes

### Clap Constraints

1. **Global short options cannot shadow subcommand options** - resolved by making globals top-level only
2. **JSON output** - currently global `-j`, Node.js uses per-command. Document as intentional difference or add per-command.

### Size Parsing

Need a utility function to parse sizes like:
- `10240` → 10240 MB
- `10G` → 10240 MB
- `1T` → 1048576 MB

---

## References

- [compatibility-report-2025-12-16.md](../../reports/compatibility-report-2025-12-16.md) - Full analysis
- [cli-option-compatibility.md](../../reference/cli-option-compatibility.md) - Technical constraints
- [Node.js triton source](../../../../target/node-triton/) - Reference implementation
