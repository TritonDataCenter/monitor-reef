<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

Copyright 2025 Edgecast Cloud LLC.
-->

# Triton CLI Comprehensive Evaluation Prompt

## Objective

Evaluate the Rust `triton-cli` implementation for completeness and compatibility against:

1. **node-triton** - Full compatibility target (100% command and option coverage)
2. **node-smartdc** - Functionality coverage only (same capabilities, different command interface is acceptable)

This evaluation should produce a prioritized action list and identify any new features unique to the Rust implementation.

## Goals and Non-Goals

### Goals

- **100% node-triton command coverage** - Every command and subcommand implemented
- **100% node-triton option compatibility** - All options available (short and long forms match where possible)
- **100% node-smartdc functionality coverage** - All capabilities available, but via triton-style commands (not `sdc-*` format)
- **Document new features** - Rust additions that enhance functionality

### Non-Goals

- Replicating `sdc-*` command naming (we use unified `triton` interface)
- Easter eggs (e.g., `triton badger`)
- Deprecated features with modern alternatives

## Source Locations

| Component | Location |
|-----------|----------|
| Rust CLI | `cli/triton-cli/` |
| Node.js triton | `target/node-triton/` |
| Node.js smartdc | `target/node-smartdc/` |
| Technical constraints | `conversion-plans/triton/reference/cli-option-compatibility.md` |

## Known Technical Constraints

Review `reference/cli-option-compatibility.md` for full details. Key points:

1. **Top-level options must come before subcommands** - Rust uses clap which handles globals differently than Node.js cmdln
2. **Short option reuse is now supported** - `-v`, `-k`, `-a` can be used at top-level for globals AND in subcommands for other purposes
3. **Ordering requirement**: `triton -v instance list` (verbose) vs `triton instance create -v vol` (volume)

## Evaluation Tasks

### Part 1: Command Coverage (node-triton)

For each command in node-triton, verify it exists in triton-cli:

**Top-Level Commands** (check `target/node-triton/lib/do_*.js`):

| Category | Commands to Check |
|----------|-------------------|
| Instance shortcuts | `create`, `delete`, `start`, `stop`, `reboot`, `ssh`, `ip` |
| Resource shortcuts | `images`, `instances`, `networks`, `packages`, `fwrules`, `volumes`, `keys`, `profiles` |
| Subcommand groups | `account`, `image`, `instance`, `network`, `vlan`, `volume`, `fwrule`, `key`, `profile`, `rbac`, `package` |
| Utility | `info`, `env`, `datacenters`, `services`, `cloudapi`, `changefeed`, `completion` |

**Instance Subcommands** (check `target/node-triton/lib/do_instance/`):

| Lifecycle | Management | I/O | Networking |
|-----------|------------|-----|------------|
| create, delete | get, list, rename, resize | ssh, vnc, wait | nic (add/get/list/delete) |
| start, stop, reboot | audit, metadata | snapshot (create/get/list/delete) | firewall (enable/disable/rules) |
| | tag, disk | | deletion-protection (enable/disable) |
| | migration | | |

**Image Subcommands** (check `target/node-triton/lib/do_image/`):
- `list`, `get`, `create`, `delete`, `clone`, `copy`, `export`, `wait`
- `share`, `unshare`, `update`, `tag`

**Network Subcommands** (check `target/node-triton/lib/do_network/`):
- `list`, `get`, `create`, `delete`, `get-default`, `set-default`
- `ip` subcommand group

**VLAN Subcommands** (check `target/node-triton/lib/do_vlan/`):
- `list`, `get`, `create`, `delete`, `update`, `networks`

**Volume Subcommands** (check `target/node-triton/lib/do_volume/`):
- `list`, `get`, `create`, `delete`, `sizes`

**Firewall Subcommands** (check `target/node-triton/lib/do_fwrule/`):
- `list`, `get`, `create`, `delete`, `update`, `enable`, `disable`, `instances`

**Key Subcommands** (check `target/node-triton/lib/do_key/`):
- `list`, `get`, `add`, `delete`

**Profile Subcommands** (check `target/node-triton/lib/do_profile/`):
- `list`, `get`, `create`, `delete`, `edit`, `set-current`
- `docker-setup`, `cmon-certgen`

**Account Subcommands** (check `target/node-triton/lib/do_account/`):
- `get`, `update`, `limits`

**RBAC Subcommands** (check `target/node-triton/lib/do_rbac/`):
- `info`, `apply`, `reset`
- `user`, `users`, `role`, `roles`, `policy`, `policies`
- `key`, `keys`, `role-tags`

### Part 2: Option Compatibility (node-triton)

For each implemented command, compare ALL options:

1. **Extract Node.js options**: Look for `options` array in `do_*.js` files
2. **Extract Rust options**: Check clap definitions in `commands/*.rs`
3. **Compare**:
   - Short form availability (`-n` vs only `--name`)
   - Long form naming (`--key-id` vs `--keyId`)
   - Default values
   - Required vs optional
   - Array/multiple support

**Priority commands for deep option analysis:**
- `instance create` - Most complex, many options
- `instance list` - Table formatting options
- `profile create` - Authentication setup
- `rbac apply` - Configuration management
- `network create` - Network provisioning

### Part 3: Functionality Coverage (node-smartdc)

Check that all `sdc-*` commands have equivalent functionality in triton-cli (NOT necessarily same command format):

| sdc Command | Functionality | Expected triton Equivalent |
|-------------|--------------|---------------------------|
| `sdc-listmachines` | List instances | `triton instance list` |
| `sdc-createmachine` | Create instance | `triton instance create` |
| `sdc-getmachine` | Get instance details | `triton instance get` |
| `sdc-fabric` | Fabric network operations | `triton network/vlan` commands |
| `sdc-chmod` | RBAC permissions | `triton rbac` commands |
| `sdc-policy` | RBAC policies | `triton rbac policy` |
| `sdc-role` | RBAC roles | `triton rbac role` |
| `sdc-user` | RBAC users | `triton rbac user` |
| `sdc-nics` | NIC operations | `triton instance nic` |
| `sdc-startmachinefromsnapshot` | Boot from snapshot | Check if supported |
| `sdc-replacemachinetags` | Replace all tags | Check if supported |

**Focus on**: Commands that do things NOT covered by node-triton commands.

### Part 4: New Features (Rust Additions)

Document features in triton-cli that don't exist in node-triton:

**Known Rust additions to verify:**
- `--limit` option for list commands (pagination control)
- `--short` option for list commands (compact output)
- `--no-proxy` for SSH command (direct connection)
- `--wait-timeout` defaults on create commands
- Global `-j/--json` flag (vs per-command in Node.js)
- VNC proxy support (`triton instance vnc`)
- RBAC `--action` flags for node-triton compatibility

**Look for additional innovations in:**
- Error handling and messages
- Output formatting options
- Performance optimizations (parallel operations)
- Security features

## Output Format

### Executive Summary

```markdown
## Summary

| Metric | Current | Target | Gap |
|--------|---------|--------|-----|
| Command Coverage (node-triton) | X/Y | 100% | Z missing |
| Option Compatibility | X% | 100% | Z TODO items |
| Functionality (node-smartdc) | X% | 100% | Z gaps |
| New Features | X documented | - | - |
```

### Command Coverage Table

```markdown
## Command Coverage

| Category | Total | Implemented | Missing |
|----------|-------|-------------|---------|
| Instance | X | X | list here |
| Image | X | X | list here |
| Network | X | X | list here |
| VLAN | X | X | list here |
| Volume | X | X | list here |
| Firewall | X | X | list here |
| Profile | X | X | list here |
| Account | X | X | list here |
| Key | X | X | list here |
| RBAC | X | X | list here |
| Utility | X | X | list here |
```

### Option Compatibility Findings

For each command with gaps:

```markdown
### `triton <command>`

| Option | Node.js | Rust | Status | Priority |
|--------|---------|------|--------|----------|
| Name | `-n, --name` | `--name` | Missing `-n` | P2 |
```

### Prioritized Action List

```markdown
## Action Items

### P0 - Critical (Blocks usage)
- [ ] Item description

### P1 - Important (Significant compatibility gap)
- [ ] Item description

### P2 - Nice-to-have (Minor compatibility gap)
- [ ] Item description

### P3 - Low priority (Rare use cases)
- [ ] Item description
```

### New Features Section

```markdown
## New Features in Rust CLI

### Implemented
1. **Feature name** - Description and benefit

### Potential Additions
1. **Feature idea** - Description and value proposition
```

## Validation Steps

After analysis:

1. **Build and test help output:**
   ```bash
   make package-build PACKAGE=triton-cli
   ./target/debug/triton --help
   ./target/debug/triton instance create --help
   ```

2. **Check for TODOs:**
   ```bash
   grep -r "TODO\|FIXME\|unimplemented\|todo!" cli/triton-cli/src/
   ```

3. **Verify API coverage:**
   - Compare `target/node-triton/lib/cloudapi2.js` against `apis/cloudapi-api/`
   - Ensure all CloudAPI endpoints used by node-triton are available

## References

- [Technical constraints](../reference/cli-option-compatibility.md)
- [Dropshot documentation](https://github.com/oxidecomputer/dropshot)
- [Clap documentation](https://docs.rs/clap)
- [Node.js triton source](target/node-triton/)
- [Node.js smartdc source](target/node-smartdc/)
