<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

Copyright 2026 Edgecast Cloud LLC.
-->

# Triton vNext Architecture

The architecture is built around one control-plane truth, one node-local
actuator, and one VPC dataplane. Users and operators write intent to `tritond`.
`tritonagent` realizes that intent on SmartOS compute nodes. Proteus enforces
network policy at the VM port and edge.

```mermaid
flowchart TD
    User[User CLI / Portal / SDK] --> API[tritond API]
    Operator[tcadm / Admin UI] --> API

    API --> FDB[(FoundationDB)]
    API --> Auth[Auth / Cedar / Audit]
    API --> Jobs[Scheduler / Job Queue]
    API --> NetIntent[VPC Intent]
    NetIntent --> Compiler[Proteus Blueprint Compiler]

    Jobs --> Agent[tritonagent on each CN]
    Compiler --> Agent

    Agent --> Vmadm[vmadm / bhyve lifecycle]
    Agent --> Proteus[Proteus kernel driver]
    Agent --> EdgeRun[fhrun / firehyve edge]
    Agent --> Realized[Realized State Reports]

    Vmadm --> VM[bhyve tenant VM]
    Proteus --> Port[VPC Port: firewall / route / overlay]
    EdgeRun --> Edge[edge-agent microVM]
    Edge --> NAT[NAT Gateway / Floating IP]
    Realized --> API
```

## Control loop

```mermaid
sequenceDiagram
    participant U as User or operator
    participant D as tritond
    participant F as FoundationDB
    participant A as tritonagent
    participant P as Proteus
    participant V as vmadm / bhyve
    participant E as firehyve edge

    U->>D: Create or change resource
    D->>F: Store desired state
    D->>F: Enqueue CN job
    A->>D: Claim job
    A->>V: Apply VM lifecycle
    A->>P: Apply VPC port blueprint
    A->>E: Apply NAT / FIP manifest when needed
    A->>D: Report accepted / applied / failed generation
    D->>F: Store realized state and audit
    U->>D: Read desired + realized state
```

The key product contract is desired state to realized state:

- `tritond` stores what should exist.
- `tritond` derives jobs and network blueprints from that intent.
- `tritonagent` applies those jobs locally.
- Proteus and firehyve edge microVMs carry packet behavior.
- The API, CLI, and UI show both the requested state and the applied state.

## Component boundaries

| Component | Owns | Does not own |
|---|---|---|
| `tritond` | API, auth, audit, scheduling, FDB metadata, resource invariants, desired state, job creation. | Direct packet forwarding, direct `vmadm` execution, UI-only workflows. |
| `tritonagent` | CN registration, heartbeats, job claiming, VM actuation, Proteus apply, edge process supervision, realized-state reporting. | Global scheduling decisions, tenant authorization policy, durable metadata truth. |
| Proteus | Port-local packet policy, distributed firewall, routing, overlays, generic dumps, trace/debug, kernel/userland control path. | Product resources such as tenants, projects, images, or operator workflows. |
| firehyve / `fhrun` | Running small Linux payloads as supervised microVMs for edge services. | The v1 end-user VM lifecycle, which remains bhyve-first. |
| Mariana UI | Admin and user workflows over the vnext API. | Private control paths around `tritond`. |

## V1 packet path

```mermaid
flowchart LR
    VM1[bhyve VM NIC] --> P1[Proteus Port]
    P1 --> EW[East / West VPC Path]
    EW --> P2[Proteus Port]
    P2 --> VM2[bhyve VM NIC]

    P1 --> Route[Route / NAT Decision]
    Route --> EdgeSouth[Edge South NIC]
    EdgeSouth --> Edge[firehyve edge-agent]
    Edge --> EdgeNorth[Edge North NIC]
    EdgeNorth --> ToR[Provider / Customer ToR]

    FIP[Floating IP] --> EdgeNorth
    Edge --> DNAT[1:1 FIP / DNAT]
    DNAT --> P1
```

V1 should make this path boring and inspectable:

- same-VPC VM-to-VM traffic works;
- firewall policy can allow or deny it;
- private subnet traffic can egress through NAT;
- a floating IP can provide ingress to a VM;
- operators can see which generation was requested, accepted, applied, or
  failed.

## Why this shape

Triton vNext keeps the number of load-bearing services small. The control plane
is stateless around FoundationDB. Compute nodes are replaceable because their
local truth is derived from `tritond`. The dataplane is Triton-owned so VPC,
security, and edge behavior can evolve with the product instead of being a
permanent fork of another system.
