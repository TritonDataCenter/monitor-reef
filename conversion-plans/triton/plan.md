<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

Copyright 2025 Edgecast Cloud LLC.
-->

# Triton CLI Implementation Plan

## Overview

Create a new `triton` CLI tool providing user-friendly access to CloudAPI with full HTTP Signature authentication support. This achieves feature parity with the Node.js `node-triton` CLI in Phase 1, and SmartDC CLI in Phase 2.

The existing `cloudapi-cli` remains as a separate tool for raw API access.

## Reference Implementations

- **node-triton**: `./target/node-triton/` - Primary reference for Phase 1
- **node-smartdc**: `./target/node-smartdc/` - Reference for Phase 2 additional features
- **node-smartdc-auth**: `./target/node-smartdc-auth/` - **Critical** reference for HTTP signature authentication

## User Design Decisions

1. **Profile Storage**: Support both `~/.triton/` (migration from node-triton) AND XDG `~/.config/triton/` for new users
2. **CLI Strategy**: Keep both CLIs - cloudapi-cli for raw API access, new triton CLI for user-friendly experience
3. **Authentication**: Full support for both SSH agent integration AND file-based key loading

## New Crate Structure

```
libs/
└── triton-auth/               # HTTP Signature authentication library
    └── src/
        ├── lib.rs             # Public API exports
        ├── signature.rs       # HTTP Signature generation
        ├── key_loader.rs      # SSH key loading from files
        ├── agent.rs           # SSH agent integration
        └── fingerprint.rs     # MD5 fingerprint utilities

cli/
└── triton-cli/                # User-friendly CLI
    └── src/
        ├── main.rs
        ├── config/            # Profile/config management
        │   ├── mod.rs
        │   ├── profile.rs
        │   └── paths.rs       # ~/.triton + XDG support
        ├── commands/          # Command implementations
        │   ├── mod.rs
        │   ├── profile.rs
        │   ├── env.rs
        │   ├── info.rs
        │   ├── instance/      # Instance subcommands
        │   │   ├── mod.rs
        │   │   ├── list.rs
        │   │   ├── get.rs
        │   │   ├── create.rs
        │   │   ├── ssh.rs
        │   │   └── ...
        │   ├── image.rs
        │   ├── key.rs
        │   ├── network.rs
        │   ├── fwrule.rs
        │   ├── vlan.rs
        │   ├── package.rs
        │   ├── volume.rs
        │   ├── account.rs
        │   └── rbac.rs
        └── output/            # Formatting (table/json)
            ├── mod.rs
            ├── table.rs
            └── json.rs
```

## Authentication Design (Critical Path)

### HTTP Signature Format

From node-triton `lib/cloudapi2.js:168-208`:

```
Authorization: Signature keyId="/:account/keys/:md5_fingerprint",algorithm="rsa-sha256",signature=":base64_sig:"
```

Signed data:
```
date: <RFC2822 date>
(request-target): <method lowercase> <path>
```

### Progenitor Integration Pattern

From Nick Wilkens' experiment:
```rust
progenitor::generate_api!(
    spec = "../../../openapi-specs/generated/cloudapi-api.json",
    interface = Builder,
    tags = Merged,
    inner_type = AuthState,
    pre_hook_async = crate::auth::add_auth_headers,
);
```

## Phase 1 Commands (node-triton Parity)

### Profile Commands
- `profile list|get|create|edit|delete|set-current`

### Core Commands
- `env` - Generate shell exports
- `info` - Account overview

### Instance Commands (33+ subcommands)
- `instance list|get|create|delete|start|stop|reboot|resize|rename`
- `instance ssh|vnc|ip|wait|audit`
- `instance enable-firewall|disable-firewall|fwrules`
- `instance enable-deletion-protection|disable-deletion-protection`
- `instance nic list|get|add|remove`
- `instance snapshot list|get|create|delete`
- `instance disk list|get|add|resize|delete`
- `instance tag list|get|set|delete`
- `instance metadata list|get|set|delete`

### Image Commands (12 subcommands)
- `image list|get|create|delete|clone|copy|share|update|export|wait`

### Other Resource Commands
- `key list|get|add|delete`
- `network list|get|set-default|get-default` + `network ip list|get|update`
- `fwrule list|get|create|delete|enable|disable|update|instances`
- `vlan list|get|create|delete|update|networks`
- `package list|get`
- `volume list|get|create|delete|sizes`
- `account get|update|limits`
- `rbac user|role|policy|role-tag` subcommands

