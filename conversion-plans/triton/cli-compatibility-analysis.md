# Triton CLI Compatibility Analysis Report

This document provides a systematic comparison between the Node.js `triton` CLI and the Rust `triton-cli` implementation, identifying differences in commands, options, and behavior.

## Executive Summary

| Metric | Value |
|--------|-------|
| **Command Coverage** | ~95% |
| **Option Compatibility** | ~98% |
| **Short Option Conflicts** | 0 (resolved) |
| **Missing Commands** | 3 commands |
| **Missing Features** | 4 features |

**Overall Assessment:** The Rust CLI provides excellent coverage of the Node.js CLI functionality. Recent improvements include:
- Short option conflicts (`-v`, `-k`, `-a`) resolved by making global options top-level only
- Added `triton ip` and `triton profiles` shortcuts
- Added plural aliases (`disks`, `snapshots`, `tags`, `metadatas`, `nics`)
- Added instance list options (`-l`, `-H`, `-s`)

### Remaining Gaps

| Gap | Priority | Effort | Notes |
|-----|----------|--------|-------|
| ~~SSH proxy support~~ | ~~Medium~~ | ~~2-3h~~ | ✅ Completed |
| ~~SSH default user detection~~ | ~~Low~~ | ~~1h~~ | ✅ Completed |
| `triton cloudapi` raw command | P3 | 2-3h | Developer debugging tool |
| RBAC apply/reset commands | P2 | 3-4h | Bulk config from files |
| RBAC role tags commands | P2 | 2-3h | Manage role tags on resources |
| Top-level instance shortcuts | P3 | 1h | `triton disks`, `triton snapshots`, etc. |

See also: `conversion-plans/triton/plan-low-priority-2025-12-16.md` for detailed specs.

---

## 1. Command Coverage Report

### 1.1 Top-Level Commands

| Node.js Command | Rust CLI | Status | Notes |
|-----------------|----------|--------|-------|
| `triton account` | `triton account` | Full | get, update, limits |
| `triton badger` | - | Missing | Easter egg, low priority |
| `triton changefeed` | `triton changefeed` | Full | |
| `triton cloudapi` | - | Missing | Raw API access, medium priority |
| `triton completion` | `triton completion` | Full | |
| `triton create` | `triton create` | Full | Shortcut |
| `triton datacenters` | `triton datacenters` | Full | |
| `triton delete` | `triton delete` | Full | Shortcut |
| `triton env` | `triton env` | Full | |
| `triton fwrule` | `triton fwrule` | Full | |
| `triton fwrules` | `triton fwrules` | Full | Shortcut |
| `triton image` | `triton image` | Full | |
| `triton images` | `triton imgs` | Full | Different shortcut name |
| `triton info` | `triton info` | Full | |
| `triton instance` | `triton instance` | Full | |
| `triton instances` | `triton insts` | Full | Different shortcut name |
| `triton ip` | `triton ip` | Full | Shortcut for `instance ip` |
| `triton key` | `triton key` | Full | |
| `triton keys` | `triton keys` | Full | Shortcut |
| `triton network` | `triton network` | Full | |
| `triton networks` | `triton nets` | Full | Different shortcut name |
| `triton package` | `triton package` | Full | |
| `triton packages` | `triton pkgs` | Full | Different shortcut name |
| `triton profile` | `triton profile` | Full | |
| `triton profiles` | `triton profiles` | Full | Shortcut for `profile list` |
| `triton rbac` | `triton rbac` | Full | |
| `triton reboot` | `triton reboot` | Full | Shortcut |
| `triton services` | `triton services` | Full | |
| `triton ssh` | `triton ssh` | Full | Shortcut |
| `triton start` | `triton start` | Full | Shortcut |
| `triton stop` | `triton stop` | Full | Shortcut |
| `triton vlan` | `triton vlan` | Full | |
| `triton volume` | `triton volume` | Full | |
| `triton volumes` | `triton vols` | Full | Different shortcut name |

### 1.2 Instance Subcommands

