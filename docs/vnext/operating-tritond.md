<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

Copyright 2026 Edgecast Cloud LLC.
-->

# Operating `tritond`: install and configure

`tritond` keeps its configuration in two places:

1. **A bootstrap config file** — the minimum needed to reach
   FoundationDB and accept connections (listen address, FDB cluster
   file, an optional initial log filter). Optional; absent at the
   default path means "use the built-in defaults".
2. **FoundationDB** — every other tunable (sweeper cadence, DHCP
   reconciler cadence, the in-process provisioner toggle, the metrics
   backend). `tritond` reads these once at startup; you change them
   with `tcadm config` or the admin console's **Settings** page. A
   change takes effect on the next `tritond` restart.

The packaging picture (SMF manifests, install paths, `tcadm doctor`,
the support bundle) lives in [`../design/operator-packaging-v1.md`].

## The bootstrap config file

TOML; every key optional:

```toml
# /etc/tritond/config.toml
bind_address     = "127.0.0.1:8080"   # HTTP listen address
fdb_cluster_file = "/etc/fdb.cluster"  # omit to use FDB's own resolution
log_filter       = "info"              # tracing env-filter directive
peer_endpoints   = []                  # reserved for the HA controller; unused in v1
```

Path resolution: `tritond serve --config PATH`, else `$TRITOND_CONFIG`,
else `/etc/tritond/config.toml`. A file requested explicitly that does
not exist is an error; a missing file at the default path is not.

Three environment variables override the file (env > file > default):
`TRITOND_BIND_ADDRESS`, `TRITOND_FDB_CLUSTER_FILE`, `RUST_LOG`.

## Cluster settings (in FoundationDB)

| Key | Default | Notes |
|---|---|---|
| `provisioner.inprocess_disabled` | `false` | skip the in-process stub provisioner (set when a real `tritonagent` drains the queue) |
| `sweeper.interval_secs` | `60` | stale-claim sweeper cadence |
| `sweeper.stale_claim_threshold_secs` | `600` | age before the sweeper reaps a job claim |
| `dhcp.reconcile_interval_secs` | `300` | DHCP lease reconciler cadence |
| `dhcp.lease_gc_threshold_secs` | `604800` | idle seconds before a DHCP lease is GC-eligible |
| `metrics.backend` | `memory` | `memory` (in-memory ring buffer) or `clickhouse` |
| `metrics.clickhouse_url` | *(unset)* | ClickHouse HTTP base URL; used only when `metrics.backend` is `clickhouse` |

Each key also has a legacy `TRITOND_*` environment variable that, when
set, overrides the FDB value at boot (env > FDB > default) — an
emergency escape hatch when FDB holds a bad value. `tcadm config list`
and the admin console flag any setting that's currently shadowed by an
env var; `tritond` logs a warning for each at startup.

## Installing a fresh controller (sketch)

1. Drop the binaries (`tritond`, `tcadm`) and bring up FoundationDB.
2. Write `/etc/tritond/config.toml` with at least `fdb_cluster_file`
   (and `bind_address` if you're not on `127.0.0.1:8080`).
3. Start `tritond`. On first run it mints the JWT signing key, the
   per-deployment identity HMAC key, and the `root` operator — the
   password prints **once** to stderr with a "save this" banner.
4. `tcadm configure --endpoint <host>:8080` — log in as `root`.
5. `tcadm api-key create ...` for automation; `tcadm config set ...`
   for any tuning. Restart `tritond` to apply config changes.

If the bootstrap banner is lost: `tritond reset-root-password`
(reads the same bootstrap config for the FDB cluster file; or pass
`--fdb-cluster-file PATH`) prints a fresh `root` password once.

## Managing settings

```sh
tcadm config list                              # table: key / value / default / env override / description
tcadm config get sweeper.interval_secs
tcadm config set sweeper.interval_secs 30      # JSON if it parses, else a string
tcadm config set metrics.backend clickhouse
tcadm config set metrics.clickhouse_url http://10.0.0.5:8123
tcadm config reset sweeper.interval_secs       # back to the built-in default
# then: restart tritond to apply
```

Same operations, plus an inline editor, on the admin console's
**Settings** page (Operate → Settings; fleet-admin only). Every set /
reset is recorded in the audit log.

See [`admin-webui-install.md`](./admin-webui-install.md) for the
admin console install runbook — including the post-`a13a9889`
`/v2/auth/login` alias fix that the currently-published binary
depends on.

[`../design/operator-packaging-v1.md`]: ../design/operator-packaging-v1.md
