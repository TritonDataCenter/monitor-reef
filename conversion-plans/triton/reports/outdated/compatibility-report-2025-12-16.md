<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

Copyright 2025 Edgecast Cloud LLC.
-->

# Triton CLI Option/Argument Compatibility Analysis

Generated: 2025-12-16

## Executive Summary

This document provides a systematic comparison of the Rust `triton-cli` implementation against the Node.js `triton` CLI, identifying all differences in option and argument handling.

**Goal: 100% compatibility with node-triton CLI options and behavior.**

### Overall Compatibility Assessment

| Metric | Current | Target |
|--------|---------|--------|
| **Command Coverage** | 100% (107/107) | 100% |
| **Short Option Compatibility** | ~85% | 100% |
| **Long Option Compatibility** | ~95% | 100% |
| **Behavioral Parity** | ~90% | 100% |

### Key Findings

1. **Short option conflicts resolved** - Global `-v`, `-k`, `-a` options are top-level only, allowing subcommands to reuse them
2. **Missing table formatting** - Need to add `-o`, `-l`, `-H`, `-s` column customization options
3. **Missing file/stdin input** - Need to add `-f FILE` and `-f -` patterns for updates
4. **Missing short forms** - Many options have long form only, need short forms added

---

## Part 1: Global Options Comparison

### Top-Level Options

| Option | Short | Node.js | Rust | Status |
|--------|-------|---------|------|--------|
| Help | -h | `--help` | `--help` | **Compatible** |
| Version | | `--version` | `--version` | **Compatible** |
| Verbose | -v | `--verbose` | `--verbose` | **Compatible** |
| Profile | -p | `--profile` | `--profile` | **Compatible** |
| Account | -a | `--account` | `--account` | **Compatible** |
| User | -u | `--user` | N/A | **TODO: Add** |
| Role | -r | `--role` | N/A | **TODO: Add** |
| Key ID | -k | `--keyId` | `--key-id` | **Compatible** (different case) |
| URL | -U | `--url` | `--url` | **Compatible** |
| JSON | -j | N/A | `--json` | **Rust addition** (Node.js uses per-command) |
| Insecure | -i | `--insecure` | N/A | **TODO: Add** |
| Act-As | | `--act-as` | N/A | **TODO: Add** |
| Accept-Version | | `--accept-version` | N/A | **TODO: Add** (hidden in Node.js) |

### Resolution: Top-Level Only Arguments

The Rust implementation resolved short option conflicts by making global arguments **top-level only**:

```bash
# Correct usage:
triton -v instance list      # verbose mode
triton instance create -v myvol image pkg  # -v for volume

# Global options must come BEFORE the subcommand
triton instance list -v      # ERROR: -v not recognized after subcommand
```

---

## Part 2: Instance Commands

### Instance Create Options

| Option | Short | Node.js | Rust | Status |
|--------|-------|---------|------|--------|
| Name | -n | `--name` | `--name` | **Compatible** |
| Brand | -b | `--brand` | `--brand` | **Compatible** |
| Tag | -t | `--tag` | `--tag` | **Compatible** |
| Affinity | -a | `--affinity` | `--affinity` | **Compatible** |
| Network | -N | `--network` | `--network` | **Compatible** |
| NIC | | `--nic` | `--nic` | **Compatible** |
| Volume | -v | `--volume` | `--volume` | **Compatible** |
| Metadata | -m | `--metadata` | `--metadata` | **Compatible** |
| Metadata-file | -M | `--metadata-file` | `--metadata-file` | **Compatible** |
| Script | | `--script` | `--script` | **Compatible** |
| Cloud-config | | `--cloud-config` | `--cloud-config` | **Compatible** |
| Firewall | | `--firewall` | `--firewall` | **Compatible** |
| Deletion-protection | | `--deletion-protection` | `--deletion-protection` | **Compatible** |
| Delegate-dataset | | `--delegate-dataset` | `--delegate-dataset` | **Compatible** |
| Encrypted | | `--encrypted` | `--encrypted` | **Compatible** |
| Allow-shared-images | | `--allow-shared-images` | `--allow-shared-images` | **Compatible** |
| Disk | | `--disk` | `--disk` | **Compatible** |
| Dry-run | | `--dry-run` | `--dry-run` | **Compatible** |
| Wait | -w | `--wait` | `--wait` | **Compatible** |
| Wait-timeout | | `--wait-timeout` | `--wait-timeout` | **Rust adds default (600s)** |
| JSON | -j | `--json` | N/A | Uses global `-j` |

