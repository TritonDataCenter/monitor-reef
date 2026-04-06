<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

Copyright 2026 Edgecast Cloud LLC.
-->

# tritonadm: Rust Successor to sdcadm

## Motivation

`sdcadm` is a Node.js CLI that administers Triton datacenters — managing
service images, provisioning instances, configuring platforms, and
orchestrating updates. As Triton services migrate to Rust in this monorepo,
having the admin tool here too brings concrete benefits:

- **Shared types and clients**: tritonadm uses the same API client crates
  (vmapi-client, etc.) as the services it manages, eliminating type
  drift between admin tooling and the services themselves.
- **Shared build infrastructure**: ships as a binary alongside other Rust
  CLIs in the monorepo, using the same Cargo workspace, CI, and zone
  image pipeline.
- **Single dependency chain**: no separate Node.js runtime or npm
  dependency tree to maintain on the headnode.

tritonadm runs on the headnode with the same constraints as sdcadm: direct
access to the admin network, local SAPI, IMGAPI, CNAPI, VMAPI, NAPI, and
PAPI endpoints.

## Command Surface Area

The CLI scaffold is complete with all commands stubbed. Every command
currently prints "not yet implemented" and exits. Commands will be
implemented incrementally as their underlying API clients become available.

### Top-level commands (11)

| Command | Description |
|---------|-------------|
| `avail` | Display images available for update |
| `check-config` | Check SAPI config versus system reality |
| `check-health` | Check that services or instances are up |
| `completion` | Output shell completion code (implemented) |
| `create` | Create instances of an existing service |
| `default-fabric` | Initialize a default fabric for an account |
| `instances` | List all service instances |
| `rollback` | Rollback services and instances |
| `self-update` | Update tritonadm itself |
| `services` | List all services |
| `update` | Update services and instances |

### Subcommand groups (5)

| Group | Subcommands | Count |
|-------|-------------|-------|
| `channel` | list, set, unset, get | 4 |
| `dc-maint` | start, stop, status | 3 |
| `platform` | list, usage, install, assign, remove, avail, set-default | 7 |
| `post-setup` | cloudapi, common-external-nics, underlay-nics, ha-binder, ha-manatee, fabrics, dev-headnode-prov, dev-sample-data, docker, cmon, cns, volapi, logarchiver, kbmapi, prometheus, grafana, firewall-logger-agent, manta, portal | 19 |
| `experimental` | avail, update, info, update-agents, update-other, update-gz-tools, add-new-agent-svcs, update-docker, install-docker-cert, fix-core-vm-resolvers, cns, nfs-volumes, remove-ca, dc-maint | 14 |

**Total**: 16 top-level entries (11 commands + 5 groups), 47 subcommands.

## Architecture

tritonadm lives in `cli/tritonadm/` in the monorepo and follows the same
patterns as `triton-cli`:

- **CLI parsing**: clap with `derive` macros, subcommand enums in
  `src/commands/` modules
- **API clients**: uses existing and future Progenitor-generated clients
  from `clients/internal/` (vmapi-client exists; others will be added)
- **Monorepo integration**: shares the Cargo workspace, formatting, lint,
  and audit targets

Most tritonadm commands need multiple internal Triton APIs. The core set:

| API | Used by |
|-----|---------|
| **SAPI** | Nearly everything — service definitions, instance metadata, config |
| **IMGAPI** | avail, update, post-setup (image lookup and import) |
| **CNAPI** | platform, create (server selection) |
| **VMAPI** | instances, create, check-health, post-setup |
| **NAPI** | post-setup (NIC provisioning), default-fabric |
| **PAPI** | post-setup, create (package lookup) |

## Implementation Strategy

**Stub-first**: the entire command tree is scaffolded. Every command has a
clap definition, help text, and a `not_yet_implemented` exit path. This
means `tritonadm --help`, `tritonadm post-setup --help`, and shell
completions all work today.

**Implement incrementally**: commands are implemented as their API clients
become available. Each command implementation is a self-contained change
that adds real behavior to one stub.

**Priority order** (based on operational need):