| Node.js Subcommand | Rust CLI | Status | Notes |
|--------------------|----------|--------|-------|
| `instance audit` | `instance audit` | Full | |
| `instance create` | `instance create` | Full | Option differences (see below) |
| `instance delete` | `instance delete` | Full | |
| `instance disable-deletion-protection` | `instance disable-deletion-protection` | Full | |
| `instance disable-firewall` | `instance disable-firewall` | Full | |
| `instance disk` | `instance disk` | Full | add, delete, get, list, resize |
| `instance disks` | `instance disks` | Full | Alias for `disk` |
| `instance enable-deletion-protection` | `instance enable-deletion-protection` | Full | |
| `instance enable-firewall` | `instance enable-firewall` | Full | |
| `instance fwrule` | - | Missing | Nested in firewall module |
| `instance fwrules` | `instance fwrules` | Full | |
| `instance get` | `instance get` | Full | |
| `instance ip` | `instance ip` | Full | |
| `instance list` | `instance list` | Full | |
| `instance metadata` | `instance metadata` | Full | get, list, set, delete |
| `instance metadatas` | `instance metadatas` | Full | Alias for `metadata` |
| `instance migration` | `instance migration` | Full | All subcommands |
| `instance nic` | `instance nic` | Full | create, delete, get, list |
| `instance reboot` | `instance reboot` | Full | |
| `instance rename` | `instance rename` | Full | |
| `instance resize` | `instance resize` | Full | |
| `instance snapshot` | `instance snapshot` | Full | create, delete, get, list |
| `instance snapshots` | `instance snapshots` | Full | Alias for `snapshot` |
| `instance ssh` | `instance ssh` | Full | Proxy + default user detection |
| `instance start` | `instance start` | Full | |
| `instance stop` | `instance stop` | Full | |
| `instance tag` | `instance tag` | Full | delete, get, list, set |
| `instance tags` | `instance tags` | Full | Alias for `tag` |
| `instance vnc` | `instance vnc` | Full | |
| `instance wait` | `instance wait` | Full | |

### 1.3 Image Subcommands

| Node.js Subcommand | Rust CLI | Status | Notes |
|--------------------|----------|--------|-------|
| `image clone` | `image clone` | Full | |
| `image copy` | `image copy` | Full | |
| `image create` | `image create` | Full | |
| `image delete` | `image delete` | Full | |
| `image export` | `image export` | Full | |
| `image get` | `image get` | Full | |
| `image list` | `image list` | Full | |
| `image share` | `image share` | Full | |
| `image tag` | `image tag` | Full | |
| `image unshare` | `image unshare` | Full | |
| `image update` | `image update` | Full | |
| `image wait` | `image wait` | Full | |

---

## 2. Option Compatibility Matrix

### 2.1 Global Options

| Option | Node.js | Rust | Compatible | Notes |
|--------|---------|------|------------|-------|
| `--help, -h` | Yes | Yes | Yes | |
| `--version` | Yes | `-V` | Yes | Different short form |
| `--verbose, -v` | Yes | Yes | Yes | **Global in Rust** |
| `--profile, -p` | Yes | Yes | Yes | |
| `--account, -a` | Yes | Yes | Yes | |
| `--user, -u` | Yes | - | No | Not implemented |
| `--role, -r` | Yes | - | No | Not implemented |
| `--keyId, -k` | Yes | `--key-id, -k` | Yes | Different long form |
| `--url, -U` | Yes | Yes | Yes | |
| `--insecure, -i` | Yes | - | No | Not implemented |
| `--act-as` | Yes | - | No | Not implemented |
| `--accept-version` | Yes (hidden) | - | No | Developer option |
| `--json, -j` | Per-command | Yes | Yes | **Global in Rust** |

### 2.2 Instance Create Options