### Instance List Options

| Option | Short | Node.js | Rust | Status |
|--------|-------|---------|------|--------|
| Name filter | | Via `name=VALUE` args | `--name` | **Different syntax** |
| State filter | | Via `state=VALUE` args | `--state` | **Different syntax** |
| Image filter | | Via `image=VALUE` args | `--image` | **Different syntax** |
| Tag filter | -t | Via `tag.KEY=VALUE` args | `--tag key=value` | **Different syntax** |
| Package filter | | Via `package=VALUE` args | `--package` | **Different syntax** |
| Brand filter | | Via `brand=VALUE` args | N/A | **TODO: Add** |
| Memory filter | | Via `memory=VALUE` args | N/A | **TODO: Add** |
| Docker filter | | Via `docker=BOOL` args | N/A | **TODO: Add** |
| Credentials | | `--credentials` | N/A | **TODO: Add** |
| Limit | | N/A | `--limit` | **Rust addition** |
| Short | | N/A | `--short` | **Rust addition** |
| Sort | -s | `-s FIELD` | `--sort-by` | **TODO: Add `-s` short form** |
| Output | -o | `-o COLUMNS` | `--output` | **TODO: Add `-o` short form** |
| Long | -l | `--long` | `--long` | **Compatible** |
| No-header | -H | N/A | `--no-header` | **New in Rust** |
| JSON | -j | `--json` | N/A | Uses global `-j` |

### Instance SSH Options

| Option | Short | Node.js | Rust | Status |
|--------|-------|---------|------|--------|
| User | | `user@instance` syntax | `--user` / `-l` | **Different approach** |
| Identity | -i | `-i FILE` | `--identity` / `-i` | **Compatible** |
| SSH Option | -o | `-o OPT` | `--ssh-option` / `-o` | **Compatible** |
| No-proxy | | N/A | `--no-proxy` | **New in Rust** |

---

## Part 3: Image Commands

### Image List Options

| Option | Short | Node.js | Rust | Status |
|--------|-------|---------|------|--------|
| All states | -a | `--all` | N/A | **TODO: Add** |
| Name | | Via args | `--name` | **Different syntax** |
| Version | | Via args | `--version` | **Different syntax** |
| OS | | Via args | `--os` | **Different syntax** |
| Public | | Via args | `--public` | **Rust addition** |
| State | | Via args | `--state` | **Different syntax** |
| Type | | Via args | `--type` | **Different syntax** |
| Long | -l | `--long` | N/A | **TODO: Add** |
| Output | -o | `-o COLUMNS` | N/A | **TODO: Add** |
| No-header | -H | N/A | N/A | **TODO: Add** |
| Sort | -s | `-s FIELD` | N/A | **TODO: Add** |
| JSON | -j | `--json` | N/A | Uses global `-j` |

### Image Create Options

| Option | Short | Node.js | Rust | Status |
|--------|-------|---------|------|--------|
| Name | | Positional | `--name` | **Different syntax** |
| Version | | Positional | `--version` | **Different syntax** |
| Description | -d | `--description` | `--description` | **Compatible** |
| Homepage | | `--homepage` | N/A | **TODO: Add** |
| EULA | | `--eula` | N/A | **TODO: Add** |
| ACL | | `--acl` (array) | N/A | **TODO: Add** |
| Tag | -t | `--tag` (array) | N/A | **TODO: Add** |
| Dry-run | | `--dry-run` | N/A | **TODO: Add** |
| Wait | -w | `--wait` | `--wait` | **Compatible** |
| JSON | -j | `--json` | N/A | Uses global `-j` |

### Image Copy Options

| Option | Short | Node.js | Rust | Status |
|--------|-------|---------|------|--------|
| Source | | Positional `DATACENTER` | `--source DATACENTER` | **TODO: Support positional syntax** |
| Dry-run | | `--dry-run` | N/A | **TODO: Add** |
| JSON | -j | `--json` | N/A | Uses global `-j` |

