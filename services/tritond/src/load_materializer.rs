// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! ClickHouse -> FDB `cn-load-summary` materializer (RFD 00005 PL-6).
//!
//! A tokio task that, on a tick (default 60s), reads per-CN load
//! metrics out of ClickHouse, rolls them into one
//! [`tritond_store::CnLoadSummary`] row per compute node, and writes
//! each row back to FDB. The placement engine's load-history scorers
//! ([`tritond_placement::ScoreAvoidHotNow`] and friends) read those
//! rows on the hot path; keeping the roll-up off the pick path is the
//! whole point of the materializer.
//!
//! ## What it computes
//!
//! For each CN over two windows (5 minutes, 1 day):
//!
//! * **CPU busy fraction** from `triton.load_per_cn` series `5m`
//!   (a `GaugeF64` load average). We take `quantile(0.5)/quantile(0.95)`
//!   + `max` over the window in ClickHouse, then divide by the CN's
//!   logical core count ([`CnCapacity::cpu_threads_logical`], min 1)
//!   in Rust and clamp to `[0, 1]`. A CN with no `CnCapacity` row has
//!   an unknown core count: its CPU fields stay 0 and the row is
//!   marked stale.
//! * **RAM used p95** from `triton.mem_per_cn` series `used`
//!   (a `GaugeU64`, bytes) -- a direct ClickHouse quantile.
//! * **NIC tx/rx p95** from `triton.net_per_cn` series
//!   `tx_bytes`/`rx_bytes` (`CumulativeU64` counters). Counters need a
//!   rate: we window `lagInFrame` over `(timeseries_key, timestamp)`,
//!   divide the byte delta by the inter-sample second delta, drop
//!   negative (counter-reset) rows, then take the p95 rate.
//! * **Disk used p95 per pool** from `triton.zpool_capacity_per_cn`
//!   series `alloc` (`GaugeU64`, bytes), grouped by the `device` pool.
//!
//! ## Staleness (decision PL-6.3)
//!
//! A row is written `stale = true` when **either** (a) the ClickHouse
//! query for the pass errored, **or** (b) the CPU/RAM sample counts
//! are below the per-window minimum (`min_samples_5m` / `min_samples_1d`).
//! Empty net/disk feeds do *not* make a row stale -- the scorers only
//! read CPU + RAM, so an absent disk/net schema just writes zeros.
//!
//! ## Leader gate (decision PL-6.2)
//!
//! There is no leader-election primitive and we do not add one. Every
//! `tritond` instance runs this tick; the per-CN
//! [`Store::put_cn_load_summary`] is a last-writer-wins idempotent
//! overwrite of a tiny row (the struct doc notes no-op writes are
//! cheap under FDB MVCC). Monroe runs a single `tritond`, so this is
//! effectively exactly-once.
//
// A CAS lease (singleton row, holder + acquired_at, sweeper-style CAS)
// is a future optimization if multiple peers ever strain ClickHouse;
// it is intentionally omitted here.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use serde::Deserialize;
use tokio::time::{MissedTickBehavior, interval};
use tracing::{debug, info, warn};
use uuid::Uuid;

use tritond_metrics::store::ClickHouseStore;
use tritond_metrics::{Datum, MetricsStore, Sample, SampleIdentity, schemas, series};
use tritond_store::{CnLoadSummary, Store};

/// Tunables for the materializer task. Built once at startup from
/// [`tritond_store::Settings`]; the task takes an owned copy.
#[derive(Debug, Clone)]
pub struct LoadMaterializerConfig {
    /// How often the materializer rolls up ClickHouse into FDB.
    pub interval: Duration,
    /// Reserved for the future CAS lease; carried so the config shape
    /// is stable. Currently informational only.
    pub staleness_ticks: u32,
    /// CPU/RAM 5-minute sample floor below which the row is stale.
    pub min_samples_5m: u32,
    /// CPU/RAM 1-day sample floor below which the row is stale.
    pub min_samples_1d: u32,
    /// Resolved ClickHouse HTTP base URL. The task is only spawned when
    /// this resolves to `Some` (see `main`).
    pub clickhouse_url: String,
}

