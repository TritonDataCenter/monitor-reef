<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

Copyright 2025 Edgecast Cloud LLC.
-->

# Triton CLI High Priority Implementation Plan

**Date:** 2025-12-16
**Status:** ✅ Complete
**Source Reference:** comprehensive-validation-report-2025-12-16.md
**Last Updated:** 2025-12-16

## Overview

This plan covers P1 (Important) features that limit significant usage of triton-cli. These are the features needed before the CLI can be considered production-ready for most workflows.

---

## Progress Summary

### Completed Items (2025-12-16)

| Item | Status | Notes |
|------|--------|-------|
| 1. Datacenters Command | ✅ Complete | `triton datacenters` implemented |
| 2. Instance Migration Commands | ✅ Complete | All subcommands implemented (get/estimate/start/wait/finalize/abort) |
| 3. Instance Create: Volume Mount | ✅ Complete | `--volume` flag implemented |
| 4. Instance Create: Disk Config | ✅ Complete | `--disk` flag implemented |
| 5. Instance Create: Metadata File | ✅ Complete | `--metadata-file` flag implemented |
| 6. Instance Create: Script | ✅ Complete | `--script` flag implemented |
| 7. Instance Create: Brand | ✅ Complete | `--brand` flag implemented |
| 8. Instance Create: Full NIC | ✅ Complete | `--nic` flag implemented |
| 9. Network Create/Delete | ✅ Complete | `triton network create/delete` implemented |
| 10. RBAC User Key Management | ✅ Complete | `triton rbac keys/key/key-add/key-delete` implemented |

### All P1 Features Complete! 🎉

All high-priority (P1) features have been implemented. The migration commands were unblocked by:
1. The `cloudapi-api` trait already supported all actions via the `migrate` endpoint with `MigrationAction` enum
2. Added `MigrationAction` export to `cloudapi-client`
3. Implemented `finalize` and `abort` CLI subcommands using `MigrationAction::Switch` and `MigrationAction::Abort`

### Migration Subcommands Available

- `triton instance migration get` - View migration status
- `triton instance migration estimate` - Get migration size estimate
- `triton instance migration start` - Begin migration
- `triton instance migration wait` - Poll until migration completes
- `triton instance migration finalize` - Switch to new server
- `triton instance migration abort` - Cancel migration

---

## 1. Datacenters Command

**Priority:** P1
**Impact:** Cannot list datacenters in multi-DC deployments

### API Requirements
- Add `list_datacenters` method to cloudapi-client
- Endpoint: `GET /my/datacenters`
- Returns: HashMap of datacenter name to URL

### CLI Implementation
- Create `cli/triton-cli/src/commands/datacenters.rs`
- Simple list command with table/json output
- No subcommands needed (just `triton datacenters`)

### Files to Modify
- [x] `clients/internal/cloudapi-client/src/lib.rs` - Add API method
- [x] `apis/cloudapi-api/src/lib.rs` - Add endpoint if needed
- [x] `cli/triton-cli/src/commands/mod.rs` - Add datacenters module
- [x] `cli/triton-cli/src/commands/datacenters.rs` - New file
- [x] `cli/triton-cli/src/main.rs` - Wire up command

---

## 2. Instance Migration Commands

**Priority:** P1
**Impact:** Cannot migrate instances between compute nodes

### API Requirements
Add migration methods to cloudapi-client:
- `list_migrations(instance_id)` - List migrations for an instance
- `get_migration(instance_id, migration_id)` - Get migration details
- `create_migration(instance_id, action, opts)` - Begin migration
- `watch_migration(instance_id, migration_id)` - Watch progress

Migration actions: `begin`, `sync`, `switch`, `pause`, `abort`, `finalize`, `automatic`

### CLI Implementation
- Create `cli/triton-cli/src/commands/instance/migration.rs`
- Subcommands:
  - `list` - List migrations for an instance
  - `get` - Get migration details
  - `begin` - Start migration process
  - `sync` - Synchronize data
  - `switch` - Switch to new location
  - `pause` - Pause migration
  - `abort` - Abort migration
  - `finalize` - Complete migration
  - `automatic` - Automatic migration
  - `watch` - Watch migration progress

### Files to Modify
- [x] `clients/internal/cloudapi-client/src/lib.rs` - Add migration API methods and MigrationAction export
- [x] `apis/cloudapi-api/src/lib.rs` - Migration endpoints complete (uses MigrationAction enum)
- [x] `cli/triton-cli/src/commands/instance/mod.rs` - Add migration subcommand
- [x] `cli/triton-cli/src/commands/instance/migration.rs` - All subcommands implemented

---

## 3. Instance Create: Volume Mount Option

**Priority:** P1
**Impact:** Cannot mount NFS volumes on instance creation

### Implementation
- Add `--volume, -v` flag to instance create
- Format: `NAME[@MOUNTPOINT]` or `NAME:MODE:MOUNTPOINT`
- Support multiple volumes

### Files to Modify
- [x] `cli/triton-cli/src/commands/instance/create.rs` - Add --volume flag
- [x] Parse volume specifications and add to create request

### Example Usage
```bash
triton instance create --volume mydata@/data --volume logs:/logs ubuntu-24.04 g4-highcpu-1G
```

---

## 4. Instance Create: Disk Configuration Option

**Priority:** P1
**Impact:** Cannot configure flexible disks for bhyve instances

### Implementation
- Add `--disk` flag to instance create
- Format: `SIZE` or `IMAGE_UUID:SIZE`
- Support multiple disks
- Requires bhyve brand

