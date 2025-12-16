<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

Copyright 2025 Edgecast Cloud LLC.
-->

# Comprehensive Triton CLI Validation Prompt

## Objective

Perform a thorough validation that the new Rust `triton-cli` implements all functionality from:
1. **node-triton** (`target/node-triton/`) - The modern Triton CLI
2. **node-smartdc** (`target/node-smartdc/`) - The legacy SDC CLI (only features still relevant)

This validation should identify gaps, missing features, and functionality parity issues.

## Methodology

For each command in the source Node.js tools:
1. Read the source implementation to understand its full capabilities
2. Check if equivalent functionality exists in `cli/triton-cli/`
3. Verify that command-line arguments and options match
4. Note any missing features or behavior differences

## Part 1: node-triton Command Validation

### Top-Level Commands

| node-triton Command | Source File | triton-cli Equivalent | Status |
|---------------------|-------------|----------------------|--------|
| `triton account` | `lib/do_account/` | `commands/account.rs` | Check |
| `triton badger` | `lib/do_badger.js` | N/A (Easter egg) | Skip |
| `triton changefeed` | `lib/do_changefeed.js` | ? | Check |
| `triton cloudapi` | `lib/do_cloudapi.js` | ? | Check |
| `triton completion` | `lib/do_completion.js` | ? | Check |
| `triton create` | `lib/do_create.js` | `commands/instance/create.rs` | Check |
| `triton datacenters` | `lib/do_datacenters.js` | ? | Check |
| `triton delete` | `lib/do_delete.js` | `commands/instance/delete.rs` | Check |
| `triton env` | `lib/do_env.js` | `commands/env.rs` | Check |
| `triton fwrule` | `lib/do_fwrule/` | `commands/fwrule.rs` | Check |
| `triton fwrules` | `lib/do_fwrules.js` | ? | Check |
| `triton image` | `lib/do_image/` | `commands/image.rs` | Check |
| `triton images` | `lib/do_images.js` | ? | Check |
| `triton info` | `lib/do_info.js` | `commands/info.rs` | Check |
| `triton instance` | `lib/do_instance/` | `commands/instance/` | Check |
| `triton instances` | `lib/do_instances.js` | ? | Check |
| `triton ip` | `lib/do_ip.js` | ? | Check |
| `triton key` | `lib/do_key/` | `commands/key.rs` | Check |
| `triton keys` | `lib/do_keys.js` | ? | Check |
| `triton network` | `lib/do_network/` | `commands/network.rs` | Check |
| `triton networks` | `lib/do_networks.js` | ? | Check |
| `triton package` | `lib/do_package/` | `commands/package.rs` | Check |
| `triton packages` | `lib/do_packages.js` | ? | Check |
| `triton profile` | `lib/do_profile/` | `commands/profile.rs` | Check |
| `triton profiles` | `lib/do_profiles.js` | ? | Check |
| `triton rbac` | `lib/do_rbac/` | `commands/rbac.rs` | Check |
| `triton reboot` | `lib/do_reboot.js` | `commands/instance/lifecycle.rs` | Check |
| `triton services` | `lib/do_services.js` | ? | Check |
| `triton ssh` | `lib/do_ssh.js` | `commands/instance/ssh.rs` | Check |
| `triton start` | `lib/do_start.js` | `commands/instance/lifecycle.rs` | Check |
| `triton stop` | `lib/do_stop.js` | `commands/instance/lifecycle.rs` | Check |
| `triton vlan` | `lib/do_vlan/` | `commands/vlan.rs` | Check |
| `triton volume` | `lib/do_volume/` | `commands/volume.rs` | Check |
| `triton volumes` | `lib/do_volumes.js` | ? | Check |

### Instance Subcommands (`lib/do_instance/`)

For each file in `target/node-triton/lib/do_instance/`:

