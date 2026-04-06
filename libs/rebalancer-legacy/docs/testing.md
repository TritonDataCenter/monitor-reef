# Rebalancer Testing Guide

This document covers end-to-end testing of the rebalancer evacuation
pipeline, including both moray (v1) and mdapi (v2 bucket) objects.

## Prerequisites

- Access to a Triton headnode (e.g., `headnode3` for coal)
- The `pgclone.sh` script deployed to the headnode (e.g., `/opt/pgclone.sh`)
- The rebalancer manager and agent binaries deployed
- A running Manta deployment with moray, buckets-mdapi, and storage nodes

## Step 1: Create pgclone Snapshots

The rebalancer uses read-only PostgreSQL clones for object discovery.
These are ZFS snapshots of the live moray and buckets-postgres databases.

### Find source VMs

For multi-shard or multi-CN deployments, use `pgclone.sh discover` to find
all postgres VMs across all compute nodes:

```bash
pgclone.sh discover
```

For single-shard coal environments, you can also find VMs directly:

```bash
# Moray postgres (pick the primary)
vmadm lookup -j alias=~'^1.postgres' | json -a uuid alias

# Buckets postgres (pick the primary)
vmadm lookup -j alias=~buckets-postgres | json -a uuid alias
```

### Create clones

```bash
# Multiple shards (one --moray-vm / --buckets-vm per shard)
pgclone.sh clone-all \
    --moray-vm <SHARD1_MORAY_UUID> \
    --moray-vm <SHARD2_MORAY_UUID> \
    --buckets-vm <SHARD1_BUCKETS_UUID> \
    --buckets-vm <SHARD2_BUCKETS_UUID>

# Single shard
pgclone.sh clone-all \
    --moray-vm <MORAY_POSTGRES_UUID> \
    --buckets-vm <BUCKETS_POSTGRES_UUID>

# Or individually
pgclone.sh clone-moray <MORAY_POSTGRES_UUID>
pgclone.sh clone-buckets <BUCKETS_POSTGRES_UUID>
```

### Verify clones

```bash
pgclone.sh list
```

Expected output: two running clones with tags `rebalancer-pg-clone` and
`rebalancer-buckets-pg-clone`.

### Verify DNS resolves from the rebalancer zone

```bash
zlogin <REBALANCER_UUID> 'dig +short 1.rebalancer-postgres.<DOMAIN>'
zlogin <REBALANCER_UUID> 'dig +short 1.rebalancer-buckets-postgres.<DOMAIN>'
```

If DNS does not resolve, check the registrar service inside the clone:

```bash
zlogin <CLONE_UUID> 'svcs registrar'
zlogin <CLONE_UUID> 'json registration.domain registration.aliases \
    < /opt/smartdc/registrar/etc/config.json'
```

## Step 2: Count Objects to Evacuate

Before starting a job, query the pgclone databases to know exactly
how many objects reference the target shark.

### Connect to pgclone databases

```bash
# Moray clone (find the UUID from pgclone.sh list)
zlogin <MORAY_CLONE_UUID> '/opt/postgresql/12.0/bin/psql -U postgres moray'

# Buckets clone
zlogin <BUCKETS_CLONE_UUID> '/opt/postgresql/12.0/bin/psql -U postgres buckets_metadata'
```

### Moray object count

```sql
SELECT count(*) AS on_target_shark
FROM manta
WHERE type = 'object'
  AND _value::text LIKE '%<SHARK_HOSTNAME>%';
```

Example:
```sql
SELECT count(*) AS on_shark2
FROM manta
WHERE type = 'object'
  AND _value::text LIKE '%2.stor.coal.joyent.us%';
```

### Bucket object count (per vnode)

```sql
SELECT 'vnode_0' AS vnode,
    count(*) FILTER (WHERE sharks::text LIKE '%<SHARK>%') AS on_target
FROM manta_bucket_0.manta_bucket_object
UNION ALL
SELECT 'vnode_1',
    count(*) FILTER (WHERE sharks::text LIKE '%<SHARK>%')
FROM manta_bucket_1.manta_bucket_object
UNION ALL
SELECT 'vnode_2',
    count(*) FILTER (WHERE sharks::text LIKE '%<SHARK>%')
FROM manta_bucket_2.manta_bucket_object
UNION ALL
SELECT 'vnode_3',
    count(*) FILTER (WHERE sharks::text LIKE '%<SHARK>%')
FROM manta_bucket_3.manta_bucket_object;
```

**Note:** Adjust the vnode list for your deployment — query
`information_schema.schemata` for all `manta_bucket_*` schemas:

```sql
SELECT schema_name FROM information_schema.schemata
WHERE schema_name LIKE 'manta_bucket_%'
ORDER BY schema_name;
```

### Bucket total (single number)

