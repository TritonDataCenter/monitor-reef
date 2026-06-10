// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! ClickHouse -> FDB `cn-load-summary` materializer (RFD 00005 PL-6).
//!
//! A tokio task that, on a tick (default 60s), reads per-CN load
//! metrics out of ClickHouse, rolls them into one
//! [`tritond_store::CnLoadSummary`] row per **Approved** compute node,
//! and writes each row back to FDB. The placement engine's
//! load-history scorers ([`tritond_placement::ScoreAvoidHotNow`] and
//! friends) read those rows on the hot path; keeping the roll-up off
//! the pick path is the whole point of the materializer.
//!
//! ## What it computes
//!
//! For each CN over two windows (5 minutes, 1 day):
//!
//! * **CPU busy fraction** from `triton.load_per_cn` series `5m`
//!   (a `GaugeF64` load average). We take `quantile(0.5)/quantile(0.95)`
//!   + `max` over the window in ClickHouse, then divide by the CN's
//!   logical core count ([`CnCapacity::cpu_threads_logical`], min 1)
//!   in Rust and clamp to `[0, 1]`. CNs are enumerated from their
//!   `cn-capacity` rows, so the core count is always at hand; a CN
//!   without a capacity row gets **no** `cn-load-summary` row at all
//!   (it is equally invisible to the placement capacity path).
//! * **RAM used p95** from `triton.mem_per_cn` series `used`
//!   (a `GaugeU64`, bytes) -- a direct ClickHouse quantile.
//! * **NIC tx/rx p95** from `triton.net_per_cn` series
//!   `tx_bytes`/`rx_bytes` (`CumulativeU64` counters). Counters need a
//!   rate: we window `lagInFrame` over `(timeseries_key, timestamp)`,
//!   divide the byte delta by the inter-sample second delta, drop
//!   negative (counter-reset) rows and each partition's first row
//!   (`lagInFrame` yields type defaults there, fabricating a sample),
//!   then take the p95 rate.
//! * **Disk used p95 per pool** from `triton.zpool_capacity_per_cn`
//!   series `alloc` (`GaugeU64`, bytes), grouped by the `device` pool.
//!   (No producer publishes that schema yet -- Phase B3 -- so the maps
//!   stay empty until the zpool collector lands.)
//!
//! ## Staleness (decision PL-6.3)
//!
//! A row is written `stale = true` when **either** (a) a CPU/RAM
//! ClickHouse query for the pass errored, **or** (b) the CPU/RAM
//! sample counts are below the per-window minimum (`min_samples_5m` /
//! `min_samples_1d`). Empty net/disk feeds do *not* make a row stale
//! -- the scorers only read CPU + RAM. Note the written zeros for
//! net/disk are therefore ambiguous ("no feed" vs "data says zero");
//! only the CPU/RAM feeds carry sample counts that disambiguate.
//!
//! Age-based staleness is enforced on the **read side**: the placement
//! projection (`placement::load_summary_view`) treats a row older than
//! `staleness_ticks × interval_secs` as stale, so this task dying
//! cannot leave frozen `stale = false` rows scoring as fresh.
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

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use serde::Deserialize;
use tokio::time::{MissedTickBehavior, interval};
use tracing::{debug, info, warn};
use uuid::Uuid;

use tritond_metrics::store::ClickHouseStore;
use tritond_metrics::{Datum, MetricsStore, Sample, SampleIdentity, schemas, series};
use tritond_store::{CnLoadSummary, CnState, Store};

