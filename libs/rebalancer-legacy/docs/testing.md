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
rebalancer-adm job create evacuate --shark <SHARK_TO_EVACUATE>
```

Example:
```bash
rebalancer-adm job create evacuate --shark 2.stor.coal.joyent.us
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

## Investigating Object-to-Shard Distribution (buckets-mdapi)

This section documents how to trace which mdapi shard an object landed on,
how vnodes are distributed across shards, and how to query object placement
from the live postgres databases. This is essential for understanding the
rebalancer's view of the world, debugging placement issues, and validating
that objects are evenly distributed after a rebalance.

### Background: How Shard Placement Works

buckets-mdapi uses a **vnode-based sharding** scheme:

1. **Vnode assignment**: `vnode = MD5(object_key) / 2^96` (see
   `manager/src/mdapi_client.rs`, constant `DEFAULT_VNODE_HASH_INTERVAL`).
   This produces a vnode number (0..N-1) from the object key.

2. **Vnode-to-shard mapping**: Vnodes are statically partitioned across shards.
   In a 2-shard / 16-vnode deployment: even vnodes (0,2,4,...14) go to shard 1,
   odd vnodes (1,3,5,...15) go to shard 2. Each vnode maps to a PostgreSQL
   schema (`manta_bucket_0`, `manta_bucket_1`, etc.).

3. **Rebalancer behavior**: The rebalancer includes the vnode in every mdapi
   RPC call. It does **not** manage the vnode ring itself — it tries each
   mdapi shard in order, and the shard that owns the vnode accepts the request.

### Step 1: Map the Infrastructure

Identify all zones involved in the metadata path. Run these from the headnode.

#### Find buckets-mdapi zones

```bash
vmadm lookup -j alias=~buckets-mdapi | json -a uuid alias state
```

#### Find the mdapi config (shows which postgres it connects to)

```bash
MDAPI_UUID=<buckets-mdapi-uuid>
zlogin $MDAPI_UUID "cat /opt/smartdc/buckets-mdapi/etc/config.toml"
```

Key fields:
- `[database]` section — shows postgres host/port
- `[zookeeper]` — the ZK path tells you which manatee cluster (shard) this instance belongs to
  (e.g., `path = "/manatee/1.buckets-mdapi.coal.joyent.us"` = shard 1)

#### Find the manatee primaries for each shard via ZooKeeper

```bash
NS_UUID=$(vmadm lookup alias=~nameservice | head -1)

# Shard 1 primary
zlogin $NS_UUID "/opt/local/bin/zkCli.sh -server 127.0.0.1:2181 \
    ls /manatee/1.buckets-mdapi.coal.joyent.us/election"

# Shard 2 primary
zlogin $NS_UUID "/opt/local/bin/zkCli.sh -server 127.0.0.1:2181 \
    ls /manatee/2.buckets-mdapi.coal.joyent.us/election"
```

Output is a list of `IP:PORT:ID-SEQUENCE` entries. The **lowest sequence
number** is the primary. Example:

```
[10.77.77.34:5432:12345-0000000007, 10.77.77.35:5432:12345-0000000004, ...]
```

Here `10.77.77.35` (sequence `04`) is the primary for shard 1.

**Important**: The sequence number determines the primary, not the position
in the list. The lowest sequence is always the current primary.

#### Find which CN hosts each postgres zone

The postgres zones may be on compute nodes, not the headnode.
Use CNAPI to list all CNs and their admin IPs:

```bash
curl -s "http://cnapi.coal.joyent.us/servers?setup=true" \
    | json -Ha uuid hostname
```

Then search each CN for buckets-postgres zones. SSH to CNs requires the
`sdc.id_rsa` key and the CN's admin IP:

```bash
CN_ADMIN_IP=<admin-ip-from-cnapi>
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
    -i /root/.ssh/sdc.id_rsa $CN_ADMIN_IP \
    "vmadm list | grep buckets-postgres"
```

Confirm the manta NIC IP matches the manatee primary from ZK:

```bash
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
    -i /root/.ssh/sdc.id_rsa $CN_ADMIN_IP \
    "vmadm get <POSTGRES_UUID> | json nics.0.ip"
```

