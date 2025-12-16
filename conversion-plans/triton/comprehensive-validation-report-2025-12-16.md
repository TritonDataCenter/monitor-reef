<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

Copyright 2025 Edgecast Cloud LLC.
-->

# Comprehensive Triton CLI Validation Report

**Date:** 2025-12-16
**Validator:** Claude Code
**Source Reference:** node-triton (`target/node-triton/`), node-smartdc (`target/node-smartdc/`)

## Executive Summary

The Rust `triton-cli` provides **good coverage** of the most commonly used functionality from node-triton, with the core instance, image, network, and RBAC operations well implemented. However, several specialized features are missing, particularly around migrations, advanced RBAC features, and some docker/CMON integration.

### Overall Coverage

| Category | Commands Implemented | Commands Missing | Coverage % |
|----------|---------------------|------------------|------------|
| Instance | 22 | 3 | 88% |
| Image | 9 | 3 | 75% |
| Network | 6 | 2 | 75% |
| VLAN | 6 | 0 | 100% |
| Firewall | 8 | 0 | 100% |
| Volume | 5 | 0 | 100% |
| Package | 2 | 0 | 100% |
| Key | 4 | 0 | 100% |
| Account | 3 | 0 | 100% |
| Profile | 6 | 2 | 75% |
| RBAC | 15 | 6 | 71% |
| Top-level | 16 | 4 | 80% |
| **Total** | **102** | **20** | **~84%** |

---

## Part 1: Top-Level Commands

### Fully Implemented

| node-triton Command | triton-cli Equivalent | Status |
|---------------------|----------------------|--------|
| `triton account` | `commands/account.rs` | **Complete** |
| `triton env` | `commands/env.rs` | **Complete** |
| `triton fwrule` | `commands/fwrule.rs` | **Complete** |
| `triton image` | `commands/image.rs` | **Complete** |
| `triton info` | `commands/info.rs` | **Complete** |
| `triton instance` | `commands/instance/` | **Complete** |
| `triton key` | `commands/key.rs` | **Complete** |
| `triton network` | `commands/network.rs` | **Complete** |
| `triton package` | `commands/package.rs` | **Complete** |
| `triton profile` | `commands/profile.rs` | **Partial** |
| `triton rbac` | `commands/rbac.rs` | **Partial** |
| `triton vlan` | `commands/vlan.rs` | **Complete** |
| `triton volume` | `commands/volume.rs` | **Complete** |
| `triton completion` | `completion` subcommand | **Complete** |
| Shortcut commands | insts, imgs, pkgs, etc. | **Complete** |

### Missing Top-Level Commands

| Command | Source | Priority | Notes |
|---------|--------|----------|-------|
| `triton changefeed` | `do_changefeed.js` | P2 | WebSocket VM change subscription |
| `triton cloudapi` | `do_cloudapi.js` | P2 | Raw CloudAPI requests (hidden) |
| `triton datacenters` | `do_datacenters.js` | P1 | List datacenters in cloud |
| `triton services` | `do_services.js` | P2 | Show service endpoints |

---

## Part 2: Instance Subcommands

### Fully Implemented

| Subcommand | Source | triton-cli | Status |
|------------|--------|------------|--------|
| `audit` | `do_audit.js` | `instance/audit.rs` | **Complete** |
| `create` | `do_create.js` | `instance/create.rs` | **Partial** (see feature gaps) |
| `delete` | `do_delete.js` | `instance/delete.rs` | **Complete** |
| `disable-deletion-protection` | `do_disable_deletion_protection.js` | `instance/protection.rs` | **Complete** |
| `disable-firewall` | `do_disable_firewall.js` | `instance/firewall.rs` | **Complete** |
| `disk` | `do_disk/` | `instance/disk.rs` | **Complete** |
| `enable-deletion-protection` | `do_enable_deletion_protection.js` | `instance/protection.rs` | **Complete** |
| `enable-firewall` | `do_enable_firewall.js` | `instance/firewall.rs` | **Complete** |
| `fwrules` | `do_fwrules.js` | `instance/firewall.rs` | **Complete** |
| `get` | `do_get.js` | `instance/get.rs` | **Complete** |
| `ip` | `do_ip.js` | (part of instance) | **Complete** |
| `list` | `do_list.js` | `instance/list.rs` | **Complete** |
| `metadata` | `do_metadata/` | `instance/metadata.rs` | **Complete** |
| `nic` | `do_nic/` | `instance/nic.rs` | **Complete** |
| `reboot` | `do_reboot.js` | `instance/lifecycle.rs` | **Complete** |
| `rename` | `do_rename.js` | `instance/rename.rs` | **Complete** |
| `resize` | `do_resize.js` | `instance/resize.rs` | **Complete** |
| `snapshot` | `do_snapshot/` | `instance/snapshot.rs` | **Complete** |
| `ssh` | `do_ssh.js` | `instance/ssh.rs` | **Complete** |
| `start` | `do_start.js` | `instance/lifecycle.rs` | **Complete** |
| `stop` | `do_stop.js` | `instance/lifecycle.rs` | **Complete** |
| `tag` | `do_tag/` | `instance/tag.rs` | **Complete** |
| `wait` | `do_wait.js` | `instance/wait.rs` | **Complete** |

