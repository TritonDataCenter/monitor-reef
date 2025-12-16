<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

Copyright 2025 Edgecast Cloud LLC.
-->

# Triton CLI Lower Priority Implementation Plan

**Date:** 2025-12-16
**Status:** Planning
**Source Reference:** comprehensive-validation-report-2025-12-16.md

## Overview

This plan covers P2 (Nice to Have) and P3 (Low Priority) features. These are not blocking production use but would improve feature completeness.

---

## P2 Features - Nice to Have

### 1. Instance VNC Command

**Priority:** P2
**Impact:** Cannot get VNC access to bhyve/KVM instances

#### Implementation
- Add `triton instance vnc INSTANCE` command
- Calls `GET /my/machines/:id/vnc`
- Returns VNC connection info (host, port, token)
- Optional: Open VNC viewer automatically

#### Files to Modify
- [ ] `clients/external/cloudapi-client/src/lib.rs` - Add `get_machine_vnc` method
- [ ] `cli/triton-cli/src/commands/instance/vnc.rs` - New file
- [ ] `cli/triton-cli/src/commands/instance/mod.rs` - Wire up command

---

### 2. Instance Create: Additional Options

**Priority:** P2

#### 2a. Delegate Dataset Option
- Add `--delegate-dataset` flag to instance create
- Creates delegated ZFS dataset in zone
- Zone-specific feature

#### 2b. Encrypted Option
- Add `--encrypted` flag to instance create
- Request placement on encrypted compute nodes

#### 2c. Cloud Config Option
- Add `--cloud-config` flag to instance create
- Shortcut for cloud-init user-data metadata

#### 2d. Allow Shared Images Option
- Add `--allow-shared-images` flag
- Permit using images shared with account

#### 2e. Dry Run Option
- Add `--dry-run` flag to instance create
- Simulate creation without actually provisioning

#### Files to Modify
- [ ] `cli/triton-cli/src/commands/instance/create.rs` - Add all flags

---

### 3. Image Share/Unshare Commands

**Priority:** P2
**Impact:** Cannot share images with other accounts

#### Implementation
- Add `triton image share IMAGE ACCOUNT` command
- Add `triton image unshare IMAGE ACCOUNT` command
- API: `POST /my/images/:id?action=share`
- API: `POST /my/images/:id?action=unshare`

#### Files to Modify
- [ ] `clients/external/cloudapi-client/src/lib.rs` - Add share/unshare methods
- [ ] `cli/triton-cli/src/commands/image.rs` - Add share/unshare subcommands

---

### 4. Image Tag Command

**Priority:** P2
**Impact:** Cannot manage image tags

#### Implementation
- Add `triton image tag` subcommand group
- Subcommands: list, get, set, delete
- Similar to instance tag implementation

#### Files to Modify
- [ ] `cli/triton-cli/src/commands/image.rs` - Add tag subcommand

---

### 5. Profile Docker Setup

**Priority:** P2
**Impact:** Cannot auto-setup Docker TLS certificates

#### Implementation
- Add `triton profile docker-setup` command
- Generate Docker TLS certificates
- Configure Docker environment
- Store certs in `~/.triton/docker/`

#### Files to Modify
- [ ] `cli/triton-cli/src/commands/profile.rs` - Add docker-setup subcommand

---

### 6. Profile CMON Certgen

**Priority:** P2
**Impact:** Cannot generate CMON certificates

#### Implementation
- Add `triton profile cmon-certgen` command
- Generate CMON TLS certificates
- Store certs in `~/.triton/cmon/`

#### Files to Modify
- [ ] `cli/triton-cli/src/commands/profile.rs` - Add cmon-certgen subcommand

---

### 7. RBAC Info Command

**Priority:** P2
**Impact:** No summary view of RBAC state

#### Implementation
- Add `triton rbac info` command
- Show summary: user count, role count, policy count
- List all users/roles/policies in compact format

#### Files to Modify
- [ ] `cli/triton-cli/src/commands/rbac.rs` - Add info subcommand

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

### 10. Changefeed Command

**Priority:** P2
**Impact:** No real-time VM change events

#### Implementation
- Add `triton changefeed` command
- WebSocket connection to CloudAPI changefeed
- Stream VM state changes in real-time
- Options: filter by instance, state, etc.

#### Files to Modify
- [ ] `clients/external/cloudapi-client/src/lib.rs` - Add WebSocket support
- [ ] `cli/triton-cli/src/commands/changefeed.rs` - New file
- [ ] `cli/triton-cli/src/main.rs` - Wire up command

---

### 11. Services Command

**Priority:** P2
**Impact:** Cannot list service endpoints

#### Implementation
- Add `triton services` command
- API: `GET /my/services`
- Returns map of service names to URLs

#### Files to Modify
- [ ] `clients/external/cloudapi-client/src/lib.rs` - Add `list_services` method
- [ ] `cli/triton-cli/src/commands/services.rs` - New file
- [ ] `cli/triton-cli/src/main.rs` - Wire up command

---

### 12. Snapshot Start Command

**Priority:** P2
**Impact:** Cannot start instance from snapshot

#### Implementation
- Add `triton instance snapshot start INSTANCE SNAPSHOT` command
- API: `POST /my/machines/:id/snapshots/:name`
- Starts instance from specified snapshot

#### Files to Modify
- [ ] `clients/external/cloudapi-client/src/lib.rs` - Add snapshot start method
- [ ] `cli/triton-cli/src/commands/instance/snapshot.rs` - Add start subcommand

---

## P3 Features - Low Priority

### 1. Instance Shortcut Commands

**Priority:** P3
**Impact:** Subcommands exist, just missing shortcuts

#### Implementation
Add shortcut commands that alias to instance subcommands:
- `triton disks INSTANCE` → `triton instance disk list INSTANCE`
- `triton snapshots INSTANCE` → `triton instance snapshot list INSTANCE`
- `triton tags INSTANCE` → `triton instance tag list INSTANCE`
- `triton metadatas INSTANCE` → `triton instance metadata list INSTANCE`

#### Files to Modify
- [ ] `cli/triton-cli/src/main.rs` - Add shortcut aliases

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

## Implementation Order

Suggested order for P2 features (based on utility):

1. **RBAC info** - Quick win, useful for auditing
2. **Services command** - Quick win, simple API
3. **Image share/unshare** - Enables image collaboration
4. **Instance VNC** - Useful for debugging bhyve/KVM
5. **Additional create options** - Incremental improvements
6. **Profile docker-setup** - Docker users need this
7. **Snapshot start** - Useful recovery feature
8. **Image tags** - Consistency with instance tags
9. **RBAC apply/reset** - Automation support
10. **RBAC role tags** - Advanced RBAC
11. **Changefeed** - Advanced monitoring
12. **Profile cmon-certgen** - Specialized use case

P3 features can be implemented as time permits or community requests.

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