/// Spawn the materializer task and detach. Returns the handle for
/// callers that want to await shutdown; production drops it and lets
/// the runtime tear it down at process exit.
pub fn spawn(
    store: Arc<dyn Store>,
    metrics: Arc<dyn MetricsStore>,
    cfg: LoadMaterializerConfig,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(run(store, metrics, cfg))
}

async fn run(store: Arc<dyn Store>, metrics: Arc<dyn MetricsStore>, cfg: LoadMaterializerConfig) {
    // Build the materializer's own ClickHouse client from the resolved
    // URL. We do NOT thread `context.metrics` for the *reads*, because
    // that store may be the in-memory ring buffer when
    // `metrics.backend = memory`; the materializer wants a real CH
    // endpoint. The constructor reuses the identical reqwest+rustls
    // client, so this is a reuse, not a second dependency.
    let ch = match ClickHouseStore::new(cfg.clickhouse_url.clone()) {
        Ok(c) => c,
        Err(e) => {
            warn!(error = %e, "load materializer: clickhouse client init failed; task not running");
            return;
        }
    };
    info!(
        interval_secs = cfg.interval.as_secs(),
        min_samples_5m = cfg.min_samples_5m,
        min_samples_1d = cfg.min_samples_1d,
        "load materializer starting",
    );
    // The first tick fires immediately; skip it so the first pass
    // happens after one full interval, after bootstrap settles.
    let mut tick = interval(cfg.interval);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    tick.tick().await;
    loop {
        tick.tick().await;
        let started = Instant::now();
        match materialize_once(store.as_ref(), &ch, &cfg).await {
            Ok(report) => {
                debug!(
                    cns = report.rows_written,
                    stale = report.stale_rows,
                    "load materializer pass complete",
                );
                emit_metrics(metrics.as_ref(), started.elapsed(), report.stale_rows).await;
            }
            Err(e) => {
                warn!(error = %e, "load materializer pass failed; retry next tick");
            }
        }
    }
}

/// Per-pass tallies surfaced for the metrics emit + tests.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct MaterializeReport {
    pub rows_written: usize,
    pub stale_rows: usize,
}

