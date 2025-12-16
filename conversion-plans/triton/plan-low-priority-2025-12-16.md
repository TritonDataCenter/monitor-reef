<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

Copyright 2025 Edgecast Cloud LLC.
-->

# Triton CLI Lower Priority Implementation Plan

**Date:** 2025-12-16
**Status:** In Progress
**Source Reference:** comprehensive-validation-report-2025-12-16.md

## Overview

This plan covers P2 (Nice to Have) and P3 (Low Priority) features. These are not blocking production use but would improve feature completeness.

---

## P2 Features - Nice to Have

### 1. Instance VNC Command ✅ COMPLETED

**Priority:** P2
**Impact:** Cannot get VNC access to bhyve/KVM instances

#### Implementation
- Add `triton instance vnc INSTANCE` command
- Returns VNC WebSocket URL for noVNC clients
- Works with bhyve/KVM instances

#### Files Modified
- [x] `cli/triton-cli/src/commands/instance/vnc.rs` - New file
- [x] `cli/triton-cli/src/commands/instance/mod.rs` - Wire up command

---

### 2. Instance Create: Additional Options ✅ COMPLETED

**Priority:** P2

#### 2a. Delegate Dataset Option ✅
- Added `--delegate-dataset` flag to instance create
- Creates delegated ZFS dataset in zone
- Zone-specific feature

#### 2b. Encrypted Option ✅
- Added `--encrypted` flag to instance create
- Request placement on encrypted compute nodes

#### 2c. Cloud Config Option ✅
- Added `--cloud-config` flag to instance create
- Shortcut for cloud-init user-data metadata
- Accepts file path or inline content

#### 2d. Allow Shared Images Option ✅
- Added `--allow-shared-images` flag
- Permit using images shared with account

#### 2e. Dry Run Option ✅
- Added `--dry-run` flag to instance create
- Simulate creation without actually provisioning
- Shows what would be created

#### Files Modified
- [x] `apis/cloudapi-api/src/types/machine.rs` - Add new fields to CreateMachineRequest
- [x] `cli/triton-cli/src/commands/instance/create.rs` - Add all flags

---

### 3. Image Share/Unshare Commands ✅ COMPLETED

**Priority:** P2
**Impact:** Cannot share images with other accounts

#### Implementation
- Added `triton image share IMAGE ACCOUNT` command
- Added `triton image unshare IMAGE ACCOUNT` command
- API: `POST /my/images/:id?action=share`
- API: `POST /my/images/:id?action=unshare`

#### Files Modified
- [x] `apis/cloudapi-api/src/types/image.rs` - Add Share/Unshare actions and request types
- [x] `clients/internal/cloudapi-client/src/lib.rs` - Add share/unshare methods
- [x] `cli/triton-cli/src/commands/image.rs` - Add share/unshare subcommands

---

### 4. Image Tag Command ✅ COMPLETED

**Priority:** P2
**Impact:** Cannot manage image tags

#### Implementation
- Added `triton image tag` subcommand group
- Subcommands: list, get, set, delete
- Uses image update mechanism to manage tags

#### Files Modified
- [x] `cli/triton-cli/src/commands/image.rs` - Add tag subcommand with list/get/set/delete

---

### 5. Profile Docker Setup ✅ COMPLETED

**Priority:** P2
**Impact:** Cannot auto-setup Docker TLS certificates

#### Implementation
- Added `triton profile docker-setup` command
- Generates Docker TLS certificates using ECDSA P-256
- Verifies Docker service exists via CloudAPI
- Downloads CA certificate from Docker host
- Stores certs in `~/.triton/docker/<profile>/`
- Creates setup.json with environment variables

#### Files Modified
- [x] `libs/triton-auth/src/certgen.rs` - New certificate generation module
- [x] `libs/triton-auth/Cargo.toml` - Add rcgen, x509-cert, time dependencies
- [x] `cli/triton-cli/src/commands/profile.rs` - Add docker-setup subcommand

---

### 6. Profile CMON Certgen ✅ COMPLETED

**Priority:** P2
**Impact:** Cannot generate CMON certificates

#### Implementation
- Added `triton profile cmon-certgen` command
- Generates CMON TLS certificates using ECDSA P-256
- Verifies CMON service exists via CloudAPI
- Writes certs to current working directory
- Generates example Prometheus configuration file

#### Files Modified
- [x] `libs/triton-auth/src/certgen.rs` - Shared certificate generation module
- [x] `cli/triton-cli/src/commands/profile.rs` - Add cmon-certgen subcommand

---

### 7. RBAC Info Command ✅ COMPLETED

**Priority:** P2
**Impact:** No summary view of RBAC state

#### Implementation
- Added `triton rbac info` command
- Shows summary: user count, role count, policy count
- Lists all users/roles/policies in compact table format
- Supports JSON output

#### Files Modified
- [x] `cli/triton-cli/src/commands/rbac.rs` - Add info subcommand

---

### 8. RBAC Apply/Reset Commands

**Priority:** P2
**Impact:** Cannot apply RBAC from config files

#### Implementation
- Add `triton rbac apply FILE` command
- Add `triton rbac reset` command
- Apply creates/updates users/roles/policies from JSON/YAML
- Reset removes all RBAC config (with confirmation)

#### Files to Modify
- [ ] `cli/triton-cli/src/commands/rbac.rs` - Add apply/reset subcommands

---

### 9. RBAC Role Tags Commands

**Priority:** P2
**Impact:** Cannot manage role tags on resources