| Subcommand | Source | triton-cli Equivalent | Status |
|------------|--------|----------------------|--------|
| `audit` | `do_audit.js` | `instance/audit.rs` | Check |
| `create` | `do_create.js` | `instance/create.rs` | Check |
| `delete` | `do_delete.js` | `instance/delete.rs` | Check |
| `disable-deletion-protection` | `do_disable_deletion_protection.js` | `instance/protection.rs` | Check |
| `disable-firewall` | `do_disable_firewall.js` | `instance/firewall.rs` | Check |
| `disk` | `do_disk/` | `instance/disk.rs` | Check |
| `disks` | `do_disks.js` | ? | Check |
| `enable-deletion-protection` | `do_enable_deletion_protection.js` | `instance/protection.rs` | Check |
| `enable-firewall` | `do_enable_firewall.js` | `instance/firewall.rs` | Check |
| `fwrule` | `do_fwrule/` | ? | Check |
| `fwrules` | `do_fwrules.js` | `instance/firewall.rs` | Check |
| `get` | `do_get.js` | `instance/get.rs` | Check |
| `ip` | `do_ip.js` | ? | Check |
| `list` | `do_list.js` | `instance/list.rs` | Check |
| `metadata` | `do_metadata/` | `instance/metadata.rs` | Check |
| `metadatas` | `do_metadatas.js` | ? | Check |
| `migration` | `do_migration/` | ? | Check |
| `nic` | `do_nic/` | `instance/nic.rs` | Check |
| `reboot` | `do_reboot.js` | `instance/lifecycle.rs` | Check |
| `rename` | `do_rename.js` | `instance/rename.rs` | Check |
| `resize` | `do_resize.js` | `instance/resize.rs` | Check |
| `snapshot` | `do_snapshot/` | `instance/snapshot.rs` | Check |
| `snapshots` | `do_snapshots.js` | ? | Check |
| `ssh` | `do_ssh.js` | `instance/ssh.rs` | Check |
| `start` | `do_start.js` | `instance/lifecycle.rs` | Check |
| `stop` | `do_stop.js` | `instance/lifecycle.rs` | Check |
| `tag` | `do_tag/` | `instance/tag.rs` | Check |
| `tags` | `do_tags.js` | ? | Check |
| `vnc` | `do_vnc.js` | ? | Check |
| `wait` | `do_wait.js` | `instance/wait.rs` | Check |

### Image Subcommands (`lib/do_image/`)

| Subcommand | Source | triton-cli Equivalent | Status |
|------------|--------|----------------------|--------|
| `clone` | `do_clone.js` | ? | Check |
| `copy` | `do_copy.js` | ? | Check |
| `create` | `do_create.js` | ? | Check |
| `delete` | `do_delete.js` | ? | Check |
| `export` | `do_export.js` | ? | Check |
| `get` | `do_get.js` | ? | Check |
| `list` | `do_list.js` | ? | Check |
| `share` | `do_share.js` | ? | Check |
| `tag` | `do_tag.js` | ? | Check |
| `unshare` | `do_unshare.js` | ? | Check |
| `update` | `do_update.js` | ? | Check |
| `wait` | `do_wait.js` | ? | Check |

### Firewall Rule Subcommands (`lib/do_fwrule/`)

| Subcommand | Source | triton-cli Equivalent | Status |
|------------|--------|----------------------|--------|
| `create` | `do_create.js` | ? | Check |
| `delete` | `do_delete.js` | ? | Check |
| `disable` | `do_disable.js` | ? | Check |
| `enable` | `do_enable.js` | ? | Check |
| `get` | `do_get.js` | ? | Check |
| `instances` | `do_instances.js` | ? | Check |
| `list` | `do_list.js` | ? | Check |
| `update` | `do_update.js` | ? | Check |

### Network Subcommands (`lib/do_network/`)

| Subcommand | Source | triton-cli Equivalent | Status |
|------------|--------|----------------------|--------|
| `create` | `do_create.js` | ? | Check |
| `delete` | `do_delete.js` | ? | Check |
| `get` | `do_get.js` | ? | Check |
| `get-default` | `do_get_default.js` | ? | Check |
| `list` | `do_list.js` | ? | Check |
| `set-default` | `do_set_default.js` | ? | Check |
| `ip` | `do_ip/` | ? | Check |

### VLAN Subcommands (`lib/do_vlan/`)

| Subcommand | Source | triton-cli Equivalent | Status |
|------------|--------|----------------------|--------|
| `create` | `do_create.js` | ? | Check |
| `delete` | `do_delete.js` | ? | Check |
| `get` | `do_get.js` | ? | Check |
| `list` | `do_list.js` | ? | Check |
| `networks` | `do_networks.js` | ? | Check |
| `update` | `do_update.js` | ? | Check |

### Volume Subcommands (`lib/do_volume/`)

