# Rebalancer Developer Architecture Guide

This document is a code navigation guide for developers working on the
rebalancer-legacy codebase.  Paths are relative to
`libs/rebalancer-legacy/` unless stated otherwise.

---

## 1. Crate and File Layout

```
rebalancer-legacy/
  manager/                          # The rebalancer manager (HTTP server + job orchestration)
    src/
      main.rs                       # Gotham HTTP server, router(), job dispatch loop
      config.rs                     # Config, ConfigOptions, MdapiConfig structs and defaults
      jobs/
        mod.rs                      # Job, JobBuilder, JobAction, JobState, Assignment,
                                    #   AssignmentCacheEntry, AssignmentCache, mark_stale_jobs_failed()
        evacuate.rs                 # EvacuateJob (~7000+ lines) -- the core evacuation logic
        status.rs                   # Job status queries (get_job, list_jobs, status counts)

  rebalancer/                       # Shared library crate (agent + common types)
    src/
      libagent.rs                   # Agent struct, router(), post_assignment_handler(),
                                    #   process_assignment(), download(), backpressure (max_queued)
      common.rs                     # ObjectSkippedReason, TaskStatus, Task, AssignmentPayload

libs/sharkspotter/                  # Separate crate for metadata tier scanning
  src/
    lib.rs                          # run(), run_multithreaded(), query_handler(), iter_ids()
    config.rs                       # Sharkspotter Config (domain, shards, chunk_size, direct_db)
    directdb.rs                     # Direct PostgreSQL scanning of moray `manta` table
    directdb_buckets.rs             # Direct PostgreSQL scanning of buckets-postgres vnodes
    mdapi_discovery.rs              # MDAPI-based object discovery
  tools/
    pgclone.sh                      # Provision read-only PostgreSQL clones for sharkspotter
```

### Key functions in `evacuate.rs`

| Function / Struct                   | Purpose |
|-------------------------------------|---------|
| `EvacuateObjectStatus` (enum)       | Object lifecycle states |
| `EvacuateObjectError` (enum)        | Error classification for objects |
| `create_evacuateobjects_table()`    | DDL for per-job PostgreSQL table |
| `EvacuateObject` (struct)           | Row in `evacuateobjects` table |
| `EvacuateJob::run()`               | Job entry point, thread orchestration |
| `PostAssignment::post()`           | POST assignment to agent, 503 retry loop |
| `GetAssignment::get()`             | GET assignment status from agent |
| `UpdateMetadata::update_object_shark()` | Replace old shark in metadata |
| `ProcessAssignment::process()`     | Handle completed assignment from agent |
| `start_local_db_generator()`       | Thread for retry job local DB reads |
| `start_sharkspotter()`             | Thread for initial object discovery |
| `start_assignment_manager()`       | Thread that distributes objects to sharks |
| `shark_assignment_generator()`     | Per-shark thread that fills assignments |
| `start_assignment_post()`          | Thread that POSTs filled assignments |
| `start_assignment_checker()`       | Thread that polls agents for completion |
| `start_metadata_update_broker()`   | Thread that dispatches metadata updates |

---

## 2. Threading Model

`EvacuateJob::run()` spawns all threads and wires them together with
crossbeam channels.  The pipeline has a strict order — each step feeds
the next:

### Step-by-step thread execution order

**Step 1 — Discovery:** Find objects on the source shark.

```
+---------------------+   bounded(10)   +---------------------+   bounded(100)
| Shard Scanner       | Sharkspotter    | Sharkspotter        | EvacuateObject
| Threads             +---------------->| Translator Thread   +---------------->  (to Step 2)
| (one per shard)     |    Message      | (single)            |
| sharkspotter/lib.rs |                 | evacuate.rs         |
+---------------------+                 +---------------------+
                                           |
                                           | Converts SharkspotterMessage
                                           | to EvacuateObject, inserts
                                           | into job DB as "unprocessed"
```

