# Manta Rebalancer Overview: Capabilities, Architecture, and Limitations

This document provides a high-level overview of the Manta Rebalancer system
for engineering and operations teams evaluating its capabilities, planning
evacuations, or assessing gaps for future development.

> **See also:** `devops_operations_guide.md` for step-by-step runbooks and
> `devops_operations_guide.md` for detailed configuration reference.

---

## Table of Contents

1. [What Is the Rebalancer?](#what-is-the-rebalancer)
2. [Capabilities](#capabilities)
3. [High-Level Architecture](#high-level-architecture)
4. [How an Evacuation Works](#how-an-evacuation-works)
5. [Object Type Support](#object-type-support)
6. [Runtime Tunability](#runtime-tunability)
7. [Observability](#observability)
8. [Limitations](#limitations)
9. [Known Edge Cases](#known-edge-cases)
10. [Feature Summary Matrix](#feature-summary-matrix)

---

## What Is the Rebalancer?

The rebalancer is a purpose-built system for **evacuating objects from a Manta
storage node (shark)**. When a shark needs to be decommissioned or taken out of
rotation, the rebalancer moves every object off that shark onto healthy  
destination sharks while keeping Manta metadata consistent with the objects'
physical location.

It is **not** a general-purpose data balancer. It does one thing — shark
evacuation —. 

---

## Capabilities

### Core Features

- **Full shark evacuation** — discovers and copies every object from a source
  shark to healthy destination sharks, updating metadata so clients 
  transparently  read from the new locations, the copied objects are not read 
  from the evacuated  shark but from it's replicas.

- **Dual metadata backend support** — handles both traditional Manta
  directory-based objects (v1, stored in Moray) and bucket objects (v2, stored
  in MDAPI/buckets-postgres). Operates in moray-only, mdapi-only, or hybrid
  mode depending on configuration.

- **pgclone-based discovery** — scans metadata using read-only ZFS snapshot
  clones of PostgreSQL databases (`pgclone.sh`), avoiding any load on the
  live metadata tier during discovery.

- **Conditional metadata updates** — uses etag-based optimistic concurrency
  control to prevent lost updates. If an object was modified between discovery
  and update, the write is safely rejected (retryable).

- **Intelligent destination selection** — periodically queries storinfo for
  shark capacity, selects the top N sharks by available space, respects a
  configurable maximum fill percentage, and balances assignments across
  destinations.

- **Job retry** — after a job completes, failed or unprocessed objects can be
  retried with a single command. The retry reads from the original job's
  database and reprocesses only what remains.

- **Dynamic metadata thread tuning** — the number of metadata update threads
  can be adjusted at runtime (1–250) via the HTTP API without restarting the
  manager or the job.

- **Prometheus metrics** — both manager and agent export metrics on port 8878
  for integration with existing monitoring infrastructure (scraped by
  cmon-agent).

- **Persistent job state** — all job progress is stored in PostgreSQL
  (manager) and SQLite (agent), surviving process restarts. A crashed manager
  can be restarted and the job retried without losing track of what was
  already completed.

- **Duplicate detection** — objects discovered on multiple metadata shards are
  tracked in a `duplicates` table and counted separately, preventing double
  processing.

### Administrative Tools

- **`rebalancer-adm`** — CLI tool for creating, listing, monitoring, and
  retrying evacuation jobs.

- **HTTP REST API** — programmatic access to all job operations (create, get,
  list, update, retry).

- **`pgclone.sh`** — self-contained script for creating and managing
  read-only PostgreSQL clone VMs from ZFS snapshots, must be executed
  from the headnode.

---

## High-Level Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                         MANAGER ZONE                            │
│                                                                 │
│  ┌──────────────────┐    ┌─────────────────┐                    │
│  │ rebalancer-      │    │  PostgreSQL 12  │                    │
│  │ manager          │◄──►│  (local, per-job│                    │
│  │                  │    │   databases)    │                    │
│  │ Port 80 (API)    │    └─────────────────┘                    │
│  │ Port 8878 (prom) │                                           │
│  └───────┬──────────┘                                           │
│          │                                                      │
│  ┌───────┴──────────┐                                           │
│  │ rebalancer-adm   │  (CLI for operators)                      │
│  └──────────────────┘                                           │
└──────────┬──────────────────────────────────────────────────────┘
           │
           │ Interacts with:
           │
     ┌─────┼──────────────────────────────────────────────┐
     │     │                                              │
     │     ▼                                              │
     │  ┌──────────┐   ┌──────────┐   ┌──────────┐        │
     │  │  Agent   │   │  Agent   │   │  Agent   │        │
     │  │ (mako-1) │   │ (mako-2) │   │ (mako-N) │        │
     │  │ :7878    │   │ :7878    │   │ :7878    │        │
     │  └──────────┘   └──────────┘   └──────────┘        │
     │         STORAGE TIER (one agent per shark)         │
     └────────────────────────────────────────────────────┘
           │
     ┌─────┼───────────────────────────────────────────────┐
     │     │         METADATA / SERVICES                   │
     │     ▼                                               │
     │  ┌──────────────┐  ┌──────────────┐                 │
     │  │ pgclone      │  │ pgclone      │  (discovery)    │
     │  │ moray clone  │  │ buckets clone│                 │
     │  │ :5432        │  │ :5432        │                 │
     │  └──────────────┘  └──────────────┘                 │
     │                                                     │
     │  ┌──────────────┐  ┌──────────────┐                 │
     │  │ Moray        │  │ MDAPI        │  (metadata      │
     │  │ (v1 updates) │  │ (v2 updates) │   updates)      │
     │  └──────────────┘  └──────────────┘                 │
     │                                                     │
     │  ┌──────────────┐                                   │
     │  │ Storinfo     │  (shark capacity/availability)    │
     │  └──────────────┘                                   │
     └─────────────────────────────────────────────────────┘
```

### Key Components

| Component | Role | Runs On |
|-----------|------|---------|
| **Manager** | Orchestrates evacuation: discovery, assignment creation, metadata updates | Dedicated rebalancer zone (single instance) |
| **Agent** | Downloads objects from source shark, stores on local disk | Every mako (storage) zone |
| **PostgreSQL** | Persists job state, object tracking, error records | Local to manager zone |
| **pgclone** | Read-only ZFS snapshot clones of metadata databases | Ephemeral VMs on headnode |
| **Storinfo** | Provides shark capacity and availability data | Existing Manta infrastructure |
| **Moray** | Metadata backend for v1 directory objects | Existing Manta infrastructure |
| **MDAPI** | Metadata backend for v2 bucket objects | Existing Manta infrastructure |

---

## How an Evacuation Works

### The Four Phases

```
 Phase 1: Discovery    Phase 2: Assignment    Phase 3: Transfer    Phase 4: Update
┌──────────────────┐  ┌──────────────────┐  ┌──────────────────┐  ┌──────────────────┐
│                  │  │                  │  │                  │  │                  │
│  Scan pgclone    │  │  Group objects   │  │  POST to agent   │  │  Write updated   │
│  (read-only PG)  ├─►│  by destination  ├─►│  on dest shark.  ├─►│  shark location  │
│  to find objects │  │  shark. Pick     │  │  Agent downloads │  │  to real Moray   │
│  on source shark │  │  destinations    │  │  from source,    │  │  or MDAPI.       │
│                  │  │  from storinfo   │  │  verifies MD5.   │  │  Etag-checked.   │
│                  │  │                  │  │                  │  │                  │
└──────────────────┘  └──────────────────┘  └──────────────────┘  └──────────────────┘

 Direct SQL to          Single thread          Per agent:            Configurable
 pgclone clones.        (assignment mgr).      workers x             1-250 threads
 10 parallel read       Batches up to 50       workers_per_          (adjustable at
 threads.               objects per             assignment            runtime).
                        assignment.             parallel downloads.
```

### Step-by-Step

1. **Operator marks the shark read-only** — disables minnow, flushes caches,
   restarts muskie and buckets-api, waits for writes to drain.

2. **Operator creates pgclone snapshots** — `pgclone.sh clone-all` creates
   read-only ZFS clones of the moray and buckets-postgres databases.

3. **Operator starts the job** — `rebalancer-adm job create evacuate --shark <ID>`.

4. **Discovery** — sharkspotter connects to the pgclone databases via direct
   SQL (not Moray/MDAPI RPC) and scans for every object referencing the source
   shark. Each object is inserted into the job's PostgreSQL database.

5. **Assignment generation** — objects are grouped into assignments of up to 50
   objects each, targeted at specific destination sharks selected from storinfo
   based on available capacity.

6. **Agent transfer** — the manager POSTs each assignment to the agent running
   on the destination shark. The agent downloads each object from the source
   shark via HTTP, verifies the MD5 checksum, and stores it locally.

7. **Metadata update** — once the agent reports completion, the manager updates
   the object's metadata in the real Moray (v1) or MDAPI (v2) to replace the
   old shark with the new one. Updates use etag-based conditional writes to
   prevent lost updates.

8. **Completion/Retry** — objects that failed (etag conflicts, network errors,
   etc.) can be retried. The operator destroys pgclone clones after the job
   finishes.

### Data Flow During Transfer

```
 Source Shark                    Destination Shark (Agent)
 (being evacuated)

 /manta/{owner}/{obj_id}  ───HTTP GET───►  /var/tmp/rebalancer/temp/{owner}/{obj_id}
                                                      │
                                                MD5 verify
                                                      │
                                                      ▼
                                           /manta/{owner}/{obj_id}  (final location)
```

For v2 bucket objects, paths include bucket ID and name hash:
`/manta/v2/{owner}/{bucket_id}/{prefix}/{object_id},{name_hash}`

---

## Object Type Support

| Object Type | Discovery | Metadata Update | MPU Support |
|-------------|-----------|-----------------|-------------|
| **v1 directory objects** (Moray) | pgclone moray clone | Moray RPC — one `put_object` per object by default | Yes — updates `preAllocatedSharks` in upload records |
| **v2 bucket objects** (MDAPI) | pgclone buckets clone  | MDAPI RPC — one `put_object` per object by default | Yes — same MPU handling |
| **Hybrid (both)** | Both clones | Routes automatically: bucket objects to MDAPI, directory objects to Moray | Yes |

### Metadata Update Modes

Metadata updates are controlled by `use_batched_updates` (default: **false**).

> **Note:** The Rust code default is `true`, but the SAPI template emits
> `false` when `REBALANCER_USE_BATCHED_UPDATES` is not set in SAPI metadata.
> In practice, deployed instances default to individual mode.

**Default mode (`use_batched_updates: false`):**

Each object in an assignment is updated individually — one RPC call per
object, sequentially within each metadata update thread:

- **Moray (v1):** One `put_object` RPC per object with etag condition.
  Each object succeeds or fails independently. An assignment of 50 objects
  generates 50 separate Moray RPCs.
- **MDAPI (v2):** One `put_object` (internally `update_object`) per object
  with etag condition. Tries each MDAPI shard until one accepts the vnode.
- **Hybrid:** Routes each object to the appropriate backend based on type.

This is the safer but slower path — higher RPC count, but minimal risk of
large atomic operations impacting the metadata tier.

**Batched mode (`use_batched_updates: true`):**

Objects are grouped by shard and updated in a single RPC per shard:

- **Moray (v1):** One `batch()` RPC per shard — **atomic** (all-or-nothing).
  If the batch fails, falls back to individual `put_object` per object.
  An assignment of 50 objects across 2 shards generates only 2 Moray RPCs.
- **MDAPI (v2):** Uses `batchupdateobjects` RPC — **non-transactional**.
  Partial success is possible: successfully updated objects are marked
  complete even if others in the same batch fail.
- **Hybrid:** Partitions by type. MDAPI objects are updated first; if they
  succeed, Moray objects are batched. This ordering avoids cross-backend
  inconsistency (Moray batch is atomic and can roll back; MDAPI cannot).

| | Default (individual) | Batched |
|---|---|---|
| **Moray RPCs per assignment** | 1 per object (e.g., 50) | 1 per shard (e.g., 2) |
| **Moray atomicity** | Per-object | Per-shard (all-or-nothing) |
| **MDAPI RPCs per assignment** | 1 per object | `batchupdateobjects` RPC (non-transactional) |
| **Failure granularity** | Individual object marked error | Batch fails → fallback to individual |
| **Metadata tier load** | Higher RPC count | Lower RPC count, larger payloads |
| **Retry with backoff** | Per-object exponential backoff | Batch first, then per-object fallback |

---

## Runtime Tunability

### Settings Adjustable Without Restart

| Tunable | Method | Range | Impact |
|---------|--------|-------|--------|
| Metadata update threads | `PUT /jobs/{uuid}` with `set_metadata_threads` | 1–250 | More parallel metadata writes |
| Configuration file | `SIGUSR1` or `svcadm refresh` | — | Rereads config.json |

### Settings Adjustable via SAPI (Require Config Refresh)

| Tunable | SAPI Key | Default | Impact |
|---------|---------|---------|--------|
| Max tasks per assignment | `REBALANCER_MAX_TASKS_PER_ASSIGNMENT` | 50 | Objects per assignment batch |
| Max metadata read threads | `REBALANCER_MAX_METADATA_READ_THREADS` | 10 | Discovery parallelism |
| Max destination sharks | `REBALANCER_MAX_SHARKS` | 5 | Destination diversity |
| Metadata read chunk size | `REBALANCER_MD_READ_CHUNK_SIZE` | 500 | Objects per SQL query |
| Assignment batch timeout | `REBALANCER_MAX_ASSIGNMENT_AGE` | 600s | Max wait before posting to agent |
| Batched metadata updates | `REBALANCER_USE_BATCHED_UPDATES` | false | Batch vs individual updates |
| Agent workers | `REBALANCER_AGENT_WORKERS` | 1 | Concurrent assignments per agent |
| Agent workers per assignment | `REBALANCER_AGENT_WORKERS_PER_ASSIGNMENT` | 1 | Parallel downloads per assignment |
| Discovery mode | `REBALANCER_DIRECT_DB` | true | pgclone-based discovery for both Moray and MDAPI vs RPC |
| Max fill percentage | `MUSKIE_MAX_UTILIZATION_PCT` | 100 | Shark utilization threshold |

---

## Observability

### Metrics (Prometheus, port 8878)

**Manager metrics:**

| Metric | Type | Description |
|--------|------|-------------|
| `object_count` | Counter | Objects processed, by action |
| `skip_count` | Counter | Objects skipped, by reason |
| `error_count` | Counter | Errors, by classification |
| `request_count` | Counter | HTTP API requests |
| `md_thread_gauge` | Gauge | Active metadata update threads |

**Agent metrics:**

| Metric | Type | Description |
|--------|------|-------------|
| `object_count` | Counter | Objects processed (complete/failed) |
| `error_count` | Counter | Errors by type |
| `bytes_count` | Counter | Total bytes transferred |
| `assignment_time` | Histogram | Assignment completion time |
| `request_count` | Counter | HTTP requests by method |

### Job Status API

The `GET /jobs/{uuid}` endpoint returns real-time progress:

```json
{
  "state": "running",
  "results": {
    "total": 1000000,
    "complete": 750000,
    "assigned": 50000,
    "post_processing": 200,
    "unprocessed": 190000,
    "error": 500,
    "skipped": 9300,
    "duplicates": 100,
    "error_breakdown": { "moray": { "etag_mismatch": 500 } },
    "skip_breakdown": { "source_is_evac_shark": 9300 }
  }
}
```

### Logs

| Component | Log Location |
|-----------|-------------|
| Manager | `svcs -L svc:/manta/application/rebalancer:default` |
| Agent | `svcs -L svc:/manta/application/rebalancer-agent:default` |
| PostgreSQL | `/var/pg/postgresql.log` |

---

## Limitations

### Architectural Limitations

| Limitation | Description | Impact |
|-----------|-------------|--------|
| **Evacuation only** | Only supports moving ALL objects off a source shark. No selective rebalancing, capacity balancing, or object placement optimization. | Cannot redistribute objects across sharks without evacuating a full shark. |
| **Single concurrent job** | Only one evacuation job runs at a time. Multiple requests are not parallelized. | Cannot evacuate multiple sharks simultaneously from the same manager. |
| **Single manager instance** | One manager per region with no redundancy or failover. | Manager crash halts the job. Must be manually restarted and job retried. |
| **Same datacenter only** | Objects can only be placed on sharks within the same datacenter. No cross-datacenter evacuation. | Cannot use remote datacenter capacity as destination. |
| **No automatic recovery** | If the manager or an agent crashes mid-job, there is no automatic resume. Operator must restart services and retry. | Requires operator intervention for any failure. |
| **pgclone dependency** | Production discovery requires pgclone clones. Clones must be manually created before each evacuation and destroyed after. | Adds operational steps and requires headnode access. |
| **Point-in-time discovery** | pgclone snapshots are frozen at creation time. Objects written to the shark after the snapshot are not discovered. | Shark must be made fully read-only before snapshot. Objects written after snapshot require a second pass. |

### Performance Constraints

| Constraint | Default | Hard Limit | Notes |
|-----------|---------|-----------|-------|
| Metadata update threads | 10 | 250 | Beyond 100 not recommended — can overload metadata tier |
| Tasks per assignment | 50 | Configurable | Larger = more memory per assignment |
| Storinfo refresh interval | 10 seconds | Not configurable | Stale capacity data between refreshes |
| Assignment manager | Single thread | Not parallelizable | Serialized assignment generation |
| Agent concurrency | 1 worker, 1 download/assignment | Configurable | Total = workers x workers_per_assignment |

### Metadata Update Constraints

| Constraint | Description |
|-----------|-------------|
| **MDAPI is non-transactional** | v2 bucket object updates can partially succeed. Some objects may be updated while others in the same batch fail. |
| **Etag conflicts are expected** | Objects modified by users during evacuation will fail with etag mismatch. These are retryable but inflate error counts. |
| **MPU update is best-effort** | If part evacuation succeeds but the upload record update fails, the upload record may reference the old shark for that part. Logged but not fatal. |

### Operational Constraints

| Constraint | Description |
|-----------|-------------|
| **Manual pre-evacuation steps** | Operator must disable minnow, flush storinfo, restart muskie and buckets-api, wait for write drain, create pgclone clones, and verify DNS — all before starting a job. |
| **Manual post-evacuation cleanup** | Operator must destroy pgclone clones, back up job databases, and clean agent queues. |
| **Snaplinks not handled** | Snaplinks require separate manual cleanup. The `snaplink_cleanup_required` config flag indicates the need but the rebalancer takes no action. |
| **No job stop API** | To stop a running job, the manager service must be restarted. In-flight agent assignments complete naturally. |
| **Stale clones cause errors** | Re-running an evacuation with old pgclone clones (without refreshing) leads to high etag mismatch errors because metadata has changed since the snapshot. |

---

## Known Edge Cases

### Objects That Cannot Be Evacuated

| Scenario | Behavior | Operator Action |
|----------|----------|----------------|
| **Single-replica objects** | Only copy is on the source shark — no other shark to download from. Skipped as `source_is_evac_shark`. | Indicates data durability concern. These objects have no other copy. |
| **Object deleted during evacuation** | Agent gets 404 from source shark. Skipped as `HTTPStatusCode(404)`. | Normal — object was legitimately deleted. |
| **Orphaned MPU parts** | Bucket object metadata exists but the file was deleted during MPU assembly. Skipped as `mpu_orphan` or `HTTPStatusCode(404)`. | Harmless metadata artifact. |
| **Object on same destination shark** | Object already has a copy on the proposed destination. Skipped as `DuplicateShark`. | Shark selection avoids this, but it can happen with limited destinations. |
| **Corrupt source file** | Downloaded file fails MD5 verification. Marked as `MD5Mismatch`. | Investigate source shark — potential data corruption. |
| **All destinations full** | No shark has sufficient available space. Skipped as `InsufficientSpace`. | Add capacity or increase `max_fill_percentage`. |
| **Concurrent modification** | Object metadata changed between discovery and update (etag mismatch). Error. | Retry the job — normal with active workloads. |

### Failure Modes

| Failure | Impact | Recovery |
|---------|--------|----------|
| **Manager crashes** | Job stops. PostgreSQL database survives (delegated ZFS dataset). | Restart manager, retry job. Completed objects are not reprocessed. |
| **Agent crashes mid-transfer** | Partial files left in `/var/tmp/rebalancer/temp/`. Assignment stuck in processing state. | Restart agent. Retry job — agent checks if file already exists with correct MD5. |
| **Storinfo unavailable** | Cannot select destination sharks. Job stalls with no forward progress. | Restore storinfo. Job resumes automatically on next 10-second poll. |
| **Moray/MDAPI unavailable** | Phase 4 metadata updates fail. Objects remain in `post_processing` or `error` state. | Restore metadata service. Retry job for failed objects. |
| **pgclone clone dies** | Discovery cannot continue for new objects. Already-discovered objects continue through pipeline. | Recreate clone. Restart job or retry for undiscovered objects. |
| **Network partition to source shark** | Agent cannot download objects. Skipped as `SourceOtherError` or `NetworkError`. | Restore connectivity. Retry job. |

---

## Feature Summary Matrix

| Feature | Supported | Notes |
|---------|-----------|-------|
| Evacuate all objects from a shark | Yes | Core capability |
| v1 directory objects (Moray) | Yes | Full support |
| v2 bucket objects (MDAPI) | Yes | Full support |
| Hybrid v1 + v2 evacuation | Yes | Automatic routing |
| Multipart upload (MPU) parts | Yes | Updates upload records |
| pgclone-based discovery | Yes | Recommended for production |
| Direct Moray/MDAPI RPC discovery | Yes | Not recommended (high metadata load) |
| Conditional metadata updates (etag) | Yes | Prevents lost updates |
| Job retry (failed objects) | Yes | Single command |
| Runtime thread tuning | Yes | 1–250, via API |
| Prometheus metrics | Yes | Port 8878 |
| Persistent job state | Yes | PostgreSQL + SQLite |
| Duplicate object detection | Yes | Cross-shard dedup |
| Selective object rebalancing | **No** | Evacuation only |
| Multiple concurrent jobs | **No** | One job at a time |
| Cross-datacenter evacuation | **No** | Same datacenter only |
| Manager HA / failover | **No** | Single instance |
| Automatic recovery from crashes | **No** | Manual restart + retry |
| Snaplink cleanup | **No** | Manual cleanup required |
| Job stop/pause API | **No** | Requires service restart |
| Automatic pgclone management | **No** | Manual create/destroy |
| Configurable storinfo poll interval | **No** | Fixed at 10 seconds |