| Subcommand | Source | triton-cli Equivalent | Status |
|------------|--------|----------------------|--------|
| `create` | `do_create.js` | ? | Check |
| `delete` | `do_delete.js` | ? | Check |
| `get` | `do_get.js` | ? | Check |
| `list` | `do_list.js` | ? | Check |
| `sizes` | `do_sizes.js` | ? | Check |

### Account Subcommands (`lib/do_account/`)

| Subcommand | Source | triton-cli Equivalent | Status |
|------------|--------|----------------------|--------|
| `get` | `do_get.js` | ? | Check |
| `limits` | `do_limits.js` | ? | Check |
| `update` | `do_update.js` | ? | Check |

### Key Subcommands (`lib/do_key/`)

| Subcommand | Source | triton-cli Equivalent | Status |
|------------|--------|----------------------|--------|
| `add` | `do_add.js` | ? | Check |
| `delete` | `do_delete.js` | ? | Check |
| `get` | `do_get.js` | ? | Check |
| `list` | `do_list.js` | ? | Check |

### Profile Subcommands (`lib/do_profile/`)

| Subcommand | Source | triton-cli Equivalent | Status |
|------------|--------|----------------------|--------|
| `cmon-certgen` | `do_cmon_certgen.js` | ? | Check |
| `create` | `do_create.js` | ? | Check |
| `delete` | `do_delete.js` | ? | Check |
| `docker-setup` | `do_docker_setup.js` | ? | Check |
| `edit` | `do_edit.js` | ? | Check |
| `get` | `do_get.js` | ? | Check |
| `list` | `do_list.js` | ? | Check |
| `set-current` | `do_set_current.js` | ? | Check |

### RBAC Subcommands (`lib/do_rbac/`)

| Subcommand | Source | triton-cli Equivalent | Status |
|------------|--------|----------------------|--------|
| `apply` | `do_apply.js` | ? | Check |
| `info` | `do_info.js` | ? | Check |
| `key` | `do_key.js` | ? | Check |
| `keys` | `do_keys.js` | ? | Check |
| `policies` | `do_policies.js` | ? | Check |
| `policy` | `do_policy.js` | ? | Check |
| `reset` | `do_reset.js` | ? | Check |
| `role` | `do_role.js` | ? | Check |
| `role-tags` | `do_role_tags.js` | ? | Check |
| `roles` | `do_roles.js` | ? | Check |
| `user` | `do_user.js` | ? | Check |
| `users` | `do_users.js` | ? | Check |

## Part 2: node-smartdc Command Validation

The `node-smartdc` tool provides individual commands prefixed with `sdc-`. Many of these are superseded by `node-triton`, but some may still be relevant.

### Commands to Check for Relevance

Review each command in `target/node-smartdc/bin/` and determine:
1. Is this functionality covered by node-triton?
2. If not, is it still relevant for modern CloudAPI?
3. If relevant and not in node-triton, does triton-cli need it?