**Step 2 — Assignment creation:** Group objects by destination shark.

```
                        bounded(100)   +---------------------+
(from Step 1)  EvacuateObject          | Assignment Manager  |
              +----------------------->| (single thread)     |
                                       | evacuate.rs         |
                                       +--+------+-----------+
                                          |      |
                  Per-shark channels      |      |  bounded(1)
                  sends: AssignmentMsg    |      |  sends: FiniMsg
                  (Data/Flush/Stop)       |      |  (shutdown signal to checker)
                            +-------------+      +----------->  (to Step 4)
                            |
                            v
                  +---------------------+
                  | Shark Assignment    |   bounded(5)
                  | Generator Threads   |   sends: Assignment
                  | (one per dest shark)+------------------>  (to Step 3)
                  | evacuate.rs         |
                  +---------------------+
                     |
                     | Batches objects into assignments of
                     | max_tasks_per_assignment (default 50).
                     | Flushes after max_assignment_age (default 3600s).
```

**Step 3 — Post to agents:** Send assignments to storage node agents.

```
                     bounded(5)   +---------------------+
(from Step 2)  Assignment         | Assignment Poster   |
              +------------------>| (single thread)     |
                                  | evacuate.rs         |
                                  +----------+----------+
                                             |
                                             | HTTP POST /assignments
                                             | (with 503 retry + backoff)
                                             v
                                  +---------------------+
                                  | Storage Agents      |
                                  | (one per shark)     |
                                  | libagent.rs :7878   |
                                  +---------------------+
                                     |
                                     | Agent downloads objects from
                                     | source shark, verifies MD5,
                                     | moves to /manta/.
```

**Step 4 — Check completion:** Poll agents for finished assignments.

```
                     bounded(1)   +---------------------+
(from Step 2)  FiniMsg            | Assignment Checker  |
(shutdown)    +------------------>| (single thread)     |
                                  | evacuate.rs         |
                                  +----------+----------+
                                             |
                                             | HTTP GET /assignments/{id}
                                             | Polls agents every 500ms.
                                             | Times out after 2 * max_assignment_age.
                                             |
                                             |  bounded(5)
                                             |  sends: AssignmentCacheEntry
                                             +-------------------->  (to Step 5)
```

**Step 5 — Metadata update:** Update live moray/mdapi with new shark location.

```
                     bounded(5)        +---------------------+
(from Step 4)  AssignmentCacheEntry    | MD Update Broker    |
              +----------------------->| (single thread)     |
                                       | evacuate.rs         |
                                       +----------+----------+
                                                  |
                                                  | Dispatches to worker threads
                                                  v
                                       +---------------------+
                                       | MD Update Workers   |
                                       | (dynamic or static  |
                                       |  pool, 1-250)       |
                                       | evacuate.rs         |
                                       +---------------------+
                                          |
                                          | Moray putObject (v1) or
                                          | MDAPI update (v2) with
                                          | etag conditional write.
                                          | Object marked "complete"
                                          | on success.
```

### Startup and shutdown

**Startup:** `EvacuateJob::run()` spawns threads in this order:
1. Metadata update broker (waits on channel)
2. Assignment checker (waits on channel)
3. Assignment poster (waits on channel)
4. Storinfo poller (background refresh every 10s)
5. Assignment manager (starts consuming objects, spawns per-shark generators)
6. Sharkspotter thread + translator (starts scanning, feeds objects to step 2)

**Shutdown:** When sharkspotter finishes scanning:
1. Sharkspotter thread exits → translator drains and exits
2. `obj_tx` channel closes → assignment manager drains remaining objects
3. Assignment manager sends `FiniMsg` to checker → exits
4. Checker drains remaining assigned items → sends to MD broker → exits
5. `md_update_tx` closes → MD broker drains → exits
6. `EvacuateJob::run()` joins all threads → job marked `Complete`

### Channel summary