/// Tunables for the materializer task. Built once at startup from
/// [`tritond_store::Settings`]; the task takes an owned copy.
/// (`staleness_ticks` is not here: the age gate it configures lives on
/// the read side, in the placement projection.)
#[derive(Debug, Clone)]
pub struct LoadMaterializerConfig {
    /// How often the materializer rolls up ClickHouse into FDB.
    pub interval: Duration,
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

/// Run one full roll-up pass: enumerate Approved CNs that have a
/// capacity row, query the four feeds over both windows, build + write
/// one `CnLoadSummary` per CN.
///
/// Returns `Err` only if enumerating the CN list itself fails (nothing
/// to write against). ClickHouse query errors do NOT abort the pass:
/// they degrade the affected feed to "all stale" / zeros and the pass
/// still writes rows.
async fn materialize_once(
    store: &dyn Store,
    ch: &ClickHouseStore,
    cfg: &LoadMaterializerConfig,
) -> Result<MaterializeReport, tritond_store::StoreError> {
    // Approved CNs only: a retired/disabled CN would otherwise keep a
    // permanently-stale row (its agent stops feeding ClickHouse) that
    // floors the stale_rows gauge above zero forever. Placement only
    // ever picks Approved CNs, so nothing downstream loses data.
    let approved: BTreeSet<Uuid> = store
        .list_cns(Some(CnState::Approved))
        .await?
        .into_iter()
        .map(|c| c.server_uuid)
        .collect();
    let capacities: Vec<_> = store
        .list_cn_capacities()
        .await?
        .into_iter()
        .filter(|c| approved.contains(&c.server_uuid))
        .collect();
    if capacities.is_empty() {
        return Ok(MaterializeReport::default());
    }

    // Query every feed once (grouped by CN), tolerating per-query
    // failures. `None` means "this query errored"; an empty map means
    // "no rows". Failed feeds are coalesced into one warn per pass.
    let mut failed: Vec<&'static str> = Vec::new();
    let mut note = |name: &'static str, errored: bool| {
        if errored {
            failed.push(name);
        }
    };

    let load_5m = query_load(ch, Window::FiveMin).await;
    note("load_5m", load_5m.is_none());
    let load_1d = query_load(ch, Window::OneDay).await;
    note("load_1d", load_1d.is_none());
    let ram_5m = query_gauge_p95(ch, schemas::MEM_PER_CN, series::USED, Window::FiveMin).await;
    note("ram_5m", ram_5m.is_none());
    let ram_1d = query_gauge_p95(ch, schemas::MEM_PER_CN, series::USED, Window::OneDay).await;
    note("ram_1d", ram_1d.is_none());
    let net_tx_5m = query_counter_rate_p95(ch, series::TX_BYTES, Window::FiveMin).await;
    note("net_tx_5m", net_tx_5m.is_none());
    let net_tx_1d = query_counter_rate_p95(ch, series::TX_BYTES, Window::OneDay).await;
    note("net_tx_1d", net_tx_1d.is_none());
    let net_rx_5m = query_counter_rate_p95(ch, series::RX_BYTES, Window::FiveMin).await;
    note("net_rx_5m", net_rx_5m.is_none());
    let net_rx_1d = query_counter_rate_p95(ch, series::RX_BYTES, Window::OneDay).await;
    note("net_rx_1d", net_rx_1d.is_none());
    let disk_5m = query_disk_p95(ch, Window::FiveMin).await;
    note("disk_5m", disk_5m.is_none());
    let disk_1d = query_disk_p95(ch, Window::OneDay).await;
    note("disk_1d", disk_1d.is_none());

    if !failed.is_empty() {
        warn!(
            feeds = ?failed,
            "load materializer: clickhouse query failures this pass \
             (cpu/ram failures mark rows stale; net/disk degrade to zeros)",
        );
    }

    // If a CPU or RAM query errored outright, every row that depends on
    // it is stale regardless of per-CN sample counts (decision 3a).
    // Net/disk errors degrade to zeros and never gate staleness.
    let query_errored =
        load_5m.is_none() || load_1d.is_none() || ram_5m.is_none() || ram_1d.is_none();

    let net_tx_5m = net_tx_5m.unwrap_or_default();
    let net_tx_1d = net_tx_1d.unwrap_or_default();
    let net_rx_5m = net_rx_5m.unwrap_or_default();
    let net_rx_1d = net_rx_1d.unwrap_or_default();
    let disk_5m = disk_5m.unwrap_or_default();
    let disk_1d = disk_1d.unwrap_or_default();

    let now = Utc::now();
    let mut report = MaterializeReport::default();