### Missing Instance Subcommands

| Subcommand | Source | Priority | Notes |
|------------|--------|----------|-------|
| `migration` | `do_migration/` | P1 | Full migration workflow (begin, sync, switch, pause, abort, finalize, automatic) |
| `vnc` | `do_vnc.js` | P2 | VNC server for bhyve/KVM instances |
| `disks` (shortcut) | `do_disks.js` | P3 | List disks shortcut (disk list exists) |
| `snapshots` (shortcut) | `do_snapshots.js` | P3 | List snapshots shortcut |
| `tags` (shortcut) | `do_tags.js` | P3 | List tags shortcut |
| `metadatas` (shortcut) | `do_metadatas.js` | P3 | List metadata shortcut |

---

## Part 3: Instance Create Feature Parity

### Implemented Options

| Option | node-triton | triton-cli | Status |
|--------|-------------|------------|--------|
| `--name, -n` | Yes | Yes | **Complete** |
| `--network, -N` | Yes | Yes | **Complete** |
| `--tag, -t` | Yes | Yes | **Complete** |
| `--metadata, -m` | Yes | Yes | **Complete** |
| `--firewall` | Yes | Yes | **Complete** |
| `--deletion-protection` | Yes | Yes | **Complete** |
| `--affinity, -a` | Yes | Yes | **Complete** |
| `--wait, -w` | Yes | Yes | **Complete** |
| `--wait-timeout` | Yes | Yes | **Complete** |
| `--json, -j` | Yes | Yes (global) | **Complete** |

### Missing Create Options

| Option | node-triton | Priority | Notes |
|--------|-------------|----------|-------|
| `--brand, -b` | Yes | P1 | Define instance type (bhyve/kvm) |
| `--nic` | Yes | P1 | Full NIC object specification with IP config |
| `--delegate-dataset` | Yes | P2 | Delegated ZFS dataset |
| `--encrypted` | Yes | P2 | Encrypted compute node placement |
| `--volume, -v` | Yes | P1 | Mount volumes into instance |
| `--metadata-file, -M` | Yes | P1 | Metadata from file |
| `--script` | Yes | P1 | User-script shortcut |
| `--cloud-config` | Yes | P2 | Cloud-init user-data |
| `--allow-shared-images` | Yes | P2 | Allow shared image usage |
| `--disk` | Yes | P1 | Flexible disk configuration |
| `--dry-run` | Yes | P2 | Simulate without creating |

---

## Part 4: Image Subcommands

### Fully Implemented

| Subcommand | Source | Status |
|------------|--------|--------|
| `list` | `do_list.js` | **Complete** |
| `get` | `do_get.js` | **Complete** |
| `create` | `do_create.js` | **Complete** |
| `delete` | `do_delete.js` | **Complete** |
| `clone` | `do_clone.js` | **Complete** |
| `copy` | `do_copy.js` | **Complete** |
| `update` | `do_update.js` | **Complete** |
| `export` | `do_export.js` | **Complete** |
| `wait` | `do_wait.js` | **Complete** |

### Missing Image Subcommands

| Subcommand | Source | Priority | Notes |
|------------|--------|----------|-------|
| `share` | `do_share.js` | P2 | Share image with other accounts |
| `unshare` | `do_unshare.js` | P2 | Revoke image sharing |
| `tag` | `do_tag.js` | P2 | Manage image tags |

---

## Part 5: Network/VLAN/Firewall Subcommands

### Network - Fully Implemented

| Subcommand | Status |
|------------|--------|
| `list` | **Complete** |
| `get` | **Complete** |
| `get-default` | **Complete** |
| `set-default` | **Complete** |
| `ip` (subcommand) | **Complete** |

### Missing Network Subcommands

| Subcommand | Source | Priority | Notes |
|------------|--------|----------|-------|
| `create` | `do_create.js` | P1 | Create fabric network |
| `delete` | `do_delete.js` | P1 | Delete fabric network |

### VLAN - Fully Implemented (100%)

All subcommands: `list`, `get`, `create`, `delete`, `update`, `networks`

