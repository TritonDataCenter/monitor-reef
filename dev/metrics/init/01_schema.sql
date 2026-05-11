-- Schema for the ClickHouse-backed `MetricsStore` impl.
--
-- One table per datum type so each row is a fixed shape; the
-- (timeseries_name, timeseries_key, timestamp) sort order matches the
-- access pattern (filter by schema + identity, range on time).
--
-- TTL of 30 days: the V5 dashboard's longest range button. Bump in
-- step with any UI button additions.
--
-- `timeseries_name` = `tritond_metrics::SchemaName.0` (e.g.
-- `triton.cpu_per_zone`).
-- `timeseries_key`  = u64 hash of the identity tuple (cn / tenant /
-- project / instance / series). Computed in tritond before insert.
-- `fields_json`     = the identity tuple verbatim, kept for ad-hoc
-- queries that don't want to reverse the hash. Optional --
-- production deploys should be able to drop it once the tritond
-- query path stops fetching it.

CREATE DATABASE IF NOT EXISTS tritond_metrics;

CREATE TABLE IF NOT EXISTS tritond_metrics.measurements_cumulative_u64
(
    timeseries_name LowCardinality(String),
    timeseries_key  UInt64,
    timestamp       DateTime64(9, 'UTC'),
    fields_json     String,
    datum           UInt64
)
ENGINE = MergeTree()
ORDER BY (timeseries_name, timeseries_key, timestamp)
TTL toDateTime(timestamp) + INTERVAL 30 DAY;

CREATE TABLE IF NOT EXISTS tritond_metrics.measurements_gauge_f64
(
    timeseries_name LowCardinality(String),
    timeseries_key  UInt64,
    timestamp       DateTime64(9, 'UTC'),
    fields_json     String,
    datum           Float64
)
ENGINE = MergeTree()
ORDER BY (timeseries_name, timeseries_key, timestamp)
TTL toDateTime(timestamp) + INTERVAL 30 DAY;

CREATE TABLE IF NOT EXISTS tritond_metrics.measurements_gauge_u64
(
    timeseries_name LowCardinality(String),
    timeseries_key  UInt64,
    timestamp       DateTime64(9, 'UTC'),
    fields_json     String,
    datum           UInt64
)
ENGINE = MergeTree()
ORDER BY (timeseries_name, timeseries_key, timestamp)
TTL toDateTime(timestamp) + INTERVAL 30 DAY;

-- Index of registered timeseries identities -- one row per distinct
-- (timeseries_name, timeseries_key). Lets the Prometheus exposition
-- and the per-tenant query path map a key back to its labelset
-- without pulling fields_json off the hot measurement tables.

CREATE TABLE IF NOT EXISTS tritond_metrics.timeseries
(
    timeseries_name LowCardinality(String),
    timeseries_key  UInt64,
    cn_id           UUID,
    tenant_id       Nullable(UUID),
    project_id      Nullable(UUID),
    instance_id     Nullable(UUID),
    series          LowCardinality(String),
    first_seen      DateTime64(9, 'UTC')
)
ENGINE = ReplacingMergeTree(first_seen)
ORDER BY (timeseries_name, timeseries_key);
