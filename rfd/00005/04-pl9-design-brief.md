# RFD 00005 · Doc 04 -- PL-9 design brief (adminui Placement section)

> Audience: a designer being handed PL-9 to wireframe / mock /
> spec for an engineering build. Engineering is comfortable
> implementing whatever the design lands on; this brief gives
> the constraints, the data shapes, the user journeys, and the
> open questions. Read alongside [`./03-admin-surface.md`](./03-admin-surface.md)
> §"adminui inventory" -- that section is the authoritative
> spec; this doc is the framing.

## Product context

**Triton Cloud.** An operator-facing control plane for a private
cloud (think: vSphere or AWS EC2, but in a single-tenant
appliance shape). The admin console (`admin/`) is a Rust + Axum
backend with an embedded React + Vite + TypeScript frontend.
Users are infrastructure operators -- fleet admins who land
instances, manage compute nodes, drain hosts for maintenance,
and debug why a VM ended up where it did.

**Visual direction (locked, from `admin/README.md`):**
"terminal" line -- JetBrains Mono, blue accent, light / dark,
hairline borders, no AI affordances. Dense and information-rich,
not playful. Existing pages (`Nodes.tsx`, `NodeDetail.tsx`,
`Operations.tsx`, `Org.tsx`) are reference for what the rest of
the console feels like.

**RFD 00005.** This RFD adds a VM placement engine to Triton:
a typed pipeline of *filter* steps (hard reject) and *scorer*
steps (soft score) that picks the right compute node ("CN") for
a new instance. The engine produces an `ExplainReport` for
every pick, naming every CN's filter verdicts and scorer
contributions. The whole design surface for the operator --
adjusting which filters / scorers run, with what weights,
inspecting why a CN was chosen, pinning CNs to specific tenants
or silos, draining a CN for maintenance -- is what we're
designing.

The placement engine is already built (PL-1 through PL-5d are
committed). PL-9 is the visual / interactive layer.

## What we're designing

Four surfaces, all inside `/admin/`:

1. **`/admin/placement`** -- a new top-level page with three
   panels: a fleet **heatmap**, a **strategy + chain editor**,
   and a **simulator**.
2. **`/admin/placement/reservations`** -- a live in-flight
   reservations table.
3. **Placement tab on `/admin/cns/<server_uuid>`** -- capacity
   card, load-history sparklines, per-CN placement editor
   (pin / cordon / traits / overprovision / fault domain),
   reservations list, drain panel, "Why did instance X land
   here?" deep-link.
4. **Placement tab on `/admin/instances/<i>`** + create-instance
   wizard additions -- affinity editor, "Re-run the chain now"
   explain panel, force-move panel, plus strategy / required
   traits / NIC tags / force-CN fields on the create flow.

The four surfaces share a few component primitives -- the
**explain table**, the **reservation row**, the **CN tile** --
that should be designed once and reused.

## User journeys

Five operator scenarios the design has to make smooth. Each
journey names which screen(s) and which decisions the operator
faces.

### J1. "Why did this VM land on that host?"

A tenant complains their VM is on a noisy host. The operator
opens the instance's Placement tab, clicks "Re-run the chain
now," and gets the same `ExplainReport` the live `designate`
produced. They scan the per-CN verdict table to see which
filters rejected the better hosts and which scorers tipped the
winner.

**Design surface:** Instance-detail Placement tab → Explain
panel. The `ExplainReport` is the *primary artefact* -- making
it scannable is the design challenge.

### J2. "Reserve this rack for one tenant"

A managed customer is paying for guaranteed capacity. The
operator opens the CN-detail Placement tab on every CN in the
rack, pins each to that tenant via the editor, and adds a
fault-domain tag.

**Design surface:** CN-detail Placement tab → Placement editor.
The editor is a multi-field form with two pickers (silo /
tenant) and a guard rail: setting a tenant pin auto-fills the
silo pin to that tenant's silo and locks it.

### J3. "Drain this CN for hardware maintenance"

A CN needs a memory module swap. The operator opens the
CN-detail Placement tab → Drain panel. The confirm modal shows
every instance that will be moved and a parallelism setting.
Submit kicks off an operation; progress streams via the
existing operations page (`/admin/operations`).

**Design surface:** CN-detail Placement tab → Drain panel. The
modal is the high-stakes moment -- the operator needs to see
exactly what's about to happen.

### J4. "Tune the chain"

An operator is rolling out a new fleet and wants to switch from
Spread to Pack on a dev cluster, OR they want to disable a
specific filter (say `cn-affinity-required`) cluster-wide.