---

## Part 4: Network/VLAN/Volume Commands

### Network Create Options

| Option | Short | Node.js | Rust | Status |
|--------|-------|---------|------|--------|
| VLAN ID | | Positional | `--vlan_id` | **TODO: Support positional syntax** |
| Name | -n | `--name` | `--name` | **Compatible** |
| Description | -D | `--description` | `--description` | **TODO: Add `-D` short form** |
| Subnet | -s | `--subnet` | `--subnet` | **TODO: Add `-s` short form** |
| Start IP | -S | `--start-ip` | `--provision_start` | **TODO: Rename to `--start-ip` with `-S`** |
| End IP | -E | `--end-ip` | `--provision_end` | **TODO: Rename to `--end-ip` with `-E`** |
| Gateway | -g | `--gateway` | `--gateway` | **TODO: Add `-g` short form** |
| Resolver | -r | `--resolver` (array) | `--resolver` | **Compatible** |
| Route | -R | `--route` (array) | N/A | **TODO: Add** |
| No-NAT | -x | `--no-nat` | `--internet_nat` | **TODO: Change to `--no-nat` with `-x`** |
| JSON | -j | `--json` | N/A | Uses global `-j` |

### VLAN Create Options

| Option | Short | Node.js | Rust | Status |
|--------|-------|---------|------|--------|
| VLAN ID | | Positional | `--vlan_id` | **TODO: Support positional syntax** |
| Name | -n | `--name` | `--name` | **TODO: Add `-n` short form** |
| Description | -D | `--description` | `--description` | **TODO: Add `-D` short form** |
| JSON | -j | `--json` | N/A | Uses global `-j` |

### Volume Create Options

| Option | Short | Node.js | Rust | Status |
|--------|-------|---------|------|--------|
| Name | -n | `--name` | `--name` | **TODO: Add `-n` short form** |
| Type | -t | `--type` | `--type` | **TODO: Add `-t` short form** |
| Size | -s | `--size` (e.g., "20G") | `--size` (MB only) | **TODO: Support GiB format ("20G")** |
| Network | -N | `--network` | `--network` | **TODO: Add `-N` short form** |
| Tag | | `--tag` (array) | N/A | **TODO: Add** |
| Affinity | -a | `--affinity` | N/A | **TODO: Add** |
| Wait | -w | `--wait` | N/A | **TODO: Add** |
| Wait-timeout | | `--wait-timeout` | N/A | **TODO: Add** |
| JSON | -j | `--json` | N/A | Uses global `-j` |

---

## Part 5: Profile/Key/Account Commands

### Profile Create Options

| Option | Short | Node.js | Rust | Status |
|--------|-------|---------|------|--------|
| File | -f | `--file FILE` | N/A | **TODO: Add** |
| Copy | | `--copy PROFILE` | N/A | **TODO: Add** |
| No-docker | | `--no-docker` | N/A | **TODO: Add** |
| Yes | -y | `--yes` | N/A | **TODO: Add** |
| URL | | N/A | `--url` | **Rust addition** |
| Account | -a | N/A | `--account` | **Rust addition** |
| Key-ID | -k | N/A | `--key-id` | **Rust addition** |
| Insecure | | N/A | `--insecure` | **Rust addition** |

### Key Add Options

| Option | Short | Node.js | Rust | Status |
|--------|-------|---------|------|--------|
| Name | -n | `--name` | `--name` | **TODO: Add `-n` short form** |
| File | | Positional (required) | Positional (optional) | **Compatible** (Rust more lenient) |
| JSON | -j | `--json` | N/A | Uses global `-j` |

### Account Update Options

| Option | Short | Node.js | Rust | Status |
|--------|-------|---------|------|--------|
| File | -f | `--file FILE` | N/A | **TODO: Add** |
| Fields | | `FIELD=VALUE` positional | Individual flags | **TODO: Support FIELD=VALUE syntax** |
| Email | | Via FIELD=VALUE | `--email` | **Rust addition** |
| Given-name | | Via FIELD=VALUE | `--given-name` | **Rust addition** |
| Surname | | Via FIELD=VALUE | `--surname` | **Rust addition** |
| Company-name | | Via FIELD=VALUE | `--company-name` | **Rust addition** |
| Phone | | Via FIELD=VALUE | `--phone` | **Rust addition** |