#### Example topology map (dc1 COAL, 2-shard, 16-vnode)

```
Shard 1 (even vnodes: 0,2,4,6,8,10,12,14)
  mdapi:   1.buckets-mdapi.coal.joyent.us (headnode)
  primary: 1.buckets-postgres on cn3 (10.77.77.34)
  sync:    1.buckets-postgres on cn2 (10.77.77.35)
  async:   1.buckets-postgres on cn1 (10.77.77.36)

Shard 2 (odd vnodes: 1,3,5,7,9,11,13,15)
  mdapi:   2.buckets-mdapi.coal.joyent.us (if deployed; may share shard 1 instance)
  primary: 2.buckets-postgres on cn2 (10.77.77.39)
  sync:    2.buckets-postgres on cn3 (10.77.77.38)
  async:   2.buckets-postgres on headnode (10.77.77.37)
```

### Step 2: Discover Vnode Schemas on Each Shard

Each vnode has its own PostgreSQL schema (e.g., `manta_bucket_0`). The
schema contains four tables: `manta_bucket`, `manta_bucket_object`,
`manta_bucket_deleted_bucket`, `manta_bucket_deleted_object`.

Connect to a shard's postgres primary and list the schemas.

**Why**: You need to know the exact vnode numbers before you can query
object distribution. Different deployments may have different vnode counts
(16, 32, 64, etc.).

```bash
# From the headnode, jump to the CN hosting the postgres primary
CN_IP=<cn-admin-ip>
PG_UUID=<postgres-zone-uuid>

ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
    -i /root/.ssh/sdc.id_rsa $CN_IP \
    "zlogin $PG_UUID \"/opt/postgresql/12.0/bin/psql \
        -U buckets_mdapi -d buckets_metadata \
        -c \\\"SELECT schema_name FROM information_schema.schemata
             WHERE schema_name LIKE 'manta_bucket_%'
             ORDER BY schema_name;\\\"\""
```

Example output (shard 1, even vnodes):

```
  schema_name
-----------------
 manta_bucket_0
 manta_bucket_10
 manta_bucket_12
 manta_bucket_14
 manta_bucket_2
 manta_bucket_4
 manta_bucket_6
 manta_bucket_8
```

**Tip**: To avoid triple-quoting hell when running SQL through
headnode -> CN -> zone, write the SQL to a file and copy it in:

```bash
# On the headnode
cat > /tmp/query.sql << 'EOSQL'
SELECT schema_name FROM information_schema.schemata
WHERE schema_name LIKE 'manta_bucket_%'
ORDER BY schema_name;
EOSQL

# Copy to CN, then into the zone's filesystem
scp -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
    -i /root/.ssh/sdc.id_rsa /tmp/query.sql $CN_IP:/tmp/

ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
    -i /root/.ssh/sdc.id_rsa $CN_IP \
    "cp /tmp/query.sql /zones/$PG_UUID/root/tmp/ && \
     zlogin $PG_UUID \"/opt/postgresql/12.0/bin/psql \
         -U buckets_mdapi -d buckets_metadata -f /tmp/query.sql\""
```

### Step 3: Count Objects and Buckets Per Vnode

This tells you how data is distributed across vnodes within a single shard.

**Why**: Before a rebalance, you need to know which vnodes have data and how
much. After a rebalance, you verify objects moved. Uneven distribution may
indicate a hashing problem or hot key.

Write this SQL to `/tmp/shard_counts.sql` on the headnode:

```sql
-- Adjust vnode numbers to match your shard's schemas.
-- This example is for shard 1 (even vnodes 0-14).

SELECT 'vnode_0' as vnode, 'buckets' as type, count(*) as cnt
    FROM manta_bucket_0.manta_bucket
UNION ALL SELECT 'vnode_0', 'objects', count(*)
    FROM manta_bucket_0.manta_bucket_object
UNION ALL SELECT 'vnode_2', 'buckets', count(*)
    FROM manta_bucket_2.manta_bucket
UNION ALL SELECT 'vnode_2', 'objects', count(*)
    FROM manta_bucket_2.manta_bucket_object
UNION ALL SELECT 'vnode_4', 'buckets', count(*)
    FROM manta_bucket_4.manta_bucket
UNION ALL SELECT 'vnode_4', 'objects', count(*)
    FROM manta_bucket_4.manta_bucket_object
UNION ALL SELECT 'vnode_6', 'buckets', count(*)
    FROM manta_bucket_6.manta_bucket
UNION ALL SELECT 'vnode_6', 'objects', count(*)
    FROM manta_bucket_6.manta_bucket_object
UNION ALL SELECT 'vnode_8', 'buckets', count(*)
    FROM manta_bucket_8.manta_bucket
UNION ALL SELECT 'vnode_8', 'objects', count(*)
    FROM manta_bucket_8.manta_bucket_object
UNION ALL SELECT 'vnode_10', 'buckets', count(*)
    FROM manta_bucket_10.manta_bucket
UNION ALL SELECT 'vnode_10', 'objects', count(*)
    FROM manta_bucket_10.manta_bucket_object
UNION ALL SELECT 'vnode_12', 'buckets', count(*)
    FROM manta_bucket_12.manta_bucket
UNION ALL SELECT 'vnode_12', 'objects', count(*)
    FROM manta_bucket_12.manta_bucket_object
UNION ALL SELECT 'vnode_14', 'buckets', count(*)
    FROM manta_bucket_14.manta_bucket
UNION ALL SELECT 'vnode_14', 'objects', count(*)
    FROM manta_bucket_14.manta_bucket_object
ORDER BY type, vnode;
```

Copy and run it using the file-copy method from Step 2.

Example output:

```
  vnode   |  type   | cnt
----------+---------+-----
 vnode_0  | buckets |   0
 vnode_2  | buckets |   0
 ...
 vnode_0  | objects |   0
 vnode_2  | objects |   0
 ...
```

Repeat for shard 2 (odd vnodes: 1,3,5,...15).

### Step 4: Inspect Individual Object Placement

To see exactly where a specific object is stored (which sharks hold copies):

```sql
-- Run on the shard that owns the object's vnode.
-- Replace manta_bucket_N with the correct vnode schema.
SELECT id, name, owner, bucket_id, content_length, content_type,
       sharks, created, modified
FROM manta_bucket_1.manta_bucket_object
WHERE name = 'wp12340507-dnd-dice-wallpapers.jpg';
```

The `sharks` column is a PostgreSQL array of `datacenter:storage_id` pairs:

```
{coal:3.stor.coal.joyent.us,coal:1.stor.coal.joyent.us}
```

This means the object has 2 copies:
- `3.stor.coal.joyent.us` in datacenter `coal`
- `1.stor.coal.joyent.us` in datacenter `coal`

### Step 5: Find Which Shard an Object Landed On

If you know the object key but not the shard, you can compute the vnode
from the key using the same algorithm the rebalancer uses.

**Why**: During rebalance debugging, you need to trace an object from
its key to the exact shard and database row. This lets you verify the
rebalancer's metadata updates hit the right place.

#### Compute vnode from object key

**Important**: buckets-api and the rebalancer use different hashing. The
authoritative placement algorithm is in buckets-api's `metadata_placement.js`
and `buckets/buckets.js`. The algorithm and interval are served dynamically
by the `buckets-mdplacement` service, not hardcoded.

The actual formula for **object placement** involves two hashes:

```
name_hash  = MD5(object_name)                          # hex string
tkey       = owner_uuid + ":" + bucket_id + ":" + name_hash
vnode      = SHA256(tkey) / VNODE_HASH_INTERVAL
```

Where:
- `owner_uuid` is the account UUID (e.g., `fe3617d8-...`)
- `bucket_id` is the bucket's **UUID** (not the bucket name!)
- `name_hash` is the MD5 hex digest of the object name
- `VNODE_HASH_INTERVAL` comes from the placement ring (typically
  `0fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff`)
- The hash algorithm (SHA-256) also comes from the placement ring

For **bucket placement**, the formula is simpler:

```
tkey  = owner_uuid + ":" + bucket_name
vnode = SHA256(tkey) / VNODE_HASH_INTERVAL
```