| sdc Command | Functionality | In node-triton? | Relevant? | In triton-cli? |
|-------------|---------------|-----------------|-----------|----------------|
| `sdc-addmachinetags` | Add tags to machine | Yes (tag) | Yes | Check |
| `sdc-chmod` | Permissions | ? | Check | ? |
| `sdc-createfirewallrule` | Create FW rule | Yes | Yes | Check |
| `sdc-createimagefrommachine` | Create image from instance | Yes | Yes | Check |
| `sdc-createkey` | Add SSH key | Yes | Yes | Check |
| `sdc-createmachine` | Create instance | Yes | Yes | Check |
| `sdc-createmachinesnapshot` | Create snapshot | Yes | Yes | Check |
| `sdc-deletefirewallrule` | Delete FW rule | Yes | Yes | Check |
| `sdc-deleteimage` | Delete image | Yes | Yes | Check |
| `sdc-deletekey` | Delete key | Yes | Yes | Check |
| `sdc-deletemachine` | Delete instance | Yes | Yes | Check |
| `sdc-deletemachinemetadata` | Delete metadata | Yes | Yes | Check |
| `sdc-deletemachinesnapshot` | Delete snapshot | Yes | Yes | Check |
| `sdc-deletemachinetag` | Delete tag | Yes | Yes | Check |
| `sdc-disablefirewallrule` | Disable FW rule | Yes | Yes | Check |
| `sdc-disablemachinefirewall` | Disable instance FW | Yes | Yes | Check |
| `sdc-enablefirewallrule` | Enable FW rule | Yes | Yes | Check |
| `sdc-enablemachinefirewall` | Enable instance FW | Yes | Yes | Check |
| `sdc-exportimage` | Export image | Yes | Yes | Check |
| `sdc-fabric` | Fabric networks | Partial | Check | ? |
| `sdc-getaccount` | Get account | Yes | Yes | Check |
| `sdc-getfirewallrule` | Get FW rule | Yes | Yes | Check |
| `sdc-getimage` | Get image | Yes | Yes | Check |
| `sdc-getkey` | Get SSH key | Yes | Yes | Check |
| `sdc-getmachine` | Get instance | Yes | Yes | Check |
| `sdc-getmachineaudit` | Get audit log | Yes | Yes | Check |
| `sdc-getmachinemetadata` | Get metadata | Yes | Yes | Check |
| `sdc-getmachinesnapshot` | Get snapshot | Yes | Yes | Check |
| `sdc-getmachinetag` | Get tag | Yes | Yes | Check |
| `sdc-getnetwork` | Get network | Yes | Yes | Check |
| `sdc-getpackage` | Get package | Yes | Yes | Check |
| `sdc-info` | Account info | Yes | Yes | Check |
| `sdc-listdatacenters` | List datacenters | Yes | Yes | Check |
| `sdc-listfirewallrulemachines` | List FW rule instances | Yes | Yes | Check |
| `sdc-listfirewallrules` | List FW rules | Yes | Yes | Check |
| `sdc-listimages` | List images | Yes | Yes | Check |
| `sdc-listkeys` | List keys | Yes | Yes | Check |
| `sdc-listmachinefirewallrules` | List instance FW rules | Yes | Yes | Check |
| `sdc-listmachinemetadata` | List metadata | Yes | Yes | Check |
| `sdc-listmachines` | List instances | Yes | Yes | Check |
| `sdc-listmachinesnapshots` | List snapshots | Yes | Yes | Check |
| `sdc-listmachinetags` | List tags | Yes | Yes | Check |
| `sdc-listnetworks` | List networks | Yes | Yes | Check |
| `sdc-listpackages` | List packages | Yes | Yes | Check |
| `sdc-nics` | NIC operations | Yes | Yes | Check |
| `sdc-policy` | RBAC policies | Yes | Yes | Check |
| `sdc-rebootmachine` | Reboot instance | Yes | Yes | Check |
| `sdc-renamemachine` | Rename instance | Yes | Yes | Check |
| `sdc-replacemachinetags` | Replace all tags | Partial? | Check | ? |
| `sdc-resizemachine` | Resize instance | Yes | Yes | Check |
| `sdc-role` | RBAC roles | Yes | Yes | Check |
| `sdc-startmachine` | Start instance | Yes | Yes | Check |
| `sdc-startmachinefromsnapshot` | Start from snapshot | ? | Check | ? |
| `sdc-stopmachine` | Stop instance | Yes | Yes | Check |
| `sdc-updateaccount` | Update account | Yes | Yes | Check |
| `sdc-updatefirewallrule` | Update FW rule | Yes | Yes | Check |
| `sdc-updateimage` | Update image | Yes | Yes | Check |
| `sdc-updatemachinemetadata` | Update metadata | Yes | Yes | Check |
| `sdc-user` | RBAC users | Yes | Yes | Check |

## Part 3: Feature Parity Deep Dive

For key commands, verify detailed feature parity:

### Instance Create (`triton instance create`)

Read `target/node-triton/lib/do_instance/do_create.js` and verify:
- [ ] `--name` / `-n` - Instance name
- [ ] `--image` / `-i` - Image ID or name@version
- [ ] `--package` / `-p` - Package ID or name
- [ ] `--network` / `-N` - Network IDs (multiple)
- [ ] `--tag` / `-t` - Tags (key=value, multiple)
- [ ] `--metadata` / `-m` - Metadata (key=value, multiple)
- [ ] `--metadata-file` - Metadata from file
- [ ] `--script` - User script
- [ ] `--affinity` / `-a` - Affinity rules (multiple)
- [ ] `--locality` - Locality hints (deprecated)
- [ ] `--firewall` - Enable firewall
- [ ] `--deletion-protection` - Enable deletion protection
- [ ] `--volume` - Attach volume
- [ ] `--wait` / `-w` - Wait for running state
- [ ] `--wait-timeout` - Wait timeout
- [ ] `--json` / `-j` - JSON output

### Instance List (`triton instance list`)