/// Run one full roll-up pass: enumerate CNs, query the four feeds over
/// both windows, build + write one `CnLoadSummary` per CN.
///
/// Returns `Err` only if enumerating the CN list itself fails (nothing
/// to write against). ClickHouse query errors do NOT abort the pass:
/// they degrade the affected feed to "all stale" / zeros and the pass
/// still writes rows so the heatmap can tell "no data" from "data says
/// zero".
async fn materialize_once(
    store: &dyn Store,
    ch: &ClickHouseStore,
    cfg: &LoadMaterializerConfig,
) -> Result<MaterializeReport, tritond_store::StoreError> {
    let capacities = store.list_cn_capacities().await?;
    if capacities.is_empty() {
        return Ok(MaterializeReport::default());
    }

    // Logical-core count per CN, for CPU normalization. A CN absent
    // here has an unknown core count -> CPU stays 0 + row stale.
    let cores: BTreeMap<Uuid, u32> = capacities
        .iter()
        .map(|c| (c.server_uuid, c.cpu_threads_logical.max(1)))
        .collect();

    // Query every feed once (grouped by CN), tolerating per-query
    // failures. `None` means "this query errored" (-> stale); an empty
    // map means "no rows" (-> zeros, not stale on its own).
    let load_5m = query_load(ch, Window::FiveMin).await;
    let load_1d = query_load(ch, Window::OneDay).await;
    let ram_5m = query_gauge_p95(ch, schemas::MEM_PER_CN, series::USED, Window::FiveMin).await;
    let ram_1d = query_gauge_p95(ch, schemas::MEM_PER_CN, series::USED, Window::OneDay).await;
    let net_tx_5m = query_counter_rate_p95(ch, series::TX_BYTES, Window::FiveMin).await;
    let net_tx_1d = query_counter_rate_p95(ch, series::TX_BYTES, Window::OneDay).await;
    let net_rx_5m = query_counter_rate_p95(ch, series::RX_BYTES, Window::FiveMin).await;
    let net_rx_1d = query_counter_rate_p95(ch, series::RX_BYTES, Window::OneDay).await;
    let disk_5m = query_disk_p95(ch, Window::FiveMin).await;
    let disk_1d = query_disk_p95(ch, Window::OneDay).await;

    // If a CPU or RAM query errored outright, every row that depends on
    // it is stale regardless of per-CN sample counts (decision 3a).
    let query_errored =
        load_5m.is_none() || load_1d.is_none() || ram_5m.is_none() || ram_1d.is_none();

    let now = Utc::now();
    let mut report = MaterializeReport::default();

    for cap in &capacities {
        let cn = cap.server_uuid;
        let cn_cores = cores.get(&cn).copied();

        let l5 = lookup_load(&load_5m, cn);
        let l1 = lookup_load(&load_1d, cn);

        // CPU busy fraction = load / cores, clamped. Unknown cores ->
        // leave 0 and force stale.
        let (cpu_p50_5m, cpu_p95_5m, cpu_max_5m) = cpu_fraction(l5, cn_cores);
        let (cpu_p50_1d, cpu_p95_1d, cpu_max_1d) = cpu_fraction(l1, cn_cores);

        let (ram_p95_5m, ram_n_5m) = lookup_gauge(&ram_5m, cn);
        let (ram_p95_1d, ram_n_1d) = lookup_gauge(&ram_1d, cn);

        let nic_tx_5m = lookup_rate(&net_tx_5m, cn);
        let nic_tx_1d = lookup_rate(&net_tx_1d, cn);
        let nic_rx_5m = lookup_rate(&net_rx_5m, cn);
        let nic_rx_1d = lookup_rate(&net_rx_1d, cn);

        let disk_5m_map = lookup_disk(&disk_5m, cn);
        let disk_1d_map = lookup_disk(&disk_1d, cn);

        // Sample counts that gate staleness: the lesser of the CPU
        // (load) and RAM counts per window, since both are required.
        let n_5m = l5.map(|r| r.n).unwrap_or(0).min(ram_n_5m);
        let n_1d = l1.map(|r| r.n).unwrap_or(0).min(ram_n_1d);

        let stale = query_errored
            || cn_cores.is_none()
            || n_5m < cfg.min_samples_5m
            || n_1d < cfg.min_samples_1d;

        let row = CnLoadSummary {
            server_uuid: cn,
            cpu_p50_5m,
            cpu_p95_5m,
            cpu_max_5m,
            cpu_p50_1d,
            cpu_p95_1d,
            cpu_max_1d,
            ram_used_p95_5m: ram_p95_5m.round() as u64,
            ram_used_p95_1d: ram_p95_1d.round() as u64,
            disk_used_bytes_p95_5m: disk_5m_map,
            disk_used_bytes_p95_1d: disk_1d_map,
            nic_tx_bps_p95_5m: nic_tx_5m.round() as u64,
            nic_tx_bps_p95_1d: nic_tx_1d.round() as u64,
            nic_rx_bps_p95_5m: nic_rx_5m.round() as u64,
            nic_rx_bps_p95_1d: nic_rx_1d.round() as u64,
            samples_5m: n_5m,
            samples_1d: n_1d,
            last_refreshed_at: now,
            stale,
        };

        if let Err(e) = store.put_cn_load_summary(row).await {
            warn!(cn = %cn, error = %e, "load materializer: write cn-load-summary failed");
            continue;
        }
        report.rows_written += 1;
        if stale {
            report.stale_rows += 1;
        }
    }

    Ok(report)
}