Note: bucket placement uses the bucket **name**, but object placement uses
the bucket **UUID**. This means you need to know the bucket UUID to predict
which vnode an object will land on.

**Why the double hash?** The `name_hash` (MD5 of object name) is stored on
the storage node's filesystem path. This allows the system to determine the
metadata shard for any file on a storage node without scanning all shards —
the inputs to the placement function (`owner`, `bucket_id`, `name_hash`) are
all available locally on the storage node.

In Python:

```python
import hashlib

# These values come from the placement ring (buckets-mdplacement service).
# Query them with the script in "Dumping the placement ring" below.
VNODE_HASH_INTERVAL = int(
    '0fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff', 16)
ALGORITHM = 'sha256'

def compute_object_vnode(owner, bucket_id, object_name):
    """Compute the vnode for an object, matching buckets-api behavior."""
    name_hash = hashlib.md5(object_name.encode()).hexdigest()
    tkey = f'{owner}:{bucket_id}:{name_hash}'
    h = int(hashlib.new(ALGORITHM, tkey.encode()).hexdigest(), 16)
    return h // VNODE_HASH_INTERVAL

def compute_bucket_vnode(owner, bucket_name):
    """Compute the vnode for a bucket."""
    tkey = f'{owner}:{bucket_name}'
    h = int(hashlib.new(ALGORITHM, tkey.encode()).hexdigest(), 16)
    return h // VNODE_HASH_INTERVAL

# Example: find bucket UUID first (from postgres), then compute object vnode
owner = 'fe3617d8-8df8-4bfd-8ef0-3f08cc6ae2ec'
bucket_id = '9a7d9ef5-6bd8-408c-9655-747e7f058901'  # UUID from manta_bucket table

v = compute_object_vnode(owner, bucket_id, 'wp12340507-dnd-dice-wallpapers.jpg')
print(f'vnode={v}')  # Output: 1 (odd -> shard 2)
```

**Rebalancer note**: The rebalancer code in `manager/src/mdapi_client.rs` uses
`MD5(key) / 2^96` which is a **different formula** from what buckets-api uses
for placement. The rebalancer does not determine placement — it reads the vnode
from the existing metadata record and includes it in mdapi RPC calls. The vnode
stored in the database is authoritative.

#### Dumping the placement ring

To see the exact algorithm, interval, and vnode-to-pnode mapping, run this
from the buckets-api zone:

```bash
BAPI_UUID=<buckets-api-uuid>
MDPLACEMENT_IP=<buckets-mdplacement-ip>   # from ZK /us/joyent/coal/buckets-mdplacement

zlogin $BAPI_UUID "cd /opt/smartdc/buckets-api && ./build/node/bin/node -e \"
var bmc = require('buckets-mdapi');
var bunyan = require('bunyan');
var log = bunyan.createLogger({name: 'test', level: 'fatal'});
var c = bmc.createClient({host: '$MDPLACEMENT_IP', port: 2021, log: log});
c.once('connect', function() {
  c.getPlacementData(function(err, pd) {
    if (err) { console.error(err); process.exit(1); }
    var keys = Object.keys(pd.ring.vnodeToPnodeMap).sort(function(a,b){return a-b;});
    console.log('algorithm:', JSON.stringify(pd.ring.algorithm));
    console.log('version:', pd.version);
    console.log('vnodes:', keys.length);
    console.log('pnodes:', JSON.stringify(pd.ring.pnodes));
    keys.forEach(function(k) {
      console.log('vnode', k, '->', pd.ring.vnodeToPnodeMap[k].pnode);
    });
    process.exit(0);
  });
});
setTimeout(function(){ process.exit(1); }, 5000);
\""
```

Example output:

```
algorithm: {"NAME":"sha256","MAX":"FFF...","VNODE_HASH_INTERVAL":"0fff..."}
version: 1.0.0
vnodes: 16
pnodes: ["tcp://1.buckets-mdapi.coal.joyent.us:2030","tcp://2.buckets-mdapi.coal.joyent.us:2030"]
vnode 0 -> tcp://1.buckets-mdapi.coal.joyent.us:2030
vnode 1 -> tcp://2.buckets-mdapi.coal.joyent.us:2030
...
```

