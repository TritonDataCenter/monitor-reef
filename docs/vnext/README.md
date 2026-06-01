<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

Copyright 2026 Edgecast Cloud LLC.
-->

# Triton vNext

Triton vNext is the product direction for Triton Cloud: an operator-ready
private cloud built around a Rust control plane, SmartOS compute nodes,
FoundationDB metadata, Triton-owned VPC networking, and a small set of
clear operator and user surfaces.

This is not a proof of concept. The current workspace already demonstrates
large parts of the path: `tritond`, `tritonagent`, `tcadm`, FoundationDB-backed
state, tenant/project resources, instance records, node registration, audit,
and the Proteus virtual-network dataplane. The remaining v1 work is turning
those pieces into an installable, diagnosable product with a complete VM +
VPC + NAT/FIP path.

## Start here

- [Architecture](./architecture.md) - the visual system map, component
  boundaries, and desired-state loop.
- [v1 Path](./v1-path.md) - the proposed product bar, acceptance path, and
  places where product input is needed.
- [Gist Summary](./gist.md) - a concise standalone version with diagrams.

## Operator runbooks

Field-tested install / operate notes for the parts that are live today.

- [`operating-tritond.md`](./operating-tritond.md) — tritond bootstrap
  config + FDB-backed cluster settings.
- [`admin-webui-install.md`](./admin-webui-install.md) — install the
  published admin webui binary, expose it, hit the `/v2/auth/login`
  fix path.
- [`s3-data-plane-workspace-gate.md`](./s3-data-plane-workspace-gate.md) —
  Phase D / Phase 2 S3 data-plane workspace isolation walkthrough +
  verify scripts.
- [`phase-d-s3-workspace-isolation.md`](./phase-d-s3-workspace-isolation.md) —
  admin-plane half of the same arc.

## Component references

These links point at the current workspace sources of truth for each major
piece. Some are sibling repos or workspace-level planning docs rather than
files inside `monitor-reef`.

| Area | Reference |
|---|---|
| Current control-plane status | [`../../../STATUS.md`](../../../STATUS.md) |
| Canonical design notes | [`../../../DESIGN.md`](../../../DESIGN.md) |
| Product map | [`../../../triton-cloud.md`](../../../triton-cloud.md) |
| `monitor-reef` control-plane repo | [`../../README.md`](../../README.md) |
| API/client workflow | [`../tutorials/api-workflow.md`](../tutorials/api-workflow.md) |
| Checked-in client generation | [`../design/checked-in-client-generation.md`](../design/checked-in-client-generation.md) |
| Proteus dataplane | [`../../../proteus/README.md`](../../../proteus/README.md) |
| Proteus architecture | [`../../../proteus/docs/architecture.md`](../../../proteus/docs/architecture.md) |
| Proteus RFD bundle | [`../../../rfd/00001/README.md`](../../../rfd/00001/README.md) |
| firehyve / `fhyve` brand work | [`../../../firehyve/docs/README.md`](../../../firehyve/docs/README.md) |
| Mariana UI/services repo | [`../../../mariana-trench/README.md`](../../../mariana-trench/README.md) |

## One-screen product map

| Part | Product role |
|---|---|
| `tritond` | Control-plane API, scheduler, auth, audit, metadata owner, and desired-state authority. |
| FoundationDB | Durable metadata substrate; `tritond` remains stateless around it. |
| `tritonagent` | Per-compute-node actuator for VM lifecycle, Proteus port apply, health, and realized-state reporting. |
| Proteus | Triton-owned VPC dataplane: distributed firewall, routing, overlay, NAT/FIP policy, and trace/debug surfaces. |
| firehyve / `fhrun` | v1 edge microVM runtime for north/south NAT and floating-IP forwarding; later a normal `fhyve` tenant runtime. |
| `tcadm` | Operator CLI for bootstrap, node approval, diagnosis, audit, tenant administration, and support workflows. |
| User CLI / SDK / portal | End-user surface for tenants, projects, VPCs, subnets, images, SSH keys, instances, disks, and IPs. |
| Admin UI | Operator-facing view over `tritond`; no classic CloudAPI/VMAPI/NAPI bypass. |

## Status language

These docs use three labels:

- **Demonstrated** - code, tests, or lab work exists in this workspace.
- **Proposed v1** - needed for the first product-ready release, but not fully
  implemented yet.
- **Future** - important direction, but not required for v1.

## Input wanted

The docs are meant to invite focused product feedback. The highest-value
questions are:

- What is the smallest v1 that still feels like Triton Cloud, not a demo?
- Which operator workflows must be excellent on day one?
- Which tenant workflows must be present before users can evaluate the product?
- How much firehyve is v1 edge infrastructure versus v1 tenant runtime?
- Which UI screens are required to explain provisioning and networking state?
- Which future capabilities must shape v1 data models now to avoid migration
  pain later?