| Option | Node.js | Rust | Compatible | Notes |
|--------|---------|------|------------|-------|
| `--name, -n` | Yes | Yes | Yes | |
| `--brand, -b` | Yes | Yes | Yes | |
| `--tag, -t` | Yes | Yes | Yes | |
| `--affinity, -a` | Yes | Yes | Yes | Resolved (globals now top-level only) |
| `--network, -N` | Yes | Yes | Yes | |
| `--nic` | Yes | Yes | Yes | |
| `--delegate-dataset` | Yes | Yes | Yes | |
| `--firewall` | Yes | Yes | Yes | |
| `--deletion-protection` | Yes | Yes | Yes | |
| `--encrypted` | Yes | Yes | Yes | |
| `--volume, -v` | Yes | Yes | Yes | Resolved (globals now top-level only) |
| `--metadata, -m` | Yes | Yes | Yes | |
| `--metadata-file, -M` | Yes | Yes | Yes | |
| `--script` | Yes | Yes | Yes | |
| `--cloud-config` | Yes | Yes | Yes | |
| `--allow-shared-images` | Yes | Yes | Yes | |
| `--disk` | Yes | Yes | Yes | |
| `--dry-run` | Yes | Yes | Yes | |
| `--wait, -w` | Yes | Yes | Yes | |
| `--json, -j` | Yes | Top-level | Partial | Must use `triton -j instance create ...` |

### 2.3 Instance List Options

| Option | Node.js | Rust | Compatible | Notes |
|--------|---------|------|------------|-------|
| `--credentials` | Yes | - | No | Not implemented |
| `-o` (columns) | Yes | Yes | Yes | |
| `-l` (long) | Yes | - | No | Not implemented |
| `-H` (no header) | Yes | - | No | Not implemented |
| `-s` (sort) | Yes | `--sort-by` | Partial | Different syntax |
| `--json, -j` | Yes | Global | Yes | |
| Filters (key=value) | Yes | `--name, --state, --image, --tag` | Partial | Different syntax |

### 2.4 Instance SSH Options

| Option | Node.js | Rust | Compatible | Notes |
|--------|---------|------|------------|-------|
| `USER@INST` | Yes | `--user, -l` | Partial | Different syntax |
| `--no-proxy` | Yes | Yes | Yes | ✅ Implemented |
| SSH arguments | Pass-through | Pass-through | Yes | |
| `-i` (identity) | Pass-through | `--identity, -i` | Yes | Explicit in Rust |
| `-o` (ssh option) | Pass-through | `--ssh-option, -o` | Yes | Explicit in Rust |

### 2.5 RBAC Key-Add Options

| Option | Node.js | Rust | Compatible | Notes |
|--------|---------|------|------------|-------|
| `--key, -k` | Yes | Yes | Yes | Resolved (globals now top-level only) |
| `--name, -n` | Yes | Yes | Yes | |

---

## 3. Categorized Differences

### 3.1 ~~Unavoidable Conflicts~~ RESOLVED

Previous short option conflicts have been **resolved** by making global options top-level only (removing `global = true` from clap arguments). This means:

| Short | Top-level Use | Subcommand Use | Status |
|-------|---------------|----------------|--------|
| `-v` | `--verbose` | `--volume` (create) | **Compatible** |
| `-k` | `--key-id` | `--key` (rbac key-add) | **Compatible** |
| `-a` | `--account` | `--affinity` (create) | **Compatible** |

**Trade-off:** Global options (`-v`, `-j`, `-p`, `-a`, `-k`, `-U`) must now come **before** the subcommand:

```bash
# Correct usage:
triton -v instance list      # verbose mode
triton -j instance get foo   # JSON output

# No longer works:
triton instance list -v      # ERROR: -v not recognized for 'list'
```

This matches the Node.js CLI behavior more closely.

### 3.2 Missing Short Options (Fixable)

These short options could be added to improve compatibility:

| Command | Option | Node.js Short | Rust | Action |
|---------|--------|---------------|------|--------|
| `instance list` | `--long` | `-l` | - | Add `-l` |
| `instance list` | `--no-header` | `-H` | - | Add `-H` |
| `instance list` | `--sort` | `-s` | `--sort-by` | Add `-s` alias |

### 3.3 Missing Commands (Implementation Required)

| Command | Priority | Effort | Notes |
|---------|----------|--------|-------|
| `triton ip` | High | Low | Shortcut for `instance ip` |
| `triton cloudapi` | Medium | Medium | Raw API access |
| `triton profiles` | Low | Low | Alias for `profile list` |
| `triton badger` | Low | Low | Easter egg |

### 3.4 Missing Subcommand Aliases