#### Determine which shard owns the vnode

In the standard 2-shard deployment:
- Even vnodes (0,2,4,...) -> shard 1 (`1.buckets-mdapi`)
- Odd vnodes (1,3,5,...) -> shard 2 (`2.buckets-mdapi`)

Then query that shard's postgres using the schema `manta_bucket_<vnode>`.

#### Targeting a specific shard for testing

When testing rebalancer behavior on a particular shard, you need to upload
objects whose keys hash to vnodes owned by that shard. Since the vnode depends
on `SHA256(owner:bucket_id:MD5(object_name))`, you need to know the owner UUID
and bucket UUID first, then scan object names to find ones that land on the
desired shard.

**Prerequisites**: Get the owner UUID and bucket UUID from postgres (see Step 4),
or from the S3 upload debug output.

**Find object keys that land on shard 1** (even vnodes):

```python
import hashlib

# -- Configuration: update these for your deployment --
OWNER = 'fe3617d8-8df8-4bfd-8ef0-3f08cc6ae2ec'
BUCKET_ID = '9a7d9ef5-6bd8-408c-9655-747e7f058901'  # UUID, not name!

# From placement ring (see "Dumping the placement ring" above)
VNODE_HASH_INTERVAL = int(
    '0fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff', 16)
ALGORITHM = 'sha256'
# -- End configuration --

def compute_vnode(owner, bucket_id, object_name):
    name_hash = hashlib.md5(object_name.encode()).hexdigest()
    tkey = f'{owner}:{bucket_id}:{name_hash}'
    h = int(hashlib.new(ALGORITHM, tkey.encode()).hexdigest(), 16)
    return h // VNODE_HASH_INTERVAL

# Scan key variants until one hashes to an even vnode (shard 1)
for i in range(100):
    key = f"test-shard1-{i}.dat"
    v = compute_vnode(OWNER, BUCKET_ID, key)
    shard = 1 if v % 2 == 0 else 2
    if shard == 1:
        print(f"Upload as: {key}  (vnode {v} -> shard 1)")
        break
```

Then upload using the key it prints:

```bash
s3cmd put localfile.dat s3://app-uploads/test-shard1-0.dat
```

**Find keys for shard 2** (odd vnode) — same script, change `if shard == 2`.

**Batch approach** — generate a mapping table for bulk testing:

```python
import hashlib

OWNER = 'fe3617d8-8df8-4bfd-8ef0-3f08cc6ae2ec'
BUCKET_ID = '9a7d9ef5-6bd8-408c-9655-747e7f058901'
VNODE_HASH_INTERVAL = int(
    '0fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff', 16)

def compute_vnode(owner, bucket_id, object_name):
    name_hash = hashlib.md5(object_name.encode()).hexdigest()
    tkey = f'{owner}:{bucket_id}:{name_hash}'
    h = int(hashlib.sha256(tkey.encode()).hexdigest(), 16)
    return h // VNODE_HASH_INTERVAL

print(f"{'key':<30} {'vnode':>5} {'shard':>5}")
print("-" * 42)
for i in range(20):
    key = f"test-object-{i}.jpg"
    v = compute_vnode(OWNER, BUCKET_ID, key)
    shard = 1 if v % 2 == 0 else 2
    print(f"{key:<30} {v:>5} {shard:>5}")
```

Pick any key from the shard column you want to target.

**Key points**:
- The vnode depends on **owner UUID + bucket UUID + MD5(object name)**.
  The same object name in a different bucket or under a different owner
  will land on a different vnode.
- You must know the **bucket UUID** (not the bucket name) to predict
  object placement. Query it from postgres: `SELECT id, name FROM
  manta_bucket_N.manta_bucket;`
- The same key in the same bucket always lands on the same vnode —
  the mapping is deterministic.
- To spread test objects across all vnodes, use the batch script above
  and pick one key per vnode.
- The placement ring (algorithm, interval, vnode count) can vary between
  deployments. Always query it from `buckets-mdplacement` first.

