# Manta Rebalancer DevOps Operations Guide

This guide covers the architecture, operation, monitoring, and troubleshooting
of the Manta Rebalancer system for production environments.

---

## Table of Contents

1. [Performance](#performance)
2. [Architecture Overview](#architecture-overview)
3. [Component Inventory](#component-inventory)
4. [How Evacuation Works](#how-evacuation-works)
5. [pgclone: Read-Only PostgreSQL Clones for Discovery](#pgclone-read-only-postgresql-clones-for-discovery)
6. [Deployment and Boot Sequence](#deployment-and-boot-sequence)
7. [Configuration Reference](#configuration-reference)
8. [Operating Procedures](#operating-procedures)
9. [Performance Tuning](#performance-tuning)
10. [Monitoring and Alerting](#monitoring-and-alerting)
11. [Troubleshooting](#troubleshooting)
12. [Database Operations](#database-operations)
13. [Appendix: Error and Skip Reason Reference](#appendix-error-and-skip-reason-reference)

---

## Performance

The performance of the rebalancer is primarily dependent on the allowable impact
on the metadata tier.  With the tunables discussed below it is possible to
increase the performance of both the metadata tier and the object download
concurrency to a level that would result in degradation of the user experience.

During testing we did notice some delays in overall job time if storage nodes
were not available.  This applies to both the destination storage nodes where
the rebalancer agent runs as well as the source storage nodes where objects are
copied from.  If we start to see a significant increase in `skip` level errors
it is worth investigating the manager and agent logs.  Some relevant Jira issues
are:

* [MANTA-5326](https://mnx.atlassian.net/browse/MANTA-5326)
* [MANTA-5330](https://mnx.atlassian.net/browse/MANTA-5330)
* [MANTA-5231](https://mnx.atlassian.net/browse/MANTA-5231)
* [MANTA-5119](https://mnx.atlassian.net/browse/MANTA-5119)
* [MANTA-5159](https://mnx.atlassian.net/browse/MANTA-5159)
* See also `rebalancer-performance` Jira label

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
storinfo so Muskie stops placing new objects on it.  Disable minnow on the
target shark, flush the storinfo cache, restart muskie and buckets-api, then
wait for in-flight writes to drain.

**2. Create pgclone snapshots (after writes have drained).**
pgclone clones must be created **after** the shark is fully read-only and all
in-flight writes have drained.  If clones are created before disabling writes,
objects modified between the snapshot and the metadata update will cause
`EtagConflictError` failures.
```bash
pgclone.sh clone-all \
  --moray-vm <UUID> [--moray-vm <UUID> ...] \
  --buckets-vm <UUID> [--buckets-vm <UUID> ...]
```

**3. Create the evacuation job.**
```bash
rebalancer-adm job create evacuate --shark 1.stor.us-east.joyent.us
```
The manager creates a job record in PostgreSQL (`rebalancer.jobs` table) and
a dedicated database named after the job UUID.

**4. Object Discovery (Phase 1) — via pgclone direct PostgreSQL.**
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
- pgclone clones must be created **after** the shark is marked read-only
  and writes have drained, but **before** starting the job (see
  [pgclone section](#pgclone-read-only-postgresql-clones-for-discovery))

**5. Assignment Generation (Phase 2).**
A single assignment-manager thread consumes unprocessed objects and groups
them into assignments destined for specific sharks:

- Destination sharks selected from storinfo (refreshed every 10 seconds)
- Selection filters: available capacity, datacenter blocklist, `max_fill_percentage`
- Top N sharks by available space (`max_sharks`, default: 5)
- Each assignment holds up to `max_tasks_per_assignment` objects (default: 50)
- Assignments are batched until they reach `max_assignment_age` seconds (default: 3600)

**6. Agent Transfer (Phase 3).**
The manager POSTs each assignment to the rebalancer-agent running on the
destination shark. The agent:

- Downloads each object from the source shark via HTTP GET
- Writes to a temp file under `/var/tmp/rebalancer/temp/`
- Verifies MD5 checksum against metadata
- Moves the file to its final location under `/manta/`
- Reports completion (or per-task failures) when the manager polls

**7. Metadata Update (Phase 4).**
Once an agent reports an assignment complete, the manager updates Manta
metadata to reflect the new shark location:

- **Moray (v1):** Atomic batch update with etag-based conditional writes
- **MDAPI (v2):** Individual put_object calls (non-transactional, partial success possible)
- **MPU parts:** If multipart upload parts were moved, the upload record's
  `preAllocatedSharks` array is updated to reference the new shark

**8. Completion.**
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

**5. (Optional) Disable hourly log upload crons** to prevent etag drift
on `/stor/logs/` objects during evacuation:
```bash
# From the headnode — disables logrotateandupload.sh across all service zones
sdc-oneachnode -a '
  for vm in $(vmadm list -Ho uuid -o uuid); do
    zlogin $vm "crontab -l 2>/dev/null" | grep -q logrotateandupload || continue
    zlogin $vm "crontab -l | grep -v logrotateandupload | crontab -" 2>/dev/null
    echo "Disabled log upload in $(vmadm get $vm | json alias)"
  done'
```

> **RISKS — read carefully before disabling:**
>
> - **Log accumulation:** Unrotated logs will grow unbounded in every
>   service zone.  If the evacuation takes many hours, zones may exhaust
>   tmpfs or local disk, causing service failures.  Monitor zone disk
>   usage (`df -h` in zones) during the evacuation.
> - **Lost observability:** Log uploads to Manta are the primary way
>   operators access historical service logs.  While the cron is
>   disabled, no new logs are uploaded — any incident during this
>   window will have incomplete log coverage in `/stor/logs/`.
> - **Forgotten re-enable:** If the cron is not re-enabled after the
>   evacuation, logs will silently stop uploading permanently.  Always
>   re-enable immediately after the job completes (see step below).
> - **Production impact:** On busy production systems with high log
>   volume, disabling rotation can fill zone tmpfs within hours.
>   **Only recommended for small/test evacuations or COAL.**
>
> For production systems with large evacuations, it is safer to accept
> the hourly etag errors and retry with fresh pgclones instead.

To re-enable after evacuation:
```bash
# Re-enable by re-adding the cron entry in each zone
sdc-oneachnode -a '
  for vm in $(vmadm list -Ho uuid -o uuid); do
    zlogin $vm "test -f /opt/smartdc/common/sbin/logrotateandupload.sh" \
      2>/dev/null || continue
    zlogin $vm "crontab -l 2>/dev/null" | grep -q logrotateandupload && continue
    zlogin $vm "(crontab -l 2>/dev/null; echo \"0 * * * * /opt/smartdc/common/sbin/logrotateandupload.sh >> /var/log/logrotateandupload.log 2>&1\") | crontab -" 2>/dev/null
    echo "Re-enabled log upload in $(vmadm get $vm | json alias)"
  done'
```

**6. Verify writes have drained** before taking the snapshot:
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

### Build and Deployment

The rebalancer manager is part of the default manta v2 deployment, and is built
using Jenkins.

The rebalancer manager can be deployed/upgraded in the same way as other manta
components using `manta-adm update -f <update_file>` where the `<update_file>`
specifies the image uuid of the rebalancer image to update to.

The rebalancer manager places its local postgres database in a delegated dataset
so that it will be maintained across reprovisions.  The memory requirements are
defined in the [sdc-manta repository](https://github.com/joyent/sdc-manta).

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

As part of [MANTA-5293](https://mnx.atlassian.net/browse/MANTA-5293) a new
`manta-hotpatch-rebalancer-agent` tool was added to the headnode global zone.
This is useful during active development when reprovisioning hundreds of
storage zones for an agent fix is impractical.

To install it requires a recent `sdcadm` and an update of your manta-deployment
zone (a.k.a. the "manta0" zone):

    sdcadm self-update --latest
    sdcadm up manta

Using the hotpatch tool should hopefully be obvious from its help output:

    [root@headnode (nightly-2) ~]# manta-hotpatch-rebalancer-agent help
    Hotpatch rebalancer-agent in deployed "storage" instances.

    Usage:
        manta-hotpatch-rebalancer-agent [OPTIONS] COMMAND [ARGS...]
        manta-hotpatch-rebalancer-agent help COMMAND

    Options:
        -h, --help      Print this help and exit.
        --version       Print version and exit.
        -v, --verbose   Verbose trace logging.

    Commands:
        help (?)        Help on a specific sub-command.

        list            List running rebalancer-agent versions.
        avail           List newer available images for hotpatching rebalancer-agent.
        deploy          Deploy the given rebalancer-agent image hotpatch to storage instances.
        undeploy        Undo the rebalancer-agent hotpatch on storage instances.

    Use this tool to hotpatch the "rebalancer-agent" service that runs in each Manta
    "storage" service instance. While hotpatching is discouraged, this tool exists
    during active Rebalancer development because reprovisioning all "storage"
    instances in a large datacenter solely for a rebalancer-agent fix can be
    painful.

    Typical usage is:

    1. List the current version of all rebalancer-agents:
            manta-hotpatch-rebalancer-agent list

    2. List available rebalancer-agent builds (in the "dev" channel of
       updates.joyent.com) to import and use for hotpatching. This only lists
       builds newer than the current oldest rebalancer-agent.
            manta-hotpatch-rebalancer-agent avail
       Alternatively a rebalancer-agent build can be manually imported
       into the local IMGAPI.

    3. Hotpatch a rebalancer-agent image in all storage instances in this DC:
            manta-hotpatch-rebalancer-agent deploy -a IMAGE-UUID

    4. If needed, revert any hotpatches and restore the storage image's original
       rebalancer-agent.
            manta-hotpatch-rebalancer-agent undeploy -a

Note that this tool only operates on instances in the current datacenter. As
with most Manta tooling, to perform upgrades across an entire region requires
running the tooling in each DC separately.

#### Example Usage

The rest of this section is an example of running this tool in nightly-2
(a small test Triton datacenter). Each subcommand has other options that are not
all shown here, e.g. controlling concurrency, selecting particular storage
instances to hotpatch, etc.

The "list" command will show the current rebalancer-agent version and whether
it is hotpatched in every storage node:

    [root@headnode (nightly-2) ~]# manta-hotpatch-rebalancer-agent list
    STORAGE NODE                          VERSION                                   HOTPATCHED
    64052e9d-c379-44ae-9036-2293b88baa7c  0.1.0 (master-20200616T185217Z-g82b8008)  false
    a83343ec-1d91-467b-b938-a0af7f86e92c  0.1.0 (master-20200616T185217Z-g82b8008)  false
    a8aaa7c4-2699-40ed-83e5-aabec7d55b3d  0.1.0 (master-20200616T185217Z-g82b8008)  false

The "avail" command lists any available rebalancer-agent builds at or newer
than what is currently deployed:

    [root@headnode (nightly-2) ~]# manta-hotpatch-rebalancer-agent avail
    UUID                                  NAME                      VERSION                           PUBLISHED_AT
    7a5529e2-3d8b-4c9c-84af-46a1f6e0bb95  mantav2-rebalancer-agent  master-20200617T234037Z-g6dc482c  2020-06-18T00:09:27.901Z

The "deploy" command does the hotpatching:

    [root@headnode (nightly-2) ~]# manta-hotpatch-rebalancer-agent deploy 7a5529e2-3d8b-4c9c-84af-46a1f6e0bb95 -a
    This will do the following:
    - Import rebalancer-agent image 7a5529e2-3d8b-4c9c-84af-46a1f6e0bb95
      (master-20200617T234037Z-g6dc482c) from updates.joyent.com.
    - Hotpatch rebalancer-agent image 7a5529e2-3d8b-4c9c-84af-46a1f6e0bb95
      (master-20200617T234037Z-g6dc482c) on all 3 storage instances in this DC

    Would you like to hotpatch? [y/N] y
    Trace logging to "/var/tmp/manta-hotpatch-rebalancer-agent.20200619T180117Z.deploy.log"
    Importing image 7a5529e2-3d8b-4c9c-84af-46a1f6e0bb95 from updates.joyent.com
    Imported image
    Hotpatched storage instance a8aaa7c4-2699-40ed-83e5-aabec7d55b3d
    Hotpatched storage instance a83343ec-1d91-467b-b938-a0af7f86e92c
    Hotpatching 3 storage insts       [================================================================>] 100%        3
    Hotpatched storage instance 64052e9d-c379-44ae-9036-2293b88baa7c
    Successfully hotpatched.

    [root@headnode (nightly-2) ~]# manta-hotpatch-rebalancer-agent list
    STORAGE NODE                          VERSION                                   HOTPATCHED
    64052e9d-c379-44ae-9036-2293b88baa7c  0.1.0 (master-20200617T234037Z-g6dc482c)  true
    a83343ec-1d91-467b-b938-a0af7f86e92c  0.1.0 (master-20200617T234037Z-g6dc482c)  true
    a8aaa7c4-2699-40ed-83e5-aabec7d55b3d  0.1.0 (master-20200617T234037Z-g6dc482c)  true

The "undeploy" command can be used to revert back to the original
rebalancer-agent in a storage instance (i.e. to undo any hotpatching):

    [root@headnode (nightly-2) ~]# manta-hotpatch-rebalancer-agent undeploy -a
    This will revert any rebalancer-agent hotpatches on all 3 storage instances in this DC

    Would you like to continue? [y/N] y
    Trace logging to "/var/tmp/manta-hotpatch-rebalancer-agent.20200619T180148Z.undeploy.log"
    Unhotpatched storage instance 64052e9d-c379-44ae-9036-2293b88baa7c
    Unhotpatched storage instance a83343ec-1d91-467b-b938-a0af7f86e92c
    Unhotpatching 3 storage insts     [================================================================>] 100%        3
    Unhotpatched storage instance a8aaa7c4-2699-40ed-83e5-aabec7d55b3d
    Successfully reverted hotpatches.

    [root@headnode (nightly-2) ~]# manta-hotpatch-rebalancer-agent list
    STORAGE NODE                          VERSION                                   HOTPATCHED
    64052e9d-c379-44ae-9036-2293b88baa7c  0.1.0 (master-20200616T185217Z-g82b8008)  false
    a83343ec-1d91-467b-b938-a0af7f86e92c  0.1.0 (master-20200616T185217Z-g82b8008)  false
    a8aaa7c4-2699-40ed-83e5-aabec7d55b3d  0.1.0 (master-20200616T185217Z-g82b8008)  false

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

All values can be set via SAPI metadata.  For example:

```bash
MANTA_APP=$(sdc-sapi /applications?name=manta | json -Ha uuid)
echo '{ "metadata": {"REBALANCER_LOG_LEVEL": "trace" } }' | sapiadm update $MANTA_APP
```

After updating SAPI metadata, config-agent renders the new values into
`/opt/smartdc/rebalancer/config.json` and runs `svcadm refresh rebalancer`.
Most tunables take effect on the next job without a restart.

> **Important:** A `log_level` change is the only tunable that requires a full
> service restart (`svcadm restart rebalancer`).  All other tunables are picked
> up by config-agent's `svcadm refresh` and applied to the next job that is run.

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
| `exclude_key_prefixes` | `REBALANCER_EXCLUDE_KEY_PREFIXES` | see below | — | Object key prefixes to skip during discovery |

**`exclude_key_prefixes`** controls which moray objects are skipped
during pgclone scanning.  System-managed objects (logs, backups, metering
assets) are overwritten by hourly crons, causing unavoidable etag errors.
Skipping them produces clean evacuations for user data.

| SAPI Value | Effect |
|-----------|--------|
| Not set (default) | Skips `/stor/logs/`, `/stor/usage/`, `/stor/manatee_backups/` |
| `["none"]` | Disables filtering — all objects are discovered (required for decommissioning) |
| `["/stor/logs/", "/custom/"]` | Custom prefix list |
| `null` | Removes override, restores defaults |

**Note:** Setting to `[]` (empty array) does NOT disable filtering —
mustache treats empty arrays as falsy and renders the defaults.  Use
`["none"]` instead.

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

1. **Mark the shark read-only:**
   - Disable minnow on target shark
   - Flush storinfo cache
   - Restart muskie and buckets-api
   - Wait for writes to drain
2. **Create pgclone clones** (must be done **after** writes have fully
   drained — see [pgclone section](#pgclone-read-only-postgresql-clones-for-discovery)):
   - Run `pgclone.sh discover` to find all postgres VMs (especially
     important for multi-shard/multi-CN deployments)
   - Create pgclone clones (`pgclone.sh clone-all ...`) — one per shard
   - Verify DNS resolution from rebalancer zone
3. **Enable directdb discovery** via SAPI:
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

### Expected Production Workflow: Convergence and Residual Errors

In production, evacuations will **always** have etag errors on
`/stor/logs/` objects.  This is inherent to the system — not a bug.

**Why convergence to zero errors is not achievable in practice:**

Multiple system crons continuously write to `/stor/logs/` in Manta,
changing moray etags:

| Cron | Schedule | Zones |
|------|----------|-------|
| `logrotateandupload.sh` | `0 * * * *` (hourly) | webapi, buckets-api, electric-moray, mdapi, garbage-collector, rebalancer, mdplacement |
| `backup.sh` | `1,2,3,4,5 * * * *` (every minute, first 5 min) | storage, moray, postgres, nameservice, storinfo, loadbalancer, authcache |
| `backup_pg_dumps.sh` | `30 * * * *` (half-hourly) | webapi, buckets-api |
| `pg_dump.sh` | `0 0 * * *` (daily) | all postgres/manatee zones |

Disabling minnow only stops **user object placement** via storinfo.
These system crons bypass storinfo entirely — they upload logs through
muskie → moray directly.  Even with minnow disabled, `/stor/logs/`
etags will keep changing.

**Recommended production workflow:**

1. **First run** — evacuate the full shark.  Expect ~40-50% etag errors
   concentrated on `/stor/logs/` objects.  All user data objects should
   complete successfully.

2. **One retry with fresh pgclones** — catches objects that failed due to
   pgclone-vs-live etag drift from the first run's duration.  Error
   count will drop but not reach zero.

3. **Accept residual `/stor/logs/` errors** — these errors are
   unavoidable and harmless:
   - The stale metadata pointing to the evacuated shark is harmless —
     the object data still exists there until the shark is physically
     decommissioned.
   - The rebalancer successfully copied the data to the destination
     shark — only the metadata update (changing the sharks list in
     moray) was rejected due to the etag mismatch.

   **Why `/stor/logs/` etag errors happen even after disabling minnow:**

   When `logrotateandupload.sh` runs, it overwrites existing log files
   via `curl -X PUT` through the Manta front door (muskie).  For PUT
   overwrites, muskie asks storinfo for new sharks — and since minnow
   is disabled, the evacuated shark is not returned.  The **new version**
   of the log file is placed on **different sharks**.  However, this
   overwrite **changes the object's etag in moray** (new content, new
   sharks list, new etag).

   The pgclone snapshot has the old etag from before the overwrite.
   When the rebalancer tries to update metadata using the old etag,
   moray rejects it with `EtagConflictError`.  The object data was
   already copied to the destination shark, but the metadata update
   fails because moray's etag no longer matches.

   This is actually harmless for two reasons:
   - The new version of the log file already lives on non-evacuated
     sharks (muskie placed it there during the overwrite)
   - The old version's data on the evacuated shark is stale — moray
     no longer references it

   **No amount of retries will converge to zero** while these crons
   run, because each cron cycle overwrites the log object, changing
   the etag again before the retry finishes.

   (Code reference: `manta-muskie/lib/obj.js` `findSharks()` calls
   `picker.choose()` for all PUTs including overwrites.
   `manta-muskie/lib/common.js` `createMetadata()` uses the newly
   selected sharks when available, falling back to previous sharks
   only for metadata-only operations.)

4. **Verify user data is fully evacuated** — check that all errors are
   in `/stor/logs/` paths:
   ```bash
   grep EtagConflict /var/svc/log/manta-application-rebalancer:default.log \
     | grep -o 'stor/logs/[a-z_-]*' | sort | uniq -c | sort -rn
   ```
   If all errors are `/stor/logs/*`, the evacuation is complete for
   operational purposes.

5. **Re-enable minnow** only if keeping the shark in service.  If
   decommissioning, leave minnow disabled and proceed with hardware
   removal.

**The evacuation is operationally complete when:**
- All non-`/stor/logs/` objects show as `complete`
- Remaining errors are exclusively `/stor/logs/` paths
- Minnow remains disabled on the evacuated shark

### Decommissioning a Shark After Evacuation

**Important:** Completing an evacuation with the system path filter
(`exclude_key_prefixes`) does NOT mean the shark is safe to
decommission.  The filter skips system objects during discovery, so
their moray metadata **still references the evacuated shark**.

System log objects use **unique hourly paths** (e.g.,
`/stor/logs/muskie/2026/04/05/12/83e3b3bc.log`).  The hourly cron
creates new files at the current hour's path — it does **not**
overwrite old ones.  Historical log objects will never be naturally
replaced by cron rotation.

**Before decommissioning**, verify that no moray metadata references
the shark.  Create fresh pgclones and run against each moray shard:

```bash
# On each moray pgclone:
psql -U postgres -h /tmp moray -c \
  "SELECT count(*) FROM manta
   WHERE _value LIKE '%<SHARK_STORAGE_ID>%';"
```

If the count is non-zero, the shark still has objects that would
become inaccessible.  Options:

1. **Run a second evacuation without the filter** — set
   `REBALANCER_EXCLUDE_KEY_PREFIXES` to an empty array `[]` in SAPI
   metadata.  This will attempt to move all system objects.  Etag
   errors are expected for recently-overwritten logs, but historical
   logs (which haven't been touched) will evacuate successfully.

2. **Accept the loss of old logs** — if the objects are expendable
   historical logs (e.g., weeks-old service logs), decommission
   anyway.  Reads for those specific objects will fail, but they
   have no operational impact.

3. **Wait and re-check** — some objects may be overwritten
   over days/weeks (daily pg_dumps, weekly audits).  Re-check
   periodically until the count reaches an acceptable level.

**Recommended approach for decommissioning:**

1. First evacuation **with** the filter — clean, zero errors, moves
   all user data.
2. Second evacuation **without** the filter — moves historical system
   objects.  Accept etag errors on recently-overwritten logs.
3. Verify with pgclone query — confirm count is zero or near-zero.
4. Decommission.

#### Second Pass Procedure (Removing the Filter)

**Step 1 — Disable the exclude filter via SAPI:**

```bash
MANTA_APP=$(sdc-sapi /applications?name=manta | json -Ha uuid)
echo '{ "metadata": {
    "REBALANCER_EXCLUDE_KEY_PREFIXES": ["none"]
} }' | sapiadm update $MANTA_APP
```

The sentinel value `"none"` tells the rebalancer to skip no objects.
An empty array `[]` does NOT work because mustache treats it as
falsy and renders the defaults instead.

Wait for config-agent to regenerate `config.json` (a few seconds),
then verify:

```bash
# In the rebalancer zone:
json exclude_key_prefixes < /opt/smartdc/rebalancer/config.json
# Should print: [ "none" ]
```

**Step 2 — Restart the rebalancer** to pick up the new config:

```bash
svcadm restart svc:/manta/application/rebalancer
```

**Step 3 — Create fresh pgclones.** Minnow is already disabled from
the first pass — do NOT re-enable it.

```bash
pgclone.sh destroy-all
pgclone.sh clone-all \
  --moray-vm <UUID> [--moray-vm ...] \
  --buckets-vm <UUID> [--buckets-vm ...]
```

**Step 4 — Run the second evacuation:**

```bash
rebalancer-adm job create evacuate --shark <SHARK>
```

This time all objects are discovered, including `/stor/logs/`,
`/stor/usage/`, and `/stor/manatee_backups/`.  Historical log files
(written days/weeks ago and never touched since) will evacuate
cleanly.  Only logs overwritten by crons during the job will get
etag errors — this is a much smaller set than the first-pass-without-
filter scenario because most system objects are historical.

**Step 5 — Retry once** with fresh pgclones to catch the remaining
etag errors from step 4.

**Step 6 — Verify safe to decommission.** Destroy pgclones, create
fresh ones, then query each moray shard:

```bash
# On each moray pgclone:
psql -U postgres -h /tmp moray -c \
  "SELECT count(*) FROM manta
   WHERE _value LIKE '%<SHARK_STORAGE_ID>%';"
```

When the count is **0 across all shards**, no metadata references the
shark and it is safe to decommission.  If a small count remains
(logs written in the last hour), one more retry will clear them.

**Step 7 — Restore the filter** for future evacuations:

```bash
MANTA_APP=$(sdc-sapi /applications?name=manta | json -Ha uuid)
echo '{ "metadata": {
    "REBALANCER_EXCLUDE_KEY_PREFIXES": null
} }' | sapiadm update $MANTA_APP
```

Setting to `null` removes the SAPI override.  Config-agent
regenerates `config.json` with the template defaults
(`/stor/logs/`, `/stor/usage/`, `/stor/manatee_backups/`).
Restart the rebalancer to apply:

```bash
# In the rebalancer zone:
svcadm restart svc:/manta/application/rebalancer

# Verify defaults restored:
json options.exclude_key_prefixes < /opt/smartdc/rebalancer/config.json
# Should print: [ "/stor/logs/", "/stor/usage/", "/stor/manatee_backups/" ]
```

### Verifying Evacuation Completeness

The job summary (`rebalancer-adm job get`) shows total error counts but
does not distinguish between harmless `/stor/logs/` etag errors and real
user data failures.  Use these SQL queries against the job database to
verify.

From the rebalancer zone, connect to the job database using the job UUID:

```bash
/opt/postgresql/12.4/bin/psql -U postgres -h /tmp <JOB_UUID>
```

**1. Check if any non-log objects failed:**

```sql
-- If this returns 0, all errors are harmless log objects
SELECT count(*) FROM evacuateobjects
WHERE status = 'error'
AND object::json->>'key' NOT LIKE '%/stor/logs/%';
```

**2. See which non-log objects failed (if any):**

```sql
SELECT object::json->>'key', error, dest_shark
FROM evacuateobjects
WHERE status = 'error'
AND object::json->>'key' NOT LIKE '%/stor/logs/%';
```

**3. Breakdown of errors by path prefix:**

```sql
SELECT
  CASE WHEN object::json->>'key' LIKE '%/stor/logs/%'
       THEN '/stor/logs/'
       ELSE 'user_data'
  END AS category,
  error,
  count(*)
FROM evacuateobjects
WHERE status = 'error'
GROUP BY category, error
ORDER BY count DESC;
```

**4. Verify all user data was evacuated:**

```sql
SELECT status, count(*) FROM evacuateobjects
WHERE object::json->>'key' NOT LIKE '%/stor/logs/%'
GROUP BY status;
```

All rows should show `complete` or `skipped` — any `error` rows
for non-log objects require investigation.

**Interpreting results:**

**Known system-managed paths** that cause etag errors (all harmless):

| Path prefix | Written by | Cron |
|-------------|-----------|------|
| `/stor/logs/` | `logrotateandupload.sh` | Hourly |
| `/stor/usage/assets/` | mackerel metering (ops zone) | Various |
| `/stor/manatee_backups/` | `backup_pg_dumps.sh` | Half-hourly |

Filter all system paths when checking for real failures:

```sql
SELECT count(*) AS real_errors FROM evacuateobjects
WHERE status = 'error'
AND object::json->>'key' NOT LIKE '%/stor/logs/%'
AND object::json->>'key' NOT LIKE '%/stor/usage/%'
AND object::json->>'key' NOT LIKE '%/stor/manatee_backups/%';
```

**Interpreting results:**

| Result | Meaning |
|--------|---------|
| Real error count = 0 | Evacuation operationally complete |
| Real errors exist | Investigate — possible concurrent user writes or bugs |
| All errors on system paths | Expected — system crons changed etags |

### Worked Example: Evacuating 1.stor on COAL (2026-04-16)

This section documents a real evacuation of `1.stor.coal.joyent.us`
(5,735 objects, ~1.1 GB) on a 4-node COAL deployment with 2 moray
shards and 2 buckets-mdapi shards.

**Environment:**
- Headnode + 3 compute nodes (dc1-cn1, dc1-cn2, dc1-cn3)
- 7 storage zones, 1 storinfo zone
- Rebalancer zone on dc1-cn2
- `direct_db: true`, `mdapi` shards configured

**Step 1 — Identify the target.**

```
1.stor (dc1-cn1):  12,670 files, 1.1 GB used, 228 MB free  ← most constrained
5.stor (dc1-cn1):  105 files, 22 MB used
6.stor (dc1-cn2):  6,230 files, 420 MB used, 8.6 GB free
7.stor (dc1-cn2):  6,331 files, 472 MB used, 8.6 GB free
3.stor (dc1-cn3):  12,806 files, 1.2 GB used, 9.4 GB free
4.stor (dc1-cn3):  6,315 files, 450 MB used, 9.4 GB free
2.stor (headnode): 12,710 files, 1.1 GB used, 60.8 GB free
```

Selected `1.stor` — most constrained CN, real workload (12,670 files).

**Step 2 — Mark shark read-only.**

```bash
# Disable minnow on 1.stor zone (15ae7372 on dc1-cn1)
sdc-oneachnode -n <CN1_UUID> \
  'zlogin 15ae7372-fa40-41db-b20c-5473e36008fb svcadm disable minnow'

# Flush storinfo cache
sdc-oneachnode -a '
  for vm in $(vmadm lookup alias=~storinfo); do
    zlogin $vm svcadm restart storinfo; done'

# Restart muskie and buckets-api (all instances)
zlogin <WEBAPI_ZONE> 'svcadm restart svc:/manta/application/muskie:muskie-8081'
# ... (repeat for all muskie and buckets-api instances)
```

**Step 3 — Create fresh pgclones (after minnow disabled).**

```bash
pgclone.sh discover   # find postgres VMs across all CNs

pgclone.sh clone-all \
  --moray-vm <shard1-postgres-uuid> \
  --moray-vm <shard2-postgres-uuid> \
  --buckets-vm <shard1-buckets-postgres-uuid> \
  --buckets-vm <shard2-buckets-postgres-uuid>
```

**Step 4 — Start evacuation.**

```bash
rebalancer-adm job create evacuate --shark 1.stor.coal.joyent.us
# Returns: 4a087bbf-4034-40aa-8022-6848cb4791fe
```

**Step 5 — Monitor and results.**

The job took ~45 minutes on COAL hardware (all VMs sharing one physical
disk).  The hourly `logrotateandupload.sh` cron fired during the job,
causing etag drift on `/stor/logs/` objects.

```
Job 4a087bbf — Final results:
  Total:       5,735
  Complete:    3,109  (54%)
  Error:       2,604  (45%)
  Skipped:     22     (MPU parts + not found)
```

**Step 6 — Verify completeness.**

The 45% error rate looks alarming, but querying the job database
reveals all errors are on system-managed paths:

```sql
-- From rebalancer zone:
-- psql -U postgres -h /tmp 4a087bbf-4034-40aa-8022-6848cb4791fe

SELECT count(*) AS real_errors FROM evacuateobjects
WHERE status = 'error'
AND object::json->>'key' NOT LIKE '%/stor/logs/%'
AND object::json->>'key' NOT LIKE '%/stor/usage/%'
AND object::json->>'key' NOT LIKE '%/stor/manatee_backups/%';
```

Result: **0 real errors.**

Full breakdown by category:

```
        category        |  status  | count
------------------------+----------+-------
 /stor/logs/            | complete |  3097
 /stor/logs/            | error    |  2590
 /stor/logs/            | skipped  |     2
 /stor/manatee_backups/ | complete |     2
 /stor/manatee_backups/ | error    |     1
 /stor/usage/           | error    |    13
 /stor/usage/           | skipped  |     4
 user_data              | complete |    10
 user_data              | skipped  |    16
```

**All 10 user data objects completed successfully.  All 2,604 errors
were system-managed objects (`/stor/logs/`, `/stor/usage/`,
`/stor/manatee_backups/`) whose etags changed due to hourly system
cron jobs.  The evacuation was operationally complete.**

**Lessons learned:**

1. **Order matters:** pgclones must be created *after* disabling minnow
   and waiting for writes to drain.  Creating pgclones first caused
   additional etag errors from writes that occurred between the snapshot
   and the minnow disable.

2. **System crons cause unavoidable etag errors:** `logrotateandupload.sh`
   (hourly), `backup.sh` (every minute), and `backup_pg_dumps.sh`
   (half-hourly) all overwrite existing objects in Manta.  Each
   overwrite changes the etag in moray.  Since minnow is disabled,
   the overwritten log files are placed on different sharks by muskie
   — but the etag change still causes the rebalancer's metadata
   update to fail.  These errors are harmless because the new version
   of the log already lives on non-evacuated sharks.

3. **Always verify with SQL, not just the job summary:** The job summary
   shows 45% errors, but the database proves 0% of those are real
   failures.  Without the SQL check, an operator might incorrectly
   conclude the evacuation failed.

4. **Retries don't help for log objects:** Each retry creates fresh
   pgclones, but the hourly cron changes etags again before the retry
   finishes.  Accept the errors and verify via SQL instead of retrying
   indefinitely.

### Verifying Object Data After Evacuation

After an evacuation completes, verify that object data was
successfully copied to destination sharks and is accessible via
the Manta/S3 API.

#### Storage-level verification

Query the job database for completed objects and check that the
data exists on both the source and destination sharks:

```bash
# From the rebalancer zone — get a sample of completed objects:
psql -U postgres -h /tmp <JOB_UUID> -c \
  "SELECT id, object::json->>'key' AS key,
          object::json->>'bucket_id' AS bucket_id,
          object::json->>'owner' AS owner,
          dest_shark
   FROM evacuateobjects
   WHERE status = 'complete' LIMIT 5;"
```

Then verify the data on the destination shark.  Note that
**bucket objects (v2)** are stored at a different path than
traditional moray objects:

- **Moray objects (v1):** `/manta/<object_id>`
- **Bucket objects (v2):** `/manta/v2/<owner>/<bucket_id>/<prefix>/<object_id>,<content_md5>`

```bash
# Find the object on the destination shark:
sdc-oneachnode -n <DEST_CN_UUID> \
  'zlogin <DEST_STORAGE_ZONE> "find /manta -name <OBJECT_ID>*"'

# Verify it also still exists on the source shark (rebalancer
# copies, it does not delete):
sdc-oneachnode -n <SOURCE_CN_UUID> \
  'zlogin <SOURCE_STORAGE_ZONE> "find /manta -name <OBJECT_ID>*"'
```

Both should show the file with the same size.  The destination
copy will have a recent timestamp (from the evacuation), while the
source copy has the original timestamp.

#### S3 API verification (bucket objects)

For bucket objects, verify they are accessible via the S3 API.
You need the bucket name (not the bucket UUID).  Look up the
bucket name from the mdapi database, or if you know which buckets
your test objects are in:

```bash
# Download an evacuated object via s3cmd:
s3cmd get s3://<BUCKET_NAME>/<OBJECT_KEY> /tmp/verify.dat

# Compare checksum against the original:
md5sum /tmp/verify.dat
# Should match the contentMD5 in the object metadata from the job DB
```

```bash
# Or verify with curl using the S3 endpoint:
curl -k https://<MANTA_S3_ENDPOINT>/<BUCKET_NAME>/<OBJECT_KEY> \
  -o /tmp/verify.dat
```

If the download succeeds, Manta resolved the metadata (which now
points to the destination shark) and served the object from the
new location.  This confirms both the data copy and the metadata
update were successful.

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
live moray.  Three common scenarios:

1. **Re-run after a previous evacuation (expected):** The previous job
   updated metadata for objects it evacuated, changing their etags.
   The current pgclone snapshot still has the old etags.  These errors
   are **harmless** — the objects are already evacuated.
2. **pgclones created before disabling writes:** If pgclone snapshots
   were taken before the shark was marked read-only, any writes between
   the snapshot and the evacuation will cause etag drift.  Always
   disable minnow and wait for writes to drain **before** creating
   pgclones.
3. **System log uploads (`/stor/logs/`):** Disabling minnow only stops
   **user object placement** via storinfo.  System services (binder,
   manatee-sitter, waferlock, zookeeper, muskie, moray, etc.) continue
   uploading their logs to `/stor/logs/` via the hourly
   `logrotateandupload.sh` cron (`0 * * * *`).  These uploads go
   through muskie → moray directly, bypassing storinfo entirely.  Each
   upload overwrites the log object's metadata, changing its etag.

   **This means etag errors on `/stor/logs/` objects are expected on
   every evacuation**, regardless of whether minnow was disabled.  The
   errors will occur whenever the hourly cron runs between the pgclone
   snapshot and the metadata update for that object.

**Impact:** Not data-loss.  The object data was successfully copied to
the destination shark, but the metadata update was rejected.  The
source shark still has the object — nothing is lost.

**How to tell the difference:** If the errors are concentrated on
`/stor/logs/*` paths (binder, waferlock, manatee-sitter, etc.), it's
scenario 3 — the hourly log cron.  Run this query against the job
database to check:
```bash
grep EtagConflict /var/svc/log/manta-application-rebalancer:default.log \
  | grep -o 'stor/logs/[a-z_-]*' | sort | uniq -c | sort -rn
```

**Timing guidance:** The `logrotateandupload.sh` cron runs at the top
of every hour (`0 * * * *`).  To minimize etag errors:

1. Create pgclones shortly after the top of the hour (e.g., at :05)
2. Start the evacuation immediately after pgclones are ready
3. If the job completes before the next hour mark, `/stor/logs/` objects
   will have matching etags

For large evacuations that span multiple hours, expect a batch of etag
errors each time the cron fires.  Each retry with fresh pgclones will
converge, but will never reach zero errors if the job takes longer than
one hour.

**Fix:** Destroy pgclones, create fresh ones, then retry:
```bash
pgclone.sh destroy-all
pgclone.sh clone-all --moray-vm <UUID> --buckets-vm <UUID>
rebalancer-adm job retry <JOB_UUID>
```
For small evacuations (< 1 hour), time the pgclone creation just after
the hourly cron to avoid etag drift entirely.

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
