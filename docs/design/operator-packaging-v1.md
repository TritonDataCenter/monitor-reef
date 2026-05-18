<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

Copyright 2026 Edgecast Cloud LLC.
-->

# Operator Experience and Packaging v1

> Owner: Agent G (operator experience and packaging).
> Status: design, not yet implemented.
> Scope: install, diagnose, audit, and operate Triton Cloud v1 on a
> real fleet without reading source code.
> Out of scope: VPC resource model (Agent A), Proteus internals
> (Agent B), CN actuation internals (Agent C), firehyve / fhrun
> internals (Agent D), end-user UX (Agent F), Kelp (Agent H).

This document defines the v1 operator-facing surface area: the set
of artifacts an operator installs, the SMF services they manage,
the diagnostics they run when something is wrong, the admin UI
pages they navigate when a customer files a ticket, the packaging
test strategy that catches packaging-only regressions, the support
bundle they hand to engineering, and the explicit handoffs to the
other agents.

## 1. Goals and non-goals

### v1 goals

* Triton Cloud is installable end-to-end on SmartOS by an operator
  who is not a Triton developer.
* Every long-lived component (`tritond`, `tritonagent`, the Proteus
  driver, firehyve / fhrun, the edge-agent microVM, FoundationDB)
  has a known package, a known SMF or installer footprint, a known
  health-check endpoint or probe, and a known reset/restart path.
* `tcadm doctor` answers the seven questions enumerated in
  `triton-cloud.md` §"Packaging and Operations" without an
  operator opening source.
* The admin UI consumes only the vnext `tritond` API (no classic
  CloudAPI/VMAPI/NAPI/FWAPI/CNS proxy) and shows desired and
  realized state side-by-side for the resources that have both.
* When `tritond` cannot reach a CN, when a CN cannot apply a
  blueprint, or when a NAT path is unhealthy, an operator can
  produce an attributable, single-cause explanation in under five
  minutes from a single command.

### v1 non-goals

* Multi-controller HA installer. v1 ships single-controller; the
  config shape and SMF service shape stay HA-compatible so the
  v2 installer can roll out in place.
* Rolling upgrades / automated schema migration. v1 ships a stop /
  package-replace / start path, documented and tested.
* Long soak / chaos testing harness — Phase 11 (per
  `proteus/STATUS.md`) item.
* Postmortem auto-collection from CN crash dumps. v1 documents
  manual collection.
* In-tree forks of FoundationDB or any third-party dependency.
  We package upstream artifacts where possible.
* Auto-upload of support bundles. The operator chooses whether to
  share.

## 2. The seven operator questions

These are the bar `tcadm doctor` and the admin UI must clear. Every
new operator-facing surface answers at least one. They come from
`triton-cloud.md` §"Packaging and Operations".

1. Is the control plane healthy?
2. Which CNs are available?
3. Why did this VM not provision?
4. Which Proteus generation is applied to this NIC?
5. Why does this VM not have egress?
6. Is NAT healthy?
7. Is this failure user-caused, capacity-caused, or platform-caused?

## 3. v1 installable artifacts

