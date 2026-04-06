# Rebalancer Developer Architecture Guide

This document is a code navigation guide for developers working on the
rebalancer-legacy codebase.  Every claim is referenced to a specific file,
line number, function, or struct.  Paths are relative to
`libs/rebalancer-legacy/` unless stated otherwise.

---

## 1. Crate and File Layout

```
rebalancer-legacy/
  manager/                          # The rebalancer manager (HTTP server + job orchestration)
    src/
      main.rs                       # Gotham HTTP server, router(), job dispatch loop (line 570-648)
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

### Key line numbers in `evacuate.rs`

| Function / Struct                   | Line   | Purpose |
|-------------------------------------|--------|---------|
| `EvacuateObjectStatus` (enum)       | 1004   | Object lifecycle states |
| `EvacuateObjectError` (enum)        | 1050   | Error classification for objects |
| `create_evacuateobjects_table()`    | 1281   | DDL for per-job PostgreSQL table |
| `EvacuateObject` (struct)           | 1340   | Row in `evacuateobjects` table |
| `EvacuateJob::run()`               | 1638   | Job entry point, thread orchestration |
| `PostAssignment::post()`           | 2781   | POST assignment to agent, 503 retry loop |
| `GetAssignment::get()`             | 2914   | GET assignment status from agent |
| `UpdateMetadata::update_object_shark()` | 2960 | Replace old shark in metadata |
| `ProcessAssignment::process()`     | 3034   | Handle completed assignment from agent |
| `start_local_db_generator()`       | 3369   | Thread for retry job local DB reads |
| `start_sharkspotter()`             | 3384   | Thread for initial object discovery |
| `start_assignment_manager()`       | 3820   | Thread that distributes objects to sharks |
| `shark_assignment_generator()`     | 4096   | Per-shark thread that fills assignments |
| `start_assignment_post()`          | 4346   | Thread that POSTs filled assignments |
| `start_assignment_checker()`       | 4435   | Thread that polls agents for completion |
| `start_metadata_update_broker()`   | 5432   | Thread that dispatches metadata updates |

---

## 2. Threading Model

The following ASCII diagram shows every thread spawned during an evacuation
job, the crossbeam channels connecting them, and the messages that flow
through each channel.

```
                                    +-----------------+
                                    | Gotham HTTP      |
                                    | (1 thread)       |
                                    | main.rs:697      |
                                    +--------+--------+
                                             |
                                             | crossbeam::bounded(5)
                                             | sends: Job
                                             v
                                    +-----------------+
                                    | Job Runner       |
                                    | ThreadPool(1)    |  THREAD_COUNT=1 (main.rs:57)
                                    | main.rs:583-604  |
                                    +--------+--------+
                                             |
                                             | job.run() -> EvacuateJob::run() (evacuate.rs:1638)
                                             v
      +-----------------------------+--------+---------+---------------------------+
      |                             |                  |                           |
      v                             v                  v                           v
+-------------+            +----------------+   +-------------+           +------------------+
| Sharkspotter|            | Assignment     |   | Assignment  |           | Metadata Update  |
| Thread      |            | Manager        |   | Poster      |           | Broker           |
| (evacuate.rs|            | (single)       |   | (single)    |           | (single)         |
|  :3384)     |            | (evacuate.rs   |   | (evacuate.rs|           | (evacuate.rs     |
|             |            |  :3820)        |   |  :4346)     |           |  :5432)          |
+------+------+            +-------+--------+   +------+------+           +--------+---------+
       |                           |                   ^                           |
       | crossbeam::bounded(100)   |                   |                           |
       | sends: EvacuateObject     |                   | crossbeam::bounded(5)     |
       v                           |                   | sends: Assignment         |
+-------------+                    |                   |                           |
| Sharkspotter|                    +-------------------+                           |
| Translator  |                    |                                               |
| Thread      |                    | Per-shark crossbeam channels                  |
| (evacuate.rs|                    | sends: AssignmentMsg (Data/Flush/Stop)        |
|  :3423)     |                    v                                               |
+-+-----------+       +------------------------+                                   |
  |                   | Shark Assignment       |                                   |
  |   +-------------->| Generator Threads      |                                   |
  |   | (one per      | (one per dest shark)   |                                   |
  |   |  shard)       | (evacuate.rs:4096)     |                                   |
  |   |               +------------------------+                                   |
  |   |                                                                            |
  |   |                                      +-------------------+                 |
  |   |  ThreadPool("shard_scanner")         | Assignment        |                 |
  |   |  (sharkspotter/lib.rs:1012)          | Checker           |                 |
  |   |  one thread per shard                | (single)          |<-----+          |
  |   +--------------------------------------| (evacuate.rs      |      |          |
  |                                          |  :4435)           |      |          |
  |                                          +--------+----------+      |          |
  |                                                   |                 |          |
  |                                                   | crossbeam      |          |
  |                                                   | ::bounded(5)   |          |
  |                                                   | sends:         |          |
  |                                                   | Assignment-    |          |
  |                                                   | CacheEntry     |          |
  |                                                   v                |          |
  |                                          +------------------+      |          |
  |                                          | MD Update Worker |      |          |
  |                                          | Threads          |      |          |
  |                                          | (dynamic or      |      |          |
  |                                          |  static pool)    |      |          |
  |                                          | (evacuate.rs     |      |          |
  |                                          |  :5178 / :5340)  |      |          |
  |                                          +------------------+      |          |
  |                                                                    |          |
  |                   crossbeam::bounded(1)                            |          |
  |                   sends: FiniMsg (shutdown signal)                 |          |
  |                   assignment_manager --> checker                   +----------+
  +                                                                    md_update_tx