| Channel | Capacity | Sender | Receiver | Message Type |
|---------|----------|--------|----------|--------------|
| Job dispatch | bounded(5) | Gotham HTTP handler | Job runner ThreadPool | `Job` |
| Sharkspotter internal | bounded(10) | Shard scanner threads | Translator thread | `SharkspotterMessage` |
| Object stream | bounded(100) | Translator thread | Assignment manager | `EvacuateObject` |
| Per-shark assignment | unbounded | Assignment manager | Shark assignment generator | `AssignmentMsg` (Data/Flush/Stop) |
| Full assignment | bounded(5) | Shark assignment generators | Assignment poster | `Assignment` |
| Checker finish | bounded(1) | Assignment manager | Assignment checker | `FiniMsg` |
| MD update | bounded(5) | Assignment checker | Metadata update broker | `AssignmentCacheEntry` |

---

## 3. State Machine

### 3.1 EvacuateObject Lifecycle

Defined as `EvacuateObjectStatus`:

```
                    +---> Skipped (at any point, with ObjectSkippedReason)
                    |
                    +---> Error   (at any point, with EvacuateObjectError)
                    |
  Unprocessed ------+---> Assigned -----> PostProcessing -----> Complete
       |                     |                  |
       |                     |                  |
  [insert_into_db]    [assignment_post]  [metadata_update]
  by sharkspotter     by poster thread   by MD worker threads
  translator or       sets status in DB  sets status in DB
  assignment manager
```

**State transitions and responsible threads:**

| From | To | Thread | Function |
|------|----|--------|----------|
| (new) | Unprocessed | Sharkspotter translator | `insert_into_db()` |
| Unprocessed | Assigned | Shark assignment generator | `insert_assignment_into_db()` |
| Unprocessed | Skipped | Shark assignment generator | `skip_object()` |
| Assigned | PostProcessing | Assignment checker | `process()` impl, then `mark_assignment_objects()` |
| Assigned | Skipped | Assignment checker | On agent timeout or error, `skip_assignment()` |
| PostProcessing | Complete | MD update worker | `mark_many_objects()` |
| PostProcessing | Error | MD update worker | On moray/mdapi update failure |
| Any | Error | Various | `mark_object_error()` |

### 3.2 AssignmentState Transitions

Defined in `jobs/mod.rs`:

```
  Init -----> Assigned -----> AgentComplete -----> PostProcessed
                |
                +-----> Rejected (agent rejects POST)
                +-----> AgentUnavailable (connection error or timeout)
```

| From | To | Thread | Trigger |
|------|----|--------|---------|
| Init | Assigned | Poster thread | Successful POST to agent |
| Init | Rejected | Poster thread | Non-503 error from agent |
| Init | AgentUnavailable | Poster thread | Connection failure |
| Assigned | AgentComplete | Checker thread | Agent reports Complete |
| Assigned | AgentUnavailable | Checker thread | Agent unreachable or timeout |

### 3.3 AssignmentCacheEntry Lifecycle

Defined in `jobs/mod.rs`.  Created via `From<Assignment>`
which stamps `created_at: Instant::now()`.

1. Created when shark assignment generator sends a full assignment
   (`_channel_send_assignment()`).
2. Inserted into `EvacuateJob.assignments: RwLock<AssignmentCache>`.
3. Assignment checker reads the cache, polls agents, transitions states.
4. On `AgentComplete`, sent to MD update broker via `md_update_tx`.
5. After metadata updates complete, the entry remains in the cache
   in `PostProcessed` state (memory reclaimed when job finishes).

---

## 4. Flow Control Implementation

Three distinct mechanisms prevent the system from overwhelming agents or
accumulating unbounded work.

### 4.1 Agent Backpressure (`max_queued` + 503)

**File:** `rebalancer/src/libagent.rs`

The agent rejects new assignments with HTTP 503 when its scheduled queue
is full.

```
// libagent.rs —
let max_queued = workers * 50;
```

In `post_assignment_handler()`, before accepting an assignment:

```rust
// libagent.rs —
let scheduled_count = WalkDir::new(REBALANCER_SCHEDULED_DIR)
    .min_depth(1)
    .into_iter()
    .filter_map(|e| e.ok())
    .count();

if scheduled_count >= agent.max_queued {
    // returns 503 SERVICE_UNAVAILABLE
}
```

The count is derived by walking `/var/tmp/rebalancer/scheduled/` on the
agent filesystem.  Each assignment is a SQLite file named by UUID.

### 4.2 Manager 503 Retry Loop

**File:** `manager/src/jobs/evacuate.rs`, `PostAssignment::post()`.

When the agent returns 503, the manager retries with capped exponential
backoff:

```rust
const MAX_RETRIES: u32 = 60;
let backoff = std::cmp::min(5 * attempt, 30);  // seconds: 5, 10, 15, 20, 25, 30, 30, 30...
thread::sleep(Duration::from_secs(backoff as u64));
```

- Maximum 60 retries (up to ~30 minutes at 30s intervals).
- On success after retry, calls `assignment_post_success()`.
- On exhausted retries, marks objects as `Skipped(AgentBusy)`.
- On connection error during retry, marks `Skipped(DestinationUnreachable)`.

### 4.3 Checker Timeout

**File:** `manager/src/jobs/evacuate.rs`, `start_assignment_checker()`.

When polling agents, if an assignment has been in `Assigned` state for
longer than `2 * max_assignment_age`, it is timed out:

```rust
let max_age = job_action.config.options.max_assignment_age;
if ace.created_at.elapsed().as_secs() > max_age * 2 {
    job_action.skip_assignment(
        &ace.id,
        ObjectSkippedReason::AgentAssignmentTimeout,
        AssignmentState::AgentUnavailable,
    );
}
```

The `created_at` field is stamped when `AssignmentCacheEntry` is created
from `Assignment` (jobs/mod.rs).  Default `max_assignment_age` is
3600 seconds (config.rs), so the timeout is 7200 seconds (2 hours).

The checker also uses concurrent polling (CHECKER_CONCURRENCY = 8,
evacuate.rs) to avoid blocking on sequential HTTP GETs across
hundreds of agents.

### 4.4 Assignment Age Flush

In each shark assignment generator thread (evacuate.rs), on
every `Flush` message from the assignment manager, assignments older than
`max_assignment_age` are sent to the poster even if they have fewer than
`max_tasks_per_assignment` tasks:

```rust
if assignment_len > 0
    && assignment_birth_time.elapsed().as_secs() > max_age
```

---

## 5. Database Schema

### 5.1 Manager-side PostgreSQL

The manager connects to a local PostgreSQL instance.  All databases use
diesel with the `Pg` backend.

#### `rebalancer` database -- global job registry

Created by `create_job_database()` in `jobs/mod.rs`.

```sql
CREATE TABLE IF NOT EXISTS jobs(
    id      TEXT PRIMARY KEY,
    action  TEXT CHECK(action IN ('evacuate', 'none')) NOT NULL,
    state   TEXT CHECK(state IN ('init', 'setup', 'running',
                                  'stopped', 'complete', 'failed')) NOT NULL
);
```

Diesel schema: `jobs/mod.rs`.

#### Per-job database (named by job UUID)

Each job creates a database named after its UUID (e.g.,
`a1b2c3d4-...`).  Created via `connect_or_create_db()`.

**`evacuateobjects` table** -- created by
`create_evacuateobjects_table()` at:

```sql
CREATE TABLE evacuateobjects(
    id              TEXT PRIMARY KEY,
    assignment_id   TEXT,
    object          JSONB,
    shard           INTEGER,
    dest_shark      TEXT,
    etag            TEXT,
    status          TEXT CHECK(status IN ('unprocessed', 'assigned',
                        'skipped', 'error', 'post_processing',
                        'complete')) NOT NULL,
    skipped_reason  TEXT CHECK(skipped_reason IN (...)),
    error           TEXT CHECK(error IN (...))
);
CREATE INDEX assignment_id ON evacuateobjects (assignment_id);
```

