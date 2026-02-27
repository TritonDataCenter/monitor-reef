<!--
    This Source Code Form is subject to the terms of the Mozilla Public
    License, v. 2.0. If a copy of the MPL was not distributed with this
    file, You can obtain one at http://mozilla.org/MPL/2.0/.
-->

<!--
    Copyright 2019, Joyent, Inc.
    Copyright 2024 MNX Cloud, Inc.
    Copyright 2025 Edgecast Cloud LLC.
-->

# Manta Rebalancer
This repository is part of the Triton Manta Project.  For contribution
guidelines, issues and general documentation, visit the
[Manta](https://github.com/TritonDataCenter/manta) project pages.

## Overview
The Manta Object Rebalancer is comprised of two main parts: a manager and an
agent.  Currently, the main function of the rebalancer is to evacuate objects
from an operator specified storage server (i.e. mako).  A rebalancer agent runs
on every mako in manta, while the rebalancer manager delegates work to various
agents in the form of something called "assignments".  An assignment is a list
containing information about objects for a given agent to download.  Through a
series of agent selections and assignments an entire storage node can be fully
evacuated.

For information in each piece of the project, please see:
* [Rebalancer Manager Guide](docs/manager.md)
* [Rebalancer Agent Guide](docs/agent.md)

## Basic Rebalancer Topology
```
                       Manager receives a
                       request to evacuate
                       all objects from a
                       given storage node.
                               +
                               |
                               v
+-----------+            +-----+------+                        +------------+
| Metadata  |            |            |   Assignment           |Storage Node|
|   Tier    |            |            |   {o1, o2, ..., oN}    |  +------+  |
| +-------+ |     +------+  Manager   +----------------------->+  |Agent |  |
| | Moray | |     |      |            |                        |  +------+  |
| |Shard 0| +<----+      |            |                        |            |
| +-------+ |     |      +------------+                        +-+---+---+--+
|           |     |                                              ^   ^   ^
| +-------+ |     |                                              |   |   |
| | Moray | |     |                                +-------------+   |   +-------------+
| |Shard 1| +<----+                                |o1               |o2               |oN
| +-------+ |     |                                |                 |                 |
|     .     |     |      +------------+      +-----+------+    +-----+------+    +-----+------+
|     .     |     |      |Storage Node|      |Storage Node|    |Storage Node|    |Storage Node|
| +-------+ |     |      |  +------+  |      |  +------+  |    |  +------+  |    |  +------+  |
| | Moray | |     |      |  |Agent |  |      |  |Agent |  |    |  |Agent |  |    |  |Agent |  |
| |Shard M| +<----+      |  +------+  |      |  +------+  |    |  +------+  |    |  +------+  |
| +-------+ |            |{o1, o2, oN}|      |            |    |            |    |            |
+-----------+            +-----+------+      +------------+    +------------+    +------------+
      ^                        ^
      |                        |             +                                                +
      +                        |             |                                                |
When all objects in            |             +-----------------------+------------------------+
an assignent have              |                                     |
been processed, the            +                                     v
manager updates the    Storage node to                     Objects in the assignment
metadata tier to       evacuate contains                   are retrieved from storage
reflect the new        objects:                            nodes other than the one
object locations.      {o1, o2, ..., oN}                   being evacuated.

```

## Metadata Backend Options

The rebalancer supports two metadata backends for storing object metadata:

### Moray (Traditional)
The default metadata backend using Moray's flexible JSON key-value storage.
Moray has been the traditional metadata storage for Manta objects.

### Buckets-MDAPI (Structured)
An alternative metadata backend using buckets-mdapi with structured PostgreSQL
tables. This provides:
- Structured schema with explicit types (vs. flexible JSON)
- Bucket-based object organization
- Compatibility with manta-buckets-api

#### Enabling MDAPI Backend

The mdapi backend is enabled by populating `BUCKETS_MORAY_SHARDS` in the manta
application SAPI metadata. Each entry corresponds to one buckets-mdapi instance.
This is the same variable that the garbage-collector uses (via
`manta-adm gc gen-shard-assignment`) to create one consumer per shard.

Configuration fields:
- `shards` (array of `{host: string}`): Mdapi shard endpoints. When non-empty, mdapi is used.
- `connection_timeout_ms` (number): Connection timeout in milliseconds

#### Schema Translation

The mdapi client (`manager/src/mdapi_client.rs`) handles automatic translation
between moray's JSON format and mdapi's structured format:

| Moray (JSON)          | Mdapi (Structured)      | Notes                    |
|-----------------------|-------------------------|--------------------------|
| `key` (string)        | `name` (string)         | Object path/key          |
| `owner` (string)      | `owner` (UUID)          | Parsed to UUID           |
| `sharks` (JSON array) | `sharks` (text[])       | Storage locations        |
| `content_md5` (base64)| `content_md5` (string)  | MD5 hash                 |
| `headers` (JSON)      | `headers` (hstore)      | HTTP headers             |
| `vnode` (i64)         | `vnode` (u64)           | Shard identifier         |
| `etag` (string)       | `conditions` (struct)   | Conditional updates      |

#### Backend Selection

By default, the rebalancer uses moray (backward compatible). To add mdapi:

1. Populate `BUCKETS_MORAY_SHARDS` in SAPI metadata (see below)
2. The manager will automatically use mdapi client functions in addition to moray
3. All schema translation happens transparently

#### Job Execution Integration

The mdapi backend is fully integrated into the job execution pipeline:

- **MetadataBackend enum**: Abstraction layer in `manager/src/jobs/evacuate.rs` that
  transparently handles both moray and mdapi clients
- **Automatic selection**: Backend is chosen at client creation time based on
  configuration (`should_use_mdapi()`)
- **Batch operations**: Moray uses native batch updates; mdapi uses native
  batchupdateobjects RPC with individual fallback
- **Single updates**: Both backends support individual object metadata updates with
  etag-based conditional updates
- **Error handling**: Unified error handling regardless of backend choice

The integration maintains backward compatibility - existing deployments continue using
moray unless explicitly configured for mdapi.

For more details on the mdapi client implementation, see the module documentation
in `manager/src/mdapi_client.rs`.

### SAPI Deployment Configuration

In production Triton deployments, the rebalancer configuration is managed via SAPI
(Services API) metadata. The SAPI template (`sapi_manifests/rebalancer/template`)
generates the configuration file dynamically based on metadata variables.

#### SAPI Metadata Variables

The following SAPI metadata variables control mdapi configuration:

| Variable | Type | Default | Description |
|----------|------|---------|-------------|
| `BUCKETS_MORAY_SHARDS` | JSON array | `[]` | Mdapi shard endpoints (application-level). Each entry: `{"host": "N.buckets-mdapi.DOMAIN"}`. When non-empty, mdapi is used. Same variable used by the garbage-collector. |
| `MDAPI_CONNECTION_TIMEOUT_MS` | Integer | 5000 | Connection timeout in milliseconds |

`BUCKETS_MORAY_SHARDS` is set as application-level SAPI metadata on the
manta application by `manta-shardadm set -b`. It follows the same
`[{host, last}]` pattern as `INDEX_MORAY_SHARDS`.

Format:
```json
[
  {"host": "1.buckets-mdapi.coal.joyent.us"},
  {"host": "2.buckets-mdapi.coal.joyent.us", "last": true}
]
```

After updating SAPI metadata, the config-agent will automatically regenerate
the configuration file and send SIGUSR1 to the rebalancer process for a hot
reload (no service restart required).

#### Deployment Scenarios

**Scenario 1: Moray-Only (Default)**
- No SAPI metadata changes required
- Empty `BUCKETS_MORAY_SHARDS` (or not set) — backward compatible
- Only moray metadata tier is used

**Scenario 2: Hybrid Mode (Complete Evacuation - Production)**
- Populate `BUCKETS_MORAY_SHARDS` via `manta-shardadm set -b`
- Both moray and mdapi are used
- Complete shark evacuation (traditional + bucket objects)
- Recommended for production deployments

#### Troubleshooting

**Connection Issues**
```bash
# Check mdapi shard is reachable from rebalancer zone
ping 1.buckets-mdapi.east.joyent.us

# Verify mdapi service is running
svcs -Z <mdapi-zone> buckets-mdapi

# Check rebalancer logs for mdapi errors
tail -f /var/log/rebalancer.log | grep -i mdapi
```

**Configuration Verification**
```bash
# View current SAPI metadata
sapiadm get <rebalancer-uuid> | json metadata

# View generated configuration file
cat /opt/smartdc/rebalancer/config.json | json mdapi
```

**Hot Reload Not Working**
```bash
# Manually trigger config reload
kill -USR1 <rebalancer-pid>

# Verify config-agent is running
svcs config-agent

# Check config-agent logs
tail -f /var/svc/log/smartdc-config-agent:default.log
```

## Build

### Binaries
Build release versions of `rebalancer-manager`, `rebalancer-agent`, and
`rebalancer-adm`:
```
make all
```

Build debug versions of `rebalancer-manager`, `rebalancer-agent`, and
`rebalancer-adm`:
```
make debug
```

For specific instructions on building individual parts of the project, please
review the instructions in their respective pages (listed above).

### Images
Information on how to building Triton/Manta components to be deployed within
an image please see the [Developer Guide for Building Triton and Manta][1].

[1]: https://github.com/TritonDataCenter/triton/blob/master/docs/developer-guide/building.md#building-a-component


### Pre-integration
Before integration of a change to any part of the rebalancer, the following
procedures must run cleanly:run `fmt`, `check`, `test`, and
[clippy](https://github.com/rust-lang/rust-clippy):
```
cargo fmt -- --check
make check
make test
```

Note: On the `cargo fmt -- --check`, this will display any lines that *would*
be impacted by an actual run of `cargo fmt`.  It is recommended to first
evaluate the scope of the change that format *would* make.  If it's the case
that the tool catches long standing format errors, it might be desirable to
address those in a separate change, otherwise a reviewer may have trouble
determing what is related to a current change and what is cosmetic, historical
clean up.

