<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

Copyright 2026 Edgecast Cloud LLC.
-->

# morayd

Moray-compatible key-value service backed by FoundationDB. Drop-in target
for any Triton service that currently speaks to a Moray/Manatee tier.

## Status

First cut — enough to carry a node-moray v2.8 client through the common
lifecycle:

- `ping`, `version`
- `createBucket`, `getBucket`, `listBuckets`, `delBucket`
- `putObject`, `getObject`, `delObject`

Verified end-to-end against the `morayping`, `morayversion`, `putbucket`,
`getbucket`, `listbuckets`, `putobject`, `getobject`, `delobject`,
`delbucket` CLIs bundled with node-moray 2.8 on a live Triton headnode.
See `services/morayd/tests/e2e.rs` for the in-process test, and
`~/workspace/triton_clean/fdb/README.md` for the FDB cluster this was
exercised against.

## Not yet implemented

- `findObjects` (LDAP filter evaluation over range scans)
- `updateObjects`, `deleteMany`, `reindexObjects`
- `batch`, `sql` (pass-through); server-side pre/post triggers
- `updateBucket` (schema migrations)
- Conditional put (`etag`), versioned buckets
- Backpressure / rate-limit headers on the response frame

## Architecture

```text
Triton service (node-moray client, fast@2.8)
    │  fast-protocol over TCP (port 2020)
    ▼
morayd   ── MorayStore trait ──▶  FdbStore       (production)
                               └▶ MemStore       (tests / laptop dev)
```

Module layout:

| file                  | role                                             |
|-----------------------|--------------------------------------------------|
| `src/fast.rs`         | Wire codec (Encoder/Decoder on tokio_util).      |
| `src/rpc.rs`          | Fast method name → store operation dispatch.     |
| `src/server.rs`       | TCP listener + per-connection task.              |
| `src/store/mod.rs`    | `MorayStore` trait (GATs, one seam).             |
| `src/store/mem.rs`    | In-process impl, parking_lot Mutex.              |
| `src/store/fdb.rs`    | FDB impl (feature `fdb`), tuple-layer keyspace.  |
| `src/types.rs`        | `Bucket`, `BucketConfig`, `ObjectMeta`.          |
| `src/error.rs`        | `MorayError` → node-moray wire name mapping.     |

## FDB keyspace (prefix `"m"` — reserved for morayd in the shared cluster)

```
("m","b",<name>)                     → Bucket       (JSON)
("m","k",<bucket>,<key>)             → ObjectMeta   (JSON)
("m","c",<bucket>,"id")              → u64 counter  (LE bytes)
```

Distinct from mantad's `"o"`/`"i"` prefixes, so morayd can share the same
FDB cluster without stepping on mantad's subspace.

## Wire-format specifics (gotchas we hit)

1. **Version byte = 1 on the wire.** The legacy `fast@2.8.2` bundled in
   Triton service zones treats any version other than 1 as a protocol
   error. Modern node-fast still accepts v1, so emitting v1 is universally
   compatible. Our decoder tolerates either.
2. **`d` must be a JSON array** in DATA and END frames. `FastMessage::data`
   auto-wraps scalar payloads; `FastMessage::end` uses `[]`.
3. **`ping` returns zero DATA frames.** node-moray's `rpcCommonNoData`
   asserts on exactly zero data rows.
4. **`version` returns a number, not a string.** node-moray's
   `versionInternal` checks `typeof versions[0].version === 'number'`.
5. **`getBucket` / `listBuckets` serialize `index`, `pre`, `post`,
   `options` as JSON-encoded strings.** node-moray's `parseBucketConfig`
   does `JSON.parse()` on each one — this is a Moray/Postgres legacy
   (those columns were TEXT). The `bucket_wire` helper in `rpc.rs`
   handles the conversion.
6. **`getBucket`'s rpcargs are `[opts, bucket]`**, not `[bucket, opts]`
   like every other verb. Historical quirk; the codebase tolerates it by
   special-casing the index.

## Building

On the illumos build host with `libfdb_c.so` installed at `/opt/fdb/lib`:

```
RUSTUP_TOOLCHAIN=1.89.0 RUSTFLAGS="-L /opt/fdb/lib" \
    cargo build -p morayd --features fdb --release
```

On a developer laptop (macOS/Linux, no libfdb_c): the default build uses
the in-memory store. `cargo test -p morayd` runs the unit tests plus the
wire-level integration test.

## Running

```
LD_LIBRARY_PATH=/opt/fdb/lib \
MORAYD_LISTEN=0.0.0.0:2020 \
MORAYD_CLUSTER_FILE=/etc/fdb/fdb.cluster \
RUST_LOG=morayd=info \
    /opt/fdb/bin/morayd
```

SMF manifest shipping is tracked as a follow-on — for now, run it by hand
or under `nohup` in the fdb zone.

## Test plan for the follow-on work

1. Implement `findObjects` with equality-filter evaluation via tuple-layer
   range scans, backed by a per-indexed-column secondary subspace.
2. Smoke test by pointing `rust-adminui1` (or a staging Triton service)
   at morayd instead of moray0 for a read-only verb first (listBuckets).
3. Add a dual-write adapter so Triton services can write to both moray0
   and morayd during cutover; reconcile and diff before flipping reads.