1. `post-setup portal` — first real command, drives initial API client work
2. `services` / `instances` — read-only listing, useful for validation
3. `check-health` — operational monitoring
4. `post-setup grafana` / `post-setup prometheus` — observability stack
5. `update` — the core workflow, likely the most complex command

## First Target: `post-setup portal`

The portal is a Rust+React web UI for Triton (see
`docs/design/portal-build-and-deploy.md` in mariana-trench). `post-setup
portal` creates the SAPI service definition and provisions the first
instance, following sdcadm's established `post-setup grafana` pattern
(AddServiceProcedure + EnsureNicOnInstancesProcedure).

### What it does

1. Check if a `portal` service already exists in SAPI — bail if so
2. Look up the latest `user-portal` image in IMGAPI (or use `--image` flag)
3. Look up the `sdc_1024` package in PAPI
4. Get the admin and external network UUIDs from NAPI
5. Create the SAPI service definition (name=portal, type=vm, params with
   image, package, networks, billing_tag, firewall_enabled)
6. SAPI automatically provisions the first instance on the headnode
7. Wait for the VM to reach "running" state (poll VMAPI)
8. Ensure the instance has a NIC on the external network

### Service configuration

| Property | Value |
|----------|-------|
| Service name | `portal` |
| Image name | `user-portal` |
| Package | `sdc_1024` (1 GB — single Rust binary) |
| Networks | admin + external (primary) |
| Delegated dataset | No (stateless) |
| Firewall | Enabled (external-facing) |

### APIs needed

| API | Operations |
|-----|------------|
| **SAPI** | ListServices, CreateService, ListInstances |
| **IMGAPI** | ListImages (filter by name=user-portal) |
| **PAPI** | ListPackages (filter by name=sdc_1024) |
| **NAPI** | ListNetworks (filter by name) |
| **VMAPI** | GetVm (poll for running state) |

## API Client Strategy

Every internal Triton API gets the full trait-based pipeline: API trait
crate (`apis/`) → OpenAPI spec (`openapi-specs/generated/`) →
Progenitor-generated client (`clients/internal/`). This is the same
pattern used by cloudapi-api and vmapi-api.

This is intentional even for APIs that don't yet have formal OpenAPI
specs. Writing the Dropshot API trait for each internal API means:

- We build toward correct, validated specs from day one
- Every new tritonadm command exercises and validates the trait
- When we rewrite those Node.js services in Rust, the trait is already
  there as the target interface
- Clients are always generated, never hand-written, so they stay in sync

The one exception is `jira-client`, which uses a hand-written client
because JIRA is a large external API we don't control. Internal Triton
APIs we own should always get the full treatment.

**What exists today:**

| API | Trait crate | Client | Status |
|-----|-------------|--------|--------|
| VMAPI | `vmapi-api` | `vmapi-client` | Complete |
| CloudAPI | `cloudapi-api` | `cloudapi-client` | Complete |
| SAPI | — | — | Needed for post-setup portal |
| IMGAPI | — | — | Needed for post-setup portal |
| NAPI | — | — | Needed for post-setup portal |
| PAPI | — | — | Needed for post-setup portal |
| CNAPI | — | — | Needed for platform, create |

**Client crates live in `clients/internal/`**, following the monorepo
convention. Each API trait starts with the endpoints needed by the
current command, and grows as more commands are implemented.

## Open Questions

1. **Replacement or coexistence?** Should tritonadm eventually replace
   sdcadm entirely, or coexist long-term? Replacing sdcadm means
   implementing every command; coexisting means operators need to know
   which tool handles what.

2. **Self-update mechanism**: sdcadm updates itself via a zone image in
   IMGAPI. Does tritonadm ship as part of a zone image, as a standalone
   binary in the GZ, or bundled with another service's image?

3. **Installation location**: Should tritonadm be installed directly in
   the GZ (like sdcadm) or run from a zone? GZ installation gives direct
   access to platform management and CN operations. Zone installation is
   cleaner but limits some operations.

4. **Operator migration path**: Operators are used to sdcadm. Options
   include compatibility aliases (`sdcadm` -> `tritonadm`), a wrapper
   script, or simply documenting the transition.