| Node.js Alias | For | Priority |
|---------------|-----|----------|
| `instance disks` | `instance disk list` | Low |
| `instance metadatas` | `instance metadata list` | Low |
| `instance snapshots` | `instance snapshot list` | Low |
| `instance tags` | `instance tag list` | Low |

### 3.5 Behavioral Differences

| Feature | Node.js | Rust | Impact |
|---------|---------|------|--------|
| SSH proxy support | Via tags (`tritoncli.ssh.proxy`) | ✅ Implemented | - |
| SSH default user | Auto-detected from image | ✅ Implemented | - |
| SSH ControlMaster disable | Automatic | Not implemented | Low |
| List filter syntax | `key=value` positional | `--key value` flags | Medium |
| Column output | `-o col1,col2` | `--output col1,col2` | Low |

### 3.6 Extra Options in Rust (Not in Node.js)

| Command | Option | Notes |
|---------|--------|-------|
| `instance create` | `--wait-timeout` | Configurable wait timeout |
| `instance list` | `--limit` | API-level pagination |
| `instance list` | `--short` | Print only short IDs |
| `image wait` | `--timeout` | Configurable timeout |

---

## 4. Actionable Fixes

### 4.1 High Priority (Script Compatibility)

1. **Add `triton ip` shortcut**
   - File: `cli/triton-cli/src/main.rs`
   - Add: `Ip(commands::instance::get::IpArgs)` to `Commands` enum
   - Effort: 10 minutes

2. **Add `-l` for long output in instance list**
   - File: `cli/triton-cli/src/commands/instance/list.rs`
   - Add: `#[arg(short = 'l', long)]` for a `long: bool` field
   - Effort: 30 minutes

3. **Add `-H` for no-header in instance list**
   - File: `cli/triton-cli/src/commands/instance/list.rs`
   - Add: `#[arg(short = 'H', long = "no-header")]` field
   - Effort: 20 minutes

### 4.2 Medium Priority (User Experience)

4. **Implement SSH proxy support**
   - File: `cli/triton-cli/src/commands/instance/ssh.rs`
   - Add: Tag lookup for `tritoncli.ssh.proxy`, `tritoncli.ssh.ip`, `tritoncli.ssh.port`
   - Effort: 2-3 hours

5. **Auto-detect SSH user from image**
   - File: `cli/triton-cli/src/commands/instance/ssh.rs`
   - Add: Image lookup, check `default_user` tag
   - Effort: 1 hour

6. **Support positional filter syntax in list commands**
   - Would be a breaking change to existing Rust CLI syntax
   - Consider: Add as alternative, not replacement
   - Effort: 2+ hours

### 4.3 Low Priority (Completeness)

7. **Add `cloudapi` raw API command**
   - New module: `cli/triton-cli/src/commands/cloudapi.rs`
   - Effort: 2-3 hours

8. **Add plural aliases (`disks`, `snapshots`, etc.)**
   - File: Various module files
   - Add: `#[command(alias = "disks")]` to `Disk` subcommand
   - Effort: 30 minutes total

---

## 5. Fundamental Limitations

These differences **cannot be resolved** due to architectural constraints:

1. **Global short option conflicts** - Clap propagates global arguments to all subcommands. Short options like `-v`, `-k`, `-a` used globally cannot be reused.

2. **Argument position flexibility** - Node.js allows `triton -v instance create`, Rust requires arguments after the subcommand for subcommand-specific options.

3. **Different CLI frameworks** - `cmdln` (Node.js) vs `clap` (Rust) have fundamentally different parsing models.

---

## 6. Recommendations

### For Script Migration

1. **Use long options** - Replace `-v` with `--volume`, `-k` with `--key`, etc.
2. **Update filter syntax** - Replace `triton ls state=running` with `triton ls --state running`
3. **Add wrappers** - Create shell aliases for missing shortcuts

### For Rust CLI Development

1. **Document differences prominently** - Add migration guide to help text
2. **Prioritize fixes by usage** - `instance create` and `instance list` options first
3. **Consider compatibility flags** - `--compat` mode that accepts old syntax

### Example Migration

```bash
# Node.js CLI
triton instance create -v myvolume:/mnt -n myvm image package

# Rust CLI
triton instance create --volume myvolume:/mnt --name myvm image package
```