    for cap in &capacities {
        let cn = cap.server_uuid;
        // Enumerating from capacity rows means the core count is
        // always at hand for CPU normalization.
        let cn_cores = cap.cpu_threads_logical.max(1);

        let l5 = lookup_load(&load_5m, cn);
        let l1 = lookup_load(&load_1d, cn);

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

        let stale = query_errored || n_5m < cfg.min_samples_5m || n_1d < cfg.min_samples_1d;

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
/// nil-CN. Failure is logged at warn (rate-bounded to once per pass)
/// but must not break the roll-up. NB: with the default deployment the
/// metrics store shares the materializer's ClickHouse endpoint, so the
/// stale-rows gauge cannot be recorded during a whole-CH outage -- the
/// coalesced query warn is the durable signal there.
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
        warn!(error = %e, "load materializer: metrics emit failed (non-fatal)");
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
/// Returns `None` on a transport/HTTP query error (-> the caller
/// forces stale). Unparsable body lines are logged + skipped; a
/// garbage body therefore degrades to an empty map, whose per-CN
/// `n = 0` trips the sample floor instead.
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
    let body = run_query(ch, sql, "load_per_cn").await?;
    Some(parse_load_body(&body))
}

fn parse_load_body(body: &str) -> BTreeMap<Uuid, LoadRow> {
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
    let mut out = BTreeMap::new();
    for raw in parse_rows::<Raw>(body) {
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
    out
}

/// Roll up a `GaugeU64` schema's p95 (+ sample count) per CN for one
/// `series` value. Used for RAM (`mem_per_cn`/`used`). `None` on a
/// transport/HTTP query error.
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
    let body = run_query(ch, sql, schema).await?;
    Some(parse_gauge_body(&body))
}

fn parse_gauge_body(body: &str) -> BTreeMap<Uuid, (f64, u32)> {
    #[derive(Deserialize)]
    struct Raw {
        cn: String,
        #[serde(default)]
        p95: serde_json::Value,
        #[serde(default)]
        n: serde_json::Value,
    }
    let mut out = BTreeMap::new();
    for raw in parse_rows::<Raw>(body) {
        let Some(cn) = parse_cn(&raw.cn) else {
            continue;
        };
        out.insert(cn, (as_f64(&raw.p95), as_u32(&raw.n)));
    }
    out
}

/// Roll up a `CumulativeU64` counter's per-sample rate p95 (bytes/sec)
/// per CN. Used for NIC tx/rx (`net_per_cn`). The counter is converted
/// to a rate inside ClickHouse with a window `lagInFrame` partitioned
/// by series key and ordered by time. Two row classes are dropped
/// before the quantile: negative deltas (counter resets) and each
/// partition's first row, where `lagInFrame` returns the column type's
/// *default* (0 / epoch) rather than NULL and would otherwise
/// fabricate a small positive `counter / seconds-since-1970` sample on
/// every pass.
///
/// Returns `None` on a transport/HTTP query error; the caller treats
/// that as zeros, because net/disk never gate staleness.
async fn query_counter_rate_p95(
    ch: &ClickHouseStore,
    series: &str,
    window: Window,
) -> Option<BTreeMap<Uuid, (f64, u32)>> {
    let sql = format!(
        "SELECT cn, quantile(0.95)(bps) AS p95, count() AS n FROM ( \
            SELECT JSONExtractString(fields_json, 'cn_id') AS cn, \
                   row_number() OVER w AS rn, \
                   (toFloat64(datum) - lagInFrame(toFloat64(datum)) OVER w) \
                     / greatest(1e-9, date_diff('second', lagInFrame(timestamp) OVER w, timestamp)) AS bps \
            FROM tritond_metrics.measurements_cumulative_u64 \
            WHERE timeseries_name = '{schema}' \
              AND JSONExtractString(fields_json, 'series') = '{series}' \
              AND {pred} \
            WINDOW w AS (PARTITION BY timeseries_key ORDER BY timestamp) \
         ) \
         WHERE bps >= 0 AND rn > 1 \
         GROUP BY cn \
         FORMAT JSONEachRow",
        schema = schemas::NET_PER_CN,
        pred = window.predicate(),
    );
    let body = run_query(ch, sql, schemas::NET_PER_CN).await?;
    Some(parse_gauge_body(&body))
}

/// Roll up `triton.zpool_capacity_per_cn` series `alloc` p95 bytes per
/// (CN, pool). Returns `None` on a transport/HTTP query error; the
/// caller treats that as zeros (never gates staleness).
async fn query_disk_p95(
    ch: &ClickHouseStore,
    window: Window,
) -> Option<BTreeMap<Uuid, BTreeMap<String, u64>>> {
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
    let body = run_query(ch, sql, schemas::ZPOOL_CAPACITY_PER_CN).await?;
    Some(parse_disk_body(&body))
}

fn parse_disk_body(body: &str) -> BTreeMap<Uuid, BTreeMap<String, u64>> {
    #[derive(Deserialize)]
    struct Raw {
        cn: String,
        #[serde(default)]
        pool: String,
        #[serde(default)]
        p95: serde_json::Value,
    }
    let mut out: BTreeMap<Uuid, BTreeMap<String, u64>> = BTreeMap::new();
    for raw in parse_rows::<Raw>(body) {
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

/// Issue the SQL; a transport/HTTP failure logs at debug (the caller
/// coalesces failed feeds into one warn per pass) and yields `None`.
async fn run_query(ch: &ClickHouseStore, sql: String, what: &str) -> Option<String> {
    match ch.query_jsoneachrow(sql).await {
        Ok(body) => Some(body),
        Err(e) => {
            debug!(feed = what, error = %e, "load materializer: clickhouse query failed");
            None
        }
    }
}

/// Parse a JSONEachRow body (one JSON object per line) into rows.
/// Blank lines are expected; non-empty lines that fail to deserialize
/// (e.g. a trailing exception ClickHouse appended to a 200 body) are
/// counted + logged rather than silently dropped, then skipped so one
/// bad line does not fail the whole pass.
fn parse_rows<T: for<'de> Deserialize<'de>>(body: &str) -> Vec<T> {
    let mut bad = 0usize;
    let mut first_bad: Option<&str> = None;
    let rows = body
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| match serde_json::from_str::<T>(l) {
            Ok(row) => Some(row),
            Err(_) => {
                bad += 1;
                first_bad.get_or_insert(l);
                None
            }
        })
        .collect();
    if bad > 0 {
        warn!(
            bad,
            first = first_bad.unwrap_or_default(),
            "load materializer: skipped unparsable clickhouse rows",
        );
    }
    rows
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

/// Net rate p95 (bytes/sec) for a CN; a missing entry is zero.
fn lookup_rate(map: &BTreeMap<Uuid, (f64, u32)>, cn: Uuid) -> f64 {
    map.get(&cn).map(|(p95, _)| *p95).unwrap_or(0.0)
}

fn lookup_disk(map: &BTreeMap<Uuid, BTreeMap<String, u64>>, cn: Uuid) -> BTreeMap<String, u64> {
    map.get(&cn).cloned().unwrap_or_default()
}

/// CPU busy fraction = load / cores, clamped to `[0, 1]`. A missing
/// load row yields zeros (the caller's sample floor marks it stale).
fn cpu_fraction(load: Option<LoadRow>, cores: u32) -> (f32, f32, f32) {
    match load {
        Some(l) => {
            let c = f64::from(cores.max(1));
            let f = |v: f64| ((v / c).clamp(0.0, 1.0)) as f32;
            (f(l.p50), f(l.p95), f(l.max))
        }
        None => (0.0, 0.0, 0.0),
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
        let (p50, p95, mx) = cpu_fraction(Some(l), 8);
        assert!((p50 - 0.5).abs() < 1e-6);
        assert!((p95 - 1.0).abs() < 1e-6);
        assert!((mx - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cpu_fraction_missing_load_is_zero() {
        assert_eq!(cpu_fraction(None, 8), (0.0, 0.0, 0.0));
        // Zero cores is defensively bumped to 1.
        let l = LoadRow {
            p50: 0.5,
            p95: 0.5,
            max: 0.5,
            n: 1,
        };
        let (p50, _, _) = cpu_fraction(Some(l), 0);
        assert!((p50 - 0.5).abs() < 1e-6);
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

    // The parse halves are pure; pin them against realistic
    // JSONEachRow bodies, including ClickHouse's default quoting of
    // 64-bit integers as strings.
    #[test]
    fn parse_load_body_handles_string_typed_numbers() {
        let cn = Uuid::new_v4();
        let body =
            format!("{{\"cn\":\"{cn}\",\"p50\":1.5,\"p95\":\"3.25\",\"mx\":4.0,\"n\":\"21\"}}\n");
        let map = parse_load_body(&body);
        let row = map.get(&cn).expect("cn parsed");
        assert!((row.p50 - 1.5).abs() < 1e-9);
        assert!((row.p95 - 3.25).abs() < 1e-9);
        assert_eq!(row.n, 21);
    }

    #[test]
    fn parse_gauge_body_multi_cn() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let body = format!(
            "{{\"cn\":\"{a}\",\"p95\":\"8589934592\",\"n\":20}}\n\
             {{\"cn\":\"{b}\",\"p95\":1024.0,\"n\":\"3\"}}\n\
             {{\"cn\":\"not-a-uuid\",\"p95\":1,\"n\":1}}\n"
        );
        let map = parse_gauge_body(&body);
        assert_eq!(map.len(), 2);
        assert!((map[&a].0 - 8_589_934_592.0).abs() < 1.0);
        assert_eq!(map[&a].1, 20);
        assert_eq!(map[&b].1, 3);
    }

    #[test]
    fn parse_disk_body_groups_per_pool() {
        let cn = Uuid::new_v4();
        let body = format!(
            "{{\"cn\":\"{cn}\",\"pool\":\"zones\",\"p95\":\"1000\"}}\n\
             {{\"cn\":\"{cn}\",\"pool\":\"tank\",\"p95\":2000.4}}\n\
             {{\"cn\":\"{cn}\",\"pool\":\"\",\"p95\":5}}\n"
        );
        let map = parse_disk_body(&body);
        let pools = map.get(&cn).expect("cn present");
        assert_eq!(pools.len(), 2);
        assert_eq!(pools["zones"], 1000);
        assert_eq!(pools["tank"], 2000);
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
        // The materializer only writes rows for Approved CNs, so the
        // fixture must register + approve, not just publish capacity.
        let cn = Uuid::new_v4();
        store
            .register_cn(cn, "cn-lm".into(), None, serde_json::json!({}), Utc::now())
            .await
            .unwrap();
        store
            .approve_cn(
                cn,
                Uuid::new_v4(),
                "pwd".into(),
                [0u8; 32],
                [0u8; 32],
                [0u8; 32],
                Utc::now(),
            )
            .await
            .unwrap();
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

    // A CN with a capacity row but no Approved state must be skipped
    // entirely -- a retired CN would otherwise floor the stale_rows
    // gauge above zero forever.
    #[tokio::test]
    async fn unapproved_cn_gets_no_row() {
        use tritond_store::{CnCapacity, MemStore, UnderlayCapability};

        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

        let store = MemStore::new();
        let cn = Uuid::new_v4();
        // Registered (Pending) but never approved.
        store
            .register_cn(
                cn,
                "cn-pending".into(),
                None,
                serde_json::json!({}),
                Utc::now(),
            )
            .await
            .unwrap();
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

        let ch = ClickHouseStore::new("http://127.0.0.1:1".to_string()).unwrap();
        let cfg = LoadMaterializerConfig {
            interval: Duration::from_secs(60),
            min_samples_5m: 3,
            min_samples_1d: 12,
            clickhouse_url: "http://127.0.0.1:1".to_string(),
        };

        let report = materialize_once(&store, &ch, &cfg).await.unwrap();
        assert_eq!(report.rows_written, 0);
        assert_eq!(report.stale_rows, 0);
        assert!(store.get_cn_load_summary(cn).await.unwrap().is_none());
    }
}