/// Emit the two PL-6 self-metrics through tritond's metrics store. CN-
/// agnostic control-plane samples, so the identity is all-`None` /
/// nil-CN. Errors are swallowed -- a metrics hiccup must not break the
/// roll-up.
async fn emit_metrics(metrics: &dyn MetricsStore, elapsed: Duration, stale_rows: usize) {
    let now = Utc::now();
    let identity = SampleIdentity {
        cn_id: Uuid::nil(),
        tenant_id: None,
        project_id: None,
        instance_id: None,
        series: None,
        device: None,
    };
    let samples = [
        Sample {
            schema: schemas::PLACEMENT_LOAD_MATERIALIZER_SECONDS.into(),
            identity: identity.clone(),
            timestamp: now,
            datum: Datum::GaugeF64 {
                value: elapsed.as_secs_f64(),
            },
        },
        Sample {
            schema: schemas::PLACEMENT_LOAD_SUMMARY_STALE_ROWS.into(),
            identity,
            timestamp: now,
            datum: Datum::GaugeU64 {
                value: stale_rows as u64,
            },
        },
    ];
    if let Err(e) = metrics.insert(&samples).await {
        debug!(error = %e, "load materializer: metrics emit failed (non-fatal)");
    }
}

// ---------------------------------------------------------------------------
// ClickHouse queries
// ---------------------------------------------------------------------------

/// The two roll-up windows. `predicate` is dropped verbatim into the
/// `WHERE` clause; `INTERVAL` literals are fixed strings (no
/// interpolation of caller input), so there is no injection surface.
#[derive(Debug, Clone, Copy)]
enum Window {
    FiveMin,
    OneDay,
}

impl Window {
    fn predicate(self) -> &'static str {
        match self {
            Window::FiveMin => "timestamp >= now64(9,'UTC') - INTERVAL 5 MINUTE",
            Window::OneDay => "timestamp >= now64(9,'UTC') - INTERVAL 1 DAY",
        }
    }
}

/// One CN's load-average roll-up row.
#[derive(Debug, Clone, Copy)]
struct LoadRow {
    p50: f64,
    p95: f64,
    max: f64,
    n: u32,
}

/// Roll up `triton.load_per_cn` series `5m` (the load average) per CN.
/// Returns `None` on query/parse error (-> the caller forces stale).
async fn query_load(ch: &ClickHouseStore, window: Window) -> Option<BTreeMap<Uuid, LoadRow>> {
    let sql = format!(
        "SELECT JSONExtractString(fields_json, 'cn_id') AS cn, \
                quantile(0.5)(toFloat64(datum)) AS p50, \
                quantile(0.95)(toFloat64(datum)) AS p95, \
                max(toFloat64(datum)) AS mx, \
                count() AS n \
         FROM tritond_metrics.measurements_gauge_f64 \
         WHERE timeseries_name = '{schema}' \
           AND JSONExtractString(fields_json, 'series') = '{series}' \
           AND {pred} \
         GROUP BY cn \
         FORMAT JSONEachRow",
        schema = schemas::LOAD_PER_CN,
        series = series::LOAD_5M,
        pred = window.predicate(),
    );

    #[derive(Deserialize)]
    struct Raw {
        cn: String,
        #[serde(default)]
        p50: serde_json::Value,
        #[serde(default)]
        p95: serde_json::Value,
        #[serde(default)]
        mx: serde_json::Value,
        #[serde(default)]
        n: serde_json::Value,
    }

    let body = run_query(ch, sql, "load_per_cn").await?;
    let mut out = BTreeMap::new();
    for raw in parse_rows::<Raw>(&body) {
        let Some(cn) = parse_cn(&raw.cn) else {
            continue;
        };
        out.insert(
            cn,
            LoadRow {
                p50: as_f64(&raw.p50),
                p95: as_f64(&raw.p95),
                max: as_f64(&raw.mx),
                n: as_u32(&raw.n),
            },
        );
    }
    Some(out)
}