**Design surface:** `/admin/placement` → Strategy + chain
editor. Two tabs: Strategy (radio + weight grid) and Chain
(drag-reorder filter / scorer list with toggle-on-off). Both
write through `POST /v2/admin/placement/config`; a diff modal
gates submit.

### J5. "What would the chain do for this hypothetical instance?"

Before bulk-provisioning, an operator wants to simulate. They
open `/admin/placement` → Simulator, fill in a placement
request (or pick a tenant + package and let it populate),
submit, and read the `ExplainReport`.

**Design surface:** `/admin/placement` → Simulator. Form +
ExplainReport viewer. The "Run as real designate" button (gated
by Cedar) re-submits with `dry_run: false` for true commit.

## Data shapes the design renders

The engineering surface produces these (canonical Rust types in
[`tritond-placement`](../../libs/tritond-placement/src/types.rs)
and [`tritond-store`](../../libs/tritond-store/src/types.rs)):

### `CnView` (per compute node, what the engine sees)

```
server_uuid           uuid
hostname              string
state                 pending | approved | disabled
role                  tenant | edge | both
last_seen             timestamp | null
capacity              {
                        cpu_cores_physical, cpu_threads_logical,
                        numa_nodes[ {node_id, cores, ram_mb} ],
                        ram_total_mb,
                        zpools[ {name, total_bytes, free_bytes,
                                 tier: ssd|nvme|hdd|mixed} ],
                        nic_tags[],
                        underlay: {ipv4, ipv6},
                        devices[ {kind: gpu|sr-iov-vf, model,
                                  free_count} ],
                        platform_version,
                        hvm_supported,
                        reported_at,
                      } | null   (null = "not visible to placement")
placement (policy)    {
                        reserved, cordoned, cordoned_reason,
                        pinned_silo_uuid, pinned_tenant_uuid,
                        traits: { key -> value },
                        overprovision_cpu | null, ram | null,
                        fault_domain | null,
                      }
active_reservations   [ {saga_id, instance_id, cpu_units, ram_mb,
                          disk: {pool -> bytes}, devices[],
                          deadline} ]
load_summary          {
                        last_refreshed_at, stale,
                        cpu_p50_5m, cpu_p95_5m,
                        cpu_p50_1d, cpu_p95_1d, cpu_p95_7d,
                        ram_used_p95_5m,
                        nic_tx_bps_p95_5m, nic_rx_bps_p95_5m,
                      } | null
assigned_instances    [ {instance_id, silo_uuid, tenant_uuid,
                          cpu_units, ram_mb} ]
```

### `ExplainReport` (per pick -- the engine's primary output)

```
request               PlacementRequest (the input)
strategy              spread | pack | balanced
weights               { scorer_name -> f32 }
chosen                uuid | null   (null = no eligible CN)
elapsed               duration
generated_at          timestamp
per_cn                [ {
                        server_uuid,
                        filter_results: [ {filter_name,
                                            verdict: accept|reject|skip,
                                            reason?: string} ],
                        scorer_results: [ {name, raw, weight,
                                            contribution} ],
                        total_score: f32 | null,
                        load_summary_stale: bool,
                        capacity_present: bool,
                        accepted: bool,
                      } ]
```

The `per_cn` array can be large (50+ CNs on a real cluster).
Each entry can have 18 filter results and 12 scorer results.
That's the dense data the explain table has to render.

### `PlacementRequest` (the input to the chain)

```
instance_id, silo_uuid, tenant_uuid, project_uuid
role                  tenant | edge | both
cpu_units             u32       (100 = 1 vCPU; matches legacy DAPI cpu_cap)
ram_mb                u64
disk                  { pool -> bytes }
required_traits       { key -> value }
required_nic_tags     [ string ]
required_underlay     {ipv4, ipv6}
required_devices      [ {kind, model, count} ]
needs_hvm             bool
min_platform          string | null
affinity              InstanceAffinity (rules + topology spread)
strategy_override     spread | pack | balanced | null
force_cn              uuid | null
ignore_scope_pin      bool
deadline              timestamp
```

### `CnReservation` (in-flight provision tickets)

```
server_uuid, saga_id, instance_id
cpu_units, ram_mb, disk: {pool -> bytes}, devices[]
created_at, expires_at
created_by_sec_id, created_at_epoch
```

### Filter / scorer registry (for the chain editor)

Eighteen filters + twelve scorers ship by default; operators can
toggle individual filters and rearrange the order via cluster
settings. Each filter / scorer has a kebab-case name and a brief
description (the names are stable; the engine ships them and the
chain editor enumerates them):