### Step 6: Verify Where an Uploaded Object Landed

After uploading an object, use this end-to-end procedure to confirm which
shard and vnode it landed on. This is useful for validating the placement
formula and for debugging rebalancer behavior.

#### 6a. Predict the vnode (before or after upload)

You need three values: the **owner UUID**, the **bucket UUID**, and the
**object name**. Compute the expected vnode:

```bash
python3 -c "
import hashlib
OWNER='fe3617d8-8df8-4bfd-8ef0-3f08cc6ae2ec'
BUCKET_ID='9a7d9ef5-6bd8-408c-9655-747e7f058901'
OBJ='test-shard1-0.dat'
IV=int('0fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff',16)
nh=hashlib.md5(OBJ.encode()).hexdigest()
v=int(hashlib.sha256(f'{OWNER}:{BUCKET_ID}:{nh}'.encode()).hexdigest(),16)//IV
print(f'{OBJ} -> vnode {v} -> shard {1 if v%2==0 else 2}')
"
```

Example output:

```
test-shard1-0.dat -> vnode 10 -> shard 1
```

#### 6b. Confirm on the database

Query the predicted shard's postgres primary for the object. Use the
file-copy method from Step 2 to run SQL through the headnode -> CN -> zone
chain.

Write `/tmp/check_object.sql`:

```sql
-- Replace manta_bucket_N with the predicted vnode schema.
-- Example: vnode 10 -> manta_bucket_10
SELECT id, name, sharks, content_length, created
FROM manta_bucket_10.manta_bucket_object
WHERE name = 'test-shard1-0.dat';
```

Copy and run on the correct shard's postgres primary:

```bash
# Variables
CN_IP=10.99.99.40                                      # CN hosting the shard primary
PG_UUID=f83412b8-e373-4bb7-8e62-0e6e926430bf           # shard 1 postgres primary

# Copy SQL into the zone and run it
scp -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
    -i /root/.ssh/sdc.id_rsa /tmp/check_object.sql $CN_IP:/tmp/

ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
    -i /root/.ssh/sdc.id_rsa $CN_IP \
    "cp /tmp/check_object.sql /zones/$PG_UUID/root/tmp/ && \
     zlogin $PG_UUID \"/opt/postgresql/12.0/bin/psql \
         -U buckets_mdapi -d buckets_metadata -x \
         -f /tmp/check_object.sql\""
```

Example output:

```
-[ RECORD 1 ]----------------------------------------------------
id      | fc005623-4a2b-4ca2-a71b-71d37f253fd8
name    | test-shard1-0.dat
sharks  | {coal:2.stor.coal.joyent.us,coal:3.stor.coal.joyent.us}
content_length | 1048576
created | 2026-04-06 20:43:58.609677+00
```

If you get `(0 rows)`, either the upload hasn't completed, or the predicted
vnode was wrong. In that case, search all vnodes on both shards using the
query from Step 4 to find where the object actually is.

#### 6c. If you don't know which vnode to check

Search all vnodes on a shard at once. Write `/tmp/search_all.sql`:

```sql
-- Search all vnodes on shard 1 (even vnodes).
-- Adapt for shard 2 (odd vnodes) as needed.
SELECT 'vnode_0' as vnode, id, name, sharks, created
    FROM manta_bucket_0.manta_bucket_object WHERE name = 'test-shard1-0.dat'
UNION ALL SELECT 'vnode_2', id, name, sharks, created
    FROM manta_bucket_2.manta_bucket_object WHERE name = 'test-shard1-0.dat'
UNION ALL SELECT 'vnode_4', id, name, sharks, created
    FROM manta_bucket_4.manta_bucket_object WHERE name = 'test-shard1-0.dat'
UNION ALL SELECT 'vnode_6', id, name, sharks, created
    FROM manta_bucket_6.manta_bucket_object WHERE name = 'test-shard1-0.dat'
UNION ALL SELECT 'vnode_8', id, name, sharks, created
    FROM manta_bucket_8.manta_bucket_object WHERE name = 'test-shard1-0.dat'
UNION ALL SELECT 'vnode_10', id, name, sharks, created
    FROM manta_bucket_10.manta_bucket_object WHERE name = 'test-shard1-0.dat'
UNION ALL SELECT 'vnode_12', id, name, sharks, created
    FROM manta_bucket_12.manta_bucket_object WHERE name = 'test-shard1-0.dat'
UNION ALL SELECT 'vnode_14', id, name, sharks, created
    FROM manta_bucket_14.manta_bucket_object WHERE name = 'test-shard1-0.dat';
```