/// Roll up a `GaugeU64` schema's p95 (+ sample count) per CN for one
/// `series` value. Used for RAM (`mem_per_cn`/`used`).
async fn query_gauge_p95(
    ch: &ClickHouseStore,
    schema: &str,
    series: &str,
    window: Window,
) -> Option<BTreeMap<Uuid, (f64, u32)>> {
    let sql = format!(
        "SELECT JSONExtractString(fields_json, 'cn_id') AS cn, \
                quantile(0.95)(toFloat64(datum)) AS p95, \
                count() AS n \
         FROM tritond_metrics.measurements_gauge_u64 \
         WHERE timeseries_name = '{schema}' \
           AND JSONExtractString(fields_json, 'series') = '{series}' \
           AND {pred} \
         GROUP BY cn \
         FORMAT JSONEachRow",
        pred = window.predicate(),
    );

    #[derive(Deserialize)]
    struct Raw {
        cn: String,
        #[serde(default)]
        p95: serde_json::Value,
        #[serde(default)]
        n: serde_json::Value,
    }

    let body = run_query(ch, sql, schema).await?;
    let mut out = BTreeMap::new();
    for raw in parse_rows::<Raw>(&body) {
        let Some(cn) = parse_cn(&raw.cn) else {
            continue;
        };
        out.insert(cn, (as_f64(&raw.p95), as_u32(&raw.n)));
    }
    Some(out)
}

/// Roll up a `CumulativeU64` counter's per-sample rate p95 (bytes/sec)
/// per CN. Used for NIC tx/rx (`net_per_cn`). The counter is converted
/// to a rate inside ClickHouse with a window `lagInFrame` partitioned
/// by series key and ordered by time; negative (reset) rows are
/// dropped before the quantile.
///
/// A missing/empty feed yields an empty map (zeros, not stale). On a
/// hard query error returns `None` so the caller can decide; for
/// net/disk the caller treats `None`/empty the same (zeros), because
/// net/disk never gate staleness.
async fn query_counter_rate_p95(
    ch: &ClickHouseStore,
    series: &str,
    window: Window,
) -> BTreeMap<Uuid, (f64, u32)> {
    let sql = format!(
        "SELECT cn, quantile(0.95)(bps) AS p95, count() AS n FROM ( \
            SELECT JSONExtractString(fields_json, 'cn_id') AS cn, \
                   (toFloat64(datum) - lagInFrame(toFloat64(datum)) OVER w) \
                     / greatest(1e-9, date_diff('second', lagInFrame(timestamp) OVER w, timestamp)) AS bps \
            FROM tritond_metrics.measurements_cumulative_u64 \
            WHERE timeseries_name = '{schema}' \
              AND JSONExtractString(fields_json, 'series') = '{series}' \
              AND {pred} \
            WINDOW w AS (PARTITION BY timeseries_key ORDER BY timestamp) \
         ) \
         WHERE bps >= 0 \
         GROUP BY cn \
         FORMAT JSONEachRow",
        schema = schemas::NET_PER_CN,
        pred = window.predicate(),
    );

    #[derive(Deserialize)]
    struct Raw {
        cn: String,
        #[serde(default)]
        p95: serde_json::Value,
        #[serde(default)]
        n: serde_json::Value,
    }

    let Some(body) = run_query(ch, sql, schemas::NET_PER_CN).await else {
        return BTreeMap::new();
    };
    let mut out = BTreeMap::new();
    for raw in parse_rows::<Raw>(&body) {
        let Some(cn) = parse_cn(&raw.cn) else {
            continue;
        };
        out.insert(cn, (as_f64(&raw.p95), as_u32(&raw.n)));
    }
    out
}

