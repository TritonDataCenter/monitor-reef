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
| Support `-e` action flag | Edit user in $EDITOR | [-] Intentional difference |
| Support `-d` action flag | Delete user (alternative to `user delete`) | [x] |
| Support `-k` flag on user get | Show keys inline | [x] |
| **Role action flags** | | |
| Support `-a` action flag on role | Add role from file/stdin/interactive | [x] |
| Support `-e` action flag on role | Edit role in $EDITOR | [-] Intentional difference |
| Support `-d` action flag on role | Delete role(s) | [x] |
| **Policy action flags** | | |
| Support `-a` action flag on policy | Add policy from file/stdin/interactive | [x] |
| Support `-e` action flag on policy | Edit policy in $EDITOR | [-] Intentional difference |
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
- `$EDITOR` integration for `-e` flag would be a separate feature request
- `--dev-create-keys-and-profiles` flag is accepted but returns an error until SSH key generation is implemented

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