Read `target/node-triton/lib/do_instance/do_list.js` and verify:
- [ ] `--name` - Filter by name
- [ ] `--state` - Filter by state
- [ ] `--image` - Filter by image
- [ ] `--package` - Filter by package
- [ ] `--brand` - Filter by brand
- [ ] `--tag` - Filter by tag
- [ ] `--limit` / `-l` - Result limit
- [ ] `--offset` - Result offset
- [ ] `--credentials` - Show credentials (for Docker)
- [ ] `-H` - No header
- [ ] `-o` - Output columns
- [ ] `--json` / `-j` - JSON output
- [ ] `-s` - Sort order

### Profile Management

Read `target/node-triton/lib/do_profile/` and verify:
- [ ] Profile file format compatibility
- [ ] Environment variable support (TRITON_*, SDC_*)
- [ ] `~/.triton/` directory structure
- [ ] Profile switching
- [ ] Docker setup integration
- [ ] CMON certificate generation

### SSH Command

Read `target/node-triton/lib/do_ssh.js` and verify:
- [ ] Instance resolution (name, UUID, shortid)
- [ ] `-l` / `--user` - Login user
- [ ] `-i` / `--identity` - Identity file
- [ ] `-o` - SSH options
- [ ] Additional SSH arguments passthrough
- [ ] IP selection (public vs private)

## Part 4: Validation Instructions

### Step 1: Read Source Implementations

For each command category:
1. Read the node-triton source file(s)
2. Extract all CLI arguments, options, and flags
3. Note any special behaviors or edge cases
4. Document environment variable support

### Step 2: Check triton-cli Implementation

For each command:
1. Read the corresponding Rust implementation
2. Verify all arguments and options exist
3. Check behavior matches (use `--help` output)
4. Note any missing functionality

### Step 3: Test Help Output

```bash
# Build triton-cli
make package-build PACKAGE=triton-cli

# Compare help outputs
./target/debug/triton --help
./target/debug/triton instance --help
./target/debug/triton instance create --help
# ... etc for all subcommands
```

### Step 4: Check for TODOs

```bash
# Search for incomplete implementations
grep -r "TODO" cli/triton-cli/src/
grep -r "unimplemented" cli/triton-cli/src/
grep -r "todo!" cli/triton-cli/src/
```

### Step 5: API Endpoint Coverage

Verify all CloudAPI endpoints used by node-triton are available in cloudapi-client:
- Read `target/node-triton/lib/cloudapi2.js` for API method list
- Compare against `apis/cloudapi-api/src/` endpoint definitions

## Part 5: Output Format

Produce a comprehensive validation report with:

### Summary Table

| Category | Total Commands | Implemented | Partial | Missing | Coverage % |
|----------|---------------|-------------|---------|---------|------------|
| Instance | X | X | X | X | X% |
| Image | X | X | X | X | X% |
| Network | X | X | X | X | X% |
| ... | ... | ... | ... | ... | ... |

### Detailed Findings

For each command category:

1. **Fully Implemented** - List commands with complete parity
2. **Partially Implemented** - List commands missing some options/features
3. **Not Implemented** - List commands that don't exist yet
4. **Not Applicable** - List commands intentionally skipped (with reason)

### Feature Gap List

Prioritized list of missing features:

| Priority | Feature | Source | Impact | Notes |
|----------|---------|--------|--------|-------|
| P0 | Critical missing feature | node-triton/xxx | Blocks usage | ... |
| P1 | Important missing feature | node-triton/xxx | Limits usage | ... |
| P2 | Nice-to-have feature | node-smartdc/xxx | Minor gap | ... |

### Recommendations

1. Critical items that must be implemented
2. Important items for feature parity
3. Items that can be deferred
4. Items that should be intentionally skipped

## Reference Files

### node-triton
- Main CLI: `target/node-triton/lib/cli.js`
- CloudAPI client: `target/node-triton/lib/cloudapi2.js`
- Triton API wrapper: `target/node-triton/lib/tritonapi.js`
- Command implementations: `target/node-triton/lib/do_*/`

### node-smartdc
- Individual commands: `target/node-smartdc/bin/sdc-*`
- Shared library: `target/node-smartdc/lib/`

### triton-cli
- Main CLI: `cli/triton-cli/src/main.rs`
- Commands: `cli/triton-cli/src/commands/`
- CloudAPI client: `clients/internal/cloudapi-client/`
- Auth library: `libs/triton-auth/`
