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

Node.js triton uses action flags (`-a`, `-e`, `-d`) instead of subcommands. The Rust CLI uses a modern subcommand pattern (`user create`, `user delete`) which is cleaner and more explicit. The action flag pattern would require significant restructuring and is documented as an intentional difference.

| Item | Description | Status |
|------|-------------|--------|
| Support `-a` action flag | Add user (alternative to `user create`) | [-] Intentional difference |
| Support `-e` action flag | Edit user in $EDITOR | [-] Intentional difference |
| Support `-d` action flag | Delete user (alternative to `user delete`) | [-] Intentional difference |
| Support `-k` flag on user get | Show keys inline | [x] |
| Add `-y/--yes` alias | For confirmation skipping | [x] |
| Add `-n` short form | Name for key commands | [x] (pre-existing) |
| Add `--dev-create-keys-and-profiles` | Development mode for apply | [x] (hidden, not implemented) |
| Add plural list aliases | `users`, `roles`, `policies` commands | [x] |

**Notes:**
- Action flags (`-a/-e/-d`) require mutually exclusive flag handling incompatible with clap subcommand pattern
- `$EDITOR` integration for `-e` flag would be a separate feature request
- `--dev-create-keys-and-profiles` flag is accepted but returns an error until SSH key generation is implemented

**Files:** `cli/triton-cli/src/commands/rbac.rs`

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