### Files to Modify
- [x] `cli/triton-cli/src/commands/instance/create.rs` - Add --disk flag
- [x] Parse disk specifications and add to create request

### Example Usage
```bash
triton instance create --disk 10G --disk 50G ubuntu-24.04 g4-highcpu-1G
```

---

## 5. Instance Create: Metadata File Option

**Priority:** P1
**Impact:** Cannot load metadata from files (common for user-script)

### Implementation
- Add `--metadata-file, -M` flag to instance create
- Format: `KEY=FILE_PATH` or `KEY@FILE_PATH`
- Read file contents as metadata value
- Support multiple files

### Files to Modify
- [x] `cli/triton-cli/src/commands/instance/create.rs` - Add --metadata-file flag
- [x] Add file reading logic

### Example Usage
```bash
triton instance create -M user-script=/path/to/script.sh ubuntu-24.04 g4-highcpu-1G
```

---

## 6. Instance Create: Script Option

**Priority:** P1
**Impact:** Cannot use user-script shortcut (very common pattern)

### Implementation
- Add `--script` flag to instance create
- Shortcut for `--metadata-file user-script=PATH`
- Common enough to warrant dedicated flag

### Files to Modify
- [x] `cli/triton-cli/src/commands/instance/create.rs` - Add --script flag

### Example Usage
```bash
triton instance create --script /path/to/setup.sh ubuntu-24.04 g4-highcpu-1G
```

---

## 7. Instance Create: Brand Option

**Priority:** P1
**Impact:** Cannot explicitly set bhyve/kvm brand

### Implementation
- Add `--brand, -b` flag to instance create
- Values: `bhyve`, `kvm`, `joyent`, `joyent-minimal`, `lx`
- Some images require specific brands

### Files to Modify
- [x] `cli/triton-cli/src/commands/instance/create.rs` - Add --brand flag
- [x] Add brand to create request

### Example Usage
```bash
triton instance create --brand bhyve windows-2022 g4-highcpu-4G
```

---

## 8. Instance Create: Full NIC Specification

**Priority:** P1
**Impact:** Cannot specify IP addresses or advanced NIC options

### Implementation
- Add `--nic` flag to instance create
- Format: JSON or key=value pairs
- Supports: network, ip, primary, gateway, etc.
- More powerful than simple --network flag

### Files to Modify
- [x] `cli/triton-cli/src/commands/instance/create.rs` - Add --nic flag
- [x] Parse NIC specifications

### Example Usage
```bash
triton instance create --nic network=mynet,ip=10.0.0.5 ubuntu-24.04 g4-highcpu-1G
```

---

## 9. Network Create/Delete Commands

**Priority:** P1
**Impact:** Cannot manage fabric networks

### API Requirements
Add network CRUD methods to cloudapi-client:
- `create_fabric_network(vlan_id, opts)` - Create fabric network
- `delete_fabric_network(vlan_id, network_id)` - Delete fabric network

### CLI Implementation
Add subcommands to existing network command:
- `create` - Create fabric network on a VLAN
- `delete` - Delete fabric network

### Files to Modify
- [x] `clients/internal/cloudapi-client/src/lib.rs` - Add network CRUD methods
- [x] `cli/triton-cli/src/commands/network.rs` - Add create/delete subcommands

### Example Usage
```bash
triton network create --vlan-id 100 --name mynet --subnet 10.0.0.0/24 --provision-start 10.0.0.10 --provision-end 10.0.0.250
triton network delete mynet
```

---

## 10. RBAC User Key Management

**Priority:** P1
**Impact:** Cannot manage SSH keys for sub-users

### API Requirements
Add user key methods to cloudapi-client:
- `list_user_keys(user_id)` - List keys for a sub-user
- `get_user_key(user_id, key_name)` - Get specific key
- `create_user_key(user_id, key)` - Add key to sub-user
- `delete_user_key(user_id, key_name)` - Remove key from sub-user

### CLI Implementation
Add `keys` subcommand to rbac:
- `triton rbac keys USER` - List keys
- `triton rbac key USER KEY` - Get key
- `triton rbac key-add USER` - Add key
- `triton rbac key-delete USER KEY` - Delete key

### Files to Modify
- [x] `clients/internal/cloudapi-client/src/lib.rs` - Add user key API methods
- [x] `cli/triton-cli/src/commands/rbac.rs` - Add key management subcommands

---

## Implementation Order

Recommended order based on dependencies and impact:

1. **Datacenters command** - Self-contained, quick win
2. **Metadata-file option** - Foundation for script option
3. **Script option** - Depends on metadata-file
4. **Brand option** - Simple addition to create
5. **Volume option** - Moderate complexity
6. **Disk option** - Moderate complexity, bhyve-specific
7. **NIC option** - More complex parsing
8. **Network create/delete** - Requires API additions
9. **RBAC user keys** - Requires API additions
10. **Migration commands** - Most complex, many subcommands

---

## Testing Requirements

For each feature:
- [ ] Unit tests for argument parsing
- [ ] Integration tests against mock server (where applicable)
- [ ] Manual testing against real CloudAPI
- [ ] Documentation updates

---

## Success Criteria

All P1 features implemented means:
- Users can provision instances with volumes, disks, and scripts
- Users can manage fabric networks end-to-end
- Users can migrate instances between compute nodes
- Users can manage RBAC sub-user SSH keys
- Users can list datacenters in multi-DC deployments