**Filters** (default order):

1. `cn-approved-and-live` -- CN state + agent heartbeat freshness
2. `cn-capacity-present` -- agent has published a capacity row
3. `cn-role-matches` -- tenant / edge / both
4. `cn-not-reserved` -- operator-out-of-service flag
5. `cn-not-cordoned` -- no new placements
6. `cn-not-evacuating` -- drain in progress
7. `cn-scope-match` -- silo / tenant pin
8. `cn-platform-min` -- legacy DAPI min_platform
9. `cn-traits-required` -- required operator traits
10. `cn-nic-tags` -- required NIC tags
11. `cn-underlay` -- v4 / v6 capability
12. `cn-zpool-has-space` -- per-pool free disk after reservations
13. `cn-ram-available` -- RAM residual after reservations + assigned
14. `cn-cpu-available` -- CPU residual
15. `cn-numa-fits` -- per-node residual (v1 stub)
16. `cn-device-available` -- GPU / SR-IOV per `(kind, model)`
17. `cn-hvm-supported` -- bhyve / KVM brand
18. `cn-affinity-required` -- hard affinity rules

Plus opt-in `cn-load-not-overheating` (operator opts in via
chain config).

**Scorers** (default order):

1. `score-ram-headroom` (default weight 2.0)
2. `score-disk-headroom` (1.0)
3. `score-spread-by-fault-domain` (1.5)
4. `score-pack-by-fault-domain` (0.0)
5. `score-affinity-preferred` (1.0)
6. `score-platform-current` (0.5)
7. `score-fewer-cotenant-zones` (0.5)
8. `score-uniform-random` (0.1) -- deterministic tie-break
9. `score-avoid-hot-now` (1.5) -- CH-load gated
10. `score-avoid-peaky` (1.0) -- CH-load gated
11. `score-prefer-low-baseline` (0.75) -- CH-load gated
12. `score-diurnal-fit` (0.0) -- off by default

## Design challenges

These are the calls the designer has to make. None of them have
a forced answer -- the choice has to fit the dense / terminal /
operator-first feel of the console.

### C1. The fleet heatmap

Rows = fault domains. Columns = CNs. Cells = utilisation.
Three controls overhead: scope (all / by silo / by tenant),
metric (CPU / RAM / disk / NIC tx / NIC rx), window (current
declared free / 5-min p95 / 24-h p95 / 7-day p95). Tooltip per
cell carries declared capacity, reserved, live load, 24-h p95,
7-day p95, fault domain, scope pin, traits, in-flight saga IDs.

**Open questions:**
- How does the heatmap handle the "CN has no `cn-load-summary`
  yet" state? (A stripe? A grey tile? An icon?) Stale data must
  read as distinct from "data says zero."
- Tenant / silo scope dimming: are unscoped CNs at 30 %
  opacity, fully greyed, or removed? Operators want to see the
  rest of the fleet for context, but the highlighted CNs need
  to pop.
- A 200-CN cluster won't fit in one row of one fault domain.
  Wrap? Scroll? Group by half-rack?
- Click-through: cell click opens CN-detail Placement tab.
  Should the heatmap support multi-select (e.g. for bulk
  cordon)? RFD says no for v1, but the design should leave
  room.

### C2. The explain table

The single most important component in PL-9. It renders an
`ExplainReport`'s `per_cn` array -- up to ~50 rows, each with
18 filter verdicts and 12 scorer contributions.

The reference sketch in doc 03 §"Simulator" is:

```
CN                          | Result   | Score | Detail
─────────────────────────────|──────────|───────|──────────────────────
nuc-headnode (fault: rack-1) | ✅ chosen | 4.31  | [expand]
nuc-0        (fault: rack-1) | ✗ filt   | --     | cn-ram-available: 32GB requested, 24GB free
nuc-1        (fault: rack-2) | ✓ ok     | 4.18  | [expand]
.41          (fault: rack-2) | ✗ filt   | --     | cn-load-not-overheating: cpu_p95_5m=0.97 > 0.90
```

Each row's expanded state shows every filter verdict (kebab
name + verdict + reason if rejected) and every scorer
contribution (kebab name + raw + weight + product). Total
score = sum of contributions.

**Open questions:**
- How are accept / reject / skip distinguished without emoji
  (the console avoids them)? Glyph? Colour bar? Letter code?
- The chosen CN should be visually unmistakable. Highlighted
  row? Side gutter glyph?
