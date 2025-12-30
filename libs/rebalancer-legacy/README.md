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

To use the mdapi backend instead of moray, add the following to your rebalancer
manager configuration file:

```toml
[mdapi]
enabled = true
endpoint = "mdapi.example.com:2030"
default_bucket_id = "550e8400-e29b-41d4-a716-446655440000"
connection_timeout_ms = 5000
```

Configuration fields:
- `enabled` (bool): Set to `true` to use mdapi, `false` for moray (default)
- `endpoint` (string): The mdapi service endpoint (host:port format)
- `default_bucket_id` (UUID, optional): Default bucket for object operations
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

By default, the rebalancer uses moray (backward compatible). To switch to mdapi:

1. Set `mdapi.enabled = true` in the configuration
2. The manager will automatically use mdapi client functions instead of moray
3. All schema translation happens transparently

#### Job Execution Integration

The mdapi backend is fully integrated into the job execution pipeline:

- **MetadataBackend enum**: Abstraction layer in `manager/src/jobs/evacuate.rs` that
  transparently handles both moray and mdapi clients
- **Automatic selection**: Backend is chosen at client creation time based on
  configuration (`should_use_mdapi()`)
- **Batch operations**: Moray uses native batch updates; mdapi falls back to individual
  updates (batch optimization planned for future)
- **Single updates**: Both backends support individual object metadata updates with
  etag-based conditional updates
- **Error handling**: Unified error handling regardless of backend choice

The integration maintains backward compatibility - existing deployments continue using
moray unless explicitly configured for mdapi.

For more details on the mdapi client implementation, see the module documentation
in `manager/src/mdapi_client.rs`.

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