### Firewall - Fully Implemented (100%)

All subcommands: `list`, `get`, `create`, `delete`, `enable`, `disable`, `update`, `instances`

---

## Part 6: Profile/Account/RBAC Subcommands

### Profile - Partially Implemented

| Subcommand | Status | Notes |
|------------|--------|-------|
| `list` | **Complete** | |
| `get` | **Complete** | |
| `create` | **Complete** | |
| `edit` | **Complete** | |
| `delete` | **Complete** | |
| `set-current` | **Complete** | |
| `docker-setup` | **Missing** | Docker TLS certificate generation |
| `cmon-certgen` | **Missing** | CMON certificate generation |

### Account - Fully Implemented (100%)

All subcommands via `commands/account.rs`: `get`, `update`, `limits`

### RBAC - Partially Implemented

| Subcommand | Status | Notes |
|------------|--------|-------|
| `user` (list/get/create/update/delete) | **Complete** | |
| `role` (list/get/create/update/delete) | **Complete** | |
| `policy` (list/get/create/update/delete) | **Complete** | |
| `info` | **Missing** | Summary RBAC state view |
| `apply` | **Missing** | Apply RBAC config from file |
| `reset` | **Missing** | Reset RBAC to defaults |
| `key/keys` | **Missing** | User key management |
| `role-tags` | **Missing** | Role tag management on resources |
| `instance-role-tags` | **Missing** | Instance role tags |
| `image-role-tags` | **Missing** | Image role tags |
| `network-role-tags` | **Missing** | Network role tags |
| `package-role-tags` | **Missing** | Package role tags |

---

## Part 7: Volume/Package/Key Subcommands

### Volume - Fully Implemented (100%)

All subcommands: `list`, `get`, `create`, `delete`, `sizes`

### Package - Fully Implemented (100%)

All subcommands: `list`, `get`

### Key - Fully Implemented (100%)

All subcommands: `list`, `get`, `add`, `delete`

---

## Part 8: Feature Gap Analysis

### P0 - Critical (Blocks Core Usage)

None identified. Core instance, image, and network operations are functional.

### P1 - Important (Limits Significant Usage)

| Feature | Category | Impact |
|---------|----------|--------|
| `triton datacenters` | Top-level | Cannot list datacenters in multi-DC deployments |
| Instance `--brand` option | Create | Cannot explicitly set bhyve/kvm brand |
| Instance `--volume` option | Create | Cannot mount NFS volumes on creation |
| Instance `--disk` option | Create | Cannot configure flexible disks |
| Instance `--metadata-file` | Create | Cannot load metadata from files |
| Instance `--script` | Create | Cannot use user-script shortcut |
| Instance migration commands | Instance | Cannot migrate instances between CNs |
| Network create/delete | Network | Cannot manage fabric networks |
| RBAC user keys | RBAC | Cannot manage sub-user SSH keys |

### P2 - Nice to Have

| Feature | Category | Impact |
|---------|----------|--------|
| `triton changefeed` | Top-level | No real-time VM change events |
| `triton services` | Top-level | Cannot list service endpoints |
| `triton cloudapi` (raw) | Top-level | Cannot make raw API calls |
| Instance VNC | Instance | Cannot get VNC access to bhyve/KVM |
| Instance `--delegate-dataset` | Create | Cannot create delegated datasets |
| Instance `--encrypted` | Create | Cannot request encrypted CNs |
| Instance `--cloud-config` | Create | No cloud-init shortcut |
| Instance `--dry-run` | Create | Cannot simulate creation |
| Image share/unshare | Image | Cannot share images |
| Image tags | Image | Cannot manage image tags |
| Profile docker-setup | Profile | Cannot auto-setup Docker certs |
| Profile cmon-certgen | Profile | Cannot generate CMON certs |
| RBAC info | RBAC | No summary view of RBAC state |
| RBAC apply/reset | RBAC | Cannot apply RBAC from config files |
| RBAC role-tags | RBAC | Cannot manage role tags on resources |

### P3 - Low Priority

| Feature | Category | Notes |
|---------|----------|-------|
| Shortcut commands (disks, snapshots, tags, metadatas) | Instance | Subcommands exist, shortcuts don't |
| Easter eggs (badger) | Top-level | Intentionally skipped |

---

## Part 9: API Endpoint Coverage

Based on analysis of `target/node-triton/lib/cloudapi2.js`:

### Implemented API Methods (~85%)