---

## Appendix A: Command Tree Comparison

### Node.js CLI Command Tree

```
triton
├── account (get, update, limits)
├── badger
├── changefeed
├── cloudapi
├── completion
├── create (shortcut)
├── datacenters
├── delete (shortcut)
├── env
├── fwrule (create, delete, disable, enable, get, instances, list, update)
├── fwrules (shortcut)
├── image (clone, copy, create, delete, export, get, list, share, tag, unshare, update, wait)
├── images (shortcut)
├── info
├── instance
│   ├── audit, create, delete, get, list, rename, resize
│   ├── start, stop, reboot
│   ├── ssh, vnc, ip, wait
│   ├── enable-firewall, disable-firewall, fwrules
│   ├── enable-deletion-protection, disable-deletion-protection
│   ├── disk (add, delete, get, list, resize)
│   ├── metadata (delete, get, list, set)
│   ├── migration (abort, automatic, begin, finalize, get, list, pause, switch, sync)
│   ├── nic (create, delete, get, list)
│   ├── snapshot (create, delete, get, list)
│   └── tag (delete, get, list, set)
├── instances (shortcut)
├── ip (shortcut)
├── key (add, delete, get, list)
├── keys (shortcut)
├── network (create, delete, get, ip, list)
├── networks (shortcut)
├── package (get, list)
├── packages (shortcut)
├── profile (create, delete, edit, get, list, set-current)
├── profiles (shortcut)
├── rbac (info, key, keys, policy, role, roles, user, users)
├── reboot (shortcut)
├── services
├── ssh (shortcut)
├── start (shortcut)
├── stop (shortcut)
├── vlan (create, delete, get, list, networks, update)
└── volume (create, delete, get, list)
```

### Rust CLI Command Tree

```
triton
├── account (get, update, limits)
├── changefeed
├── completion
├── create (shortcut)
├── datacenters
├── delete (shortcut)
├── env
├── fwrule (create, delete, disable, enable, get, instances, list, update)
├── fwrules (shortcut)
├── image (clone, copy, create, delete, export, get, list, share, tag, unshare, update, wait)
├── imgs (shortcut)
├── info
├── instance
│   ├── audit, create, delete, get, list, rename, resize
│   ├── start, stop, reboot
│   ├── ssh, vnc, ip, wait
│   ├── enable-firewall, disable-firewall, fwrules
│   ├── enable-deletion-protection, disable-deletion-protection
│   ├── disk (add, delete, get, list, resize)
│   ├── metadata (delete, get, list, set)
│   ├── migration (abort, automatic, begin, finalize, get, list, pause, switch, sync)
│   ├── nic (create, delete, get, list)
│   ├── snapshot (create, delete, get, list)
│   └── tag (delete, get, list, set)
├── insts (shortcut)
├── key (add, delete, get, list)
├── keys (shortcut)
├── network (get, list)
├── nets (shortcut)
├── package (get, list)
├── pkgs (shortcut)
├── profile (create, delete, edit, get, list, set-current)
├── rbac (info, key, key-add, key-delete, keys, policy, role, user)
├── reboot (shortcut)
├── services
├── ssh (shortcut)
├── start (shortcut)
├── stop (shortcut)
├── vlan (create, delete, get, list, networks, update)
├── vlans (shortcut)
├── volume (create, delete, get, list)
└── vols (shortcut)
```

---

## Appendix B: Testing Commands

To verify compatibility, test these common workflows:

```bash
# List instances
node-triton: triton ls state=running
rust-triton: triton ls --state running

# Create instance with volume (short option conflict)
node-triton: triton create -v myvol:/mnt -n test image pkg
rust-triton: triton create --volume myvol:/mnt -n test image pkg

# SSH with user
node-triton: triton ssh root@myvm
rust-triton: triton ssh myvm -l root

# Add RBAC key (short option conflict)
node-triton: triton rbac key-add user -k @~/.ssh/id_rsa.pub -n mykey
rust-triton: triton rbac key-add user --key @~/.ssh/id_rsa.pub -n mykey

# JSON output (global in Rust)
node-triton: triton instance list -j
rust-triton: triton -j instance list  # OR: triton instance list -j
```