/// Roll up `triton.zpool_capacity_per_cn` series `alloc` p95 bytes per
/// (CN, pool). A missing/empty feed yields an empty map (zeros, not
/// stale).
async fn query_disk_p95(
    ch: &ClickHouseStore,
    window: Window,
) -> BTreeMap<Uuid, BTreeMap<String, u64>> {
    let sql = format!(
        "SELECT JSONExtractString(fields_json, 'cn_id')  AS cn, \
                JSONExtractString(fields_json, 'device') AS pool, \
                quantile(0.95)(toFloat64(datum)) AS p95 \
         FROM tritond_metrics.measurements_gauge_u64 \
         WHERE timeseries_name = '{schema}' \
           AND JSONExtractString(fields_json, 'series') = '{series}' \
           AND {pred} \
         GROUP BY cn, pool \
         FORMAT JSONEachRow",
        schema = schemas::ZPOOL_CAPACITY_PER_CN,
        series = series::ALLOC,
        pred = window.predicate(),
    );

    #[derive(Deserialize)]
    struct Raw {
        cn: String,
        #[serde(default)]
        pool: String,
        #[serde(default)]
        p95: serde_json::Value,
    }

    let Some(body) = run_query(ch, sql, schemas::ZPOOL_CAPACITY_PER_CN).await else {
        return BTreeMap::new();
    };
    let mut out: BTreeMap<Uuid, BTreeMap<String, u64>> = BTreeMap::new();
    for raw in parse_rows::<Raw>(&body) {
        let Some(cn) = parse_cn(&raw.cn) else {
            continue;
        };
        if raw.pool.is_empty() {
            continue;
        }
        out.entry(cn)
            .or_default()
            .insert(raw.pool, as_f64(&raw.p95).round() as u64);
    }
    out
}

/// Issue the SQL, log + swallow CH errors into `None`.
async fn run_query(ch: &ClickHouseStore, sql: String, what: &str) -> Option<String> {
    match ch.query_jsoneachrow(sql).await {
        Ok(body) => Some(body),
        Err(e) => {
            warn!(feed = what, error = %e, "load materializer: clickhouse query failed");
            None
        }
    }
}

/// Parse a JSONEachRow body (one JSON object per line) into rows,
/// skipping blank/garbage lines rather than failing the whole pass.
fn parse_rows<T: for<'de> Deserialize<'de>>(body: &str) -> Vec<T> {
    body.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<T>(l).ok())
        .collect()
}

// ---------------------------------------------------------------------------
// Coercion + lookup helpers
// ---------------------------------------------------------------------------