```sql
SELECT sum(c) FROM (
    SELECT count(*) FILTER (WHERE sharks::text LIKE '%<SHARK>%') AS c
    FROM manta_bucket_0.manta_bucket_object
    UNION ALL
    SELECT count(*) FILTER (WHERE sharks::text LIKE '%<SHARK>%')
    FROM manta_bucket_1.manta_bucket_object
    UNION ALL
    SELECT count(*) FILTER (WHERE sharks::text LIKE '%<SHARK>%')
    FROM manta_bucket_2.manta_bucket_object
    UNION ALL
    SELECT count(*) FILTER (WHERE sharks::text LIKE '%<SHARK>%')
    FROM manta_bucket_3.manta_bucket_object
) t;
```

## Step 3: Clean Up Before Running a Job

Before each test run, clean up state from previous jobs.

### Stop rebalancer and drop job databases

```bash
zlogin <REBALANCER_UUID> 'svcadm disable rebalancer'
zlogin <REBALANCER_UUID> 'sleep 2'

# Drop all job databases
zlogin <REBALANCER_UUID> '
for db in $(/opt/postgresql/12.4/bin/psql -U postgres -t -c \
    "SELECT datname FROM pg_database
     WHERE datname NOT IN ('\''postgres'\'', '\''template0'\'', '\''template1'\'')" \
    | tr -d " " | grep -v "^$"); do
    /opt/postgresql/12.4/bin/psql -U postgres -c "DROP DATABASE \"$db\""
done'

zlogin <REBALANCER_UUID> 'svcadm enable rebalancer'
```

### Clear agent queues on all storage nodes

```bash
for STOR_UUID in <STOR1_UUID> <STOR2_UUID> <STOR3_UUID>; do
    zlogin $STOR_UUID 'rm -rf /var/tmp/rebalancer/scheduled/* \
        /var/tmp/rebalancer/completed/*'
    zlogin $STOR_UUID 'svcadm restart rebalancer-agent'
done
```

### Verify clean state

```bash
zlogin <REBALANCER_UUID> 'curl -s http://localhost:80/jobs'
# Should return: []
```

## Step 4: Start the Evacuation Job

From the rebalancer zone:

```bash
zlogin <REBALANCER_UUID>
rebalancer-adm evacuate <SHARK_TO_EVACUATE>
```

Example:
```bash
rebalancer-adm evacuate 2.stor.coal.joyent.us
```

## Step 5: Monitor the Job

### Job status (from the rebalancer zone)

```bash
curl -s http://localhost:80/jobs/<JOB_UUID> | json
```

The response includes:

```json
{
  "results": {
    "Total": 75382,
    "Complete": 1287,
    "Assigned": 73091,
    "Post Processing": 0,
    "Error": 2,
    "Skipped": 1002,
    "Duplicates": 0,
    "Unprocessed": 0,
    "error_breakdown": {
      "moray": { "update_failed": 2 }
    },
    "skip_breakdown": {
      "{http_status_code:404}": 622,
      "source_is_evac_shark": 192,
      "source_other_error": 188
    }
  },
  "state": "Running"
}
```

### Monitor rebalancer manager logs

```bash
tail -f $(svcs -L svc:/manta/application/rebalancer:default)
```

Key messages to watch for:

| Log message | Meaning |
|---|---|
| `Direct_db Connecting to ...` | Connecting to pgclone for discovery |
| `Discovered N vnodes` | Bucket vnode auto-discovery |
| `Scan complete for ...: N objects across M vnodes` | Bucket discovery finished |
| `Moray directdb shard N complete: M rows scanned` | Moray discovery finished |
| `Hybrid: routing bucket object ... to mdapi` | Bucket metadata update routed to mdapi |
| `Hybrid: routing traditional object to moray` | Moray metadata update |
| `mdapi put_object success: ...` | Mdapi metadata update succeeded |
| `mdapi put_object failed: ...` | Mdapi metadata update failed |
| `eq_any update of N took Xms` | Moray batch metadata update |
| `Assignment Complete: ...` | Assignment fully processed |
| `Metadata update broker: draining N remaining assignments` | Drain sweep for late assignments |

### Monitor agent logs (on storage nodes)

```bash
tail -f $(svcs -L svc:/manta/application/rebalancer-agent:default)
```

Key messages:

| Log message | Meaning |
|---|---|
| `process_task: v1 object ...` | Processing moray object |
| `process_task: v2 object ...` | Processing bucket object |
| `Checksum passed -- no need to download` | File already exists on destination |
| `Download response for ... is 404 Not Found` | File not on source shark |
| `Begin processing assignment ...` | Assignment started |
| `Finished processing assignment ... in N seconds` | Assignment done |

### Check mdapi update progress

Count successful mdapi metadata updates:
```bash
cat /var/svc/log/manta-application-rebalancer:default.log* \
    | grep -c "mdapi put_object success"
```

### Verify live metadata (during or after job)

Check how many bucket objects still reference the evacuated shark on
the **live** buckets-postgres (not the clone):

```bash
zlogin <LIVE_BUCKETS_POSTGRES_UUID> \
    '/opt/postgresql/current/bin/psql -U postgres buckets_metadata' \
    -c "SELECT sum(c) FROM (
        SELECT count(*) FILTER (WHERE sharks::text LIKE '%<SHARK>%') AS c
        FROM manta_bucket_0.manta_bucket_object
        UNION ALL ...
    ) t"
```