- When a CN's score is high but it wasn't chosen (a competitor
  beat it by a hair), how does that surface? Should runners-up
  be visually different from middle-of-the-pack?
- Sort order: by score descending (winner at top) is the
  obvious default. Should rejected CNs be at the bottom or
  interleaved by name?
- The expanded scorer breakdown is a per-scorer mini bar chart
  vs. a flat list -- which reads faster?
- This table is rendered in three places (simulator,
  instance-detail Explain panel, audit drill-down). Same
  component, three contexts; the design has to be context-
  agnostic.

### C3. The CN placement editor

A multi-field form with two pickers and a cross-field guard
(tenant pin auto-fills + locks silo pin). The form is on a
CN-detail tab next to a capacity card and a load-history
panel -- three sections in one tab; balancing density vs.
scanability is the call.

**Open questions:**
- The fields are heterogeneous: two toggles (reserved /
  cordoned), one note text, one fault-domain text, three
  overprovision numbers (each with "use cluster default"
  checkbox), one traits key/value grid, two pickers (silo /
  tenant). Group how? RFD doesn't say; doc 03 has the field
  list but no layout.
- Per-field audit: every field save is a separate
  `POST /v2/admin/cn-placement/{cn}:*` so the audit log is
  per-field. The form has to support "save just this field" --
  but operators usually batch. Solution: edit-then-batch-
  confirm with a diff modal. Design that modal.
- Pin conflict surfacing: setting tenant pin + a silo pin that
  doesn't match the tenant's silo → 409. The form has to show
  the conflict before submit (cross-field validation) AND
  handle the server-side 409 gracefully (toast + highlight the
  conflicting field).

### C4. The strategy + chain editor

Two tabs in the `/admin/placement` page. Strategy tab is
radio + weight grid; Chain tab is drag-reorder + toggle.

**Open questions:**
- Strategy radio is three options (Spread / Pack / Balanced)
  with a weight grid below. The grid shows every registered
  scorer (12 of them) with a current weight (resolved from
  strategy preset). Operators can override any cell.
  Visualising "default vs. overridden" -- colour? Italic? An
  "Reset to default" link per cell?
- Chain tab: drag-reorder a list of 18+ filter names. Each row
  has a toggle (active / inactive) and the filter's brief
  description. RFD says the order matters (filters run in
  registration order) -- should the list show "order index"
  numerals?
- Diff modal before submit: shows the diff between current
  cluster settings and the operator's edits, with last-
  modified-by metadata on each line of the current. Design
  that diff render.
- The chain editor is gated on Cedar (only fleet-admins can
  edit). Read-only mode for tenant-admins -- how does that
  read? Greyed-out controls vs. a "View" header?

### C5. The simulator

Form + ExplainReport viewer. The form populates a
`PlacementRequest` (see the data shape above -- 15+ fields).
Defaults from a tenant + package picker; manual override on
every field.

**Open questions:**
- The form is dense -- 15 fields including nested ones
  (affinity rules, required devices). Tab grouping? Disclosed
  sections? Single-column or two-column?
- "Run as real designate" button (with Cedar gate) re-runs
  the same input with `dry_run: false`. The transition from
  preview → commit needs a clear confirm step. Modal? Inline?
  This is a destructive action (creates a real reservation).

### C6. The reservations table (`/admin/placement/reservations`)

Standard tabular list -- server, instance, saga, cpu_units,
ram_mb, disk, created_at, expires_at, created_by_sec. Filter
by CN, filter by saga. Manual release button (modal confirm,
audited).

**Open questions:**
- This table competes for attention with the operations page
  (`/admin/operations`) which already shows saga state. Should
  it link aggressively to that page, or stand alone? Doc 03
  says "saga ID linked to the operations page" -- so the link
  is one way (out, not in).
- Release-confirm modal: "Are you sure?" plus a textarea for a
  reason field that lands in the audit log. The "`--yes-i-mean-it`
  semantics" from doc 03 suggests typing the saga ID to
  confirm. Match `tcadm` aesthetics? Or softer?

### C7. The instance-detail Placement tab

Three panels: current placement card, affinity editor, explain
panel, force-move panel. The explain panel is the **same**
explain table component as the simulator (C2).

**Open questions:**
- "Would-now-choose CN X" -- if the engine would pick a
  different CN than the current host (e.g. because the host has
  since become hot), how does the panel render that delta?
  Suggested CN as a banner? A "Force-move to suggested" button?
- The force-move panel is high-stakes (it's a live VM
  movement). Confirm flow needs to be clear about what
  happens: stop → release reservation → re-designate → start.
  Streaming progress via operations page.

