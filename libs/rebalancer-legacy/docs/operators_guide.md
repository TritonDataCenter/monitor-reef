# Manta Rebalancer Operators Guide

The manta-rebalancer consists of a manager and a set of agents.  The manager is
a single service that is deployed to its own container/zone.  A rebalancer agent
is deployed to each of the mako zones on a storage node.

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

* [MANTA-5326](https://jira.joyent.us/browse/MANTA-5326)
* [MANTA-5330](https://jira.joyent.us/browse/MANTA-5330)
* [MANTA-5231](https://jira.joyent.us/browse/MANTA-5231) 
* [MANTA-5119](https://jira.joyent.us/browse/MANTA-5119)
* [MANTA-5159](https://jira.joyent.us/browse/MANTA-5159)
* See also `rebalancer-performance` Jira label

## Rebalancer Manager

### Build and Deployment 

The rebalancer manager is part of the default manta v2 deployment, and is built
using Jenkins.

The rebalancer manager can be deployed/upgraded in the same way as other manta
components using `manta-adm update -f <update_file>` where the `<update_file>`
specifies the image uuid of the rebalancer image to update to.

The rebalancer manager places its local postgres database in a delegated dataset
so that it will be maintained across reprovisions.  The memory requirements are
defined in the [sdc-manta repository](https://github.com/joyent/sdc-manta).


### Configuration and Troubleshooting
The rebalancer manager runs as an [SMF service](https://github.com/joyent/manta-rebalancer/blob/master/docs/manager.md#service-parameters) on its own zone:
```
svc:/manta/application/rebalancer:default
```

Logs are located in the SMF log directory and rotated hourly:
```
$ svcs -L svc:/manta/application/rebalancer:default
/var/svc/log/manta-application-rebalancer:default.log
```

The log level defaults to `debug`.  To change this specify a higher log level in
SAPI like so (this process is the same for all other [tunables](https://github.com/joyent/manta-rebalancer/blob/master/docs/manager.md#job-options)):
```
MANTA_APP=$(sdc-sapi /applications?name=manta | json -Ha uuid)
echo '{ "metadata": {"REBALANCER_LOG_LEVEL": "trace" } }' | sapiadm update $MANTA_APP
```

This should be propagated to the configuration file found in
`/opt/smartdc/rebalancer/config.json`
```
"log_level": "trace"
```

A Log level change is the only tunable that require a full service restart
(`svcadm restart rebalancer`), other's should be refreshed by config-agent's
invocation `svcadm refresh` and applied to the next Job that is run.
```
svcadm restart svc:/manta/application/rebalancer:default
```

Logs are located in the SMF log directory and rotated hourly:
```
$ svcs -L svc:/manta/application/rebalancer-agent:default
/var/svc/log/manta-application-rebalancer-agent:default.log
```

### Metadata throttle

The metadata throttle is an undocumented feature that exposes the ability to dynamically (while
a job is running) update the [REBALANCER_MAX_METADATA_UPDATE_THREADS](https://github.com/joyent/manta-rebalancer/blob/master/docs/manager.md#job-options) tunable.  This can be done with curl like so:
```
curl localhost/jobs/<job_uuid> -X PUT -d '{
    "action": "set_metadata_threads",
    "params": 30
}'
```

It is not currently possible to increase the number of metadata update threads beyond 100.  This maximum value is hard coded to minimize the impact to the
metadata tier by an accidental update. At the time of writing even the maximum
of 100 is not advised.



### Metrics

Rebalancer manager metrics can be accessed on port `8878` and the following
metrics are exposed:

* Request count, categorized by request type.
* Total number of bytes processed.
* Object count, indicating the total number of objects which have been processed.
* Error count, categorized by type of error observed.
* Skipped object, count categorized by reason that an object was skipped.
* Assignment processing times (in the form of a histogram).

### Marking evacuate target read-only
When an evacuate job is run the target storage node needs to be marked read-only
and remain read-only for the duration of the job.

1. Login to the target storage node and disable the minnow service: 
```
svcadm disable svc:/manta/application/minnow:default
```

1. If [storinfo](https://github.com/joyent/manta-storinfo) is deployed issue
   each storinfo endpoint the flush command:
```
for ip in `dig +short storinfo.<domain>`; do curl $ip/flush -X POST; done
```

1. Restart Muskie on all webapi instances:
```
manta-oneach -s webapi 'for s in `svcs -o FMRI muskie | grep muskie-`; do svcadm restart $s; done'
```

1. Restart buckets-api on all buckets-api instances (buckets-api generates
   metadata for bucket objects via mdapi and also caches shark lists):
```
manta-oneach -s buckets-api 'svcadm restart svc:/manta/application/buckets-api'
```


### Local Database Backup
Until [MANTA-5105](https://jira.joyent.us/browse/MANTA-5105) is implemented the local database corresponding to each job
should be backed up once that job is complete. 

The backup can be accomplished as follows
```
pg_dump -U postgres <job_uuid> > <job_uuid>.backup
```

The `<job_uuid>.backup` file should be saved outside the rebalancer zone as a
backup.

The database can then be restored like so:
```
createdb -U postgres <job_uuid>
psql -U postgres <job_uuid> < <job_uuid>.backup
```

### Cleaning up old assignments
Currently the rebalancer manager does not clean up assignments created when the
job ends [MANTA-5288](https://jira.joyent.us/browse/MANTA-5288).  On each
rebalancer-agent the completed assignments are stored in
`/var/tmp/rebalancer/completed/`.

The number of assignments can be determined via:
```
manta-oneach -s storage 'ls  /var/tmp/rebalancer/completed/ | wc -l'
```

Those assignments can be removed *ONLY* when there are no rebalancer jobs
running via:
```
manta-oneach -s storage 'rm /var/tmp/rebalancer/completed/*'
```


## Rebalancer Agent

### Deployment

As part of [MANTA-5293](https://jira.joyent.us/browse/MANTA-5293) a new
`manta-hotpatch-rebalancer-agent` tool was added to the headnode global zone.

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


### Example usage

The rest of this document is an example of running this tool in nightly-2
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


### Configuration and Troubleshooting
* The rebalancer agent runs as an SMF service on each mako zone:
```
svc:/manta/application/rebalancer-agent:default
```

* Logs are located in the SMF log directory and rotated hourly:
```
$ svcs -L svc:/manta/application/rebalancer-agent:default
/var/svc/log/manta-application-rebalancer-agent:default.log
```

* Finding `error` or `skipped` reasons:
    1.  Enter the local job database
    ```
    psql -U postgres <job uuid>
    ```
    2.  Query the `evacuateobjects` table:
    ```
    SELECT skipped_reason,count(skipped_reason) FROM evacuateobjects WHERE status = 'skipped' GROUP BY skipped_reason;
    ```
    or 
    ```
    SELECT error,count(error) FROM evacuateobjects WHERE status = 'error' GROUP BY error;
    ```

See [agent documentation](https://github.com/joyent/manta-rebalancer/blob/master/docs/agent.md) for additional details.

### Metrics
Rebalancer agent metrics can be accessed on port `8878` and the following
metrics are exposed:

* Request count, categorized by request type.
* Object count, indicating the total number of objects which have been processed.
* Total bytes processed.
* Error count, categorized by type of error observed.
* Assignment processing times (in the form of a histogram).


## pgclone: Read-Only PostgreSQL Clones

The rebalancer scans metadata databases to discover which objects live on the
storage node being evacuated.  Scanning the live Manatee primary puts load on
the metadata tier that can degrade the user experience.  `pgclone.sh` avoids
this by creating ephemeral, read-only PostgreSQL VMs from ZFS snapshots of
Manatee instances.  The rebalancer then reads from the clone instead of the
production primary.

`pgclone.sh` is a self-contained script located at:
```
libs/sharkspotter/tools/pgclone.sh
```

It is designed to be copied to the headnode global zone via `scp` — no
additional dependencies beyond `sdc-*` and `json(1)` are required.

### Subcommands

```
pgclone.sh clone-moray <manatee VM UUID>
pgclone.sh clone-buckets <buckets-postgres VM UUID>
pgclone.sh clone-all --moray-vm <UUID> --buckets-vm <UUID>
pgclone.sh list [--type moray|buckets|all] [--json]
pgclone.sh destroy <clone VM UUID>
pgclone.sh destroy-all [--type moray|buckets]
```

For backwards compatibility, the bare form still works:
```
pgclone.sh <manatee VM UUID>   # equivalent to clone-moray
```

### Pre-Evacuation Procedure

Before running `pgclone.sh` or starting an evacuation job the target storage
node must be made read-only.  This ensures no new objects are written to the
shark after the clone snapshot is taken.

1. Disable minnow on the target storage node:
```
svcadm disable svc:/manta/application/minnow:default
```

2. If [storinfo](https://github.com/joyent/manta-storinfo) is deployed, flush
   the cache on every storinfo instance:
```
for ip in $(dig +short storinfo.<domain>); do
    curl $ip/flush -X POST
done
```

3. Restart muskie on all webapi instances so they drop cached shark lists:
```
manta-oneach -s webapi \
    'for s in $(svcs -o FMRI muskie | grep muskie-); do
        svcadm restart $s
    done'
```

4. Restart buckets-api on all buckets-api instances.  buckets-api generates
   metadata for bucket objects via mdapi, so it also caches shark lists that
   must be invalidated:
```
manta-oneach -s buckets-api 'svcadm restart svc:/manta/application/buckets-api'
```

5. Verify that writes have drained before taking the snapshot:
```
# Confirm minnow is off
svcs minnow

# Watch for in-flight PUTs on the mako zone
tail -f /var/log/mako-access.log | grep PUT
```
When no PUTs appear for several minutes after the muskie and buckets-api
restarts it is safe to proceed.

### Creating Clones

#### Moray (v1 directory-based objects)

Find the Manatee primary VM for the shard you want to scan.  On the headnode:
```
sdc-vmapi '/vms?tag.manta_role=postgres&state=running' \
    | json -Ha uuid alias
```

Pick the primary for your shard and create the clone:
```
pgclone.sh clone-moray <manatee VM UUID>
```

This creates a surrogate VM that:
- ZFS-clones the Manatee data directory (point-in-time snapshot)
- Registers as `{shard}.rebalancer-postgres.{domain}` via registrar
- Starts PostgreSQL with autovacuum disabled and recovery.conf removed
- Is tagged with `manta_role=rebalancer-pg-clone` for discovery

#### Buckets-postgres (v2 bucket objects)

Find the buckets-postgres Manatee primary:
```
sdc-vmapi '/vms?tag.manta_role=buckets-postgres&state=running' \
    | json -Ha uuid alias
```

Create the clone:
```
pgclone.sh clone-buckets <buckets-postgres VM UUID>
```

The clone registers as `{shard}.rebalancer-buckets-postgres.{domain}` and is
tagged `manta_role=rebalancer-buckets-pg-clone`.

#### Both at once

To set up a full evacuation with both moray and buckets-postgres clones:
```
pgclone.sh clone-all --moray-vm <MORAY_UUID> --buckets-vm <BUCKETS_UUID>
```

### Listing Clones

```
pgclone.sh list                       # all clones
pgclone.sh list --type moray          # moray clones only
pgclone.sh list --type buckets        # buckets-postgres clones only
pgclone.sh list --json                # JSON output
```

### Destroying Clones

After the evacuation job completes, tear down the clones:
```
pgclone.sh destroy <clone VM UUID>    # single clone
pgclone.sh destroy-all                # all clones
pgclone.sh destroy-all --type moray   # moray clones only
```

The destroy operation stops the VM, removes the ZFS clone and its source
snapshot, and deletes the VM via VMAPI.  It is idempotent and safe to run
multiple times.

### How Clones Are Used

When the rebalancer manager starts an evacuation job, sharkspotter connects
to the clone instead of the production Manatee:

| Clone type | Sharkspotter connects to | Config flag |
|---|---|---|
| Moray | `{shard}.rebalancer-postgres.{domain}:5432` | `direct_db: true` |
| Buckets-postgres | `{shard}.rebalancer-buckets-postgres.{domain}:5432` | `direct_db: true` |

The clone is only used for **step 1 (discovery)** of the evacuation pipeline.
Steps 2 (data transfer) and 3 (metadata update) always go to the real moray
and mdapi services.  The clone never receives writes — it is a frozen
point-in-time ZFS snapshot.

When `direct_db` is enabled, both moray and mdapi RPC discovery are bypassed
in favor of direct PostgreSQL queries to the pgclone instances.  The RPC
endpoints (moray shards, mdapi shards) are still used for metadata updates
regardless of the discovery mode.

### Safety Properties

* The source Manatee VM is never modified (read-only ZFS snapshot).
* The clone has autovacuum disabled and recovery.conf removed.
* Failed clone creation automatically cleans up all artifacts (VM, snapshot).
* Each clone gets a unique ZFS snapshot name (`rebalancer-<uuid_short>`).
* Clone VMs are tagged with `manta_role` for easy discovery and cleanup.

### Troubleshooting

* **DNS not resolving**: Check that registrar is running inside the clone zone.
  Verify the clone alias is correct with `pgclone.sh list`.  Registrar needs
  a working binder/ZooKeeper; see the ZK topology notes in the deployment
  documentation.
* **Connection refused on port 5432**: PostgreSQL may have failed to start.
  Log into the clone zone and check `svcs -x` and `/var/pg/postgresql.log`.
* **Stale clones after job failure**: Run `pgclone.sh list` to find orphaned
  clones and `pgclone.sh destroy <UUID>` to clean them up.


## Evacuation with directdb: Step-by-Step Runbook

This section describes the complete end-to-end procedure for evacuating objects
from a storage node using directdb (direct PostgreSQL scanning via pgclone).
The directdb path avoids load on the production metadata tier by reading from
read-only ZFS clones instead of live Manatee primaries.

The evacuation pipeline has three phases:

1. **Discovery** -- sharkspotter reads from pgclone clones (read-only)
2. **Transfer** -- remora agents copy object data from source to destination shark
3. **Metadata update** -- manager writes to the **real** moray/mdapi to swap the
   shark entry (never to the clone)

### Step 1: Identify the target shark

Determine which storage node needs to be evacuated, e.g. `2.stor.<domain>`.

### Step 2: Make the shark read-only

All four substeps are required.  Skipping any one of them risks new objects
being written to the shark after the pgclone snapshot is taken.

1. Disable minnow on the target storage node (stops advertising to storinfo):
```
svcadm disable svc:/manta/application/minnow:default
```

2. Flush storinfo cache (if storinfo is deployed):
```
for ip in $(dig +short storinfo.<domain>); do
    curl $ip/flush -X POST
done
```

3. Restart muskie on all webapi instances (drops in-memory shark lists):
```
manta-oneach -s webapi \
    'for s in $(svcs -o FMRI muskie | grep muskie-); do
        svcadm restart $s
    done'
```

4. Restart buckets-api on all buckets-api instances (buckets-api generates
   metadata for bucket objects via mdapi and caches shark lists):
```
manta-oneach -s buckets-api 'svcadm restart svc:/manta/application/buckets-api'
```

| Step | Without it |
|---|---|
| Disable minnow | Storinfo keeps advertising the shark |
| Flush storinfo | Stale cache entries direct writes for minutes |
| Restart muskie | Muskie in-memory shark lists still include the shark |
| Restart buckets-api | buckets-api caches shark lists and routes bucket object writes to the shark |

### Step 3: Verify writes have drained

```
# Confirm minnow is off
svcs minnow

# Check across all storage nodes from the headnode
manta-oneach -s storage 'svcs minnow'

# Watch for in-flight PUTs on the mako zone
tail -f /var/log/mako-access.log | grep PUT
```

Wait until no PUTs appear for several minutes after the muskie and buckets-api
restarts.  In-flight requests drain within the muskie request timeout (a few
minutes).

### Step 4: Find the Manatee primaries

On the headnode, identify the Manatee primary VMs for the shards to scan.

For moray (v1 directory-based objects):
```
sdc-vmapi '/vms?tag.manta_role=postgres&state=running' \
    | json -Ha uuid alias
```

For buckets-postgres (v2 bucket objects):
```
sdc-vmapi '/vms?tag.manta_role=buckets-postgres&state=running' \
    | json -Ha uuid alias
```

Pick the primary for each shard.  In a typical deployment there is one primary
per shard.

### Step 5: Create pgclone clones

Copy `pgclone.sh` to the headnode global zone if not already there:
```
scp libs/sharkspotter/tools/pgclone.sh headnode:/var/tmp/
```

For moray only:
```
pgclone.sh clone-moray <MORAY_MANATEE_UUID>
```

For buckets-postgres only:
```
pgclone.sh clone-buckets <BUCKETS_MANATEE_UUID>
```

For both at once:
```
pgclone.sh clone-all \
    --moray-vm <MORAY_MANATEE_UUID> \
    --buckets-vm <BUCKETS_MANATEE_UUID>
```

Verify the clones are running:
```
pgclone.sh list
```

### Step 6: Verify DNS resolution

From the rebalancer zone, confirm the clones are reachable:
```
# Moray clone
dig +short 1.rebalancer-postgres.<domain>

# Buckets-postgres clone
dig +short 1.rebalancer-buckets-postgres.<domain>
```

If DNS does not resolve, check that registrar is running inside the clone zone
and that the binder/ZooKeeper topology is correct.

### Step 7: Configure the rebalancer

The rebalancer manager reads its configuration from
`/opt/smartdc/rebalancer/config.json`, which is rendered by config-agent from
the SAPI template.

There are two sides to configure:

* **Discovery** -- how the rebalancer finds objects on the target shark
* **Metadata update** -- where the rebalancer writes the updated sharks array
  after the object has been copied to a new destination

#### Discovery configuration

Discovery for each object type has two mutually exclusive modes: **direct**
(pgclone, reads from a read-only PostgreSQL clone) or **RPC** (queries the
live service).  Only one mode is active per object type — they cannot both
run at the same time.

**Moray objects (v1):**

| Mode | When | Connects to |
|---|---|---|
| Direct (pgclone) | `direct_db: true` | `{shard}.rebalancer-postgres.{domain}:5432` |
| Moray RPC | `direct_db: false` (default) | `{shard}.moray.{domain}:2021` |

**Bucket objects (v2):**

| Mode | When | Connects to |
|---|---|---|
| Direct (pgclone) | `direct_db: true` | `{shard}.rebalancer-buckets-postgres.{domain}:5432` |
| Mdapi RPC | `direct_db: false` and `mdapi.shards` non-empty | mdapi endpoints from config |
| None | `direct_db: false` and `mdapi.shards` empty | no bucket discovery |

When `direct_db` is enabled, both moray RPC and mdapi RPC discovery are
skipped in favor of direct PostgreSQL queries to the pgclone instances.
The RPC endpoints (moray shards, mdapi shards) are still used for
**metadata updates** (step 3 of the pipeline) — they are only skipped
for discovery.

The following SAPI metadata key controls discovery:

| SAPI metadata key | config.json field | Default | Description |
|---|---|---|---|
| `REBALANCER_DIRECT_DB` | `direct_db` | `true` | Use direct PostgreSQL for moray and bucket object discovery (requires pgclone clones) |

Shard ranges are auto-discovered from SAPI arrays (`INDEX_MORAY_SHARDS` for moray,
`BUCKETS_MORAY_SHARDS` for buckets). No manual min/max shard configuration is needed.

#### Metadata update configuration

The rebalancer uses two backends for metadata updates.  The backend is chosen
automatically based on configuration:

| Backend | Handles | Required SAPI metadata | config.json section |
|---|---|---|---|
| Moray | v1 directory-based objects | `INDEX_MORAY_SHARDS` | `shards` |
| Mdapi | v2 bucket objects | `BUCKETS_MORAY_SHARDS` | `mdapi.shards` |

Both are populated automatically by `manta-adm` during initial deployment.
The `BUCKETS_MORAY_SHARDS` entries contain buckets-mdapi hostnames like
`1.buckets-mdapi.<domain>`.

If both `shards` (moray) and `mdapi.shards` are configured, the rebalancer
operates in **hybrid mode**: bucket objects are updated via mdapi, traditional
directory objects via moray.  This is the recommended configuration for a
complete evacuation.

If `mdapi.shards` is empty, the rebalancer cannot update metadata for bucket
objects — those objects will be discovered but skipped during the metadata
update phase.

You can also tune the mdapi client behaviour:

| SAPI metadata key | config.json field | Default | Description |
|---|---|---|---|
| `MDAPI_CONNECTION_TIMEOUT_MS` | `mdapi.connection_timeout_ms` | `5000` | Connection timeout in ms |

#### Verify current configuration

From the rebalancer zone, inspect the rendered config:
```
json direct_db shards mdapi.shards \
    < /opt/smartdc/rebalancer/config.json
```

Check moray shards (discovery + metadata updates for v1 objects):
```
json shards < /opt/smartdc/rebalancer/config.json
```

Check mdapi shards (metadata updates for v2 bucket objects):
```
json mdapi.shards < /opt/smartdc/rebalancer/config.json
```

#### Example: Enable moray directdb only (v1 objects)

```
MANTA_APP=$(sdc-sapi /applications?name=manta | json -Ha uuid)
echo '{ "metadata": {
    "REBALANCER_DIRECT_DB": true
} }' | sapiadm update $MANTA_APP
```

#### Example: Enable buckets-postgres directdb (v2 objects)

This requires both the discovery flag and mdapi shards for metadata updates.
`BUCKETS_MORAY_SHARDS` is normally already set by `manta-adm`:
```
# Check if BUCKETS_MORAY_SHARDS is already configured
MANTA_APP=$(sdc-sapi /applications?name=manta | json -Ha uuid)
sdc-sapi /applications/$MANTA_APP | json metadata.BUCKETS_MORAY_SHARDS

# Enable directdb discovery (covers both moray and bucket objects)
echo '{ "metadata": {
    "REBALANCER_DIRECT_DB": true
} }' | sapiadm update $MANTA_APP
```

#### Example: Full evacuation (both v1 and v2 objects)

```
MANTA_APP=$(sdc-sapi /applications?name=manta | json -Ha uuid)
echo '{ "metadata": {
    "REBALANCER_DIRECT_DB": true
} }' | sapiadm update $MANTA_APP
```

This configures:
* Discovery: directdb for moray objects + directdb for bucket objects
* Metadata updates: moray RPC for v1 objects + mdapi RPC for v2 objects
  (hybrid mode — both backends active)

After updating SAPI, config-agent will render the new config and run
`svcadm refresh rebalancer`.  The new settings take effect on the next
evacuation job.

### Step 8: Start the evacuation job

```
rebalancer-adm job evacuate <N.stor.domain>
```

The pipeline now runs:
1. Sharkspotter reads from the pgclone clones (discovery, read-only)
2. Remora agents copy object data from source shark to destination shark
3. Manager writes to the real moray/mdapi to update the sharks array in the
   object metadata, replacing the old shark with the new one

### Step 9: Monitor progress

Job status:
```
rebalancer-adm job get <job_uuid>
```

Metrics (from the rebalancer zone):
```
curl localhost:8878/metrics
```

Logs:
```
svcs -L svc:/manta/application/rebalancer:default
tail -f $(svcs -L svc:/manta/application/rebalancer:default)
```

To dynamically adjust metadata update concurrency while the job is running:
```
curl localhost/jobs/<job_uuid> -X PUT -d '{
    "action": "set_metadata_threads",
    "params": 30
}'
```

The maximum is 100 threads (hard-coded safety limit).

### Step 10: Post-evacuation cleanup

Once the job completes:

1. Back up the local job database:
```
pg_dump -U postgres <job_uuid> > <job_uuid>.backup
```

2. Save the backup outside the rebalancer zone.

3. Destroy the pgclone clones:
```
pgclone.sh destroy-all
```

4. Verify no clones remain:
```
pgclone.sh list
```

5. Clean up completed assignments on agents (only when no jobs are running):
```
manta-oneach -s storage 'rm /var/tmp/rebalancer/completed/*'
```

### Important Notes

* **Clones are discovery-only.**  All metadata updates go to the real moray and
  mdapi services.  The clone is a frozen ZFS snapshot and never receives writes.
* **Evacuations can take days.**  The clone remains valid for the entire duration
  because it is a point-in-time snapshot.  Running a new pgclone mid-evacuation
  is not necessary and would not help.
* **To speed up evacuation**, increase the metadata update thread count (step 9)
  or the agent concurrency rather than re-cloning.
* **If the job fails mid-way**, the clone is still valid.  Fix the issue and
  restart the job.  Destroy the clone only after the evacuation is fully
  complete.