---

## Part 6: RBAC Commands

### RBAC User Commands

| Aspect | Node.js | Rust | Status |
|--------|---------|------|--------|
| Show user | `triton rbac user USER` | `triton rbac user get USER` | **TODO: Support action-flag syntax** |
| Add user | `triton rbac user -a [FILE]` | `triton rbac user create LOGIN` | **TODO: Support `-a` action flag** |
| Edit user | `triton rbac user -e USER` | N/A | **TODO: Add** |
| Delete user | `triton rbac user -d [-y] USER` | `triton rbac user delete [-f] USER` | **TODO: Support `-d` and `-y` flags** |
| Show keys | `triton rbac user -k USER` | `triton rbac keys USER` | **TODO: Support `-k` flag on user** |

### RBAC Key Commands

| Option | Short | Node.js | Rust | Status |
|--------|-------|---------|------|--------|
| Name | -n | `--name` | `--name` | **TODO: Add `-n` short form** |
| Key | -k | N/A | `--key` | **Rust addition** |
| Force | -y | `--yes` | `--force` / `-f` | **TODO: Add `-y/--yes` alias** |

**Known Conflict Resolution:**
- Node.js `-k, --keys` is a boolean flag for showing user keys
- Rust uses `--key` (long form only) for key data to avoid conflict with global `-k`

### RBAC Apply Options

| Option | Short | Node.js | Rust | Status |
|--------|-------|---------|------|--------|
| File | -f | `--file FILE` | Positional `FILE` | **TODO: Support `-f FILE` syntax** |
| Dry-run | -n | `--dry-run` | `--dry-run` / `-n` | **Compatible** |
| Yes | -y | `--yes` | `--force` / `-f` | **TODO: Add `-y/--yes` alias** |
| Dev-keys | | `--dev-create-keys-and-profiles` | N/A | **TODO: Add** |

---

## Part 7: TODO Summary for 100% Compatibility

### Global Options

| TODO | Description |
|------|-------------|
| Add `-u/--user` | RBAC user login name |
| Add `-r/--role` | RBAC role assumption |
| Add `-i/--insecure` | Skip TLS certificate validation |
| Add `--act-as` | Masquerade as another account |
| Add `--accept-version` | CloudAPI version header |

### Instance Commands

| TODO | Description |
|------|-------------|
| Add `-o` short form | Output column selection for list |
| Add `-s` short form | Sort field for list |
| Add `--brand` filter | Filter by brand in list |
| Add `--memory` filter | Filter by memory in list |
| Add `--docker` filter | Filter by docker flag in list |
| Add `--credentials` | Include credentials in get/list |

### Image Commands

| TODO | Description |
|------|-------------|
| Add `-a/--all` | Include inactive images in list |
| Add `-l/--long` | Long format for list |
| Add `-o COLUMNS` | Output columns for list |
| Add `-H` | No-header for list |
| Add `-s FIELD` | Sort field for list |
| Add `--homepage` | Image homepage for create |
| Add `--eula` | Image EULA for create |
| Add `--acl` | Access control list for create |
| Add `-t/--tag` | Tags for create |
| Add `--dry-run` | Dry-run for create/copy/clone |
| Support positional `DATACENTER` | For image copy |

### Network/VLAN/Volume Commands

| TODO | Description |
|------|-------------|
| Support positional VLAN_ID | For network/vlan create |
| Add `-D` short form | Description |
| Add `-s` short form | Subnet |
| Rename `--provision_start` to `--start-ip` | With `-S` short form |
| Rename `--provision_end` to `--end-ip` | With `-E` short form |
| Add `-g` short form | Gateway |
| Add `-R/--route` | Static routes for network create |
| Change `--internet_nat` to `--no-nat` | With `-x` short form |
| Add `-n` short form | Name for volume/vlan |
| Add `-t` short form | Type for volume |
| Support GiB size format | "20G" instead of MB |
| Add `-N` short form | Network for volume |
| Add `--tag` | Tags for volume create |
| Add `-a/--affinity` | Affinity rules for volume |
| Add `-w/--wait` | Wait for volume creation |
| Add `--wait-timeout` | Wait timeout for volume |