### C8. Create-instance wizard additions

Strategy radio, required traits chip input, required NIC tags
chip input, fault-domain spread toggle, affinity rules editor
(same component as instance-detail tab), force CN field
(admin-only with Cedar gate).

**Open questions:**
- The placement step is added to an existing wizard. Where
  does it slot in? Likely between the package step and the
  review step. Design the step transition.
- The force-CN field is admin-only. For non-admins it hides
  entirely. Design the conditional state.
- The chip inputs validate against fleet-wide sets (every
  trait in the fleet, every NIC tag in the fleet). Autocomplete?

## Reference materials

- **The RFD section the design implements:**
  [`./03-admin-surface.md`](./03-admin-surface.md) §"adminui inventory"
  (lines 187-348).
- **Engine reference (data shapes):**
  - `tritond-placement/src/types.rs` -- `CnView`, `PlacementRequest`, `ExplainReport`
  - `tritond-store/src/types.rs` -- `CnCapacity`, `CnPlacement`, `CnReservation`, `CnLoadSummary`, `InstanceAffinity`
- **Existing console pages for visual reference:**
  - `admin/frontend/src/pages/Nodes.tsx` -- fleet list, the closest
    existing analogue to the heatmap's CN cells
  - `admin/frontend/src/pages/NodeDetail.tsx` + `NodeHardware.tsx` +
    `NodeMetrics.tsx` -- the existing CN detail tabs the Placement tab
    slots in next to
  - `admin/frontend/src/pages/Operations.tsx` -- the saga / operations
    page the reservations table links into
  - `admin/frontend/src/pages/Audit.tsx` -- the audit log the
    placement edits surface in
- **Design system primitives:** JetBrains Mono, blue accent, hairline
  borders, light + dark, no AI affordances. The "terminal" line
  noted in `admin/README.md`.

## What engineering needs from the design

A wireframe / mock per surface, in this priority order:

1. **The explain table** (C2) -- most important component;
   reused across simulator, instance-detail, audit drill-down.
2. **The fleet heatmap** (C1) -- the most visually novel piece;
   needs the scale / scope / metric / window controls worked
   out.
3. **The CN placement editor** (C3) -- the densest form;
   field grouping + pin-conflict UX.
4. **The strategy + chain editor** (C4) -- operator's tuning
   surface; default-vs-overridden + diff modal.
5. **The simulator** (C5) -- form + the embedded explain table.
6. **The reservations table** (C6) -- straightforward but with
   a careful release-confirm flow.
7. **The instance-detail Placement tab** (C7) + **wizard
   additions** (C8) -- extend the existing instance page; less
   novel.

For each: layout at default density, behaviour notes for
hover / focus / error / empty / loading / no-permission states,
and the dark-mode pass.

## Constraints

- **No emoji or playful affordances.** The console is operator-
  first; the visual line is "terminal."
- **Hairline borders, JetBrains Mono.** Match existing pages.
- **Light + dark.** Both at parity; no second-class mode.
- **Cedar-gated controls.** Many actions are admin-only; the
  design has to show the gated state clearly (greyed, hidden,
  or labeled "requires `Fleet::Admin`" -- designer's call).
- **Real-time-ish, not real-time.** Heatmap and load
  sparklines refresh on a 30s tick, not via websocket. Design
  for "fresh enough" not "live."
- **Stale data is a first-class state.** A `cn-load-summary`
  marked `stale = true` must read as "data says nothing" -- not
  blank, not zero, not an error. RFD invariant 3.
- **The chain config is operator-edited, hot-reloaded, and
  audited.** Operators edit the active chain via the editor;
  changes apply without a `tritond` restart. The design has to
  reflect that immediacy.

## Open questions for the designer to feed back

- Q1: Heatmap row grouping (fault domain) when a fault domain
  has > 50 CNs -- wrap, scroll, sub-group?
- Q2: Explain table -- accept / reject / skip glyphs without
  emoji. Letters? Bars? Background tints?
- Q3: Where does the Placement section live in the top-level
  nav? It's not under Nodes (it's broader) and not under
  Operations (it's not just one operation). New top-level
  item?
- Q4: Mobile / narrow viewport -- out of scope for v1, or is
  there a degraded layout?
- Q5: Operator-defined named chain presets (e.g. "production"
  vs. "dev") -- RFD v1 doesn't include them; should the design
  leave room?

---

*Author this brief for design hand-off; engineering will land
PL-9 against the resulting wireframes / mocks. Questions to
nick@mnxsolutions.com or via the RFD 00005 thread.*