```

### Channel summary

| Channel | Capacity | Sender | Receiver | Message Type |
|---------|----------|--------|----------|--------------|
| Job dispatch | bounded(5) | Gotham HTTP handler | Job runner ThreadPool | `Job` |
| Object stream | bounded(100) | Sharkspotter translator | Assignment manager | `EvacuateObject` |
| Sharkspotter internal | bounded(10) | Shard scanner threads | Sharkspotter translator | `SharkspotterMessage` |
| Per-shark assignment | unbounded | Assignment manager | Shark assignment generator | `AssignmentMsg` (Data/Flush/Stop) |
| Full assignment | bounded(5) | Shark assignment generators | Assignment poster | `Assignment` |
| Checker finish | bounded(1) | Assignment manager | Assignment checker | `FiniMsg` |
| MD update | bounded(5) | Assignment checker | Metadata update broker | `AssignmentCacheEntry` |

---

## 3. State Machine

### 3.1 EvacuateObject Lifecycle

Defined in `evacuate.rs:1004` as `EvacuateObjectStatus`:

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
| (new) | Unprocessed | Sharkspotter translator | `insert_into_db()` evacuate.rs:2086 |
| Unprocessed | Assigned | Shark assignment generator | `insert_assignment_into_db()` |
| Unprocessed | Skipped | Shark assignment generator | `skip_object()` |
| Assigned | PostProcessing | Assignment checker | `process()` impl at :3034, then `mark_assignment_objects()` |
| Assigned | Skipped | Assignment checker | On agent timeout or error, `skip_assignment()` |
| PostProcessing | Complete | MD update worker | `mark_many_objects()` at :1821 |
| PostProcessing | Error | MD update worker | On moray/mdapi update failure |
| Any | Error | Various | `mark_object_error()` |

### 3.2 AssignmentState Transitions

Defined in `jobs/mod.rs:381`:

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

Defined in `jobs/mod.rs:416`.  Created via `From<Assignment>` (line 426)
which stamps `created_at: Instant::now()`.

1. Created when shark assignment generator sends a full assignment
   (`_channel_send_assignment()` at evacuate.rs:3861).
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
// libagent.rs:1388
let max_queued = workers * 50;
```

In `post_assignment_handler()` (line 615), before accepting an assignment:

```rust
// libagent.rs:659-680
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

**File:** `manager/src/jobs/evacuate.rs`, `PostAssignment::post()` at line 2781.

When the agent returns 503, the manager retries with capped exponential
backoff:

```rust
// evacuate.rs:2816-2832
const MAX_RETRIES: u32 = 60;
let backoff = std::cmp::min(5 * attempt, 30);  // seconds: 5, 10, 15, 20, 25, 30, 30, 30...
thread::sleep(Duration::from_secs(backoff as u64));
```

- Maximum 60 retries (up to ~30 minutes at 30s intervals).
- On success after retry, calls `assignment_post_success()`.
- On exhausted retries, marks objects as `Skipped(AgentBusy)`.
- On connection error during retry, marks `Skipped(DestinationUnreachable)`.

### 4.3 Checker Timeout

**File:** `manager/src/jobs/evacuate.rs`, `start_assignment_checker()` at line 4435.

When polling agents, if an assignment has been in `Assigned` state for
longer than `2 * max_assignment_age`, it is timed out:

```rust
// evacuate.rs:4555-4571
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
from `Assignment` (jobs/mod.rs:432).  Default `max_assignment_age` is
3600 seconds (config.rs:54), so the timeout is 7200 seconds (2 hours).

The checker also uses concurrent polling (CHECKER_CONCURRENCY = 8,
evacuate.rs:4501) to avoid blocking on sequential HTTP GETs across
hundreds of agents.

### 4.4 Assignment Age Flush

In each shark assignment generator thread (evacuate.rs:4168-4177), on
every `Flush` message from the assignment manager, assignments older than
`max_assignment_age` are sent to the poster even if they have fewer than
`max_tasks_per_assignment` tasks:

```rust
// evacuate.rs:4168-4170
if assignment_len > 0
    && assignment_birth_time.elapsed().as_secs() > max_age
```

---

## 5. Database Schema

### 5.1 Manager-side PostgreSQL

The manager connects to a local PostgreSQL instance.  All databases use
diesel with the `Pg` backend.

#### `rebalancer` database -- global job registry

Created by `create_job_database()` in `jobs/mod.rs:600`.

```sql
CREATE TABLE IF NOT EXISTS jobs(
    id      TEXT PRIMARY KEY,
    action  TEXT CHECK(action IN ('evacuate', 'none')) NOT NULL,
    state   TEXT CHECK(state IN ('init', 'setup', 'running',
                                  'stopped', 'complete', 'failed')) NOT NULL
);
```

Diesel schema: `jobs/mod.rs:258-265`.

#### Per-job database (named by job UUID)

Each job creates a database named after its UUID (e.g.,
`a1b2c3d4-...`).  Created via `connect_or_create_db()`.

**`evacuateobjects` table** -- created by
`create_evacuateobjects_table()` at evacuate.rs:1281:

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

Diesel schema: evacuate.rs:901-914.  Rust struct: `EvacuateObject` at
evacuate.rs:1340.

**`config` table** -- stores the from_shark for this job:

```sql
-- Diesel schema at evacuate.rs:916-922
config(id INTEGER, from_shark JSONB)
```

**`duplicates` table** -- tracks objects seen on multiple shards:

```sql
-- Diesel schema at evacuate.rs:924-931
duplicates(id TEXT PRIMARY KEY, key TEXT, shards INTEGER[])
```

### 5.2 Agent-side SQLite

The agent stores assignments as SQLite database files on the local
filesystem.

**Paths** (libagent.rs:46-48):
- `/var/tmp/rebalancer/scheduled/<assignment-uuid>` -- queued
- `/var/tmp/rebalancer/completed/<assignment-uuid>` -- finished
- `/var/tmp/rebalancer/temp/` -- partial downloads (cleaned on startup)

Each SQLite file contains two tables, created in `assignment_save()`
(libagent.rs:297):

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
(libagent.rs:86-103).

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

**For moray clones** (`do_clone()` at pgclone.sh:714-722):

```
alias_replace:  /\.moray\./, ".rebalancer-postgres."
domain_replace: /^.*\.moray\./, "rebalancer-postgres."
```

Example: source domain `1.moray.us-east.joyent.us` becomes
`rebalancer-postgres.us-east.joyent.us`, and the alias becomes
`1.rebalancer-postgres.us-east.joyent.us`.

**For buckets clones** (pgclone.sh:724-731):

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

- `{shard}.rebalancer-postgres.{domain}` (directdb.rs:61)
- `{shard}.rebalancer-buckets-postgres.{domain}` (directdb_buckets.rs:325)

### 6.3 Startup Sequence

In `generate_setup_script()` (pgclone.sh:361-536), the in-zone
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
`POST /jobs/<uuid>/retry` (main.rs:638), which reads the previous job's
database for objects in non-`Complete` status
(`start_local_db_generator()` at evacuate.rs:3369).

**Rationale:** The single-pass design avoids infinite retry loops on
persistent failures (e.g., corrupt objects, permanently dead sharks).
It also simplifies the threading model -- each thread runs to
completion and the channel DAG has a clear shutdown sequence.

### 7.2 Bounded Agent Queue (workers * 50)

The agent's `max_queued` is set to `workers * 50` (libagent.rs:1388).

**History:** An earlier version used an unlimited queue.  This caused the
manager to blast hundreds of assignments that sat in `Scheduled` state on
the agent, consuming disk space and eventually timing out in the checker
(after `2 * max_assignment_age`).  The bounded queue with 503
backpressure (libagent.rs:654-680) ensures the manager backs off when
agents are saturated, and the 503 retry loop in `post()`
(evacuate.rs:2815-2889) handles the backoff gracefully.

### 7.3 max_assignment_age Increased from 600 to 3600

Default is 3600 seconds (config.rs:54,
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

In `main.rs:685`, the manager calls `mark_stale_jobs_failed()`
(jobs/mod.rs:630) at startup, which transitions any `Running`, `Setup`,
or `Init` jobs to `Failed`.

**Rationale:** The manager cannot resume in-flight jobs.  The in-memory
assignment cache (`RwLock<AssignmentCache>`), all worker threads, and
crossbeam channel state are lost on restart.  Leaving stale `Running`
entries would confuse operators into thinking a job is still active.
Marking them `Failed` makes it clear that a new job (or retry) is
needed.

### 7.5 Download Retries (3x) but No Metadata Update Retries Within Assignments

The agent retries downloads up to 3 times with exponential backoff
(libagent.rs:986-1015):

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
