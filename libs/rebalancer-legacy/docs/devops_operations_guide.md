# Manta Rebalancer DevOps Operations Guide

This guide covers the architecture, operation, monitoring, and troubleshooting
of the Manta Rebalancer system for production environments.

---

## Table of Contents

1. [Architecture Overview](#architecture-overview)
2. [Component Inventory](#component-inventory)
3. [How Evacuation Works](#how-evacuation-works)
4. [pgclone: Read-Only PostgreSQL Clones for Discovery](#pgclone-read-only-postgresql-clones-for-discovery)
5. [Deployment and Boot Sequence](#deployment-and-boot-sequence)
6. [Configuration Reference](#configuration-reference)
7. [Operating Procedures](#operating-procedures)
8. [Performance Tuning](#performance-tuning)
9. [Monitoring and Alerting](#monitoring-and-alerting)
10. [Troubleshooting](#troubleshooting)
11. [Database Operations](#database-operations)
12. [Appendix: Error and Skip Reason Reference](#appendix-error-and-skip-reason-reference)

---

## Architecture Overview

The rebalancer evacuates objects from a source storage node (shark) to healthy
destination sharks while keeping Manta metadata consistent. It consists of a
centralized **manager** that orchestrates jobs, and **agents** running on every
storage node that perform the actual data movement.

[!NOTE]
> Objects evacuated from target shark are not removed from it, the objects
> are evacuated using the replicas from the rest of the sharks and the metadata
> is updated to point to the new sharks where the objects now live. 


### System Topology

```
                          ┌──────────────┐
                          │   STORINFO   │
                          │ (shark list  │
                          │  + capacity) │
                          └──────┬───────┘
                                 │ HTTP (periodic poll every 10s)
                                 ▼
┌─────────────────────────────────────────────────────────┐
│                   MANAGER ZONE                          │
│                                                         │
│  ┌──────────────────┐    ┌────────────────────────┐     │
│  │rebalancer-manager│    │     PostgreSQL 12.4    │     │
│  │   Port 80 (API)  │◄──►│  Port 5432 (local)     │     │
│  │   Port 8878      │    │  DB: rebalancer        │     │
│  │   (metrics)      │    │  DB: <job-uuid> per job│     │
│  └────────┬─────────┘    └────────────────────────┘     │
│           │                                             │
│  ┌────────┴─────────┐                                   │
│  │ rebalancer-adm   │  CLI tool for operators           │
│  └──────────────────┘                                   │
└───────────┬─────────────────────────────────────────────┘
            │
            │  POST /assignments (send work)
            │  GET  /assignments/{id} (poll status)
            │
    ┌───────┴──────────────────────────────────────┐
    │              STORAGE TIER                    │
    │                                              │
    │  ┌──────────┐  ┌──────────┐  ┌──────────┐    │
    │  │ mako-1   │  │ mako-2   │  │ mako-N   │    │
    │  │ (agent)  │  │ (agent)  │  │ (agent)  │    │
    │  │ :7878 API│  │ :7878 API│  │ :7878 API│    │
    │  │ :8878 met│  │ :8878 met│  │ :8878 met│    │
    │  └──────────┘  └──────────┘  └──────────┘    │
    └──────────────────────────────────────────────┘
            │
            │  HTTP GET (download objects from source shark)
            ▼
    ┌──────────────┐
    │ SOURCE SHARK │  (being evacuated, marked read-only)
    │ e.g. 1.stor  │
    └──────────────┘
```

### Metadata Tier Integration

The manager interacts with the metadata tier in **two distinct phases** that
use different backends:

```
                         DISCOVERY (Phase 1)              METADATA UPDATE (Phase 4)
                         Read-only, uses pgclone          Read-write, uses live services
                         ─────────────────────            ──────────────────────────────

┌──────────────┐    direct_db: true        ┌──────────────────────────────────────┐
│  pgclone     │◄──── direct SQL ──────────│                                      │
│  moray clone │  (read-only PG clone)     │                                      │
│  {shard}.    │                           │                                      │
│  rebalancer- │                           │          MANAGER                     │
│  postgres    │                           │                                      │
│  :5432       │                           │    sharkspotter ──► assignment ──►   │
└──────────────┘                           │    (discovery)      manager          │
                                           │                                      │
┌──────────────┐    direct_db:true         │                          │           │
│  pgclone     │◄──── direct SQL ──────────│                          ▼           │
│  buckets     │  (read-only PG clone)     │                   metadata update ──►├──► Moray RPC
│  clone       │                           │                   broker             │    (v1 objects)
│  {shard}.    │                           │                                      │
│  rebalancer- │                           │                                   ──►├──► MDAPI RPC
│  buckets-    │                           │                                      │    (v2 objects)
│  postgres    │                           │                                      │
│  :5432       │                           └──────────────────────────────────────┘
└──────────────┘

Key: pgclone is ONLY used for discovery (Phase 1).
     All metadata writes go to the REAL moray/mdapi services.
     Clones are frozen ZFS snapshots — they never receive writes.
```

The manager supports three metadata backend modes:

| Mode | Config | Object Types |
|------|--------|-------------|
| **Moray-only** | `domain_name` set, no `mdapi` | Traditional directory-based objects (v1) |
| **MDAPI-only** | `mdapi.shards` set, no `domain_name` | Bucket objects (v2) |
| **Hybrid** | Both set | Both v1 and v2 objects routed to correct backend |

---

## Component Inventory

| Component | Zone | Ports | Binary Path | Config Path |
|-----------|------|-------|-------------|-------------|
| Manager | `rebalancer` | 80 (API), 8878 (metrics) | `/opt/smartdc/rebalancer/bin/rebalancer-manager` | `/opt/smartdc/rebalancer/config.json` |
| CLI | `rebalancer` | — | `/opt/smartdc/rebalancer/bin/rebalancer-adm` | `/opt/smartdc/rebalancer/config.json` |
| Agent | each `mako` node | 7878 (API), 8878 (metrics) | `/opt/smartdc/rebalancer-agent/bin/rebalancer-agent` | `/opt/smartdc/rebalancer-agent/etc/config.toml` |
| PostgreSQL | `rebalancer` | 5432 (local) | `/opt/postgresql/12.4/bin/` | `/rebalancer/pg/data/postgresql.conf` |

### SMF Services

| Service FMRI | Zone | Dependencies |
|-------------|------|-------------|
| `svc:/manta/application/rebalancer:default` | manager | network, filesystem, postgresql |
| `svc:/manta/application/rebalancer-agent:default` | mako | network, filesystem |
| `svc:/manta/postgresql:default` | manager | network, filesystem |

### Run-as Users

| Component | User | Group | Privileges |
|-----------|------|-------|-----------|
| Manager | root | root | all |
| Agent | nobody | nobody | basic, net_privaddr |
| PostgreSQL | postgres (uid=907) | postgres (gid=907) | — |

---

## How Evacuation Works

### End-to-End Flow

```
┌─────────────────────────────────────────────────────────────────────┐
│                      EVACUATION PIPELINE                            │
│                                                                     │
│  Phase 1         Phase 2          Phase 3         Phase 4           │
│  ┌──────────┐   ┌─────────────┐   ┌───────────┐   ┌────────────┐    │
│  │ Object   │──►│ Assignment  │──►│  Agent    │──►│ Metadata   │    │
│  │ Discovery│   │ Generation  │   │  Transfer │   │ Update     │    │
│  └──────────┘   └─────────────┘   └───────────┘   └────────────┘    │
│       │               │                │                │           │
│  Direct SQL to   Group objects    POST to agent     Update REAL     │
│  pgclone         by destination   on dest shark.    Moray/MDAPI     │
│  (read-only      shark. Select    Agent downloads   via RPC with    │
│  PG clones).     dest sharks      from source,      new shark       │
│  NOT Moray/      from storinfo.   verifies MD5.     location.       │
│  MDAPI RPC.                                         Batch mode.     │
│                                                                     │
│  Threads:        Single thread    Per agent:        Configurable:   │
│  max_md_read     (assignment      workers ×         1-250 threads   │
│  _threads (10)    manager)        workers_per       (runtime        │
│                                   _assignment       adjustable)     │
└─────────────────────────────────────────────────────────────────────┘
```

### Step-by-Step Walkthrough

**1. Pre-condition: Mark the shark read-only.**
Before starting evacuation, the source shark must be marked read-only in
storinfo so Muskie stops placing new objects on it.

**2. Create the evacuation job.**
```bash
rebalancer-adm job create evacuate --shark 1.stor.us-east.joyent.us
```
The manager creates a job record in PostgreSQL (`rebalancer.jobs` table) and
a dedicated database named after the job UUID.

**3. Object Discovery (Phase 1) — via pgclone direct PostgreSQL.**
The manager uses **sharkspotter** to scan metadata databases for every object
stored on the source shark. **In production, discovery always uses direct
PostgreSQL connections to read-only pgclone clones** — not Moray/MDAPI RPC.
This avoids putting scan load on the live metadata tier. (This is the default)

For each object found, it inserts a row into the job database's
`evacuateobjects` table with status `unprocessed`.

- **Moray objects (v1):** `direct_db: true` — sharkspotter connects to
  `{shard}.rebalancer-postgres.{domain}:5432` (pgclone moray clone)
- **Bucket objects (v2):** `direct_db: true` — sharkspotter connects to
  `{shard}.rebalancer-buckets-postgres.{domain}:5432` (pgclone buckets clone)
- Chunk size controlled by `md_read_chunk_size` (default: 10,000)
- Parallelism controlled by `max_md_read_threads` (default: 10)
- pgclone clones must be created **before** starting the job (see
  [pgclone section](#pgclone-read-only-postgresql-clones-for-discovery))

**4. Assignment Generation (Phase 2).**
A single assignment-manager thread consumes unprocessed objects and groups
them into assignments destined for specific sharks:

- Destination sharks selected from storinfo (refreshed every 10 seconds)
- Selection filters: available capacity, datacenter blacklist, `max_fill_percentage`
- Top N sharks by available space (`max_sharks`, default: 5)
- Each assignment holds up to `max_tasks_per_assignment` objects (default: 50)
- Assignments are batched until they reach `max_assignment_age` seconds (default: 3600)

**5. Agent Transfer (Phase 3).**
The manager POSTs each assignment to the rebalancer-agent running on the
destination shark. The agent:

- Downloads each object from the source shark via HTTP GET
- Writes to a temp file under `/var/tmp/rebalancer/temp/`
- Verifies MD5 checksum against metadata
- Moves the file to its final location under `/manta/`
- Reports completion (or per-task failures) when the manager polls

**6. Metadata Update (Phase 4).**
Once an agent reports an assignment complete, the manager updates Manta
metadata to reflect the new shark location:

- **Moray (v1):** Atomic batch update with etag-based conditional writes
- **MDAPI (v2):** Individual put_object calls (non-transactional, partial success possible)
- **MPU parts:** If multipart upload parts were moved, the upload record's
  `preAllocatedSharks` array is updated to reference the new shark

**7. Completion.**
Objects are marked `complete` in the job database. The job transitions to
`complete` state when all objects are processed. Objects that failed can be
retried via `rebalancer-adm job retry <UUID>`.

### Object File Paths on Storage Nodes

| Object Type | Path on Shark |
|------------|---------------|
| v1 (directory) | `/manta/{owner_uuid}/{object_id}` |
| v2 (bucket) | `/manta/v2/{owner_uuid}/{bucket_id}/{prefix}/{object_id},{name_hash}` |

Where `prefix` = first 2 characters of `object_id`.

---

## pgclone: Read-Only PostgreSQL Clones for Discovery

### Why pgclone?

The rebalancer scans metadata databases to discover which objects live on
the storage node being evacuated. Scanning the **live** Manatee primary puts
heavy read load on the metadata tier, which can degrade the user experience.

`pgclone.sh` solves this by creating **ephemeral, read-only PostgreSQL VMs**
from ZFS snapshots of Manatee instances. The rebalancer's sharkspotter then
reads from the clone instead of the production primary. The clone never
receives writes — it is a frozen point-in-time snapshot.

### How It Works

```
┌──────────────────┐     ZFS snapshot     ┌──────────────────────────┐
│ Manatee Primary  │ ──────────────────►  │ pgclone VM (read-only)   │
│ (live production)│     + ZFS clone      │                          │
│                  │                      │ - Autovacuum disabled    │
│ 1.postgres.      │                      │ - recovery.conf removed  │
│ <domain>         │                      │ - Registers as:          │
└──────────────────┘                      │   {shard}.rebalancer-    │
                                          │   postgres.{domain}      │
                                          │   via registrar          │
                                          └─────────────┬────────────┘
                                                         │
                                                    :5432 (PG)
                                                         │
                                                         ▼
                                           ┌──────────────────────────┐
                                           │ Rebalancer Manager       │
                                           │ (sharkspotter thread)    │
                                           │ direct_db: true          │
                                           │ Reads objects via SQL    │
                                           └──────────────────────────┘
```

The same flow applies to buckets-postgres clones (for v2 bucket objects),
registering as `{shard}.rebalancer-buckets-postgres.{domain}`.

### pgclone.sh Location and Subcommands

The script is located at `libs/sharkspotter/tools/pgclone.sh`. Copy it to
the headnode global zone:

```bash
scp libs/sharkspotter/tools/pgclone.sh headnode:/var/tmp/
```

**Subcommands:**

| Command | Description |
|---------|------------|
| `pgclone.sh clone-moray <manatee_UUID>` | Clone a moray Manatee primary (v1 objects) |
| `pgclone.sh clone-buckets <buckets_pg_UUID>` | Clone a buckets-postgres primary (v2 objects) |
| `pgclone.sh clone-all --moray-vm <UUID> [--moray-vm ...] --buckets-vm <UUID> [--buckets-vm ...]` | Clone multiple shards at once; accepts repeated flags |
| `pgclone.sh discover` | Find all postgres VMs across all CNs and shards; outputs a suggested `clone-all` command |
| `pgclone.sh list [--type moray\|buckets\|all] [--json]` | List active clones |
| `pgclone.sh destroy <clone_UUID>` | Destroy a single clone |
| `pgclone.sh destroy-all [--type moray\|buckets]` | Destroy all clones |

For backwards compatibility: `pgclone.sh <manatee_UUID>` is equivalent to `clone-moray`.

### Multi-Shard Deployments

In a multi-shard deployment, one pgclone VM is created per shard. Each clone
registers in DNS with the shard number preserved from the source VM:

- Moray shard N (`N.postgres.<domain>`) -> clone at `N.rebalancer-postgres.<domain>`
- Buckets shard N (`N.buckets-postgres.<domain>`) -> clone at `N.rebalancer-buckets-postgres.<domain>`

**Shard discovery is automatic.** Moray shards come from `shards[]` in the
rebalancer config (populated by SAPI `INDEX_MORAY_SHARDS`). Buckets shards
come from `mdapi.shards[]` (populated by SAPI `BUCKETS_MORAY_SHARDS`). No
manual min/max shard configuration is needed.

**Multi-CN caveat:** In multi-CN deployments, postgres VMs for different
shards may live on different compute nodes. `vmadm list` on the headnode
won't show them all. Use `pgclone.sh discover` to find every postgres VM
across all CNs.

**Example: 2-shard deployment**

```bash
# 1. Discover all postgres VMs across all CNs
pgclone.sh discover

# 2. Clone all shards (one per shard)
pgclone.sh clone-all \
  --moray-vm <shard1-postgres-uuid> \
  --moray-vm <shard2-postgres-uuid> \
  --buckets-vm <shard1-buckets-postgres-uuid> \
  --buckets-vm <shard2-buckets-postgres-uuid>

# 3. Verify clones are running
pgclone.sh list

# 4. Start evacuation — rebalancer scans all shards automatically
rebalancer-adm job create evacuate --shark <shark>
```

### Pre-Evacuation Procedure (Before Creating Clones)

All four substeps are **required**. Skipping any one risks new objects being
written to the shark after the pgclone snapshot is taken.

**1. Disable minnow on the target storage node** (stops advertising to storinfo):
```bash
svcadm disable svc:/manta/application/minnow:default
```

**2. Flush storinfo cache** (if storinfo is deployed):
```bash
for ip in $(dig +short storinfo.<domain>); do
    curl $ip/flush -X POST
done
```

**3. Restart muskie on all webapi instances** (drops in-memory shark lists):
```bash
manta-oneach -s webapi \
    'for s in $(svcs -o FMRI muskie | grep muskie-); do
        svcadm restart $s
    done'
```

**4. Restart buckets-api on all buckets-api instances** (caches shark lists for bucket writes):
```bash
manta-oneach -s buckets-api 'svcadm restart svc:/manta/application/buckets-api'
```

| Step | Without it |
|------|-----------|
| Disable minnow | Storinfo keeps advertising the shark |
| Flush storinfo | Stale cache entries direct writes for minutes |
| Restart muskie | Muskie in-memory shark lists still include the shark |
| Restart buckets-api | buckets-api routes bucket object writes to the shark |

**5. Verify writes have drained** before taking the snapshot:
```bash
svcs minnow                                    # Confirm minnow is off
tail -f /var/log/mako-access.log | grep PUT    # Watch for in-flight PUTs
```

When no PUTs appear for several minutes after the muskie and buckets-api
restarts, it is safe to proceed.

### Creating Clones

**Find Manatee primaries:**

For multi-shard or multi-CN deployments, use `pgclone.sh discover` to find
all postgres VMs across all compute nodes (see
[Multi-Shard Deployments](#multi-shard-deployments)).

For single-shard deployments:
```bash
# Moray postgres
sdc-vmapi '/vms?tag.manta_role=postgres&state=running' | json -Ha uuid alias

# Buckets postgres
sdc-vmapi '/vms?tag.manta_role=buckets-postgres&state=running' | json -Ha uuid alias
```

**Create clones:**
```bash
# Multiple shards (recommended for full evacuation)
pgclone.sh clone-all \
    --moray-vm <SHARD1_MORAY_UUID> \
    --moray-vm <SHARD2_MORAY_UUID> \
    --buckets-vm <SHARD1_BUCKETS_UUID> \
    --buckets-vm <SHARD2_BUCKETS_UUID>

# Single shard
pgclone.sh clone-all \
    --moray-vm <MORAY_MANATEE_UUID> \
    --buckets-vm <BUCKETS_MANATEE_UUID>

# Or individually
pgclone.sh clone-moray <MORAY_MANATEE_UUID>
pgclone.sh clone-buckets <BUCKETS_MANATEE_UUID>
```

**Verify clones are running:**
```bash
pgclone.sh list
```

### Verify DNS Resolution

From the **rebalancer zone**, confirm the clones are reachable. For
multi-shard deployments, check each shard number:
```bash
# Single shard
dig +short 1.rebalancer-postgres.<domain>
dig +short 1.rebalancer-buckets-postgres.<domain>

# Multi-shard (verify each shard resolves)
dig +short 1.rebalancer-postgres.<domain>
dig +short 2.rebalancer-postgres.<domain>
dig +short 1.rebalancer-buckets-postgres.<domain>
dig +short 2.rebalancer-buckets-postgres.<domain>
```

If DNS does not resolve, check that registrar is running inside the clone zone:
```bash
zlogin <CLONE_UUID> 'svcs registrar'
zlogin <CLONE_UUID> 'json registration.domain registration.aliases \
    < /opt/smartdc/registrar/etc/config.json'
```

### Discovery Mode Configuration

Discovery has two mutually exclusive modes per object type:

**Moray objects (v1):**

| Mode | Config Flag | Connects To |
|------|-----------|------------|
| Direct (pgclone) | `direct_db: true` | `{shard}.rebalancer-postgres.{domain}:5432` |
| Moray RPC | `direct_db: false` (default) | `{shard}.moray.{domain}:2021` |

**Bucket objects (v2):**

| Mode | Config Flag | Connects To |
|------|-----------|------------|
| Direct (pgclone) | `direct_db: true` | `{shard}.rebalancer-buckets-postgres.{domain}:5432` |
| MDAPI RPC | `direct_db: false` + `mdapi.shards` set | mdapi endpoints from config |

When `direct_db` is enabled, the RPC discovery path is
**skipped**. The Moray/MDAPI RPC endpoints are still used for **metadata
updates** (Phase 4) — they are only bypassed for discovery.

**Enable via SAPI:**
```bash
MANTA_APP=$(sdc-sapi /applications?name=manta | json -Ha uuid)

# Full evacuation (both v1 and v2 via pgclone)
echo '{ "metadata": {
    "REBALANCER_DIRECT_DB": true
} }' | sapiadm update $MANTA_APP
```

Shard ranges are auto-discovered from SAPI arrays (`INDEX_MORAY_SHARDS` and
`BUCKETS_MORAY_SHARDS`) — no manual min/max shard config is needed.

After updating SAPI, config-agent renders the new config and runs
`svcadm refresh rebalancer`. The new settings take effect on the next job.

**Verify current configuration:**
```bash
json direct_db shards mdapi.shards \
    < /opt/smartdc/rebalancer/config.json
```

### Counting Objects Before Evacuation

Query the pgclone databases to know the scope of the evacuation:

**Moray objects (v1):**
```bash
zlogin <MORAY_CLONE_UUID> '/opt/postgresql/12.0/bin/psql -U postgres moray'
```
```sql
SELECT count(*) AS on_target_shark
FROM manta
WHERE type = 'object'
  AND _value::text LIKE '%<SHARK_HOSTNAME>%';
```

**Bucket objects (v2):**
```bash
zlogin <BUCKETS_CLONE_UUID> '/opt/postgresql/12.0/bin/psql -U postgres buckets_metadata'
```
```sql
SELECT sum(c) FROM (
    SELECT count(*) FILTER (WHERE sharks::text LIKE '%<SHARK>%') AS c
    FROM manta_bucket_0.manta_bucket_object
    UNION ALL
    SELECT count(*) FILTER (WHERE sharks::text LIKE '%<SHARK>%')
    FROM manta_bucket_1.manta_bucket_object
    -- ... repeat for all vnode schemas
) t;
```

To discover all vnode schemas:
```sql
SELECT schema_name FROM information_schema.schemata
WHERE schema_name LIKE 'manta_bucket_%'
ORDER BY schema_name;
```

### Clone Lifecycle and Cleanup

After the evacuation job completes:

```bash
# 1. Back up the job database
pg_dump -U postgres <job_uuid> > <job_uuid>.backup

# 2. Destroy pgclone clones
pgclone.sh destroy-all

# 3. Verify no clones remain
pgclone.sh list
# Should show: No clones found.
```

**Restoring a job database from backup:**

```bash
createdb -U postgres <job_uuid>
psql -U postgres <job_uuid> < <job_uuid>.backup
```

This is useful for post-mortem analysis or re-running a retry job
against a previous job's object list.

### Safety Properties

- The source Manatee VM is **never modified** (read-only ZFS snapshot)
- The clone has autovacuum disabled and `recovery.conf` removed
- Failed clone creation automatically cleans up all artifacts (VM, snapshot)
- Each clone gets a unique ZFS snapshot name (`rebalancer-<uuid_short>`)
- Clone VMs are tagged with `manta_role` for easy discovery and cleanup
- Clones are valid for the entire duration of an evacuation (days) — they
  are point-in-time snapshots that don't need refreshing mid-job

### pgclone Troubleshooting

| Issue | Diagnosis | Fix |
|-------|-----------|-----|
| DNS not resolving | Registrar not running in clone zone | `zlogin <CLONE> 'svcs registrar'` — check binder/ZooKeeper |
| Connection refused on :5432 | PostgreSQL failed to start | `zlogin <CLONE> 'svcs -x'` and check `/var/pg/postgresql.log` |
| Stale clones after job failure | Orphaned clones left running | `pgclone.sh list` then `pgclone.sh destroy <UUID>` |
| Clone alias wrong | Registrar misconfigured | `pgclone.sh list` to verify, check registrar config in clone |

---

## Deployment and Boot Sequence

### Manager Zone Boot (`boot/setup.sh`)

1. Common Manta zone pre-setup (`manta_common_presetup`)
2. PATH configured: `/opt/smartdc/rebalancer/bin:/opt/postgresql/12.4/bin`
3. PostgreSQL user created (uid=907, gid=907)
4. Delegated ZFS dataset mounted at `/rebalancer`
5. PostgreSQL initialized via `initdb` at `/rebalancer/pg/data`
6. PostgreSQL SMF service imported and enabled
7. `postgresql.conf` copied to data directory
8. Rebalancer manager SMF service imported
9. Log rotation configured
10. Metrics port published via `mdata-put metricPorts "8878"`

### Agent Deployment

Agents are deployed to every mako (storage) zone. The agent binary and
config are delivered via the `rebalancer-agent` SAPI service. No local
database is required — agents persist assignment state to SQLite files
under `/var/tmp/rebalancer/`.

### Agent Hotpatching

For rapid agent updates without reprovisioning all storage instances,
use the `manta-hotpatch-rebalancer-agent` tool from the headnode
global zone.  This is useful during active development when
reprovisioning hundreds of storage zones for an agent fix is
impractical.

**Prerequisites:**

```bash
sdcadm self-update --latest
sdcadm up manta
```

**Subcommands:**

| Command | Description |
|---------|------------|
| `list` | Show current agent version and hotpatch status on every storage node |
| `avail` | List available agent builds (from dev channel) newer than what's deployed |
| `deploy <IMAGE-UUID> -a` | Hotpatch all storage instances with the specified agent image |
| `undeploy -a` | Revert all hotpatches to the original agent from the storage image |

**Example workflow:**

```bash
# 1. Check current versions
manta-hotpatch-rebalancer-agent list

# 2. Find available builds
manta-hotpatch-rebalancer-agent avail

# 3. Deploy a new agent to all storage nodes
manta-hotpatch-rebalancer-agent deploy <IMAGE-UUID> -a

# 4. Verify
manta-hotpatch-rebalancer-agent list

# 5. Revert if needed
manta-hotpatch-rebalancer-agent undeploy -a
```

**Notes:**
- The tool only operates on storage instances in the **current
  datacenter**. For multi-DC regions, run it separately in each DC.
- Hotpatching replaces the agent binary and restarts the service.
  In-flight assignments on agents will complete before the restart.
- Use `-c` to control concurrency (how many agents are patched in
  parallel).

### PostgreSQL Configuration Highlights

| Parameter | Value | Notes |
|-----------|-------|-------|
| `max_connections` | 100 | Sufficient for typical job parallelism |
| `shared_buffers` | 128MB | |
| `shared_memory_type` | sysv | Required on SmartOS/illumos |
| `dynamic_shared_memory_type` | sysv | Required on SmartOS/illumos |
| `max_wal_size` | 1GB | |
| Data directory | `/rebalancer/pg/data` | Delegated ZFS dataset |
| Log file | `/var/pg/postgresql.log` | |

---

## Configuration Reference

### Manager Configuration (`/opt/smartdc/rebalancer/config.json`)

All values can be set via SAPI metadata. After updating SAPI metadata, run
`svcadm refresh rebalancer` or send SIGUSR1 to the manager process.

#### Core Settings

| Config Key | SAPI Variable | Default | Description |
|-----------|--------------|---------|-------------|
| `domain_name` | `DOMAIN_NAME` | *required* | Moray domain (e.g., `us-east.joyent.us`) |
| `shards[].host` | `INDEX_MORAY_SHARDS` | *required* | Moray shard hostnames |
| `listen_port` | — | 80 | Manager HTTP API port |
| `log_level` | `REBALANCER_LOG_LEVEL` | `debug` | critical/error/warning/info/debug/trace |
| `max_fill_percentage` | `MUSKIE_MAX_UTILIZATION_PCT` | 100 | Max % utilization on destination sharks |
| `snaplink_cleanup_required` | `SNAPLINK_CLEANUP_REQUIRED` | false | Block jobs until snaplinks cleaned |
| `direct_db` | `REBALANCER_DIRECT_DB` | true | Use pgclone direct PostgreSQL for moray and bucket discovery (requires pgclone clones) |

#### Job Execution Tunables (`options` block)

| Config Key | SAPI Variable | Default | Range | Description |
|-----------|--------------|---------|-------|-------------|
| `max_tasks_per_assignment` | `REBALANCER_MAX_TASKS_PER_ASSIGNMENT` | 50 | 1+ | Objects per assignment sent to agent |
| `max_metadata_update_threads` | `REBALANCER_MAX_METADATA_UPDATE_THREADS` | 10 | 1-250 | Parallel metadata update workers |
| `max_md_read_threads` | `REBALANCER_MAX_METADATA_READ_THREADS` | 10 | 1+ | Parallel metadata read threads |
| `max_sharks` | `REBALANCER_MAX_SHARKS` | 5 | 1+ | Max destination sharks to use |
| `use_static_md_update_threads` | `REBALANCER_USE_STATIC_MD_UPDATE_THREADS` | false | — | Lock thread count (disables runtime adjustment) |
| `static_queue_depth` | `REBALANCER_STATIC_QUEUE_DEPTH` | 10 | 1+ | Queue depth for static thread pool |
| `max_assignment_age` | `REBALANCER_MAX_ASSIGNMENT_AGE` | 3600 | seconds | Max wait before posting assignment to agent. The checker timeout is 2× this value (7200s). Assignments not completed by agents within the timeout are skipped. |
| `use_batched_updates` | `REBALANCER_USE_BATCHED_UPDATES` | false | — | Batch metadata updates |
| `md_read_chunk_size` | `REBALANCER_MD_READ_CHUNK_SIZE` | 10000 | 1+ | Objects per metadata query |

#### MDAPI Settings (Bucket Objects)

| Config Key | SAPI Variable | Default | Description |
|-----------|--------------|---------|-------------|
| `mdapi.shards[].host` | `BUCKETS_MORAY_SHARDS` | *optional* | Buckets-MDAPI shard hostnames |
| `mdapi.connection_timeout_ms` | `MDAPI_CONNECTION_TIMEOUT_MS` | 5000 | Connection timeout (ms) |
| `mdapi.max_batch_size` | — | 100 | Max objects per MDAPI batch |
| `mdapi.operation_timeout_ms` | — | 30000 | Per-operation timeout (ms) |
| `mdapi.max_retries` | — | 3 | Retry count for MDAPI calls |
| `mdapi.initial_backoff_ms` | — | 100 | Initial retry backoff (ms) |
| `mdapi.max_backoff_ms` | — | 5000 | Max retry backoff (ms) |

### Agent Configuration (`/opt/smartdc/rebalancer-agent/etc/config.toml`)

| Config Key | SAPI Variable | Default | Description |
|-----------|--------------|---------|-------------|
| `server.host` | — | `0.0.0.0` | Listen address |
| `server.port` | `REBALANCER_AGENT_PORT` | 7878 | HTTP API port |
| `server.workers` | `REBALANCER_AGENT_WORKERS` | 4 | Concurrent assignments processed |
| `server.workers_per_assignment` | `REBALANCER_AGENT_WORKERS_PER_ASSIGNMENT` | 4 | Parallel downloads per assignment |
| `metrics.host` | — | `0.0.0.0` | Metrics listen address |
| `metrics.port` | `REBALANCER_AGENT_METRICS_PORT` | 8878 | Prometheus metrics port |

---

## Operating Procedures

### Starting an Evacuation

**Pre-flight checklist:**

1. **Mark the shark read-only** and create pgclone clones (see
   [pgclone section](#pgclone-read-only-postgresql-clones-for-discovery)):
   - Disable minnow on target shark
   - Flush storinfo cache
   - Restart muskie and buckets-api
   - Wait for writes to drain
   - Run `pgclone.sh discover` to find all postgres VMs (especially
     important for multi-shard/multi-CN deployments)
   - Create pgclone clones (`pgclone.sh clone-all ...`) — one per shard
   - Verify DNS resolution from rebalancer zone
2. **Enable directdb discovery** via SAPI:
   ```bash
   MANTA_APP=$(sdc-sapi /applications?name=manta | json -Ha uuid)
   echo '{ "metadata": {
       "REBALANCER_DIRECT_DB": true
   } }' | sapiadm update $MANTA_APP
   ```
   Shards are auto-discovered from SAPI arrays — no min/max shard config needed.
3. Verify destination sharks have sufficient capacity
4. Verify the manager service is running:
   ```bash
   svcs svc:/manta/application/rebalancer
   ```
5. Verify PostgreSQL is running:
   ```bash
   svcs svc:/manta/postgresql
   ```
6. Verify agents are running on destination storage nodes:
   ```bash
   # From headnode or any CN
   sdc-oneachnode -n <storage_CN> 'svcs svc:/manta/application/rebalancer-agent'
   ```

**Create the job:**

```bash
# From the rebalancer zone
rebalancer-adm job create evacuate --shark 1.stor.us-east.joyent.us

# With an object limit (useful for testing)
rebalancer-adm job create evacuate --shark 1.stor.us-east.joyent.us --max_objects 1000
```

The command returns a job UUID.

**Or via the HTTP API directly:**

```bash
curl -X POST http://localhost/jobs \
  -H 'Content-Type: application/json' \
  -d '{
    "action": "evacuate",
    "params": {
      "from_shark": "1.stor.us-east.joyent.us",
      "max_objects": null
    }
  }'
```

### Monitoring Job Progress

```bash
# Get job status
rebalancer-adm job get <UUID>

# Or via API
curl -s http://localhost/jobs/<UUID> | json
```

**Example response:**

```json
{
  "config": {
    "action": "evacuate",
    "from_shark": {
      "manta_storage_id": "1.stor.us-east.joyent.us",
      "datacenter": "us-east-1"
    }
  },
  "results": {
    "unprocessed": 850000,
    "assigned": 5000,
    "complete": 140000,
    "error": 500,
    "skipped": 1200,
    "post_processing": 200,
    "duplicates": 100,
    "total": 997000,
    "error_breakdown": {
      "moray": { "update_failed": 300, "etag_mismatch": 100 },
      "mdapi": { "etag_mismatch": 50 },
      "other": { "internal_error": 50 }
    },
    "skip_breakdown": {
      "insufficient_space": 800,
      "no_destination_sharks": 400
    }
  },
  "state": "running"
}
```

**Key fields to watch:**

| Field | What It Means |
|-------|-------------|
| `unprocessed` | Objects still waiting to be assigned to agents |
| `assigned` | Objects currently in-flight to agents |
| `complete` | Successfully evacuated objects |
| `error` | Objects that failed (see `error_breakdown`) |
| `skipped` | Objects skipped (see `skip_breakdown`) |
| `post_processing` | Metadata updates in progress |

**Job states:**

| State | Meaning |
|-------|---------|
| `init` | Job created, initializing |
| `setup` | Configuring, connecting to metadata tier |
| `running` | Actively processing objects |
| `stopped` | Operator stopped the job |
| `complete` | All objects processed |
| `failed` | Job failed with error |

### Dynamically Adjusting Metadata Threads

During a running job, you can increase or decrease the number of metadata
update threads without restarting:

```bash
# Increase to 30 threads (requires use_static_md_update_threads = false)
curl -X PUT http://localhost/jobs/<UUID> \
  -H 'Content-Type: application/json' \
  -d '{"action": "set_metadata_threads", "params": 30}'
```

Valid range: 1–250.  There is a hard-coded maximum of 100 in the code
(`MAX_TUNABLE_MD_UPDATE_THREADS`) to minimize accidental impact to the
metadata tier.  Values above 100 are silently capped.  In practice,
even 100 is aggressive — start with 10-30 and increase only if the
metadata tier has headroom.

### Retrying Failed Objects

There are **no in-job retries**.  Once an object is marked `Skipped` or
`Error` during an evacuation, it stays that way for the rest of that job.
The rebalancer makes a single pass through the object list:

```
Scan → Assign → Copy → Update metadata → Done
                  ↓ fail
               Skip/Error (permanent for this job)
```

**`rebalancer-adm job retry`** retries only `error` and `unprocessed`
objects from the job's local database.  It does NOT retry `skipped`
objects (such as `agent_assignment_timeout`):

```bash
rebalancer-adm job retry <UUID>

# Or via API
curl -X POST http://localhost/jobs/<UUID>/retry
```

**To retry skipped objects, run a new evacuation job with fresh pgclones.**
The new job re-scans all objects on the source shark and picks up anything
that wasn't moved.  In practice, 2-3 runs fully evacuates a shark:

| Run | What happens |
|-----|-------------|
| **Run 1** | Evacuates the bulk (~55-95% depending on agent throughput) |
| **Run 2** | Fresh pgclone snapshot, catches timeouts and etag conflicts from run 1 |
| **Run 3** | Mops up the last few stragglers |

**Multi-run evacuation procedure:**

```bash
# --- Run 1 ---
# For multi-shard: use pgclone.sh discover to get UUIDs, then repeat flags
pgclone.sh clone-all --moray-vm <UUID> [--moray-vm ...] --buckets-vm <UUID> [--buckets-vm ...]
# Start evacuation, wait for completion
# Check results: note skipped/error counts

# --- Run 2 (if skipped > 0) ---
pgclone.sh destroy-all
pgclone.sh clone-all --moray-vm <UUID> [--moray-vm ...] --buckets-vm <UUID> [--buckets-vm ...]
# Start new evacuation against same shark
# Fresh etags from new snapshot avoid etag conflicts
# Smaller object set means agents finish within timeout

# --- Repeat until skipped ≈ 0 ---
```

**Why etag conflicts happen between runs:** The pgclone snapshot is
frozen at a point in time.  When run 1 updates metadata in live moray
(changing `shark1 → shark2` in the sharks array), the etag changes.
Run 2's snapshot still has the old etag, so those already-evacuated
objects get etag conflicts.  A fresh pgclone for run 2 has the
current etags and avoids this.

**Why `agent_assignment_timeout` happens:** The manager sends
assignments faster than agents can process them.  Assignments that
agents don't finish within `2 * max_assignment_age` (default 7200s)
are skipped.  Subsequent runs have fewer objects, so the queue is
smaller and timeouts decrease.

### Listing All Jobs

```bash
rebalancer-adm job list

# Or via API
curl -s http://localhost/jobs | json
```

### Reloading Configuration

```bash
# Via SMF (preferred)
svcadm refresh svc:/manta/application/rebalancer

# Or send SIGUSR1 directly
kill -USR1 $(pgrep rebalancer-manager)
```

### Post-Evacuation Cleanup

Once the job completes:

```bash
# 1. Back up the job database (from rebalancer zone)
pg_dump -U postgres <job_uuid> > <job_uuid>.backup

# 2. Destroy pgclone clones (from headnode)
pgclone.sh destroy-all

# 3. Verify no clones remain
pgclone.sh list

# 4. Clean up completed assignments on agents.
#    The manager does not auto-cleanup agent assignments after a job
#    ends.  Each completed assignment is a SQLite file in
#    /var/tmp/rebalancer/completed/ on every storage node.  Over time
#    these accumulate and waste disk space.
#
#    Check how many are queued:
manta-oneach -s storage 'ls /var/tmp/rebalancer/completed/ | wc -l'
#
#    Remove them ONLY when no rebalancer jobs are running:
manta-oneach -s storage 'rm /var/tmp/rebalancer/completed/*'
```

### Verifying Evacuation Results

After the job completes, verify the source shark is empty (or near-empty)
by creating fresh pgclones and querying the metadata:

```bash
# 1. Create fresh pgclones (with current metadata)
pgclone.sh destroy-all
pgclone.sh clone-all --moray-vm <UUID> --buckets-vm <UUID>

# 2. Count remaining moray objects on the evacuated shark
zlogin <MORAY_CLONE_UUID> \
  '/opt/postgresql/12.0/bin/psql -U postgres moray -c "
    SELECT count(*) AS remaining
    FROM manta WHERE type = '\''object'\''
    AND _value::text LIKE '\''%<SHARK_HOSTNAME>%'\'';"'

# 3. Count remaining buckets objects (check each vnode schema)
zlogin <BUCKETS_CLONE_UUID> \
  '/opt/postgresql/12.0/bin/psql -U buckets_mdapi buckets_metadata -c "
    SELECT schema_name FROM information_schema.schemata
    WHERE schema_name LIKE '\''manta_bucket_%'\''
    ORDER BY schema_name;"'
# Then for each vnode schema:
#   SELECT count(*) FROM manta_bucket_N.manta_bucket_object
#   WHERE sharks::text LIKE '%<SHARK_HOSTNAME>%';
```

If the count is zero, the shark is fully evacuated. If objects
remain, they fall into these categories:

| Remaining objects | Cause | Action |
|------------------|-------|--------|
| `source_is_evac_shark` objects | Both copies on the same shark — no replica to copy from | Manual intervention: create a new copy on another shark |
| `source_object_not_found` objects | File missing from disk but metadata still references the shark | Investigate: genuinely missing data or garbage collection race |
| Small number (<100) | Transient errors during the run | Re-run with fresh pgclones — should complete quickly |

### Cleaning Up Orphaned Files on the Evacuated Shark

Evacuation copies objects to destination sharks and updates metadata,
but **does not delete the original files** from the source shark.
After evacuation, the source shark has orphaned files that no metadata
points to. These waste disk space and should be cleaned up.

**Procedure:**

```bash
# 1. Get the list of object IDs still referenced in metadata
#    (from a fresh pgclone — these must NOT be deleted)
zlogin <MORAY_CLONE_UUID> \
  '/opt/postgresql/12.0/bin/psql -U postgres moray -t -A -c "
    SELECT _value::json->>'\''objectId'\''
    FROM manta WHERE type = '\''object'\''
    AND _value::text LIKE '\''%<SHARK_HOSTNAME>%'\'';"' \
  > /zones/<SHARK_UUID>/root/var/tmp/keep_objects.txt

# 2. Count files on disk vs referenced in metadata
zlogin <SHARK_UUID> 'find /manta -type f 2>/dev/null | wc -l'
wc -l /zones/<SHARK_UUID>/root/var/tmp/keep_objects.txt

# 3. Build the list of files to delete
zlogin <SHARK_UUID> '
  sort /var/tmp/keep_objects.txt > /var/tmp/keep_sorted.txt
  find /manta -type f -print0 | xargs -0 -n1 basename \
    | sort > /var/tmp/all_objects.txt
  comm -23 /var/tmp/all_objects.txt /var/tmp/keep_sorted.txt \
    > /var/tmp/delete_objects.txt
  echo "Files to delete: $(wc -l < /var/tmp/delete_objects.txt)"
  echo "Files to keep:   $(wc -l < /var/tmp/keep_sorted.txt)"'

# 4. Delete orphaned files (this can take a while for large sharks)
zlogin <SHARK_UUID> '
  find /manta -type f | while read f; do
    obj=$(basename "$f")
    if grep -qF "$obj" /var/tmp/delete_objects.txt; then
      rm -f "$f"
    fi
  done'

# 5. Verify
zlogin <SHARK_UUID> 'find /manta -type f 2>/dev/null | wc -l'
# Should be close to the keep_objects.txt count
```

**Important:**
- Always use **fresh pgclones** for the keep list — stale snapshots
  may not reflect the latest metadata updates.
- The keep list must include objects from **all shards** (moray and
  buckets). For buckets objects, query each vnode schema and extract
  the object IDs.
- On large sharks (millions of files), the delete loop can take hours.
  Monitor with `zpool list` to track freed space.
- Do NOT delete files while an evacuation job is running — in-flight
  metadata updates could reference objects that haven't been copied yet.

### Capacity Planning

Before starting an evacuation, ensure destination sharks have enough
free space to absorb the evacuated objects.

**Calculate required space:**

```bash
# Total size of objects on the source shark (from pgclone)
zlogin <MORAY_CLONE_UUID> \
  '/opt/postgresql/12.0/bin/psql -U postgres moray -c "
    SELECT pg_size_pretty(sum((_value::json->>'\''contentLength'\'')::bigint))
    AS total_size
    FROM manta WHERE type = '\''object'\''
    AND _value::text LIKE '\''%<SHARK_HOSTNAME>%'\'';"'
```

**Check destination capacity:**

```bash
# From headnode — check all storage nodes
curl -s http://storinfo.<domain>/storagenodes \
  | json -a manta_storage_id available_mb percent_used \
  | sort -k3 -n
```

**Rules of thumb:**

| Factor | Guideline |
|--------|-----------|
| Minimum free space per destination | 1.5× the largest single object |
| Recommended free space | At least 20% of the total data to evacuate, per destination shark |
| `max_fill_percentage` | Default 100 — lower to 90 if you want headroom for user writes during evacuation |
| Single-node (coal) | Evacuation **duplicates** data on the same zpool — free space shrinks by the total size of evacuated objects. Clean up orphans promptly. |

**Single-node warning:** In coal/dev environments where all sharks share
one ZFS pool, every evacuated object **doubles** its disk usage (original
stays on source, copy written to destination). Monitor pool usage with
`zpool list` and clean up orphaned files between runs to avoid filling
the pool.

### Stopping a Job

There is no explicit stop API. To halt processing:

1. Restart the manager service: `svcadm restart rebalancer`
2. In-flight agent assignments will complete naturally
3. The job can be retried later for remaining objects

---

## Performance Tuning

### Agent Concurrency

The total parallel downloads per agent is:

```
parallel_downloads = workers × workers_per_assignment
```

Across the cluster:

```
total_cluster_parallelism = N_destination_agents × workers × workers_per_assignment
```

**Recommended profiles:**

| Profile | workers | workers_per_assignment | Parallel Downloads | Use Case |
|---------|---------|----------------------|-------------------|----------|
| Light | 2 | 2 | 4 | Small evacuations, testing |
| Default | 4 | 4 | 16 | Standard evacuations |
| Aggressive | 8 | 8 | 64 | Large urgent evacuations |

**How to tune via SAPI:**

```bash
MANTA_APP=$(sdc-sapi /applications?name=manta | json -Ha uuid)

# Set agent parallelism
echo '{"metadata":{"REBALANCER_AGENT_WORKERS": 2}}' | sapiadm update $MANTA_APP
echo '{"metadata":{"REBALANCER_AGENT_WORKERS_PER_ASSIGNMENT": 4}}' | sapiadm update $MANTA_APP
```

After updating SAPI metadata, the agent config is regenerated on the next
config-agent cycle. Restart the agent service to pick up changes:

```bash
svcadm restart svc:/manta/application/rebalancer-agent
```

**Tuning strategy:**

1. Start conservative (1×1)
2. Monitor source shark I/O and network utilization
3. Monitor destination shark disk I/O
4. Increase `workers_per_assignment` first (more impact per change)
5. Increase `workers` second (adds assignment-level parallelism)
6. Watch for user-facing latency regressions on co-located workloads

### Flow Control: Backpressure and Timeouts

The rebalancer uses three mechanisms to prevent agents from being
overwhelmed:

1. **Agent backpressure (503):** Each agent accepts at most
   `workers × 50` assignments into its queue.  Beyond that, it
   returns HTTP 503 Service Unavailable.

2. **Manager retry on 503:** When an agent returns 503, the manager
   retries with linear backoff (5s, 10s, 15s, 20s, 25s, 30s, capped at 30s, max
   60 retries).  This makes the manager self-throttle to the agents'
   processing capacity — scanning pauses until agents have room.

3. **Checker timeout:** Assignments not completed by agents within
   `2 × max_assignment_age` (default 7200s) are skipped with
   `agent_assignment_timeout`.  This is a safety net for stuck
   assignments, not a throughput control.

**How they work together:**

```
Manager scans objects from pgclone
  → batches into assignments of 50
  → POSTs to agent
    → Agent has room → 200 OK → assignment queued
    → Agent queue full → 503 → manager sleeps, retries
      → Agent drains → retry succeeds → next assignment
  → Checker polls agents for completed assignments
    → Complete → metadata update → done
    → Not complete after 7200s → skip (agent_assignment_timeout)
```

**Tuning the balance:**
- To go **faster**: increase `workers` on agents (more concurrent
  assignments, agents fill up less often, fewer 503 retries)
- To be **safer**: decrease `workers` (slower but less load on storage
  nodes, more headroom for user traffic)
- If seeing many `agent_assignment_timeout`: increase
  `max_assignment_age` or increase `workers`

### Agent Download Retries

The agent retries transient connection errors (refused, timeout, DNS)
up to 3 times with exponential backoff (1s, 2s, 4s) before
permanently skipping the object with `source_other_error`.  HTTP
error responses (404, 500, etc.) fail immediately without retry.

### Manager Metadata Throughput

| Tunable | Effect | When to Adjust |
|---------|--------|---------------|
| `max_metadata_update_threads` | More parallel metadata writes | Metadata tier has headroom |
| `max_md_read_threads` | Faster object discovery | Discovery is the bottleneck |
| `md_read_chunk_size` | Larger query batches | Reduce query round-trips |
| `use_batched_updates` | Batch multiple updates per RPC | Reduce metadata tier load per object |
| `max_tasks_per_assignment` | Larger assignments to agents | Reduce assignment overhead |

### Key Bottleneck Indicators

| Symptom | Bottleneck | Fix |
|---------|-----------|-----|
| `unprocessed` not decreasing | Object discovery slow | Increase `max_md_read_threads`, `md_read_chunk_size` |
| `assigned` growing, `complete` flat | Agents slow | Increase agent `workers`/`workers_per_assignment` |
| `post_processing` growing | Metadata updates slow | Increase `max_metadata_update_threads`, enable `use_batched_updates` |
| High `error` count with `etag_mismatch` | Concurrent metadata writes | Normal with active workloads; retry resolves |
| `skipped` with `insufficient_space` | Destination sharks filling up | Add more sharks or increase `max_fill_percentage` |

---

## Monitoring and Alerting

### Prometheus Metrics

Both the manager and agents expose Prometheus metrics on port **8878**.

**Manager metrics** (`http://<manager_ip>:8878/metrics`):

| Metric | Type | Labels | Description |
|--------|------|--------|-------------|
| `object_count` | Counter | `action` | Objects processed by action (evacuate) |
| `skip_count` | Counter | `reason` | Objects skipped with reason |
| `error_count` | Counter | `type` | Errors by classification |
| `request_count` | Counter | `endpoint` | HTTP requests to manager API |
| `md_thread_gauge` | Gauge | — | Current active metadata update threads |

**Agent metrics** (`http://<agent_ip>:8878/metrics`):

| Metric | Type | Labels | Description |
|--------|------|--------|-------------|
| `request_count` | Counter | `req` (GET/POST/DELETE) | HTTP requests by type |
| `object_count` | Counter | `type` (complete/failed) | Objects by result |
| `error_count` | Counter | `error` | Errors by type |
| `bytes_count` | Counter | — | Total bytes transferred |
| `assignment_time` | Histogram | — | Assignment completion time (seconds) |

**Constant labels on all agent metrics:**
- `service`: rebalancer service name
- `server`: server UUID
- `datacenter`: datacenter name
- `zonename`: agent zone name

Metrics are automatically scraped by **cmon-agent** (configured via
`mdata-put metricPorts "8878"` during zone setup).

### Log Locations

| Component | Log |
|-----------|-----|
| Manager | `svcs -L svc:/manta/application/rebalancer:default` |
| Agent | `svcs -L svc:/manta/application/rebalancer-agent:default` |
| PostgreSQL | `/var/pg/postgresql.log` |

**Useful log grep patterns:**

```bash
# Manager — track assignment posting
grep "posting assignment" /var/svc/log/manta-application-rebalancer:default.log

# Manager — track metadata update errors
grep -i "etag_mismatch\|update_failed" /var/svc/log/manta-application-rebalancer:default.log

# Agent — track download activity
grep "process_task" /var/svc/log/manta-application-rebalancer-agent:default.log

# Agent — track checksum results
grep "Checksum\|MD5" /var/svc/log/manta-application-rebalancer-agent:default.log

# Agent — track assignment completion times
grep "Finished processing assignment" /var/svc/log/manta-application-rebalancer-agent:default.log
```

### Suggested Alerts

| Alert | Condition | Severity |
|-------|-----------|----------|
| Manager down | SMF service not online | Critical |
| Agent down | SMF service not online on storage node | Warning |
| PostgreSQL down | SMF service not online | Critical |
| High error rate | `error_count` increasing > 100/min | Warning |
| Job stalled | `unprocessed` + `assigned` unchanged for 30 min | Warning |
| Destination sharks full | All `skip_breakdown.insufficient_space` increasing | Critical |
| MD5 mismatches | `error_count{error="MD5Mismatch"}` > 0 | Warning |

---

## Troubleshooting

### Common Issues

#### Job stuck in `setup` state

**Cause:** Manager cannot connect to metadata tier (Moray or MDAPI shards).

**Diagnose:**
```bash
# Check manager logs for connection errors
grep -i "error\|connect\|timeout" $(svcs -L rebalancer)

# Verify Moray shard reachability
dig +short _moray._tcp.1.moray.us-east.joyent.us SRV
```

**Fix:** Verify Moray/MDAPI shards are healthy and DNS resolves correctly.

#### Jobs marked `failed` after rebalancer restart

**Cause:** The rebalancer cannot resume in-flight jobs after a restart.
On startup, any jobs left in `running`, `setup`, or `init` state are
automatically marked as `failed`.  This is expected behavior — the
in-memory assignment cache, worker threads, and channels are lost when
the process exits.

**What happens to in-flight work:**
- Objects already **copied** by agents but not yet metadata-updated are
  orphaned on the destination shark (harmless, just wasted space).
- Objects in **assigned** state on agents may still be processing, but
  the manager no longer tracks them.
- The source shark still has all its objects — nothing is removed during
  evacuation.

**Recovery:**
```bash
# 1. Check which jobs were marked failed
curl -s http://localhost/jobs | json -a id state action

# 2. Destroy and recreate pgclones (fresh etags)
pgclone.sh destroy-all
pgclone.sh clone-all --moray-vm <UUID> --buckets-vm <UUID>

# 3. Start a new evacuation job
rebalancer-adm job create evacuate --shark <shark>
```

The new job re-scans all objects on the source shark.  Objects that
were successfully evacuated before the restart already have updated
metadata and will be skipped (they no longer appear on the source
shark in the fresh pgclone snapshot).

#### High `moray.update_failed` / `etag_mismatch` errors

**Cause:** The etag in the pgclone snapshot doesn't match the etag in
live moray.  Two common scenarios:

1. **Re-run after a previous evacuation (expected):** The previous job
   updated metadata for objects it evacuated, changing their etags.
   The current pgclone snapshot still has the old etags.  These errors
   are **harmless** — the objects are already evacuated.
2. **Active writes on a production system:** Users or services modified
   the object's metadata between the snapshot and the update.

**Impact:** Not data-loss.  The object data was successfully copied to
the destination shark, but the metadata update was rejected.  The
source shark still has the object — nothing is lost.

**How to tell the difference:** If the error count roughly matches the
number of objects completed by a previous run against the same shark,
it's scenario 1.

**Fix:** Start a new job with **fresh pgclones**.  Fresh snapshots have
current etags, eliminating the conflicts.  `rebalancer-adm job retry`
does NOT help here — it retries from the same stale pgclone etags.

#### `source_object_not_found` skips

**Cause:** The object was not found (HTTP 404) on the source shark.
This is **normal on re-runs** — it means the object was already
evacuated by a previous job.  The metadata was updated to point to a
new shark, but the pgclone snapshot (taken before the previous run)
still lists the old shark.

**How to tell the difference:**
- **After a restart or re-run:** High `source_object_not_found` counts
  are expected and harmless.  The objects are already safe on their
  new sharks.
- **On a first run with no prior evacuations:** 404s indicate genuinely
  missing files — potential data corruption or garbage collection.
  Investigate with:
  ```bash
  # Check if the object exists on the source shark
  ls -la /manta/<owner>/<object_id>
  # Check live moray for current shark list
  echo '{"key": "<manta_key>"}' | moraygetobject manta
  ```

**Fix:** For re-run 404s, no action needed.  For genuinely missing
files, investigate the source shark's filesystem.

#### `insufficient_space` skips

**Cause:** All destination sharks are too full.

**Diagnose:**
```bash
# Check storinfo for shark capacity
curl -s http://storinfo.<domain>/storagenodes | json -a manta_storage_id percent_used available_mb
```

**Fix:** Increase `max_fill_percentage` if safe, or add storage capacity.

#### Agent returning `AgentBusy`

**Cause:** Agent's `workers` limit reached; all slots occupied.

**Fix:** Wait for current assignments to complete, or increase `REBALANCER_AGENT_WORKERS`.

#### Agent `MD5Mismatch` errors

**Cause:** Data corruption on source shark or network corruption during transfer.

**Diagnose:**
```bash
# On the source shark, verify the file
md5sum /manta/<owner>/<object_id>
# Compare with metadata checksum
```

**Fix:** If source file is corrupt, the object may need to be recovered from
another copy (if durability > 1) or flagged as data loss.

#### Cross-filesystem move errors (EXDEV)

**Cause:** Normal on SmartOS — `/var/tmp/` and `/manta/` are on different ZFS
datasets. The agent handles this automatically by falling back to copy+delete.

**Impact:** Slightly slower than atomic rename but functionally correct. Not
an actionable error.

#### Manager `SIGUSR1` config reload not taking effect

**Diagnose:**
```bash
# Verify config.json is valid JSON
json < /opt/smartdc/rebalancer/config.json

# Check manager received the signal
grep -i "reload\|config\|SIGUSR1" $(svcs -L rebalancer) | tail -5
```

**Fix:** Ensure the config file is valid JSON and the manager process is running.

#### pgclone clone not reachable from rebalancer zone

**Cause:** DNS not resolving `{shard}.rebalancer-postgres.{domain}` or connection
refused on port 5432.

**Diagnose:**
```bash
# From rebalancer zone
dig +short 1.rebalancer-postgres.<domain>

# If no result, check registrar inside the clone
zlogin <CLONE_UUID> 'svcs registrar'
zlogin <CLONE_UUID> 'json registration.domain < /opt/smartdc/registrar/etc/config.json'
```

**Fix:** Verify binder/ZooKeeper is healthy. If registrar is offline, restart it
inside the clone zone. If the clone failed to start PostgreSQL, check
`/var/pg/postgresql.log` inside the clone.

#### Stale pgclone after re-running evacuation

**Cause:** Running a new evacuation without fresh pgclone clones results in
high `etag_mismatch` errors because the clone still has old metadata.

**Diagnose:**
```bash
# Check error breakdown
curl -s http://localhost/jobs/<UUID> | json results.error_breakdown
# High moray.update_failed count = stale clone
```

**Fix:** Destroy old clones and create fresh ones:
```bash
pgclone.sh destroy-all
pgclone.sh clone-all --moray-vm <UUID> --buckets-vm <UUID>
```

---

## Database Operations

### Connecting to the Rebalancer Database

```bash
# From the rebalancer zone
psql -U postgres rebalancer
```

### Useful Queries

**List all jobs:**
```sql
SELECT id, action, state FROM jobs;
```

**Count objects by status for a job:**
```sql
-- Connect to the job's database (UUID is the database name)
\c <job-uuid>

SELECT status, COUNT(*) FROM evacuateobjects GROUP BY status;
```

**View error breakdown:**
```sql
\c <job-uuid>

SELECT error, COUNT(*)
FROM evacuateobjects
WHERE status = 'error'
GROUP BY error
ORDER BY count DESC;
```

**View skip reasons:**
```sql
\c <job-uuid>

SELECT skipped_reason, COUNT(*)
FROM evacuateobjects
WHERE status = 'skipped'
GROUP BY skipped_reason
ORDER BY count DESC;
```

**Check assignment progress:**
```sql
\c <job-uuid>

SELECT assignment_id, status, COUNT(*)
FROM evacuateobjects
WHERE assignment_id IS NOT NULL
GROUP BY assignment_id, status
ORDER BY assignment_id;
```

**Find objects still in flight:**
```sql
\c <job-uuid>

SELECT COUNT(*) FROM evacuateobjects WHERE status = 'assigned';
```

**Check for duplicate objects:**
```sql
\c <job-uuid>

SELECT COUNT(*) FROM duplicates;
```

### Database Maintenance

The rebalancer creates one PostgreSQL database per job. After a job is fully
complete and verified, old job databases can be cleaned up:

```bash
# List all databases
psql -U postgres -l

# Drop a completed job database (CAUTION: irreversible)
psql -U postgres -c "DROP DATABASE \"<job-uuid>\";"
```

---

## Appendix: Error and Skip Reason Reference

### Object Error Types

| Error | Source | Description | Retryable |
|-------|--------|------------|-----------|
| `MetadataUpdateFailed` | Manager | Generic metadata update failure | Yes |
| `MorayUpdateFailed` | Manager | Moray RPC call failed | Yes |
| `MorayEtagMismatch` | Manager | Object modified since read | Yes |
| `MdapiUpdateFailed` | Manager | MDAPI call failed | Yes |
| `MdapiEtagMismatch` | Manager | Bucket object modified since read | Yes |
| `MdapiObjectNotFound` | Manager | Object deleted before update | No |
| `BadMorayClient` | Manager | Cannot create Moray client | Yes |
| `BadMorayObject` | Manager | Malformed object in Moray | No |
| `BadMantaObject` | Manager | Invalid Manta object metadata | No |
| `BadShardNumber` | Manager | Invalid shard in metadata | No |
| `BadContentLength` | Manager | Missing or invalid content-length | No |
| `MissingSharks` | Manager | No sharks in object metadata | No |
| `DuplicateShark` | Manager | Object already on destination shark | No |

### Object Skip Reasons

These correspond to the `ObjectSkippedReason` enum in the code.

| Reason | Source | Description | Retryable |
|--------|--------|------------|-----------|
| `agent_fs_error` | Agent | Filesystem error on destination (disk full, permissions) | Yes |
| `agent_assignment_no_ent` | Agent | Assignment not found on agent | No |
| `agent_busy` | Manager | Agent returned 503 (queue full) after exhausting retries | Yes |
| `agent_assignment_timeout` | Manager | Agent didn't complete assignment within 2 × `max_assignment_age` | Yes |
| `assignment_error` | Manager | Internal error creating/sending assignment | Yes |
| `assignment_mismatch` | Manager | Assignment data inconsistent between agent and manager | No |
| `assignment_rejected` | Agent | Agent rejected assignment (non-503 error) | Yes |
| `destination_insufficient_space` | Manager | No destination shark has enough space | Yes (if space freed) |
| `destination_unreachable` | Manager | Could not connect to destination agent | Yes |
| `md5_mismatch` | Agent | Downloaded data doesn't match expected checksum | No (data issue) |
| `network_error` | Manager | General network failure posting assignment | Yes |
| `object_already_on_dest_shark` | Manager | Object already has a copy on the destination shark | No |
| `object_already_in_datacenter` | Manager | Object copy already in the destination datacenter | No |
| `source_other_error` | Agent | Connection error to source shark (after 3 retries) | Yes |
| `source_object_not_found` | Agent | 404 from source shark — file missing or already evacuated | No (if missing) / Yes (re-run) |
| `source_is_evac_shark` | Manager | Both copies are on the shark being evacuated | No (manual intervention) |
| `http_status_code(N)` | Agent | Source shark returned non-404 HTTP error N | Depends on error |

### Job State Transitions

```
                 ┌──────┐
                 │ init │
                 └──┬───┘
                    │
                    ▼
                 ┌──────┐
                 │setup │
                 └──┬───┘
                    │
              ┌─────┴─────┐
              ▼            ▼
         ┌────────┐   ┌────────┐
         │running │   │ failed │
         └──┬──┬──┘   └────────┘
            │  │
      ┌─────┘  └──────┐
      ▼                ▼
 ┌──────────┐    ┌─────────┐
 │ complete │    │ stopped │
 └──────────┘    └─────────┘
       │
       │  POST /jobs/{uuid}/retry
       ▼
  ┌──────────┐
  │ new job  │  (reads from original job's DB)
  │ (init)   │
  └──────────┘
```

### Assignment State Machine (Internal)

```
  Init ──► Assigned ──► AgentComplete ──► PostProcessed
```

- **Init:** Assignment created, grouping objects
- **Assigned:** Posted to agent, awaiting completion
- **AgentComplete:** Agent reported done, pending metadata update
- **PostProcessed:** Metadata updated, objects marked complete/error