#### Implementation
- Add `triton rbac role-tags` subcommand group
- Resource-specific subcommands:
  - `triton rbac instance-role-tags INSTANCE`
  - `triton rbac image-role-tags IMAGE`
  - `triton rbac network-role-tags NETWORK`
  - `triton rbac package-role-tags PACKAGE`

#### API Methods Needed
- `GET/PUT /my/machines/:id` (role_tags field)
- Similar for images, networks, packages

#### Files to Modify
- [ ] `clients/external/cloudapi-client/src/lib.rs` - Add role tag methods
- [ ] `cli/triton-cli/src/commands/rbac.rs` - Add role-tags subcommands

---

### 10. Changefeed Command ✅ COMPLETED

**Priority:** P2
**Impact:** No real-time VM change events

#### Implementation
- Added `triton changefeed` command
- WebSocket connection to CloudAPI changefeed endpoint
- Streams VM state changes in real-time with formatted output
- Options: `--instances` to filter by specific instance UUIDs
- Supports JSON output with `-j` flag

#### Files Modified
- [x] `cli/triton-cli/src/commands/changefeed.rs` - New file
- [x] `cli/triton-cli/src/commands/mod.rs` - Export module
- [x] `cli/triton-cli/src/main.rs` - Wire up command

---

### 11. Services Command ✅ COMPLETED

**Priority:** P2
**Impact:** Cannot list service endpoints

#### Implementation
- Added `triton services` command (alias: `triton svcs`)
- API: `GET /my/services`
- Returns map of service names to URLs

#### Files Modified
- [x] `cli/triton-cli/src/commands/services.rs` - New file
- [x] `cli/triton-cli/src/commands/mod.rs` - Export module
- [x] `cli/triton-cli/src/main.rs` - Wire up command

---

### 12. Snapshot Start Command ✅ ALREADY EXISTED

**Priority:** P2
**Impact:** Cannot start instance from snapshot

#### Implementation
- Command exists as `triton instance snapshot boot INSTANCE SNAPSHOT`
- API: `POST /my/machines/:id/snapshots/:name`
- Starts instance from specified snapshot

#### Files (Already Implemented)
- [x] `cli/triton-cli/src/commands/instance/snapshot.rs` - Boot subcommand exists

---

## P3 Features - Low Priority

### 1. Instance Shortcut Commands (Partially Complete)

**Priority:** P3
**Impact:** Subcommands exist, just missing shortcuts

#### Implementation
Add shortcut commands that alias to instance subcommands:
- `triton disks INSTANCE` → `triton instance disk list INSTANCE`
- `triton snapshots INSTANCE` → `triton instance snapshot list INSTANCE`
- `triton tags INSTANCE` → `triton instance tag list INSTANCE`
- `triton metadatas INSTANCE` → `triton instance metadata list INSTANCE`

#### Progress (2025-12-16)
- [x] Added subcommand aliases: `instance disks`, `instance snapshots`, `instance tags`, `instance metadatas`, `instance nics`
- [ ] Top-level shortcuts (`triton disks INSTANCE`) still needed

#### Files to Modify
- [ ] `cli/triton-cli/src/main.rs` - Add top-level shortcut commands

---

### 2. CloudAPI Raw Command (Hidden)

**Priority:** P3
**Impact:** Developer debugging tool

#### Implementation
- Add `triton cloudapi METHOD PATH [BODY]` command
- Hidden from help (developer tool)
- Make raw CloudAPI requests
- Useful for debugging/testing

#### Files to Modify
- [ ] `cli/triton-cli/src/commands/cloudapi.rs` - New file
- [ ] `cli/triton-cli/src/main.rs` - Wire up (hidden)

---

## Intentionally Skipped

These features are intentionally not planned:

1. **`triton badger`** - Easter egg, not needed
2. **Legacy `SDC_*` environment variables** - node-triton specific

---

## Progress Summary

### Completed (10/12 P2 features):
1. ✅ RBAC info command
2. ✅ Services command
3. ✅ Image share/unshare commands
4. ✅ Instance VNC command
5. ✅ Additional instance create options (all 5 flags)
6. ✅ Snapshot start command (already existed as `boot`)
7. ✅ Image tag command
8. ✅ Profile docker-setup
9. ✅ Profile cmon-certgen
10. ✅ Changefeed command

### Remaining (2/12 P2 features):
1. RBAC apply/reset commands
2. RBAC role tags commands

### P3 features (0.5/2):
1. Instance shortcut commands - **Partial**: subcommand aliases done, top-level shortcuts pending
2. CloudAPI raw command - Not started

### Additional Improvements (2025-12-16)
These items were identified during CLI compatibility analysis:

**Completed:**
- ✅ Resolved short option conflicts (`-v`, `-k`, `-a`) by making globals top-level only
- ✅ Added `triton ip` shortcut
- ✅ Added `triton profiles` shortcut
- ✅ Added instance list options (`-l`, `-H`, `-s`)
- ✅ Added plural aliases for instance subcommands

**Still Pending:**
- SSH proxy support (`tritoncli.ssh.proxy` tag)
- SSH default user detection (from image tags)

See: `conversion-plans/triton/cli-compatibility-analysis.md` for full details.

---

## Testing Requirements

For each feature:
- [ ] Unit tests for argument parsing
- [ ] Integration tests where applicable
- [ ] Manual testing against real CloudAPI
- [ ] Documentation updates

---

## Notes

- P2 features enhance usability but don't block core workflows
- P3 features are conveniences that experienced users might want
- Community feedback should drive prioritization adjustments
- Some P2 features may be promoted to P1 based on user needs