/// ClickHouse renders aggregate results as JSON numbers, but large
/// `UInt64`s and some quantiles arrive as strings; accept both.
fn as_f64(v: &serde_json::Value) -> f64 {
    match v {
        serde_json::Value::Number(n) => n.as_f64().unwrap_or(0.0),
        serde_json::Value::String(s) => s.trim().parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

fn as_u32(v: &serde_json::Value) -> u32 {
    let f = as_f64(v);
    if f.is_finite() && f >= 0.0 {
        f.round().min(f64::from(u32::MAX)) as u32
    } else {
        0
    }
}

fn parse_cn(s: &str) -> Option<Uuid> {
    Uuid::parse_str(s.trim()).ok()
}

fn lookup_load(map: &Option<BTreeMap<Uuid, LoadRow>>, cn: Uuid) -> Option<LoadRow> {
    map.as_ref().and_then(|m| m.get(&cn).copied())
}

fn lookup_gauge(map: &Option<BTreeMap<Uuid, (f64, u32)>>, cn: Uuid) -> (f64, u32) {
    map.as_ref()
        .and_then(|m| m.get(&cn).copied())
        .unwrap_or((0.0, 0))
}

/// Net rate p95 (bytes/sec) for a CN; the net feeds are non-optional
/// maps (a missing feed yields zeros, never stale).
fn lookup_rate(map: &BTreeMap<Uuid, (f64, u32)>, cn: Uuid) -> f64 {
    map.get(&cn).map(|(p95, _)| *p95).unwrap_or(0.0)
}

fn lookup_disk(map: &BTreeMap<Uuid, BTreeMap<String, u64>>, cn: Uuid) -> BTreeMap<String, u64> {
    map.get(&cn).cloned().unwrap_or_default()
}

/// CPU busy fraction = load / cores, clamped to `[0, 1]`. Unknown cores
/// (CN has no `CnCapacity`) -> all zeros (the caller also marks stale).
fn cpu_fraction(load: Option<LoadRow>, cores: Option<u32>) -> (f32, f32, f32) {
    match (load, cores) {
        (Some(l), Some(c)) => {
            let c = f64::from(c.max(1));
            let f = |v: f64| ((v / c).clamp(0.0, 1.0)) as f32;
            (f(l.p50), f(l.p95), f(l.max))
        }
        _ => (0.0, 0.0, 0.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_fraction_divides_by_cores_and_clamps() {
        let l = LoadRow {
            p50: 4.0,
            p95: 8.0,
            max: 32.0,
            n: 10,
        };
        // 8 cores: p50 0.5, p95 1.0, max clamps to 1.0.
        let (p50, p95, mx) = cpu_fraction(Some(l), Some(8));
        assert!((p50 - 0.5).abs() < 1e-6);
        assert!((p95 - 1.0).abs() < 1e-6);
        assert!((mx - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cpu_fraction_unknown_cores_is_zero() {
        let l = LoadRow {
            p50: 4.0,
            p95: 8.0,
            max: 32.0,
            n: 10,
        };
        assert_eq!(cpu_fraction(Some(l), None), (0.0, 0.0, 0.0));
        assert_eq!(cpu_fraction(None, Some(8)), (0.0, 0.0, 0.0));
    }

    #[test]
    fn as_f64_accepts_number_and_string() {
        assert!((as_f64(&serde_json::json!(3.5)) - 3.5).abs() < 1e-9);
        assert!((as_f64(&serde_json::json!("7")) - 7.0).abs() < 1e-9);
        assert_eq!(as_f64(&serde_json::json!(null)), 0.0);
    }

    #[test]
    fn parse_rows_skips_blank_and_garbage() {
        #[derive(Deserialize)]
        struct R {
            cn: String,
        }
        let body = "{\"cn\":\"a\"}\n\nnot json\n{\"cn\":\"b\"}\n";
        let rows = parse_rows::<R>(body);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].cn, "a");
        assert_eq!(rows[1].cn, "b");
    }

    // A thin/empty result (no reachable ClickHouse) must mark the row
    // stale even when the CN has a capacity row with known cores.
    #[tokio::test]
    async fn thin_samples_yield_stale_row() {
        use tritond_store::{CnCapacity, MemStore, UnderlayCapability};

        // `ClickHouseStore::new` arms a rustls client whose builder needs
        // a process-default crypto provider; `main` installs one at
        // startup but the unit-test harness does not.
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

        let store = MemStore::new();
        let cn = Uuid::new_v4();
        store
            .put_cn_capacity(CnCapacity {
                server_uuid: cn,
                cpu_cores_physical: 8,
                cpu_threads_logical: 16,
                numa_nodes: Vec::new(),
                ram_total_mb: 65_536,
                ram_available_mb: 0,
                cpu_utilization_pct: 0.0,
                zpools: Vec::new(),
                nic_tags: Vec::new(),
                underlay: UnderlayCapability::default(),
                devices: Vec::new(),
                platform_version: String::new(),
                hvm_supported: false,
                reported_at: Utc::now(),
                vmm_protocol_version: None,
                cpu_features: Vec::new(),
                tsc_offset_ns: None,
                zpool_props: std::collections::BTreeMap::new(),
            })
            .await
            .unwrap();

        // No CH endpoint reachable -> every feed query errors -> the row
        // is forced stale (decision 3a), independent of sample counts.
        let ch = ClickHouseStore::new("http://127.0.0.1:1".to_string()).unwrap();
        let cfg = LoadMaterializerConfig {
            interval: Duration::from_secs(60),
            staleness_ticks: 3,
            min_samples_5m: 3,
            min_samples_1d: 12,
            clickhouse_url: "http://127.0.0.1:1".to_string(),
        };

        let report = materialize_once(&store, &ch, &cfg).await.unwrap();
        assert_eq!(report.rows_written, 1);
        assert_eq!(report.stale_rows, 1);

        let row = store.get_cn_load_summary(cn).await.unwrap().unwrap();
        assert!(row.stale, "thin/empty CH result must be stale");
        assert_eq!(row.cpu_p95_5m, 0.0);
        assert_eq!(row.samples_5m, 0);
    }
}