The v1 operator deals with the following artifact set. Each row
specifies who builds it, where it lands at install time, and which
SMF service (if any) supervises it. Anything marked **v1** must
ship for v1 acceptance; anything marked **v1 doc-only** ships
documentation but not yet a binary package (because it depends on
upstream Git resolution or another agent's gate).

| Artifact | Built by | Install location | SMF service | Status |
|---|---|---|---|---|
| `tritond` | `monitor-reef` cargo workspace, illumos amd64 | `/opt/tritond/bin/tritond` | `svc:/site/tritond:default` | v1 |
| `tcadm` | same workspace | `/opt/tritond/bin/tcadm` | (CLI; no service) | v1 |
| `tritonagent` | same workspace | `/opt/tritonagent/bin/tritonagent` | `svc:/site/tritonagent:default` | v1 (manifest exists, needs config templating — slice G-3) |
| Proteus driver (`proteus` kmod) | `proteus` workspace | `/usr/kernel/drv/amd64/proteus` + `/etc/devlink.tab` (`/dev/proteus`) | (kmod; loaded via `modload`, dep'd on by tritonagent) | v1 (Slice 6.x done; packaging follow-up) |
| `proteusadm` | `proteus` workspace | `/opt/proteus/bin/proteusadm` | (CLI) | v1 |
| FoundationDB server (single node) | upstream FDB 7.3.x .deb / .ips | `/opt/fdb` | `svc:/site/fdb:default` | v1 (we package upstream) |
| FDB client lib (`libfdb_c.so`) | upstream | `/opt/fdb/lib/libfdb_c.so` | (shared lib only) | v1 |
| firehyve (VMM binary) | `firehyve` workspace (linux-loader build issue per Agent D) | `/opt/firehyve/bin/firehyve` | (no direct SMF; launched by `fhrun`) | v1 doc-only (blocked: build error in `firehyve/src/{boot,pvh}.rs`) |
| `fhrun` (host launcher) | `firehyve/tools/fhrun` | `/opt/firehyve/bin/fhrun` | (no direct SMF; spawned by tritonagent or per-edge SMF) | v1 |
| `fhrun-init` (guest PID 1) | `firehyve/tools/fhrun-init` | inside the edge initramfs | n/a (guest binary) | v1 |
| edge-agent (Linux NAT/FIP forwarder) | `firehyve/tools/fhrun/examples/edge-agent` | inside the edge image | n/a (guest binary) | v1 (v0 dataplane; per Agent D) |
| Edge Linux kernel | upstream LTS, vendored | `/opt/firehyve/kernels/<version>/bzImage` | n/a | v1 |
| Edge initramfs builder | bundled with `fhrun` | n/a (built at runtime) | n/a | v1 |
| Admin UI (`mariana-trench/triton-admin`) | `mariana-trench` workspace, Vite + tritond-client | `/opt/triton-admin/web` | `svc:/site/triton-admin:default` (static fileserver) | v1 (port classic UI to vnext APIs — slice G-9..G-13) |

We do not ship: classic CloudAPI, classic VMAPI, classic NAPI,
classic FWAPI, sdc-headnode tooling, Manta storage. v1 has no
manta-storage substrate; the audit chain stays on FDB until the
manta-storage Phase 0 substrate ships (Locked Decision #7).

### 3.1 SmartOS zone or GZ?

v1 runs `tritond`, FoundationDB, and `triton-admin` inside a single
LX-branded zone on the headnode (Locked Decision #4 keeps FDB as
the only metadata backend; LX gives us the existing upstream
artifacts without a custom SmartOS port). `tritonagent` runs in
the global zone of every CN because it must call `vmadm`,
`/dev/proteus`, `dladm`, `fhrun`, and FDB clients.

The headnode-zone vs CN-GZ split is the design seam for HA later:
v2 turns the single zone into N zones behind a dataplane LB, with
the `tritond` config file exposing `bind_address`, `fdb_cluster_file`,
and `peer_endpoints` already today.

### 3.2 The three test environments

v1 testing covers three environments, each with its own gate. Every
slice that lands must declare which environments it has been run
in.

| Env | What | Where | When it runs |
|---|---|---|---|
| **Workstation Docker** | tritond + FDB single-node | `make docker-up` on macOS / Linux | every slice; matches `monitor-reef/docker-compose.yml` |
| **SmartOS lab fleet** | tritond + tritonagent + FDB on `.10` / `.41` | the dev hosts in `STATUS.md` | network/CN/firehyve/Proteus slices |
| **Single-node fresh install** | the v1 product as an external operator runs it | a clean SmartOS image, no developer state | v1 acceptance, plus per-package release |

## 4. SMF service inventory and gaps

### 4.1 Today

Only `services/tritonagent/smf/tritonagent.xml` exists in-tree.
That manifest:

- enables itself off (`enabled="false"`); operator must
  `svcadm enable` after first boot.
- depends on `svc:/milestone/network:default` and
  `svc:/system/filesystem/local`.
- start exec is `/usr/bin/ctrun -l child -o noorphan
  /opt/tritonagent/bin/tritonagent &`.
- runs as `root:root`.
- hardcodes `TRITONAGENT_ENDPOINT=http://10.199.199.10:8080`,
  `TRITONAGENT_CREDENTIAL_PATH=/var/lib/tritonagent/credentials`,
  `TRITONAGENT_POLL_INTERVAL_SECS=5`.

### 4.2 v1 gaps

| Service | Today | v1 gap | Slice |
|---|---|---|---|
| `svc:/site/tritonagent:default` | manifest exists, hardcoded endpoint | move endpoint to a property group; default to discovery via `/etc/tritonagent.conf` | G-3 |
| `svc:/site/tritonagent:default` | runs as root | document why (vmadm/Proteus/fhrun all need root); add `working_directory` mode 0700 | G-3 |
| `svc:/site/tritond:default` | none | new manifest mirroring tritonagent, env: `TRITOND_BIND_ADDRESS`, `TRITOND_FDB_CLUSTER_FILE`, `RUST_LOG`; depends on `svc:/site/fdb:default` | G-4 |
| `svc:/site/fdb:default` | none (upstream FDB ships a SysV init / systemd unit, not SMF) | new manifest wrapping `fdbserver`; depends on `network` + `filesystem`; produces `/etc/fdb.cluster` | G-5 |
| `svc:/site/triton-admin:default` | none | new manifest serving the built admin UI bundle on `:8443` (TLS via stored cert, optional self-signed for v1) | G-12 |
| `svc:/site/triton-edge:<edge-id>` | none | per-edge multi-instance manifest invoking `fhrun` with a generated manifest; depends on Agent D Git resolution + Agent E manifest contract | v1 doc-only (slice G-14, gated) |

Every manifest ships a matching `tcadm doctor` check (§5) so SMF
state and tritond's view of the world can be compared without
reading `svcs -xv` and `svcs -p` by hand.

### 4.3 Service-startup ordering

```
filesystem/local + network/default
        |
        v
   svc:/site/fdb:default       (single-node single-coord)
        |
        v
  svc:/site/tritond:default     (waits on /etc/fdb.cluster)
        |
        v
 svc:/site/triton-admin:default (HTTP fileserver; talks to tritond)

CN-side:
  svc:/site/tritonagent:default (depends on filesystem + network +
                                 modload of /dev/proteus when present)
```

`svc:/site/triton-edge:<edge-id>` is started by `tritonagent` on
the CN that hosts the edge VM, not by the headnode. The instance
identifier mirrors Agent A's `EdgeCluster.id`.

## 5. `tcadm doctor` check inventory

`tcadm doctor` is the operator's first-call diagnostic. It is
**read-only by default**, prints to a single terminal, exits non-zero
when anything is failing, and emits structured JSON when given
`--json`. Every check returns one of `Ok`, `Warning(reason)`,
`Failure(reason)`, with an explicit `not_applicable` for
environments that legitimately don't have the resource (no Proteus
on the headnode zone, no firehyve on a CN that doesn't host an
edge).

The check inventory below is the operator contract: each check maps
to one of the seven questions in §2.

### 5.1 Control plane

| Check | Maps to | Reads | Failure shape |
|---|---|---|---|
| `tritond.health` | Q1 | `GET /v2/health` | unreachable, non-200, slow |
| `tritond.fdb_reachable` | Q1 | `tritond` health includes FDB ping (added in slice G-2) | FDB up but tritond can't talk to it |
| `tritond.audit_chain_head` | Q1 | `GET /v2/audit/head` | chain stuck, head unreachable |
| `tritond.bootstrap_complete` | Q1 | bootstrap state via `/v2/bootstrap-status` (added in G-2) | first-run not finished, root not minted |

### 5.2 FoundationDB

| Check | Maps to | Reads | Failure shape |
|---|---|---|---|
| `fdb.cluster_file_present` | Q1 | `/etc/fdb.cluster` exists, mode 0644 | missing, world-writable |
| `fdb.coordinators_reachable` | Q1 | `fdbcli --exec 'status minimal'` | partial reachability, no coordinators |
| `fdb.database_available` | Q1 | `fdbcli --exec 'status'` | unavailable, recovering, healing |
| `fdb.client_lib_loaded` | Q1 | `which libfdb_c.so` + version | mismatch (server 8.0 vs client 7.3, see STATUS.md SmartOS-host notes) |

### 5.3 Compute nodes

| Check | Maps to | Reads | Failure shape |
|---|---|---|---|
| `cn.list` | Q2 | `GET /v2/cns` | none registered |
| `cn.heartbeat_freshness` | Q2, Q7 | `Cn.last_seen` | stale > heartbeat × 3 |
| `cn.pending_approvals` | Q2 | `GET /v2/cns?state=pending` | unread claim codes (operator hint) |
| `cn.disabled_reasons` | Q2 | `Cn.last_status` | summarize disabled CNs and reasons |

### 5.4 Compute-node host capabilities (per CN, requires bound key)

| Check | Maps to | Reads | Failure shape |
|---|---|---|---|
| `cn_host.smartos_version` | Q3, Q7 | sysinfo via tritonagent status payload | unsupported platform image |
| `cn_host.bhyve_capable` | Q3 | sysinfo `Bhyve_Capable` | hardware lacks VT-x/AMD-V |
| `cn_host.proteus_dev_present` | Q3, Q4 | `stat /dev/proteus` reported by tritonagent | kmod not loaded; devlink missing |
| `cn_host.fdb_client_lib` | Q3, Q7 | tritonagent reports library path / version | absent or incompatible |
| `cn_host.disk_capacity` | Q7 | sysinfo `Disk_Pool_Size_Bytes` | undersized for image cache |

These checks need the heartbeat / status payload to carry the
fields. **Dependency on Agent C** in §8.

### 5.5 Proteus

| Check | Maps to | Reads | Failure shape |
|---|---|---|---|
| `proteus.kmod_loaded` | Q4 | tritonagent reports modinfo | not loaded; wrong build |
| `proteus.api_version` | Q4 | `proteusadm --backend kernel version` (tritonagent runs it) | mismatch with `proteus-api::CURRENT` |
| `proteus.networks_listed` | Q4 | `proteusadm networks` schema version | schema drift between control plane and driver |
| `proteus.applied_generation` | Q4 | per-VPC, per-NIC `applied_generation` from realized-state (Agent A H-1 + Agent C report) | desired > applied for > N seconds |

### 5.6 VPC desired vs realized

| Check | Maps to | Reads | Failure shape |
|---|---|---|---|
| `vpc.summary` | Q4, Q5 | `GET /v2/tenants/{t}/projects/{p}/vpcs` + realized-state | any VPC with desired_generation > applied_generation past threshold |
| `vpc.subnet_alloc_pressure` | Q3, Q5 | NIC IP allocation density per subnet | > 80% utilization warns |
| `vpc.orphan_resources` | Q7 | scan tenants for projects/VPCs/subnets with no parent | reports tenant deletion gap (deferred per STATUS.md) |

### 5.7 NAT / FIP / Edge

| Check | Maps to | Reads | Failure shape |
|---|---|---|---|
| `nat.gateways_attached` | Q5, Q6 | `GET /v2/tenants/{t}/projects/{p}/nat-gateways` (Agent A H-2..H-3) | NAT GW with no attached subnet routes |
| `nat.edge_health` | Q6 | edge-agent `/v0/status` polled by tritonagent (Agent D contract) | edge unreachable or last-applied stale |
| `fip.attachments` | Q5 | floating IPs whose `attached_to` NIC no longer exists | inconsistent state |
| `fip.advertisement` | Q6 | reserved for BGP/Maglev — v1 reports "n/a" | not applicable in v1 |

### 5.8 Image / runtime

| Check | Maps to | Reads | Failure shape |
|---|---|---|---|
| `image.public_count` | Q3 | `GET /v2/images` | empty catalog |
| `image.checksum_drift` | Q3, Q7 | future: cross-CN image presence check | not in v1 |
| `image.brand_compatibility` | Q3 | image manifest declares `brand=bhyve` | mismatch between requested image and CN bhyve capability |
| `runtime.firehyve_kernels_present` | Q6 | `/opt/firehyve/kernels/<version>/bzImage` | edge can't boot |

### 5.9 Output shape

```
$ tcadm doctor
[ ok  ] tritond.health                     200 in 4ms
[ ok  ] tritond.fdb_reachable              7.3.x client → 7.3.x server
[ ok  ] fdb.database_available             healthy, single-node
[ warn] cn.pending_approvals               2 CNs awaiting approval; run `tcadm cn list --state pending`
[ fail] proteus.applied_generation (cn-A)  desired=14, applied=12, last=180s ago
[ ok  ] vpc.summary                        12 VPCs, all in sync
[ ok  ] nat.gateways_attached              3 NAT GWs, all attached
[ skip] fip.advertisement                  not applicable in v1 (BGP deferred)

1 failure, 1 warning, 24 ok, 1 skipped.
```

`--json` emits a stable schema documented in
`apis/tritond-api/src/types.rs::DoctorReport` (added in slice G-2).

## 6. Admin UI v1 pages

The mariana-trench `triton-admin` SPA is ported page-by-page from
classic Triton to the vnext API. Every page consumes
`tritond-client` exclusively. Pages that have no vnext analogue are
removed; pages that have a vnext analogue are renamed and rewired.

### 6.1 Page list

| Page | Route | Replaces classic | Uses vnext API | Slice |
|---|---|---|---|---|
| Fleet health | `/` (Dashboard) | classic dashboard | `/v2/health`, `/v2/cns`, `/v2/audit/head`, `/v2/jobs` | G-9 |
| Compute Nodes | `/cns` | classic compute-nodes | `/v2/cns`, per-CN show, approve, disable, auto-approve | G-10 |
| Tenants & Projects | `/tenants` | n/a (new) | `/v2/silos`, `/v2/silos/{}/tenants`, `/v2/tenants/{}/projects` | G-11 |
| Jobs | `/jobs` | n/a (new) | `/v2/jobs` (depends on Agent A or coordinator) | G-12 |
| Audit | `/audit` | n/a (new) | `/v2/audit/list`, `/v2/audit/{seq}`, `/v2/audit/verify` | G-13 |
| Images | `/images` | classic images | five Image scope endpoints | G-14 |
| VPCs / Networks | `/networks` | classic networks | tenant-project VPC + subnet endpoints + realized state | G-15 (depends on Agent A) |
| NAT / FIP / Edge | `/nat-edge` | n/a (new) | NAT GWs, FIPs, edge realized state | G-16 (depends on Agents A, D, E) |
| Failed operations | `/failed-operations` | n/a (new) | `/v2/jobs?state=failed`, audit cross-link | G-17 |

### 6.2 Pages removed in v1

The classic admin UI ships pages that have no vnext analogue or
are explicitly out of scope:

* `Packages` (classic Triton package catalog) — vnext uses
  flavor selection per Agent A H-?. Remove route + component.
* `Settings/System/CloudAPI` — classic CloudAPI is not in v1.
* `Settings/System/General/Advanced` — pre-vnext config; keep
  shell, fold into a single "tritond config" view.
* `VMs` — replaced by Tenants/Projects/Instances drill-down.
* `Activity Log` — superseded by Audit page.

### 6.3 Auth

Admin UI sits behind the existing `tcadm` auth model: HS256 JWT
login through `POST /v2/auth/login`, with cookie or
`Authorization: Bearer` from the SPA. Federated OIDC redirect
(Authorization Code Flow) is deferred (per STATUS.md "Authorization
Code Flow + browser redirects (OIDC) — UI work begins"); v1 admin
UI ships HS256 only.

## 7. Packaging and build test strategy

### 7.1 Per-artifact

| Artifact | Build cmd | Test cmd | Package shape |
|---|---|---|---|
| `tritond` | `cargo build --release -p tritond --features foundationdb` | `cargo test -p tritond` | `/opt/tritond/bin/tritond` + `/lib/svc/manifest/site/tritond.xml` |
| `tcadm` | `cargo build --release -p tcadm` | `cargo test -p tcadm` | `/opt/tritond/bin/tcadm` |
| `tritonagent` | `cargo build --release -p tritonagent` | `cargo test -p tritonagent -p tritond-cn-platform` | `/opt/tritonagent/bin/tritonagent` + `/lib/svc/manifest/site/tritonagent.xml` |
| Proteus driver | `proteus/scripts/build_kmod.sh` | `cargo test --workspace` in proteus | `/usr/kernel/drv/amd64/proteus` + `proteus.conf` + `/etc/devlink.tab` rule |
| `proteusadm` | `cargo build --release -p proteusadm` | `cargo test -p proteusadm` | `/opt/proteus/bin/proteusadm` |
| firehyve | `cargo build --release -p firehyve` (broken: linux-loader 0.13.2 imports — Agent D blocker) | `cargo test -p firehyve` | `/opt/firehyve/bin/firehyve` |
| `fhrun` | `cargo build --release -p fhrun` | `cargo test -p fhrun` | `/opt/firehyve/bin/fhrun` |
| `fhrun-init` | `cargo build --release -p fhrun-init --target x86_64-unknown-linux-musl` | guest-side smoke (Agent D) | embedded in initramfs |
| edge-agent | `cargo build --release` in `firehyve/tools/fhrun/examples/edge-agent` | manifest validation tests | embedded in edge image |
| Edge Linux kernel | upstream LTS; vendored via SHA-pinned download | n/a | `/opt/firehyve/kernels/<v>/bzImage` |
| Admin UI | `make frontend-build` in mariana-trench | `make frontend-typecheck` | static `/opt/triton-admin/web/` |

### 7.2 Per-environment gates

| Environment | Required gates | When |
|---|---|---|
| Per-commit (workstation) | `make format && make clippy && make openapi-check && cargo test --workspace` | every slice |
| Docker (`make docker-up && make docker-smoke`) | health + silo round-trip | every slice that touches tritond |
| SmartOS lab single-CN | `tritonagent register` round-trip; `tcadm doctor` fully green | every CN-touching slice |
| Single-node fresh install | the install runbook (§9) executed by hand | per release tag |

### 7.3 Versioning

Every artifact embeds its semver in the binary (`tritond --version`,
`tritonagent --version`, etc.). The admin UI displays
`tritond.version` from `/v2/health` (currently the daemon emits
`VERSION` from `Cargo.toml`). Mismatched UI/server versions surface
the existing `VersionNotification` component.

## 8. Support bundle and log collection

`tcadm support-bundle` produces a single tarball at
`/var/tmp/tritond-support-<timestamp>.tar.gz` containing:

* `tritond.health.json` — `/v2/health` snapshot
* `tritond.bootstrap.json` — bootstrap state
* `tritond.audit.head.json` — audit chain head + tail of recent
  events (no PII; see §8.1)
* `cns.json` — `GET /v2/cns` + per-CN status
* `jobs.json` — recent job state including failures (depends on
  jobs read endpoint — Agent A handoff)
* `fdb.status.json` — `fdbcli --exec 'status json'`
* `svcs.txt` — `svcs -xv | head -200`, `svcs -lp` for tritond /
  tritonagent / fdb / triton-admin / triton-edge instances
* `dladm.txt` — `dladm show-link`, `dladm show-vnic` (CN only)
* `proteus.txt` — `modinfo | grep proteus`,
  `proteusadm capabilities`, `proteusadm networks`,
  `dtrace -ln 'sdt:proteus::*'`
* `tcadm.doctor.json` — full `tcadm doctor --json`
* `manifest.txt` — package versions, kernel build, platform image,
  build host hostname, the `tcadm` config endpoint (no api-key)

The script is shipped as a `tcadm` subcommand so it cannot be
accidentally bypassed; it shells out to existing `tritond`,
`fdbcli`, `svcs`, `dladm`, `proteusadm` commands and never touches
private state.

### 8.1 Redaction policy (open question — coordinator decision)

Default redactions:

* Drop API keys, passwords, JWT signing keys, OIDC client secrets.
* Drop `RedactedString` fields client-side; server-side they are
  already not returned.
* Drop full audit-event payloads beyond the last 1000 entries
  unless `--include-full-audit` is given.
* Drop tenant user lists unless `--include-users` is given.

Open question: do we automatically redact tenant names? Operator
decision; default is "no, names are visible in audit anyway".

## 9. Single-node fresh install runbook

This is the v1 acceptance bar in §1. It is what an external
operator runs.

```text
# 1. Install platform
operator $ install SmartOS platform image with proteus driver baked in
operator $ pkgsrc install foundationdb-clients foundationdb-server

# 2. FDB
operator $ svccfg import /lib/svc/manifest/site/fdb.xml
operator $ svcadm enable site/fdb
operator $ fdbcli --exec 'configure new single ssd'

# 3. tritond
operator $ svccfg import /lib/svc/manifest/site/tritond.xml
operator $ svcadm enable site/tritond
operator $ tcadm bootstrap                         # health
operator $ tcadm configure --endpoint <host>:8080  # bootstrap root + login

# 4. admin UI
operator $ svccfg import /lib/svc/manifest/site/triton-admin.xml
operator $ svcadm enable site/triton-admin
operator $ open https://<host>:8443                # admin UI

# 5. CN approval (per CN)
cn-A    $ svccfg import /lib/svc/manifest/site/tritonagent.xml
cn-A    $ svcadm enable site/tritonagent
operator $ tcadm cn list --state pending
operator $ tcadm cn approve <claim-code>

# 6. networking
operator $ tcadm tenant project vpc create ...
operator $ tcadm tenant project vpc subnet create ...

# 7. user-side workflow handed to end-user CLI

# 8. health
operator $ tcadm doctor
[ all ok ]

# 9. when something is wrong
operator $ tcadm doctor              # narrows the problem
operator $ tcadm support-bundle      # if engineering needs context
```

## 10. Atomic commit queue

Numbered `G-N`; each is one commit. The first cluster ships
without dependencies on other agents; the later cluster gates on
A/C/D/E contract resolution. The `Reviewer` column names the
agent or coordinator whose handoff is on the line.

| # | Slice | Repo | Reviewer | Depends on |
|---|---|---|---|---|
| G-1 | `tcadm doctor` skeleton: read-only check trait, two checks (`tritond.health`, `cn.list`), human + JSON output | monitor-reef | coordinator | none |
| G-2 | `tritond` bootstrap-state read endpoint + `tritond.bootstrap_complete` doctor check | monitor-reef | coordinator | G-1 |
| G-3 | `tritonagent.xml`: move endpoint to property group, document property names, drop the `.10` lab default | monitor-reef | Agent C | none |
| G-4 | `tritond.xml` SMF manifest + install instructions in design doc | monitor-reef | coordinator | none |
| G-5 | `fdb.xml` SMF manifest wrapping `fdbserver` + `fdb.cluster_file_present` + `fdb.coordinators_reachable` doctor checks | monitor-reef | coordinator | G-1 |
| G-6 | tritonagent status payload: report `proteus_dev_present`, `bhyve_capable`, `fdb_client_lib`; mirror in `Cn.last_status` | monitor-reef | Agent C | G-3 |
| G-7 | `tcadm doctor cn-host`: per-CN host-capability checks reading G-6 fields | monitor-reef | Agent C | G-6 |
| G-8 | `tcadm doctor proteus`: `proteus.kmod_loaded`, `proteus.api_version`, `proteus.networks_listed` reading G-6 status | monitor-reef | Agent C | G-6 |
| G-9 | Admin UI: prune classic-only routes (`/packages`, classic settings); add `/tenants`, `/jobs`, `/audit` shells | mariana-trench | coordinator | none |
| G-10 | Admin UI: Compute Nodes page (vnext API only) | mariana-trench | coordinator | G-9 |
| G-11 | Admin UI: Tenants & Projects page | mariana-trench | coordinator | G-9 |
| G-12 | `tcadm jobs list/get` + Admin UI Jobs page (depends on a queue read endpoint — Agent A handoff) | both | Agent A | G-9 |
| G-13 | Admin UI: Audit page (uses existing `/v2/audit/*`) | mariana-trench | coordinator | G-9 |
| G-14 | Admin UI: Images page (multi-scope) | mariana-trench | coordinator | G-9 |
| G-15 | Admin UI: VPCs/Networks page (desired vs realized columns from Agent A H-1) | mariana-trench | Agent A | G-9 |
| G-16 | `tcadm doctor` NAT/FIP/edge: realized-state polling per Agent A H-2..H-3, Agent E NAT GW, Agent D edge `/v0/status` | monitor-reef | Agents A, D, E | G-15 |
| G-17 | Admin UI: NAT / FIP / Edge page; Failed Operations page | mariana-trench | Agents A, D, E | G-16 |
| G-18 | `tcadm support-bundle` subcommand: aggregator + redactor | monitor-reef | coordinator | G-1..G-17 (best-effort) |
| G-19 | Single-node install runbook automation: integration test that runs §9 against a fresh container | monitor-reef | coordinator | G-1..G-5, G-18 |
| G-20 | (deferred) firehyve packaging + edge SMF instance shape; gated on Agent D Git resolution | firehyve / monitor-reef | Agent D | Agent D Git resolution |

The first five slices (G-1..G-5) are unblocked by other agents and
make `tcadm doctor` and the headnode-zone install bootable. The UI
cluster (G-9..G-14) is unblocked by other agents in its early
slices and gates on Agent A only at G-15. The realized-state
cluster (G-16..G-17) waits on Agent A/D/E.

## 11. Open questions for the coordinator

1. **Multi-controller HA scope in v1**. Current default is "no";
   v1 ships single-controller-zone. Confirm.
2. **Support-bundle redaction policy** (§8.1). Default is
   "redact secrets, keep tenant names". Confirm.
3. **SMF service grouping**. Should `tritond`, `fdb`, and
   `triton-admin` live under one milestone (`svc:/site/triton-cloud`)
   so an operator can `svcadm enable site/triton-cloud` once?
   Default proposal is "yes, ship a milestone in G-12".
4. **Headnode-zone vs GZ for `tritond` + admin UI**. Default is
   "LX-branded zone on the headnode". Confirm.
5. **`tcadm doctor` exit code on warnings**. Default is "exit
   code 0 on warnings, exit code 2 on failures, exit code 3 on
   doctor itself failing". Confirm.
6. **Does `tcadm support-bundle` need an upload destination in
   v1?** Default is "no, write tarball to disk; the operator
   uploads"; matches §8 non-goal.
7. **Slice G-3 needs to land on a clean tritonagent manifest,
   but the manifest is currently untracked (`?? services/tritonagent/smf/`).
   Coordinator: should Agent G commit it first, or wait for
   whichever agent staged the file to commit it on the
   `nick-tritond-phase0` branch?**

## 12. Cross-agent handoff matrix

The handoffs below are the smallest set Agent G needs to ship the
v1 bar. Anything else is best-effort.

| Agent | Provides Agent G | Agent G provides back |
|---|---|---|
| A (VPC control plane) | `RealizedNetworkState` shape; `Job` / queue read endpoint; `EdgeCluster` read endpoint; `NatGateway` shape | `tcadm doctor` checks for desired vs realized; admin UI VPC + NAT-GW pages |
| B (Proteus integration) | `proteus-api` version constants; `proteusadm` `capabilities` / `networks` shapes (already stable per `proteus/STATUS.md`) | `tcadm doctor proteus` integration; admin UI Proteus generation column |
| C (CN dataplane) | tritonagent host-capability fields on heartbeat / status; per-CN agent status payload schema; `tritonagent.xml` config knobs | `tcadm doctor cn-host`; admin UI Compute Nodes page |
| D (firehyve edge) | edge-agent `/v0/status` shape; firehyve build resolution; Git strategy for the firehyve tree | edge SMF service shape; `tcadm doctor nat.edge_health`; admin UI NAT/FIP/Edge page |
| E (NAT north/south) | NAT GW realized-state shape; FIP termination shape | `tcadm doctor nat.gateways_attached`; admin UI NAT-GW columns |

## 13. Why this design avoids the obvious traps

- **No cross-control-plane proxying**. The admin UI talks only to
  `tritond`. It does not proxy to classic CloudAPI; STATUS.md
  Locked Decision #1 forces hierarchical URLs. Locking the UI to
  `tritond-client` keeps the wire shape from drifting.
- **Doctor is read-only**. Operators can run `tcadm doctor` on
  prod without thinking; engineering can run it from a support
  bundle's `tcadm.doctor.json`. A `--repair` mode is a separate
  doc decision that does not land in v1.
- **Per-resource realized state, not a global health blob**. Agent
  A's H-1 hands Agent G a stable shape for "desired vs realized"
  per VPC/NAT/FIP. The doctor check, the admin UI page, and the
  support bundle all read the same field. We don't invent a
  parallel "is this OK" view that drifts.
- **Single-node first, HA-shaped**. The config files exist as
  files and as SMF property groups so HA-2 shows up as N copies
  of the same manifest with a different `bind_address` and a
  shared FDB cluster file. No re-architecture needed.
- **No silent failure paths**. Every doctor check has an explicit
  failure shape and exit code. Support bundle is a documented
  artifact with a documented redaction policy. Operator can
  always reproduce an engineering's diagnosis from the same
  inputs.

## 14. References

- `triton-vnext/triton-cloud.md` §"Packaging and Operations" — the
  seven operator questions.
- `triton-vnext/STATUS.md` — locked decisions, deferred items.
- `monitor-reef/AGENTS.md` — atomic commit workflow.
- `monitor-reef/services/tritonagent/smf/tritonagent.xml` — current
  SMF manifest baseline.
- `monitor-reef/cli/tcadm/src/main.rs` — current `tcadm` subcommand
  surface (no `doctor` yet).
- `monitor-reef/docs/design/vpc-control-plane-v1.md` — Agent A's
  in-flight VPC desired-state design.
- `proteus/STATUS.md`, `proteus/TODO.md` — Proteus packaging
  baseline.