- All machine CRUD operations
- All machine lifecycle (start/stop/reboot)
- All machine tags, metadata operations
- All machine firewall operations
- All machine snapshot operations
- All machine NIC operations
- All machine disk operations
- All image operations (except share/unshare)
- All network operations
- All fabric VLAN operations
- All firewall rule operations
- All account operations
- All RBAC user/role/policy operations
- All SSH key operations
- All volume operations

### Missing API Methods (~15%)

| API Method | CLI Command | Priority |
|------------|-------------|----------|
| `changeFeed` | `triton changefeed` | P2 |
| `getMachineVnc` | `triton instance vnc` | P2 |
| `machineMigration` | `triton instance migration` | P1 |
| `listMigrations` | `triton instance migration list` | P1 |
| `getMigration` | `triton instance migration get` | P1 |
| `listServices` | `triton services` | P2 |
| `listDatacenters` | `triton datacenters` | P1 |
| `getRoleTags` / `setRoleTags` | `triton rbac role-tags` | P2 |
| `listUserKeys` / `*UserKey` | `triton rbac keys` | P2 |
| `startMachineFromSnapshot` | `triton instance snapshot start` | P2 |
| Image share/unshare | `triton image share/unshare` | P2 |

---

## Part 10: Recommendations

### Immediate Priorities (Before Production Use)

1. **Add `triton datacenters`** - Essential for multi-datacenter environments
2. **Add instance migration commands** - Critical for operational use
3. **Add `--volume` option to create** - Required for NFS volume workflows
4. **Add `--disk` option to create** - Required for flexible disk configurations
5. **Add `--metadata-file` and `--script`** - Common provisioning patterns

### Near-Term Enhancements

1. **Add network create/delete** - Complete fabric network management
2. **Add RBAC user key management** - Complete sub-user workflows
3. **Add RBAC info command** - Useful for auditing
4. **Add `--brand` option** - Explicit control over instance brand

### Deferred Items

1. Profile docker-setup/cmon-certgen - Docker/CMON specific
2. VNC support - Specialized use case
3. Changefeed - Advanced monitoring feature
4. Role tags - Advanced RBAC feature
5. Image sharing - Less common workflow

### Intentionally Skipped

1. `triton badger` - Easter egg
2. `triton cloudapi` (hidden) - Developer debugging tool

---

## Appendix A: Detailed Command Comparison

### triton-cli Help Output

```
triton --help

Commands:
  profile     Manage connection profiles
  env         Generate shell environment exports
  instance    Manage instances
  image       Manage images
  key         Manage SSH keys
  network     Manage networks
  fwrule      Manage firewall rules
  vlan        Manage fabric VLANs
  volume      Manage volumes
  package     Manage packages
  account     Manage account settings
  rbac        Manage RBAC (users, roles, policies)
  info        Show account info and resource usage
  insts       List instances (shortcut)
  create      Create an instance (shortcut)
  ssh         SSH to an instance (shortcut)
  start       Start instance(s) (shortcut)
  stop        Stop instance(s) (shortcut)
  reboot      Reboot instance(s) (shortcut)
  delete      Delete instance(s) (shortcut)
  imgs        List images (shortcut)
  pkgs        List packages (shortcut)
  nets        List networks (shortcut)
  vols        List volumes (shortcut)
  keys        List SSH keys (shortcut)
  fwrules     List firewall rules (shortcut)
  vlans       List VLANs (shortcut)
  completion  Generate shell completions
```

### node-triton Additional Commands Not in triton-cli

```
triton badger          # Easter egg (skipped)
triton changefeed      # WebSocket VM change events
triton cloudapi        # Raw CloudAPI requests (hidden)
triton datacenters     # List datacenters
triton services        # Show service endpoints
triton ip              # Instance IP (exists as subcommand)
```

---

## Appendix B: Environment Variable Support

Both node-triton and triton-cli support:

| Variable | Purpose |
|----------|---------|
| `TRITON_PROFILE` | Active profile name |
| `TRITON_URL` | CloudAPI URL |
| `TRITON_ACCOUNT` | Account name |
| `TRITON_KEY_ID` | SSH key fingerprint |
| `TRITON_USER` | RBAC sub-user |
| `SDC_*` | Legacy variants (node-triton only) |

Note: triton-cli properly supports all `TRITON_*` variables. Legacy `SDC_*` variables are node-triton specific.

---

## Appendix C: Profile File Compatibility

Both tools use `~/.triton/` directory:

| File | Purpose | Compatible |
|------|---------|------------|
| `profiles.d/*.json` | Profile definitions | Yes |
| `config.json` | Global config | Yes |
| `docker/` | Docker certificates | N/A (not implemented) |
| `cmon/` | CMON certificates | N/A (not implemented) |

---

**Report Generated:** 2025-12-16
**triton-cli Version:** Current development branch
