# Triton Cloud metrics stack (dev)

This directory bundles the dev-environment metrics sink for the
admin UI's V5 dashboard and the per-tenant Prometheus exposition.

## Architecture

```
                                ┌──────────────────────────┐
                                │  tritond                 │
                                │                          │
  tritonagent (per CN) ───POST─▶│  /v2/agent/metrics       │
  kstat zones:::nsec_*           │       │                  │
  every 15s                      │       ▼                  │
                                 │  MetricsStore trait      │
                                 │   • RingBufferStore (dev)│
                                 │   • ClickHouseStore (TBD)│──tcp/9000──▶ ClickHouse
                                 │       │                  │   (this compose file)
                                 │       ▼                  │
                                 │  /v2/.../instances/.../  │
                                 │   metrics?range=1h       │
                                 │   /metrics  (Prom text)  │
                                 │   /v2/tenants/{}/metrics │
                                 └──────────────────────────┘
                                          ▲
                                          │ JSON RangeResult
                                ┌─────────┴────────┐
                                │ admin-backend    │
                                │  /api/.../metrics│
                                └─────────┬────────┘
                                          │
                                ┌─────────┴────────┐
                                │ admin-frontend   │
                                │ VmMetrics (V5)   │
                                └──────────────────┘
```

## Storage backends

* **`RingBufferStore`** (default). In-memory, keyed by
  `(schema, identity)`. Lossy on restart. Targets `cargo test`
  + laptop dev where standing up ClickHouse is overkill.
* **`ClickHouseStore`** (planned). Single MergeTree table per
  datum type, ordered `(timeseries_name, timeseries_key, timestamp)`.
  Schema in `init/01_schema.sql`. The trait is in place; the
  client wiring lands when tritond gains the
  [`clickhouse`](https://crates.io/crates/clickhouse) crate
  dependency.

Switch with environment variables on the `tritond` binary:

```sh
export TRITOND_METRICS_STORE=clickhouse
export TRITOND_METRICS_CLICKHOUSE_URL=http://localhost:8123
```

Unset, the ring buffer takes over.

## Running ClickHouse locally

```sh
docker compose -f dev/metrics/docker-compose.yml up -d
# Schema initialises automatically from init/01_schema.sql.
docker compose -f dev/metrics/docker-compose.yml exec clickhouse \
    clickhouse-client --query 'SHOW TABLES IN tritond_metrics'
# measurements_cumulative_u64
# measurements_gauge_f64
# measurements_gauge_u64
# timeseries
```

Tear down (drops volume too):

```sh
docker compose -f dev/metrics/docker-compose.yml down -v
```

## Schemas the agent currently emits

* **`triton.cpu_per_zone`** — per-zone CPU counters (ns), series
  values `user`, `system`, `iowait`. Sampled every 15s by
  `tritonagent::metrics`. Fed by
  `tritond_cn_platform::smartos::KstatTool::cpu_per_zone()`.
* **`triton.cpu_per_cn`** — per-CN aggregate (sum across all
  zones plus the GZ). Same series shape; identity has no
  `instance_id`. Same emission path.

Memory / disk-io / network / load / sockets schemas are placeholders
in the V5 dashboard until the relevant kstat / stat collectors land.

## V5 dashboard time ranges

| Button | Window | Step  | Buckets |
|--------|--------|-------|---------|
| 5m     | 5 min  | 5s    | 60      |
| 15m    | 15 min | 15s   | 60      |
| 1h     | 1 h    | 60s   | 60      |
| 6h     | 6 h    | 5m    | 72      |
| 24h    | 24 h   | 15m   | 96      |
| 7d     | 7 d    | 1h    | 168     |
| 30d    | 30 d   | 6h    | 120     |

The mapping lives in `tritond::resolve_metrics_range`. Adjust there
if a UI redesign tightens or loosens the cadence.

## Prometheus exposition

`tritond_metrics::prometheus::write_text` formats the latest sample
per `(schema, identity)` into Prometheus text format 0.0.4 with
metric names like `triton_cpu_per_zone_user_ns` and labels
`{cn, tenant, project, instance, series}`.

The HTTP listener for `/metrics` (admin) and `/v2/tenants/{tid}/metrics`
(per-tenant scope) is wired separately on its own port to keep
scrape traffic off the API surface; the listener implementation
is the next slice.