Run on shard 1's primary, then shard 2's primary if not found.

### Step 7: Cross-Shard Object Distribution Summary

To get a complete picture of object distribution across the entire deployment,
run the count query (Step 3) on **both** shard primaries and combine results.

Write `/tmp/shard_summary.sql`:

```sql
-- Run on each shard's primary, replacing vnode numbers accordingly.
-- This produces one row per vnode with bucket and object counts.
SELECT schema_name as vnode,
    (SELECT count(*) FROM manta_bucket_0.manta_bucket) as buckets,
    (SELECT count(*) FROM manta_bucket_0.manta_bucket_object) as objects
-- ... UNION ALL for each vnode schema
ORDER BY vnode;
```

Example combined output across both shards:

```
Shard 1 (cn3, 10.77.77.34):
  vnode   | buckets | objects
----------+---------+---------
 vnode_0  |       0 |       0
 vnode_2  |       0 |       0
 vnode_4  |       0 |       0
 vnode_6  |       0 |       0
 vnode_8  |       0 |       0
 vnode_10 |       0 |       0
 vnode_12 |       0 |       0
 vnode_14 |       0 |       0

Shard 2 (cn2, 10.77.77.39):
  vnode   | buckets | objects
----------+---------+---------
 vnode_1  |       1 |       1
 vnode_3  |       0 |       0
 vnode_5  |       0 |       0
 vnode_7  |       0 |       0
 vnode_9  |       0 |       0
 vnode_11 |       0 |       0
 vnode_13 |       0 |       0
 vnode_15 |       1 |       0
```

### Step 8: Verify Storage Node Placement

After identifying which sharks an object is on (from the `sharks` column),
verify the file actually exists on the storage node.

```bash
STOR_UUID=<storage-zone-uuid>
OWNER_UUID=<owner-uuid-from-object-record>
OBJECT_UUID=<object-id-from-object-record>

# Check if the file exists on the shark
zlogin $STOR_UUID "ls -la /manta/$OWNER_UUID/$OBJECT_UUID"
```

The mako nginx stores files at `/manta/<owner_uuid>/<object_uuid>`.

### Reference: Zone Discovery Cheat Sheet

```bash
# All commands run from the headnode.
# For zones on CNs, use: ssh -i /root/.ssh/sdc.id_rsa <CN_ADMIN_IP> "..."

# List all CNs
curl -s "http://cnapi.coal.joyent.us/servers?setup=true" \
    | json -Ha uuid hostname

# Find zones by alias pattern
vmadm lookup -j alias=~buckets-mdapi | json -a uuid alias state
vmadm lookup -j alias=~buckets-postgres | json -a uuid alias state
vmadm lookup -j alias=~nameservice | json -a uuid alias state

# Find zones on a CN (via headnode jump)
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
    -i /root/.ssh/sdc.id_rsa <CN_ADMIN_IP> \
    "vmadm list | grep buckets"

# Get zone IPs
vmadm get <UUID> | json nics.0.ip   # manta network
vmadm get <UUID> | json nics.1.ip   # admin network

# ZooKeeper queries (from nameservice zone)
NS_UUID=$(vmadm lookup alias=~nameservice | head -1)
zlogin $NS_UUID "/opt/local/bin/zkCli.sh -server 127.0.0.1:2181 \
    ls /us/joyent/coal/buckets-mdapi"

# Manatee election (find primary)
zlogin $NS_UUID "/opt/local/bin/zkCli.sh -server 127.0.0.1:2181 \
    ls /manatee/1.buckets-mdapi.coal.joyent.us/election"

# psql path on buckets-postgres zones
/opt/postgresql/12.0/bin/psql -U buckets_mdapi -d buckets_metadata
```