Diesel schema and Rust struct `EvacuateObject` are defined in `evacuate.rs`.

**`config` table** -- stores the from_shark for this job:

```sql
-- Diesel schema in evacuate.rs
config(id INTEGER, from_shark JSONB)
```

**`duplicates` table** -- tracks objects seen on multiple shards:

```sql
-- Diesel schema in evacuate.rs
duplicates(id TEXT PRIMARY KEY, key TEXT, shards INTEGER[])
```

### 5.2 Agent-side SQLite

The agent stores assignments as SQLite database files on the local
filesystem.

**Paths** (libagent.rs):
- `/var/tmp/rebalancer/scheduled/<assignment-uuid>` -- queued
- `/var/tmp/rebalancer/completed/<assignment-uuid>` -- finished
- `/var/tmp/rebalancer/temp/` -- partial downloads (cleaned on startup)

Each SQLite file contains two tables, created in `assignment_save()`
(libagent.rs):

```sql
CREATE TABLE IF NOT EXISTS tasks (
    object_id        TEXT PRIMARY KEY NOT NULL UNIQUE,
    owner            TEXT NOT NULL,
    md5sum           TEXT NOT NULL,
    datacenter       TEXT NOT NULL,
    manta_storage_id TEXT NOT NULL,
    status           TEXT NOT NULL,
    bucket_id        TEXT,
    object_name_hash TEXT
);

CREATE TABLE IF NOT EXISTS stats (
    stats TEXT NOT NULL
);
```

The `stats` column contains JSON-serialized `AgentAssignmentStats`
(libagent.rs).

---

## 6. pgclone DNS Registration

**File:** `libs/sharkspotter/tools/pgclone.sh`

The `pgclone.sh` script creates read-only PostgreSQL clone VMs from
Manatee (moray) or buckets-postgres instances.  Each clone registers
itself in Triton's DNS (via the `registrar` service) so that
sharkspotter can find it.

### 6.1 Registrar Config Mutation

The script copies the source VM's registrar config and mutates it using
JavaScript `replace()` expressions passed to `json(1)`.

**For moray clones** (`do_clone()` at pgclone.sh):

```
alias_replace:  /\.moray\./, ".rebalancer-postgres."
domain_replace: /^.*\.moray\./, "rebalancer-postgres."
```

Example: source domain `1.moray.us-east.joyent.us` becomes
`rebalancer-postgres.us-east.joyent.us`, and the alias becomes
`1.rebalancer-postgres.us-east.joyent.us`.

**For buckets clones** (pgclone.sh):

```
alias_replace:  /\.(buckets-postgres|buckets-mdapi)\./, ".rebalancer-buckets-postgres."
domain_replace: /^.*\.(buckets-postgres|buckets-mdapi)\./, "rebalancer-buckets-postgres."
```

Example: `1.buckets-postgres.us-east.joyent.us` becomes
`rebalancer-buckets-postgres.us-east.joyent.us`, alias
`1.rebalancer-buckets-postgres.us-east.joyent.us`.

### 6.2 Shard Number Preservation

The shard number is preserved because the regex replacements only match
the service name portion after the leading digit.  The `alias` field
retains the `N.` prefix.  Sharkspotter connects via:

- `{shard}.rebalancer-postgres.{domain}` (directdb.rs)
- `{shard}.rebalancer-buckets-postgres.{domain}` (directdb_buckets.rs)

### 6.3 Startup Sequence

In `generate_setup_script()` (pgclone.sh), the in-zone
`setup.sh` performs:

1. Create `postgres` user/group (uid/gid 907).
2. Disable `autovacuum` in `postgresql.conf`.
3. Disable `recovery.conf` to prevent WAL replay from live Manatee.
4. Open `pg_hba.conf` to trust all connections.
5. Generate SMF service manifest and `svccfg import pg.xml`.
6. Determine the zone's Manta IP from `mdata-get sdc:nics`.
7. Validate the source registrar domain matches the expected pattern.
8. Apply the `alias_replace` and `domain_replace` regex mutations to the
   copied registrar `config.json.in`, writing `config.json`.
9. `svccfg import registrar.xml && svcadm enable registrar`.

---

## 7. Key Design Decisions and Trade-offs

### 7.1 No In-Job Retries (Single-Pass Design)

Objects that fail during an evacuation job are marked `Skipped` or
`Error` in the per-job database and are not retried within the same job
run.  Instead, the operator starts a new retry job via
`POST /jobs/<uuid>/retry`, which reads the previous job's
database for objects in non-`Complete` status
(`start_local_db_generator()`).

**Rationale:** The single-pass design avoids infinite retry loops on
persistent failures (e.g., corrupt objects, permanently dead sharks).
It also simplifies the threading model -- each thread runs to
completion and the channel DAG has a clear shutdown sequence.

### 7.2 Bounded Agent Queue (workers * 50)

The agent's `max_queued` is set to `workers * 50` (libagent.rs).

**History:** An earlier version used an unlimited queue.  This caused the
manager to blast hundreds of assignments that sat in `Scheduled` state on
the agent, consuming disk space and eventually timing out in the checker
(after `2 * max_assignment_age`).  The bounded queue with 503
backpressure (libagent.rs) ensures the manager backs off when
agents are saturated, and the 503 retry loop in `post()`
(evacuate.rs) handles the backoff gracefully.

### 7.3 max_assignment_age Increased from 600 to 3600

Default is 3600 seconds (config.rs,
`DEFAULT_MAX_ASSIGNMENT_AGE`).

**Rationale:** With the original 600-second (10 minute) max age,
assignments were being flushed with very few tasks when the object
discovery phase was slow (large shards, slow moray queries).  This
produced many small assignments, increasing per-assignment overhead
(HTTP POST, SQLite save, agent scheduling, metadata update).  The
3600-second default allows assignments to accumulate up to
`max_tasks_per_assignment` (default 50) tasks before being flushed,
significantly reducing the total number of assignments.

The checker timeout is `2 * max_assignment_age` = 7200 seconds (2 hours).
This is generous enough for agents with large downloads.

### 7.4 mark_stale_jobs_failed() on Startup

In `main.rs`, the manager calls `mark_stale_jobs_failed()`
(jobs/mod.rs) at startup, which transitions any `Running`, `Setup`,
or `Init` jobs to `Failed`.

**Rationale:** The manager cannot resume in-flight jobs.  The in-memory
assignment cache (`RwLock<AssignmentCache>`), all worker threads, and
crossbeam channel state are lost on restart.  Leaving stale `Running`
entries would confuse operators into thinking a job is still active.
Marking them `Failed` makes it clear that a new job (or retry) is
needed.

### 7.5 Download Retries (3x) but No Metadata Update Retries Within Assignments

The agent retries downloads up to 3 times with exponential backoff
(libagent.rs):

```rust
const MAX_RETRIES: u32 = 3;
// backoff: 1s, 2s, 4s (2^attempt)
```

Metadata updates within the manager's MD update workers are **not**
retried within a single job pass.  If a moray `put` or mdapi `update`
fails, the object is marked with `EvacuateObjectError::MorayUpdateFailed`
(or `MdapiUpdateFailed`) and left for a retry job.

**Rationale:** Download failures are often transient (network blips,
brief unavailability of source sharks).  Retrying 3 times at the agent
level handles the vast majority of cases without adding complexity to
the manager.

Metadata update failures, on the other hand, can be caused by etag
conflicts (concurrent modifications), which would require re-reading the
object and re-computing the shark replacement -- a much more complex
retry loop.  The simpler approach is to let the retry job handle it with
fresh etag values from the database.
