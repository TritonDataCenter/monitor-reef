# RFD 00005 · Doc 03 — Admin surface: `tcadm` + adminui + Cedar + audit

> Slices PL-7 (cn-placement edits + Cedar + audit), PL-8 (`tcadm
> placement *` + the read endpoints), PL-9 (the adminui Placement
> section + CN-detail tab + instance-detail tab + create-wizard
> controls) implement this document. Companion code:
> `monitor-reef/cli/tcadm/src/cmds/`,
> `monitor-reef/apis/tritond-api/src/types/placement.rs`,
> `admin/backend/src/handlers/placement.rs`,
> `admin/frontend/src/pages/placement/`.

Every placement primitive is operable from both `tcadm` and adminui;
the two surfaces share the same Dropshot endpoints and the same
Cedar gating. The `--explain` flag and the simulator are the
killer-app pieces — the difference between "the picker chose this CN
for inscrutable reasons" and "here is the per-filter verdict and per-
scorer contribution for every CN, in the operator's hands". DAPI's
reasons-in-logs is the failure mode to beat (D-Pl-10).

## The Dropshot endpoint surface

All endpoints live in `apis/tritond-api/src/types/placement.rs` and
are implemented in `services/tritond/src/handlers/placement.rs`. The
endpoints split into three groups by Cedar action:

### Read / explain (`Placement::Pick`)

| Method | Path | Body / query | Returns |
|---|---|---|---|
| POST | `/v2/placement/pick` | `PlacementPickRequest` (`dry_run: bool`, plus the full `PlacementRequest` shape) | `PlacementPickResponse { chosen: Option<Uuid>, explain: ExplainReport }` |
| GET | `/v2/placement/explain/{instance_id}` | — | `PlacementPickResponse` — re-runs the pipeline against the original `DesignateParams` (persisted on the saga record) and returns "what would happen today" |
| GET | `/v2/placement/materialiser` | — | `MaterialiserStatus { leader_sec, last_tick, stale_cn_count, clickhouse_healthy }` |
| GET | `/v2/placement/reservations` | `cn?` query | list of `CnReservation` rows; `cn=` narrows |
| GET | `/v2/placement/config` | — | the current `PlacementConfig` |

`POST /v2/placement/pick { dry_run: true }` is the chain runner
without the FDB write — no reservation taken, no `Instance` mutation,
just the `ExplainReport`. The handler still goes through the
read-only half of `designate_in_txn` (one FDB read version) so the
preview is a faithful "what `designate` would do right now".

### Operator edits (per-action Cedar — see table below)

