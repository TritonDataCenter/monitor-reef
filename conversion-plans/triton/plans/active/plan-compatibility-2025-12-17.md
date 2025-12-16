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
| Short Option Compatibility | ~85% | 100% |
| Long Option Compatibility | ~95% | 100% |
| Behavioral Parity | ~90% | 100% |

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
| Add `-o` short form | Output column selection (`-o field1,field2`) | [ ] |
| Add `-s` short form | Sort field selection | [ ] |
| Add `--brand` filter | Filter by instance brand | [ ] |
| Add `--memory` filter | Filter by memory size | [ ] |
| Add `--docker` filter | Filter by docker flag | [ ] |
| Add `--credentials` | Include credentials in output | [ ] |

**Files:** `cli/triton-cli/src/commands/instance/list.rs`

---

## P2: Image Commands

| Item | Description | Status |
|------|-------------|--------|
| Add `-a/--all` | Include inactive images | [ ] |
| Add `-l/--long` | Long format output | [ ] |
| Add `-o COLUMNS` | Output columns | [ ] |
| Add `-H` | No-header option | [ ] |
| Add `-s FIELD` | Sort field | [ ] |
| Add `--homepage` | Image homepage for create | [ ] |
| Add `--eula` | Image EULA for create | [ ] |
| Add `--acl` | Access control list for create | [ ] |
| Add `-t/--tag` | Tags for create | [ ] |
| Add `--dry-run` | Dry-run for create/copy/clone | [ ] |
| Support positional `DATACENTER` | For `image copy IMAGE DC` | [ ] |

**Files:** `cli/triton-cli/src/commands/image/`

---

## P2: Network/VLAN/Volume Short Forms

| Item | Description | Status |
|------|-------------|--------|
| Support positional VLAN_ID | For network/vlan create | [ ] |
| Add `-D` short form | Description | [ ] |
| Add `-s` short form | Subnet | [ ] |
| Rename `--provision_start` → `--start-ip` | With `-S` short form | [ ] |
| Rename `--provision_end` → `--end-ip` | With `-E` short form | [ ] |
| Add `-g` short form | Gateway | [ ] |
| Add `-R/--route` | Static routes | [ ] |
| Change `--internet_nat` → `--no-nat` | With `-x` short form | [ ] |
| Add `-n` short form | Name for volume/vlan | [ ] |
| Add `-t` short form | Type for volume | [ ] |
| Support GiB size format | "20G" instead of MB only | [ ] |
| Add `-N` short form | Network for volume | [ ] |
| Add `--tag` | Tags for volume create | [ ] |
| Add `-a/--affinity` | Affinity rules for volume | [ ] |
| Add `-w/--wait` | Wait for volume creation | [ ] |
| Add `--wait-timeout` | Wait timeout for volume | [ ] |

**Files:** `cli/triton-cli/src/commands/network/`, `cli/triton-cli/src/commands/vlan/`, `cli/triton-cli/src/commands/volume/`

---

## P2: Profile/Key/Account Commands

| Item | Description | Status |
|------|-------------|--------|
| Add `--copy PROFILE` | Copy from existing profile | [ ] |
| Add `--no-docker` | Skip docker setup | [ ] |
| Add `-y/--yes` | Non-interactive mode | [ ] |
| Add `-n` short form | Name for key add | [ ] |

**Files:** `cli/triton-cli/src/commands/profile/`, `cli/triton-cli/src/commands/key/`

---

## P3: RBAC Action Flags (Legacy Compat)

Node.js triton uses action flags (`-a`, `-e`, `-d`) instead of subcommands. Consider supporting both patterns for backwards compatibility.

| Item | Description | Status |
|------|-------------|--------|
| Support `-a` action flag | Add user (alternative to `user create`) | [ ] |
| Support `-e` action flag | Edit user | [ ] |
| Support `-d` action flag | Delete user (alternative to `user delete`) | [ ] |
| Support `-k` flag on user | Show keys inline | [ ] |
| Add `-y/--yes` alias | For confirmation skipping | [ ] |
| Add `-n` short form | Name for key commands | [ ] |
| Add `--dev-create-keys-and-profiles` | Development mode for apply | [ ] |

**Files:** `cli/triton-cli/src/commands/rbac/`

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