This number should decrease as mdapi metadata updates land.

## Step 6: Post-Job Validation

After the job completes, verify the evacuation:

### Check final job results

```bash
curl -s http://localhost:80/jobs/<JOB_UUID> | json results
```

Verify:
- `Post Processing` is 0 (no stuck objects)
- `error_breakdown` shows expected errors only
- `Complete + Error + Skipped + Duplicates = Total`

### Validate live metadata

Query the live databases (not the clones) to confirm objects were moved:

```sql
-- Live moray: should be 0 or near 0
SELECT count(*) FROM manta
WHERE type = 'object'
  AND _value::text LIKE '%<EVACUATED_SHARK>%';

-- Live buckets-postgres: should be 0 or near 0
SELECT sum(c) FROM (
    SELECT count(*) FILTER (WHERE sharks::text LIKE '%<EVACUATED_SHARK>%') AS c
    FROM manta_bucket_0.manta_bucket_object
    UNION ALL ...
) t;
```

### Clean up

```bash
# Destroy pgclone snapshots
pgclone.sh destroy-all

# Verify
pgclone.sh list
# Should show: No clones found.

# Clear agent queues (only when no jobs are running)
manta-oneach -s storage 'rm /var/tmp/rebalancer/completed/*'
```

## Error and Skip Reason Reference

### Job Status Fields

| Field | Description |
|---|---|
| `Total` | Total objects discovered by sharkspotter |
| `Complete` | Objects successfully evacuated (file copied + metadata updated) |
| `Assigned` | Objects sent to agents, awaiting processing |
| `Post Processing` | Agent completed, metadata update in progress |
| `Error` | Metadata update failed |
| `Skipped` | Agent could not copy the file |
| `Duplicates` | Same object discovered from multiple shards |
| `Unprocessed` | Objects not yet assigned to an agent |

### error_breakdown

Errors occur during the **metadata update** phase — the agent
successfully copied the file, but updating the metadata (moray or
mdapi) failed.

| Error | Backend | Description |
|---|---|---|
| `moray.update_failed` | Moray | Generic moray put_object failure. Common cause: etag mismatch (object was already updated by a prior run or concurrent write). |
| `mdapi.update_failed` | Mdapi | Generic mdapi RPC failure (connection error, timeout). |
| `mdapi.etag_mismatch` | Mdapi | Mdapi conditional update rejected — the object's etag changed since discovery. Already updated by a prior run or concurrent write. |
| `mdapi.object_not_found` | Mdapi | Object no longer exists in mdapi — deleted between discovery and metadata update. |
| `other.internal_error` | General | Unexpected internal error (bug, serialization failure). |
| `other.bad_manta_object` | General | Object metadata is malformed — required fields missing or wrong type. |
| `other.bad_moray_client` | General | Moray client connection failed during metadata update. |
| `other.duplicate_shark` | General | Object already has a copy on the destination shark. |

### skip_breakdown

Skips occur during the **agent copy** phase — the agent could not
download the file from the source shark.

| Skip Reason | Description |
|---|---|
| `source_is_evac_shark` | The only copy of the object is on the shark being evacuated. No replica available to download from. These are single-replica objects — a data durability concern. |
| `source_object_not_found` | File not found (404) on the source shark. On re-runs this means already evacuated. On first runs it means genuinely missing file. |
| `source_other_error` | Agent could not reach the source shark after 3 retries (connection refused, DNS timeout, TCP timeout). Usually transient. |
| `agent_fs_error` | Agent filesystem error writing the downloaded file. Check disk space on the destination storage node. |
| `agent_assignment_timeout` | Agent didn't complete the assignment within 2 × `max_assignment_age` (default 7200s). Agents were overloaded. |
| `agent_busy` | Agent rejected the assignment (503) after manager exhausted 60 retries. |
| `md5_mismatch` | Downloaded file checksum does not match metadata. The source file may be corrupt. |
| `destination_insufficient_space` | Destination storage node does not have enough free space. |
| `destination_unreachable` | Could not connect to destination agent. |
| `network_error` | Generic network error posting assignment to agent. |
| `http_status_code(N)` | Source shark returned non-404 HTTP error N. |

### Interpreting Results

**Clean run (production with real replication):**
```
Complete: 74000    # Objects successfully evacuated
Error: 0           # No metadata failures
Skipped: 200       # source_is_evac_shark (single-replica objects)
```

**Coal/test environment:**
```
Complete: 1200     # Objects with real replicas
Error: 2           # Rare moray etag races
Skipped: 1000      # 454 source_object_not_found + 192 source_is_evac_shark + 252 source_other_error + 102 other
```

**Stale clone (re-running without fresh pgclone):**
```
Complete: 100      # Only newly added objects since last run
Error: 21000       # moray.update_failed (etag mismatch — already evacuated)
Skipped: 600       # Same as before
```
The fix: destroy clones and create fresh ones before re-running.