| Method | Path | Body | Returns |
|---|---|---|---|
| POST | `/v2/admin/cn-placement/{server_uuid}:reserve` | `{ reason: Option<String> }` | updated `CnPlacement` |
| POST | `/v2/admin/cn-placement/{server_uuid}:unreserve` | — | updated `CnPlacement` |
| POST | `/v2/admin/cn-placement/{server_uuid}:pin` | `{ silo: Option<Uuid>, tenant: Option<Uuid> }` (exactly one set, mutually exclusive checked at the handler) | updated `CnPlacement`; 409 with the pin-conflict reason if the invariant fails |
| POST | `/v2/admin/cn-placement/{server_uuid}:unpin` | `{ silo: bool, tenant: bool }` (either or both) | updated `CnPlacement` |
| POST | `/v2/admin/cn-placement/{server_uuid}:cordon` | `{ reason: Option<String> }` | updated `CnPlacement` |
| POST | `/v2/admin/cn-placement/{server_uuid}:uncordon` | — | updated `CnPlacement` |
| POST | `/v2/admin/cn-placement/{server_uuid}:traits` | `{ set: BTreeMap<String,String>, clear: Vec<String> }` | updated `CnPlacement` |
| POST | `/v2/admin/cn-placement/{server_uuid}:overprovision` | `{ cpu: Option<f32>, ram: Option<f32>, disk: Option<f32> }` (each None = "use cluster default") | updated `CnPlacement` |
| POST | `/v2/admin/cn-placement/{server_uuid}:fault-domain` | `{ name: Option<String> }` | updated `CnPlacement` |
| POST | `/v2/admin/cn-placement/{server_uuid}:drain` | `{ instances: Option<Vec<Uuid>>, parallelism: u32 }` | `OperationHandle` (long-running per RFD 00004 SG-4) |
| GET | `/v2/admin/cn-placement/{server_uuid}` | — | `CnPlacement` |
| GET | `/v2/admin/cn-placement` | — | every `CnPlacement` |
| GET | `/v2/admin/cn-capacity/{server_uuid}` | — | `CnCapacity` |
| GET | `/v2/admin/cn-load-summary/{server_uuid}` | `window=5m|1d|7d` (optional; default returns the full summary) | `CnLoadSummary` (full row when window absent; just the requested window's fields when present) |
| POST | `/v2/admin/cn-load/{server_uuid}:query` | `{ window: { start, end }, metrics: Vec<MetricName> }` | direct ClickHouse passthrough (bypasses `cn-load-summary` — for ad-hoc triage) |

### Placement config + per-instance affinity + force-place

| Method | Path | Body | Cedar action |
|---|---|---|---|
| POST | `/v2/admin/placement/config` | `PlacementConfig` (the new value to write) | `Placement::ConfigEdit` |
| POST | `/v2/admin/placement/materialiser:kick` | — | `Placement::ConfigEdit` |
| POST | `/v2/admin/placement/reservations/{server_uuid}/{saga_id}:release` | `{ confirm: bool }` (must be true) | `Placement::ReservationRelease` |
| POST | `/v2/tenants/{t}/projects/{p}/instances/{i}/affinity` | `InstanceAffinity` (the new rules) | `Instance::AffinityEdit` |
| GET  | `/v2/tenants/{t}/projects/{p}/instances/{i}/affinity` | — | `Instance::View` (existing) |
| POST | `/v2/tenants/{t}/projects/{p}/instances/{i}:force-place` | `{ cn: Uuid, reason: String, ignore_scope_pin: bool }` | `Instance::ForcePlace` |

## Cedar actions

New actions, all audited per the existing `tritond-audit` substrate.
The Cedar policy file in `tritond/config/cedar/` adds:

| Action | Resource | Default policy |
|---|---|---|
| `Placement::Pick` | `Fleet` | every authenticated principal (read-only) |
| `Placement::ConfigEdit` | `Fleet` | fleet-admin role |
| `Placement::ReservationRelease` | `Fleet` | fleet-admin |
| `Cn::Reserve` | `Cn` | fleet-admin |
| `Cn::Unreserve` | `Cn` | fleet-admin |
| `Cn::Pin` | `Cn` | fleet-admin |
| `Cn::Unpin` | `Cn` | fleet-admin |
| `Cn::Cordon` | `Cn` | fleet-admin |
| `Cn::Uncordon` | `Cn` | fleet-admin |
| `Cn::Drain` | `Cn` | fleet-admin (audited heavily) |
| `Cn::TraitsEdit` | `Cn` | fleet-admin |
| `Cn::OverprovisionEdit` | `Cn` | fleet-admin |
| `Cn::FaultDomainEdit` | `Cn` | fleet-admin |
| `Instance::AffinityEdit` | `Instance` | tenant-admin of the owning tenant |
| `Instance::ForcePlace` | `Instance` | fleet-admin (audited heavily) |

The existing `tritond-auth` middleware gates each endpoint at the
top of the handler; Cedar policy is consulted with the principal and
the typed resource. Force-place and Drain are the two
operator-visible "this is going to hurt" actions; their audit row
carries the full `ExplainReport` (in the force-place case, the
*skipped* report — the operator's chosen CN plus a note that the
chain was bypassed).

## Audit routing

Per RFD 00004 D-Sg-11:

- **Fleet chain** (`audit/saga/fleet`):
  - Every `Placement::*` and `Cn::*` edit (operator-on-fleet
    actions).
  - The `designate` / `undesignate` saga lifecycle events for every
    operation (creation, step, completion).

- **Per-silo chain** (`audit/saga/<silo>`):
  - `Instance::AffinityEdit` (operator-on-tenant-resource).
  - `Instance::ForcePlace` (the force lands on a tenant's instance;
    the per-silo chain owns that fact).
  - The side-effect rows that `designate` writes (`cn-reservation`
    insert, `Instance.host_cn_uuid` update, `instance-affinity`
    write) — they mutate per-tenant state.

Cross-link: every audit row carries the `operation_id` so an
operator looking at the per-silo chain can pivot to the fleet chain
to see the full saga lifecycle, and vice versa.

Force-place writes to **both** chains: the fleet chain for the
operator-identity portion ("operator X overrode placement on
instance Y") and the per-silo chain for the side effect ("instance Y
on CN Z by override"). Both rows carry the same `operation_id`.

## `tcadm` command inventory

All commands live under `tcadm placement` and `tcadm cn`. Each runs
through the matching Dropshot endpoint via `tritond-client`. The
default output is human-readable; `--json` returns the raw response
type.

### `tcadm placement *` — engine + global

| Command | Endpoint | Notes |
|---|---|---|
| `tcadm placement pick --package <id> [--tenant <uuid>] [--image <uuid>] [--strategy spread\|pack\|balanced] [--require gpu=a100,...] [--nic-tag T] [--affinity ...] [--force-cn <uuid>] [--ignore-scope-pin] [--explain] [--dry-run]` | `POST /v2/placement/pick` | `--dry-run` is the default unless invoked with `--force-pick` (which performs a real `designate`); `--explain` prints the per-CN per-filter / per-scorer table. |
| `tcadm placement explain <instance_id>` | `GET /v2/placement/explain/{id}` | Re-runs the pipeline against the original request; shows the current host CN and what would happen today. |
| `tcadm placement strategy show [--package <id>]` | `GET /v2/placement/config` | Global strategy + weight vector; per-package overrides table when packages land. |
| `tcadm placement strategy set <name> [--weight scorer=N ...]` | `POST /v2/admin/placement/config` | Validates against the registered scorer set; rejects unknown names with the list of valid ones. |
| `tcadm placement config show` | `GET /v2/placement/config` | Active filter chain, scorer weights, materialiser settings. |
| `tcadm placement config set --chain f1,f2,... [--scorers s1=w,...] [--strategy ...]` | `POST /v2/admin/placement/config` | Writes the new `PlacementConfig`; the chain is rebuilt without a `tritond` restart; rejection on unknown filter / scorer names with the list of valid ones. |
| `tcadm placement materialiser status` | `GET /v2/placement/materialiser` | Leader SEC, last tick, stale-CN count, CH health. |
| `tcadm placement materialiser kick` | `POST /v2/admin/placement/materialiser:kick` | Forces an immediate refresh. Audited; idempotent if a tick is already in flight. |
| `tcadm placement reservations list [--cn <uuid>] [--saga <uuid>]` | `GET /v2/placement/reservations` | In-flight reservation table; surfaces "this CN looks full because of in-flight sagas X / Y / Z". |
| `tcadm placement reservation release <cn> <saga> --yes-i-mean-it` | `POST /v2/admin/placement/reservations/{cn}/{saga}:release` | Manual release for a wedged saga. Requires `--yes-i-mean-it`. Audited. |
| `tcadm placement simulate --file scenario.json` | `POST /v2/placement/pick` (with `dry_run: true`, one request per scenario step) | Replays a synthetic workload; outputs a placement plan + per-step `ExplainReport`. For capacity planning. |

### `tcadm cn *` — per-CN state + edits

| Command | Endpoint |
|---|---|
| `tcadm cn capacity <server_uuid>` | `GET /v2/admin/cn-capacity/{cn}` |
| `tcadm cn load <server_uuid> [--window 5m\|1d\|7d] [--raw]` | `GET /v2/admin/cn-load-summary/{cn}` (or `POST /v2/admin/cn-load/{cn}:query` with `--raw` and `--since`) |
| `tcadm cn placement show <server_uuid>` | `GET /v2/admin/cn-placement/{cn}` |
| `tcadm cn placement list` | `GET /v2/admin/cn-placement` |
| `tcadm cn reserve <server_uuid> [--reason ...]` | `POST .../:reserve` |
| `tcadm cn unreserve <server_uuid>` | `POST .../:unreserve` |
| `tcadm cn pin <server_uuid> --silo <uuid>` | `POST .../:pin` |
| `tcadm cn pin <server_uuid> --tenant <uuid>` | `POST .../:pin` |
| `tcadm cn unpin <server_uuid> [--silo] [--tenant]` (no flags = clear both) | `POST .../:unpin` |
| `tcadm cn cordon <server_uuid> [--reason ...]` | `POST .../:cordon` |
| `tcadm cn uncordon <server_uuid>` | `POST .../:uncordon` |
| `tcadm cn trait set <server_uuid> key=value [...]` | `POST .../:traits` (with `set` populated) |
| `tcadm cn trait clear <server_uuid> key [...]` | `POST .../:traits` (with `clear` populated) |
| `tcadm cn overprovision set <server_uuid> [--cpu R] [--ram R] [--disk R]` | `POST .../:overprovision` |
| `tcadm cn overprovision clear <server_uuid> [--cpu] [--ram] [--disk]` | `POST .../:overprovision` (each cleared field as `None`) |
| `tcadm cn fault-domain set <server_uuid> <name>` | `POST .../:fault-domain` |
| `tcadm cn fault-domain clear <server_uuid>` | `POST .../:fault-domain` (with `None`) |
| `tcadm cn drain <server_uuid> [--instances ...] [--parallelism N] [--force-stop]` | `POST .../:drain` (long-running — returns an `OperationHandle`; `tcadm operations get <id>` follows it per RFD 00004 doc 03) |

### `tcadm instance *` — per-instance affinity + force-place

| Command | Endpoint |
|---|---|
| `tcadm instance affinity show <instance>` | `GET .../instances/{i}/affinity` |
| `tcadm instance affinity set <instance> --rule kind=...,scope=...,op=...,selector=...` (repeatable) | `POST .../instances/{i}/affinity` |
| `tcadm instance affinity spread <instance> --key fault-domain --max-skew N --scope required\|preferred` | `POST .../instances/{i}/affinity` (sets the `spread` field) |
| `tcadm instance affinity clear <instance>` | `POST .../instances/{i}/affinity` (with empty rules + `spread: None`) |
| `tcadm instance force-place <instance> --cn <server_uuid> [--reason ...] [--ignore-scope-pin]` | `POST .../instances/{i}:force-place` |

## adminui inventory

The Placement section is a new top-level navigation entry plus
additions to the existing CN and Instance pages plus a few create-
wizard fields.

### `/admin/placement` — overview page

Three panels:

**1. Fleet heatmap.** Rows = fault domains (`cn-placement.fault_domain`
keyed; CNs with no fault domain in a "Unzoned" row at the top).
Columns = CNs in that fault domain. Cell colour scaled by
utilisation.

Selectors:

- **Scope** — `All` / `By silo` (picker) / `By tenant` (picker). CNs
  whose `pinned_silo_uuid` / `pinned_tenant_uuid` excludes the
  chosen scope are dimmed; CNs that match are highlighted.
- **Metric** — CPU / RAM / disk / NIC tx / NIC rx.
- **Window** — Current (declared free) / 5-min p95 / 24-h p95 / 7-day
  p95.

Tooltip per cell: declared capacity, reserved (per the active
`cn-reservation` rows), live load (5-min), 24-h p95, 7-day p95, fault
domain, scope pin, traits, in-flight saga IDs. CNs with a stale
`cn-load-summary` row carry a "stale data" badge so the operator
doesn't read phantom values as truth.

**2. Strategy + chain editor.** Two tabs:

- **Strategy** — radio (`Spread` / `Pack` / `Balanced`) with a per-
  scorer weight override grid (default reflects the strategy preset;
  edits override). "Apply" writes via
  `POST /v2/admin/placement/config`; "Diff" shows the diff against
  the current setting before submit.
- **Chain** — drag-reorder list of filter names and scorer names from
  the registry; toggle each filter on / off (active vs. registered-
  but-unused). "Apply" is the same endpoint; "Diff" again before
  submit.

Both edits write through the existing settings infrastructure (FDB-
backed cluster settings, hot-reload, audited). Operators see the
last-modified timestamp and principal next to the editor.

**3. Simulator.** Free-form `PlacementRequest` editor (default
populated from a tenant + package picker), submit button →
`POST /v2/placement/pick { dry_run: true, explain: true }` → renders
the `ExplainReport` as a per-CN expandable table:

```
CN                          | Result   | Score | Detail
─────────────────────────────|──────────|───────|──────────────────────
nuc-headnode (fault: rack-1) | ✅ chosen | 4.31  | [expand: filters + scorers]
nuc-0        (fault: rack-1) | ✗ filt   | —     | cn-ram-available: 32GB requested, 24GB free
nuc-1        (fault: rack-2) | ✓ ok     | 4.18  | [expand]
.41          (fault: rack-2) | ✗ filt   | —     | cn-load-not-overheating: cpu_p95_5m=0.97 > 0.90
```

Expanding a row shows every filter verdict (kebab name + verdict)
and every scorer contribution (kebab name + raw + weight + product).
"Run as real designate" button (gated on `Placement::Pick` +
Fleet-admin) re-submits with `dry_run: false`.

### `/admin/placement/reservations` — live in-flight

Table of `cn-reservation` rows: server, instance, saga ID (linked to
the RFD 00004 operations page), cpu_units / ram_mb / disk, created_at,
expires_at, created_by_sec. Manual release button (modal confirm
with `--yes-i-mean-it` semantics, audited via the
`Placement::ReservationRelease` action). Filterable by CN and by
saga.

### CN-detail page additions (`/admin/cns/<server_uuid>`)

A new **Placement** tab next to the existing tabs:

1. **Capacity card** — cores, RAM, NUMA layout (per-node cores +
   RAM), zpools per tier (name + total + free + tier), NIC tags,
   devices (GPU model + free count, SR-IOV VFs free), platform
   version. Sourced from `cn-capacity` via
   `GET /v2/admin/cn-capacity/{cn}`.
2. **Load history panel** — four side-by-side sparklines (CPU / RAM
   / disk / NIC), each with three numerical badges (current = 5-min
   p95, 24-h p95, 7-day p95) and a "stale" badge when the
   `cn-load-summary` is stale. Sparkline data points come from a
   bounded ClickHouse passthrough (`POST /v2/admin/cn-load/{cn}:query`
   with the last hour); the badges come from `cn-load-summary`.
3. **Placement editor** — form bound to `cn-placement`:
   - Reserved (toggle + reason text)
   - Cordoned (toggle + reason text)
   - Traits (key/value grid; add / remove rows)
   - Fault domain (text)
   - Overprovision CPU / RAM / disk (each: number + "use cluster
     default" checkbox)
   - Silo pin (typeahead from existing silos; or "no pin")
   - Tenant pin (typeahead; auto-fills silo pin to the tenant's silo
     and locks it)
   - Note (free-form text)

   Save button → `POST /v2/admin/cn-placement/{cn}:*` per field
   (one request per change so the audit log is per-field;
   batch-confirm modal shows the diff before submit). Pin conflicts
   surface as a clear error toast naming the conflicting field.
4. **Reservations list** — every `cn-reservation` row against this
   CN (same fields as the global reservations table), linked back to
   the owning saga.
5. **Drain panel** — "Drain CN" button (gated on `Cn::Drain`),
   confirm modal lists every instance the drain will affect and a
   parallelism input; on submit, kicks off
   `POST /v2/admin/cn-placement/{cn}:drain` and streams progress
   via the RFD 00004 operations page.
6. **Explain link** — "Why was instance X chosen for this CN?" picks
   any instance currently homed here and deep-links to
   `/admin/instances/<i>` → Placement tab → Explain panel.

### Instance-detail page additions

A new **Placement** tab on the instance detail page:

1. **Current placement card** — host CN (linked), fault domain, the
   `DesignateParams` snapshot used at create time (the package, the
   strategy, the requested traits / NIC tags / devices / affinity).
2. **Affinity editor** — form bound to `instance-affinity`:
   - Rules grid (kind / scope / op / selector picker)
   - Topology spread (key picker / max-skew / scope toggle)
   - Save → `POST /v2/tenants/{t}/projects/{p}/instances/{i}/affinity`.
3. **Explain panel** — "Re-run the pipeline now" button →
   `GET /v2/placement/explain/{instance_id}` → renders the
   `ExplainReport` as the same table the simulator uses. Shows
   "would now choose CN X" (which may differ from the current host
   CN); preview of the future DRS slice.
4. **Force-move panel** — "Force-move to CN" button (gated on
   `Instance::ForcePlace`), CN typeahead, optional
   `ignore_scope_pin` checkbox, reason text. Submit →
   `POST .../instances/{i}:force-place`; v1 executes as stop →
   release reservation → designate (with `force_cn: Some(uuid)`) →
   start, audited as one operation.

### Create-instance wizard additions

In the existing adminui create flow, the placement step gains:

- **Strategy** radio (Spread / Pack / Balanced) — defaults to the
  cluster default; tenant-admins can override per instance.
- **Required traits** chip input — `key=value` chips, validated
  against the union of all `cn-placement.traits` in the fleet.
- **Required NIC tags** chip input — validated against the union of
  all `cn-capacity.nic_tags` in the fleet.
- **Fault-domain spread** toggle — per-project default ("instances
  in this project should spread across fault domains") with an
  override per instance.
- **Affinity rules** editor — same component as the instance-detail
  Placement tab.
- **Force CN** field (admin-only, gated on
  `Instance::ForcePlace`): CN typeahead. When set, the wizard
  calls `POST /v2/placement/pick { dry_run: true, force_cn:
  Some(uuid) }` in the background and shows the verdict before
  submit (e.g. "Force-placing on this CN will fail the scope-pin
  filter — do you want to set `ignore_scope_pin: true`?").

## Metrics + dashboards

`tritond-metrics` adds the histograms / counters / gauges named in
doc 02. The adminui Settings / Metrics page already graphs whatever
the metrics substrate exposes; the placement-specific dashboards
live under `/admin/metrics/placement` (a small landing page with
pre-built panels: pick latency, top-rejecting filter, scorer
contribution mix on winners, reservation count, materialiser lag).

## What `tcadm` doesn't have to do

- **Build the ExplainReport itself.** The server returns the typed
  report; `tcadm` only renders it. New filters / scorers added by
  out-of-tree built-ins surface on `tcadm` without a `tcadm` rebuild
  because the report is generic.
- **Know about ClickHouse.** Only the materialiser talks to CH;
  `tcadm cn load` reads `cn-load-summary` by default, and the
  `--raw` mode hits the `:query` passthrough endpoint, not CH
  directly.
- **Re-implement leader election logic.** `tcadm placement
  materialiser status` just reads the leader's identity from
  `tritond`.

The aim is a `tcadm` whose surface is wide but whose internal logic
is thin: every command is a typed request + a typed response + one
rendering function.