### Profile/Key/Account Commands

| TODO | Description |
|------|-------------|
| Add `-f/--file FILE` | File input for profile create |
| Add `--copy PROFILE` | Copy from existing profile |
| Add `--no-docker` | Skip docker setup |
| Add `-y/--yes` | Non-interactive mode |
| Add `-n` short form | Name for key add |
| Add `-f/--file FILE` | File input for account update |
| Support `FIELD=VALUE` syntax | For account update |

### RBAC Commands

| TODO | Description |
|------|-------------|
| Support `-a` action flag | Add user (alternative to subcommand) |
| Support `-e` action flag | Edit user |
| Support `-d` action flag | Delete user (alternative to subcommand) |
| Support `-k` flag on user | Show keys inline |
| Add `-y/--yes` alias | For confirmation skipping |
| Add `-n` short form | Name for key commands |
| Support `-f FILE` syntax | For apply command |
| Add `--dev-create-keys-and-profiles` | Development mode for apply |

---

## Part 8: Technical Considerations

### Clap Framework Constraints

1. **Global short options** - Cannot shadow at subcommand level
   - **Current resolution:** Top-level only arguments
   - **This matches Node.js behavior** where globals must come before subcommand

2. **Action flags vs subcommands** (RBAC)
   - Node.js: `triton rbac user -a`, `-e`, `-d` flags
   - Rust: `triton rbac user create`, `update`, `delete` subcommands
   - **To achieve 100% compatibility:** Could support both patterns

3. **Positional vs named arguments**
   - Node.js: `triton image copy IMAGE DATACENTER`
   - Rust: `triton image copy IMAGE --source DATACENTER`
   - **To achieve 100% compatibility:** Add positional argument support

4. **JSON output**
   - Node.js: Per-command `-j` flag
   - Rust: Global `-j` flag
   - **Decision needed:** Add per-command `-j` for exact compatibility, or document difference

### Implementation Notes

1. **File/stdin input** - Needs implementation for:
   - `profile create -f FILE` or `-f -`
   - `account update -f FILE` or `-f -`
   - `rbac apply -f FILE`
   - `rbac user -a FILE`

2. **FIELD=VALUE parsing** - Needs implementation for:
   - `account update email=foo@bar.com`
   - Generic field updates

3. **Size parsing** - Need to support both:
   - `--size 10240` (current MB)
   - `--size 10G` (GiB format like Node.js)

---

## Summary

### Currently Compatible

- All major instance lifecycle commands (create, start, stop, reboot, delete)
- Instance metadata, tags, disks, NICs, snapshots management
- SSH and VNC connectivity
- Core network and VLAN operations
- Core profile management
- Firewall rules management
- Short option handling for conflicting globals (-v, -k, -a)

### Gaps to Close for 100% Compatibility

1. **Missing global options** - `--user`, `--role`, `--insecure`, `--act-as`
2. **Missing short forms** - Many options have long form only
3. **Missing table formatting** - `-o`, `-l`, `-s`, `-H` for list commands
4. **Missing file/stdin input** - `-f FILE` patterns
5. **Missing FIELD=VALUE syntax** - For updates
6. **Size format** - Need to support "20G" format
7. **Positional arguments** - Some commands use flags where Node.js uses positional
8. **Action flags** - RBAC uses subcommands instead of `-a`, `-e`, `-d` flags
9. **Per-command `-j`** - Currently global only

### Rust Additions (Keep)

These Rust-specific additions don't break compatibility and can be kept:
- `--limit` for list commands
- `--short` for list commands
- `--no-proxy` for SSH
- `--wait-timeout` defaults
- Explicit flags for account update fields

---

## References

- [cli-option-compatibility.md](cli-option-compatibility.md) - Short option conflict resolution
- [validation-report.md](validation-report.md) - Full feature coverage report
- [Clap documentation](https://docs.rs/clap)
- [Node.js triton source](target/node-triton/)