## Implementation Phases

### Phase 0: Foundation (Critical Path) - `phase0-auth.md`
1. Create `libs/triton-auth` crate
   - SSH key loading (ssh-key crate)
   - SSH agent integration (ssh-agent-client-rs)
   - HTTP Signature generation
   - MD5 fingerprint calculation
2. Update `cloudapi-client` with pre_hook_async
3. End-to-end auth test

### Phase 1: Core CLI - `phase1-cli-foundation.md`
1. Create `cli/triton-cli` crate structure
2. Profile management commands
3. `env` command

### Phase 2: Instance Management - `phase2-instance-commands.md`
1. Core ops: list/get/create/delete/start/stop/reboot
2. Utilities: ssh/ip/wait/audit
3. Firewall: enable/disable/fwrules
4. Sub-resources: nic/snapshot/tag/metadata/disk

### Phase 3: Images, Keys, Networks - `phase3-resources.md`
1. Image commands
2. Key commands
3. Network commands
4. Firewall rule commands
5. VLAN commands
6. Volume commands
7. Package commands
8. Account commands
9. Info command

### Phase 4: RBAC and Polish - `phase4-rbac-polish.md`
1. RBAC user/role/policy commands
2. Top-level shortcuts (triton ssh -> triton instance ssh)
3. Shell completions

## Key Dependencies

### triton-auth
```toml
ssh-key = { version = "0.6", features = ["ed25519", "rsa", "ecdsa", "encryption"] }
ssh-agent-client-rs = "0.3"
base64 = "0.22"
md-5 = "0.10"
chrono = "0.4"
secrecy = "0.10"
```

### triton-cli
```toml
triton-auth = { path = "../../libs/triton-auth" }
cloudapi-client = { path = "../../clients/internal/cloudapi-client" }
clap = { workspace = true, features = ["derive", "env"] }
directories = "5.0"
comfy-table = "7.0"
dialoguer = "0.11"
indicatif = "0.17"
```

## Critical Reference Files

| File | Purpose |
|------|---------|
| `apis/cloudapi-api/src/lib.rs` | 200+ endpoint definitions |
| `clients/internal/cloudapi-client/src/lib.rs` | TypedClient pattern |
| `target/node-smartdc-auth/lib/index.js` | HTTP Signature auth implementation |
| `target/node-smartdc-auth/lib/keypair.js` | Key parsing and signing |
| `target/node-smartdc-auth/lib/kr-agent.js` | SSH agent integration |
| `target/node-smartdc-auth/test/signers.test.js` | Test vectors for signature validation |
| `target/node-triton/lib/cloudapi2.js:168-208` | CloudAPI client auth usage |
| `target/node-triton/lib/config.js` | Profile/env var handling |
| `target/node-triton/lib/do_instance/` | Subcommand organization |

## Testing Strategy

- Unit tests: profile loading, key loading, signature generation, fingerprints
- Integration tests: auth e2e, profile management, CLI commands
- Test fixtures: sample profiles, test SSH keys (RSA/ECDSA/Ed25519), mock responses

## Environment Variable Support

| Variable | Fallback | Purpose |
|----------|----------|---------|
| `TRITON_PROFILE` | - | Use named profile |
| `TRITON_URL` | `SDC_URL` | CloudAPI URL |
| `TRITON_ACCOUNT` | `SDC_ACCOUNT` | Account name |
| `TRITON_KEY_ID` | `SDC_KEY_ID` | SSH key fingerprint |
| `TRITON_USER` | `SDC_USER` | RBAC sub-user |
| `TRITON_TLS_INSECURE` | `SDC_TLS_INSECURE` | Skip TLS verify |

## Profile Structure

```rust
pub struct Profile {
    pub name: String,
    pub url: String,
    pub account: String,
    pub key_id: String,
    pub insecure: bool,
    pub user: Option<String>,
    pub roles: Option<Vec<String>>,
    pub act_as_account: Option<String>,
}
```

## Phase Status

- [x] Phase 0: Foundation (triton-auth, client update)
- [x] Phase 1: Core CLI (structure, profiles, env)
- [x] Phase 2: Instance commands
- [ ] Phase 3: Resource commands (images, keys, networks, etc.)
- [ ] Phase 4: RBAC and polish
