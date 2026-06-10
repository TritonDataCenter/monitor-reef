// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! ClickHouse-backed [`MetricsStore`].
//!
//! Gated behind the `clickhouse` cargo feature so the default
//! `tritond-metrics` build (used by the agent + the generated client)
//! stays dep-light. The `tritond` crate turns the feature on.
//!
//! Talks to ClickHouse over the HTTP interface (port 8123) -- no
//! native-protocol dependency, just `reqwest`. Writes go to one of
//! three per-datum-type MergeTree tables (`measurements_cumulative_u64`,
//! `measurements_gauge_f64`, `measurements_gauge_u64`), plus an upsert
//! into the `timeseries` ReplacingMergeTree index so the Prometheus
//! exposition path can map a `timeseries_key` back to its labelset
//! without decoding `fields_json` off the hot tables.
//!
//! Range queries fetch the raw rows in the requested window and reuse
//! the in-memory store's [`bucket_samples`] so the UI sees identical
//! bucketing whichever backend is configured.
//!
//! Schema: see `monitor-reef/dev/metrics/init/01_schema.sql`.

use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};
use std::time::Duration as StdDuration;

use chrono::{DateTime, Utc};
use reqwest::Client as HttpClient;

use super::ring::bucket_samples;
use super::{
    MetricsHealth, MetricsStore, MetricsStoreError, RangeQuery, RangeResult, SeriesPoints,
};
use crate::sample::{Datum, Sample, SampleIdentity};
use crate::schema::SchemaName;

/// Database the schema lives in.
const DATABASE: &str = "tritond_metrics";

/// HTTP request timeout. Inserts are tiny; the slowest call is a
/// 30-day range scan, which ClickHouse handles in well under this.
const HTTP_TIMEOUT: StdDuration = StdDuration::from_secs(20);

/// ClickHouse-backed metrics store.
pub struct ClickHouseStore {
    /// Base URL, e.g. `http://10.199.199.75:8123` (no trailing slash).
    base_url: String,
    http: HttpClient,
}

impl ClickHouseStore {
    /// Build a store pointing at the given ClickHouse HTTP endpoint.
    /// Does not validate connectivity -- the first `insert`/`tail`
    /// surfaces a [`MetricsStoreError::Unavailable`] if the server is
    /// down.
    pub fn new(base_url: impl Into<String>) -> Result<Self, MetricsStoreError> {
        // On SmartOS GZs reqwest's default `ClientBuilder::build()`
        // fails arming its TLS connector (no system CA bundle, and
        // the rustls provider dance is finicky). ClickHouse here is
        // plain HTTP, so hand it a minimal preconfigured rustls
        // config -- mirrors what tritonagent does for the same
        // reason. `ClientConfig::builder()` uses the process-default
        // crypto provider, which tritond's `main` installs at
        // startup.
        let tls = rustls::ClientConfig::builder()
            .with_root_certificates(rustls::RootCertStore::empty())
            .with_no_client_auth();
        let http = HttpClient::builder()
            .timeout(HTTP_TIMEOUT)
            .use_preconfigured_tls(tls)
            .build()
            .map_err(|e| {
                MetricsStoreError::Unavailable(format!("build clickhouse http client: {e} ({e:?})"))
            })?;
        Ok(Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            http,
        })
    }

    /// POST a query (and optional inline data) to ClickHouse, return
    /// the response body. Errors on a non-2xx status.
    async fn post(&self, body: String) -> Result<String, MetricsStoreError> {
        self.post_to(self.base_url.clone(), body).await
    }

    async fn post_to(&self, url: String, body: String) -> Result<String, MetricsStoreError> {
        let resp = self
            .http
            .post(&url)
            .body(body)
            .send()
            .await
            .map_err(|e| MetricsStoreError::Unavailable(format!("clickhouse request: {e}")))?;
        let status = resp.status();
        let text = resp.text().await.map_err(|e| {
            MetricsStoreError::Unavailable(format!("clickhouse response body: {e}"))
        })?;
        if !status.is_success() {
            return Err(MetricsStoreError::Unavailable(format!(
                "clickhouse {status}: {}",
                text.trim()
            )));
        }
        Ok(text)
    }

    /// Run an aggregation query and return the raw response body.
    ///
    /// The caller composes the SQL (typically ending in `FORMAT
    /// JSONEachRow`) and parses the body; this is just the transport.
    /// Used by the placement load-materializer (RFD 00005 PL-6), whose
    /// roll-up SQL has no home on the `MetricsStore` trait. Public
    /// because [`post`](Self::post) is private to the module.
    ///
    /// `wait_end_of_query=1` makes ClickHouse buffer the full result
    /// before sending headers, so a mid-stream query failure surfaces
    /// as a non-2xx status instead of an exception line appended to a
    /// 200 body that the row parser would silently skip.
    pub async fn query_jsoneachrow(&self, sql: String) -> Result<String, MetricsStoreError> {
        self.post_to(format!("{}/?wait_end_of_query=1", self.base_url), sql)
            .await
    }

    /// Run the embedded schema migrations (idempotent). Call once at
    /// startup; safe to call repeatedly.
    pub async fn ensure_schema(&self) -> Result<(), MetricsStoreError> {
        for stmt in SCHEMA_DDL {
            self.post(stmt.to_string()).await?;
        }
        Ok(())
    }
}

/// DDL applied by [`ClickHouseStore::ensure_schema`]. Mirrors
/// `monitor-reef/dev/metrics/init/01_schema.sql` so a fresh tritond
/// against an empty ClickHouse self-bootstraps.
const SCHEMA_DDL: &[&str] = &[
    "CREATE DATABASE IF NOT EXISTS tritond_metrics",
    "CREATE TABLE IF NOT EXISTS tritond_metrics.measurements_cumulative_u64 (timeseries_name LowCardinality(String), timeseries_key UInt64, timestamp DateTime64(9, 'UTC'), fields_json String, datum UInt64) ENGINE = MergeTree() ORDER BY (timeseries_name, timeseries_key, timestamp) TTL toDateTime(timestamp) + INTERVAL 30 DAY",
    "CREATE TABLE IF NOT EXISTS tritond_metrics.measurements_gauge_f64 (timeseries_name LowCardinality(String), timeseries_key UInt64, timestamp DateTime64(9, 'UTC'), fields_json String, datum Float64) ENGINE = MergeTree() ORDER BY (timeseries_name, timeseries_key, timestamp) TTL toDateTime(timestamp) + INTERVAL 30 DAY",
    "CREATE TABLE IF NOT EXISTS tritond_metrics.measurements_gauge_u64 (timeseries_name LowCardinality(String), timeseries_key UInt64, timestamp DateTime64(9, 'UTC'), fields_json String, datum UInt64) ENGINE = MergeTree() ORDER BY (timeseries_name, timeseries_key, timestamp) TTL toDateTime(timestamp) + INTERVAL 30 DAY",
    "CREATE TABLE IF NOT EXISTS tritond_metrics.timeseries (timeseries_name LowCardinality(String), timeseries_key UInt64, cn_id UUID, tenant_id Nullable(UUID), project_id Nullable(UUID), instance_id Nullable(UUID), series LowCardinality(String), first_seen DateTime64(9, 'UTC')) ENGINE = ReplacingMergeTree(first_seen) ORDER BY (timeseries_name, timeseries_key)",
];

#[async_trait::async_trait]
impl MetricsStore for ClickHouseStore {
    async fn insert(&self, samples: &[Sample]) -> Result<(), MetricsStoreError> {
        if samples.is_empty() {
            return Ok(());
        }

        // Bucket rows by destination measurement table.
        let mut measurement_rows: BTreeMap<&'static str, Vec<String>> = BTreeMap::new();
        // Dedup timeseries-index rows by key within this batch.
        let mut index_rows: BTreeMap<u64, String> = BTreeMap::new();

        for s in samples {
            let table = datum_table(&s.datum);
            let key = timeseries_key(s.schema.as_str(), &s.identity);
            let fields_json =
                serde_json::to_string(&s.identity).unwrap_or_else(|_| "{}".to_string());
            let ts = clickhouse_timestamp(&s.timestamp);
            let datum_value = datum_json(&s.datum);

            let row = serde_json::json!({
                "timeseries_name": s.schema.as_str(),
                "timeseries_key": key,
                "timestamp": ts,
                "fields_json": fields_json,
                "datum": datum_value,
            });
            measurement_rows
                .entry(table)
                .or_default()
                .push(row.to_string());

            index_rows.entry(key).or_insert_with(|| {
                serde_json::json!({
                    "timeseries_name": s.schema.as_str(),
                    "timeseries_key": key,
                    "cn_id": s.identity.cn_id.to_string(),
                    "tenant_id": s.identity.tenant_id.map(|u| u.to_string()),
                    "project_id": s.identity.project_id.map(|u| u.to_string()),
                    "instance_id": s.identity.instance_id.map(|u| u.to_string()),
                    "series": s.identity.series.clone().unwrap_or_default(),
                    "first_seen": ts.clone(),
                })
                .to_string()
            });
        }

        // One INSERT per measurement table.
        for (table, rows) in measurement_rows {
            let mut body = format!(
                "INSERT INTO {DATABASE}.{table} (timeseries_name, timeseries_key, timestamp, fields_json, datum) FORMAT JSONEachRow\n"
            );
            body.push_str(&rows.join("\n"));
            self.post(body).await?;
        }

        // One INSERT for the timeseries index. ReplacingMergeTree
        // dedups on merge, so re-inserting known keys is harmless.
        if !index_rows.is_empty() {
            let mut body = format!(
                "INSERT INTO {DATABASE}.timeseries (timeseries_name, timeseries_key, cn_id, tenant_id, project_id, instance_id, series, first_seen) FORMAT JSONEachRow\n"
            );
            body.push_str(&index_rows.into_values().collect::<Vec<_>>().join("\n"));
            self.post(body).await?;
        }

        Ok(())
    }

    async fn query_range(&self, q: &RangeQuery) -> Result<RangeResult, MetricsStoreError> {
        q.validate()?;
        let schema = sanitize_ident(&q.schema)?;
        let table = schema_table(&schema);
        let since = clickhouse_timestamp(&q.since);
        let until = clickhouse_timestamp(&q.until);

        let mut sql = format!(
            "SELECT fields_json, toString(timestamp) AS ts, toFloat64(datum) AS v FROM {DATABASE}.{table} WHERE timeseries_name = '{schema}' AND timestamp >= toDateTime64('{since}', 9, 'UTC') AND timestamp <= toDateTime64('{until}', 9, 'UTC')"
        );
        if let Some(inst) = q.instance_id {
            sql.push_str(&format!(
                " AND JSONExtractString(fields_json, 'instance_id') = '{inst}'"
            ));
        }
        if let Some(tenant) = q.tenant_id {
            sql.push_str(&format!(
                " AND JSONExtractString(fields_json, 'tenant_id') = '{tenant}'"
            ));
        }
        if let Some(cn) = q.cn_id {
            sql.push_str(&format!(
                " AND JSONExtractString(fields_json, 'cn_id') = '{cn}'"
            ));
        }
        if let Some(dev) = &q.device {
            let dev = sanitize_ident(dev)?;
            sql.push_str(&format!(
                " AND JSONExtractString(fields_json, 'device') = '{dev}'"
            ));
        }
        sql.push_str(" ORDER BY timestamp ASC FORMAT JSONEachRow");

        let body = self.post(sql).await?;

        // Reconstruct Samples grouped by series, then bucket.
        let is_cumulative = table == "measurements_cumulative_u64";
        let mut grouped: BTreeMap<String, Vec<Sample>> = BTreeMap::new();
        for line in body.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let row: ChRow = match serde_json::from_str(line) {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(error = %e, line, "clickhouse: skip unparsable row");
                    continue;
                }
            };
            let ident: SampleIdentity =
                serde_json::from_str(&row.fields_json).unwrap_or(SampleIdentity {
                    cn_id: uuid::Uuid::nil(),
                    tenant_id: None,
                    project_id: None,
                    instance_id: None,
                    series: None,
                    device: None,
                });
            let series = ident.series.clone().unwrap_or_default();
            let timestamp = parse_ch_timestamp(&row.ts).unwrap_or_else(Utc::now);
            let datum = if is_cumulative {
                Datum::CumulativeU64 {
                    value: row.v.max(0.0) as u64,
                }
            } else {
                Datum::GaugeF64 { value: row.v }
            };
            grouped.entry(series).or_default().push(Sample {
                schema: SchemaName::new(schema.clone()),
                identity: ident,
                timestamp,
                datum,
            });
        }

        let mut series_out: Vec<SeriesPoints> = Vec::with_capacity(grouped.len());
        for (name, mut samples) in grouped {
            samples.sort_by_key(|s| s.timestamp);
            let refs: Vec<&Sample> = samples.iter().collect();
            let points = bucket_samples(&refs, q.since, q.until, q.step);
            series_out.push(SeriesPoints { name, points });
        }

        Ok(RangeResult {
            schema,
            since: q.since,
            until: q.until,
            step_seconds: q.step.num_seconds().max(1) as u64,
            series: series_out,
        })
    }

    async fn latest_for_schema(&self, schema: &str) -> Result<Vec<Sample>, MetricsStoreError> {
        let schema = sanitize_ident(schema)?;
        let table = schema_table(&schema);
        let is_cumulative = table == "measurements_cumulative_u64";
        let sql = format!(
            "SELECT fields_json, toString(max(timestamp)) AS ts, toFloat64(argMax(datum, timestamp)) AS v FROM {DATABASE}.{table} WHERE timeseries_name = '{schema}' GROUP BY timeseries_key, fields_json FORMAT JSONEachRow"
        );
        let body = self.post(sql).await?;

        let mut out = Vec::new();
        for line in body.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let row: ChRow = match serde_json::from_str(line) {
                Ok(r) => r,
                Err(_) => continue,
            };
            let ident: SampleIdentity = match serde_json::from_str(&row.fields_json) {
                Ok(i) => i,
                Err(_) => continue,
            };
            let timestamp = parse_ch_timestamp(&row.ts).unwrap_or_else(Utc::now);
            let datum = if is_cumulative {
                Datum::CumulativeU64 {
                    value: row.v.max(0.0) as u64,
                }
            } else {
                Datum::GaugeF64 { value: row.v }
            };
            out.push(Sample {
                schema: SchemaName::new(schema.clone()),
                identity: ident,
                timestamp,
                datum,
            });
        }
        Ok(out)
    }

    async fn health(&self) -> MetricsHealth {
        // Cheap liveness probe -- `SELECT 1` round-trips without
        // touching the measurement tables.
        let (reachable, detail) = match self.post("SELECT 1".to_string()).await {
            Ok(_) => (true, None),
            Err(e) => (false, Some(e.to_string())),
        };
        MetricsHealth {
            backend: "clickhouse".to_string(),
            endpoint: Some(self.base_url.clone()),
            reachable,
            detail,
        }
    }
}

#[derive(serde::Deserialize)]
struct ChRow {
    fields_json: String,
    ts: String,
    v: f64,
}

/// Which measurement table a datum lands in.
fn datum_table(d: &Datum) -> &'static str {
    match d {
        Datum::CumulativeU64 { .. } => "measurements_cumulative_u64",
        Datum::GaugeF64 { .. } => "measurements_gauge_f64",
        Datum::GaugeU64 { .. } => "measurements_gauge_u64",
    }
}

/// Which measurement table to read for a schema. Counters (CPU ns,
/// disk/net bytes) live in `measurements_cumulative_u64`; integer
/// gauges (memory bytes, socket counts) in `measurements_gauge_u64`;
/// float gauges (load average) in `measurements_gauge_f64`. Unknown
/// schemas default to the float-gauge table. Keep in sync with the
/// datum types the agent emits in `tritonagent::metrics`.
fn schema_table(schema: &str) -> &'static str {
    use crate::schema::schemas as s;
    match schema {
        x if x == s::CPU_PER_ZONE
            || x == s::CPU_PER_CN
            || x == s::DISK_PER_ZONE
            || x == s::DISK_PER_CN
            || x == s::NET_PER_ZONE
            || x == s::NET_PER_CN
            || x == s::DISK_IOSTAT_PER_CN
            || x == s::ZFS_ARC_PER_CN =>
        {
            "measurements_cumulative_u64"
        }
        x if x == s::MEM_PER_ZONE
            || x == s::MEM_PER_CN
            || x == s::SOCKETS_PER_ZONE
            || x == s::SOCKETS_PER_CN
            || x == s::DISK_LATENCY_PER_CN
            || x == s::ZFS_ARC_SIZE_PER_CN
            || x == s::ZPOOL_CAPACITY_PER_CN
            || x == s::DISK_TEMP_PER_CN
            || x == s::PLACEMENT_LOAD_SUMMARY_STALE_ROWS =>
        {
            "measurements_gauge_u64"
        }
        x if x == s::LOAD_PER_CN
            || x == s::DISK_BUSY_PER_CN
            || x == s::PLACEMENT_LOAD_MATERIALIZER_SECONDS =>
        {
            "measurements_gauge_f64"
        }
        _ => "measurements_gauge_f64",
    }
}

/// JSON value for the `datum` column, typed to match the table.
fn datum_json(d: &Datum) -> serde_json::Value {
    match *d {
        Datum::CumulativeU64 { value } => serde_json::Value::Number(value.into()),
        Datum::GaugeU64 { value } => serde_json::Value::Number(value.into()),
        Datum::GaugeF64 { value } => serde_json::Number::from_f64(value)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Number(0.into())),
    }
}

/// Stable-ish hash of the timeseries identity. SipHash with the std
/// default key (which is fixed at `(0, 0)`), so it's deterministic
/// within and across runs of the same std version. A std upgrade
/// could change the algorithm -- worst case the index gains a
/// duplicate row for a series, which is cosmetic, not a data loss.
fn timeseries_key(schema: &str, ident: &SampleIdentity) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    schema.hash(&mut h);
    ident.cn_id.hash(&mut h);
    ident.tenant_id.hash(&mut h);
    ident.project_id.hash(&mut h);
    ident.instance_id.hash(&mut h);
    ident.series.hash(&mut h);
    ident.device.hash(&mut h);
    h.finish()
}

/// Format a `DateTime<Utc>` the way ClickHouse's `toDateTime64(...)`
/// parser likes: `YYYY-MM-DD HH:MM:SS.nnnnnnnnnn`.
fn clickhouse_timestamp(t: &DateTime<Utc>) -> String {
    t.format("%Y-%m-%d %H:%M:%S%.9f").to_string()
}

/// Parse a `toString(timestamp)` value back. ClickHouse prints
/// `DateTime64(9)` as `YYYY-MM-DD HH:MM:SS.nnnnnnnnn`.
fn parse_ch_timestamp(s: &str) -> Option<DateTime<Utc>> {
    chrono::NaiveDateTime::parse_from_str(s.trim(), "%Y-%m-%d %H:%M:%S%.f")
        .ok()
        .map(|ndt| ndt.and_utc())
}

/// Reject anything that isn't a bare identifier so it can't break out
/// of a single-quoted SQL literal. Schema names and the like are all
/// `[a-zA-Z0-9._-]+`; UUIDs come through as typed `Uuid`s and don't
/// pass through here.
fn sanitize_ident(s: &str) -> Result<String, MetricsStoreError> {
    if s.is_empty()
        || !s
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        return Err(MetricsStoreError::InvalidQuery(format!(
            "invalid identifier: {s:?}"
        )));
    }
    Ok(s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn timestamp_round_trips() {
        let t = chrono::DateTime::parse_from_rfc3339("2026-05-11T12:34:56.789012345Z")
            .unwrap()
            .with_timezone(&Utc);
        let s = clickhouse_timestamp(&t);
        assert_eq!(s, "2026-05-11 12:34:56.789012345");
        let back = parse_ch_timestamp(&s).expect("parse");
        // Round-trips to at least microsecond precision (chrono's
        // %.f handles up to nanos).
        assert_eq!(back.timestamp(), t.timestamp());
    }

    #[test]
    fn sanitize_rejects_quotes() {
        assert!(sanitize_ident("triton.cpu_per_zone").is_ok());
        assert!(sanitize_ident("a-b_c.d").is_ok());
        assert!(sanitize_ident("x'; DROP TABLE y--").is_err());
        assert!(sanitize_ident("").is_err());
    }

    #[test]
    fn datum_json_is_typed() {
        assert_eq!(
            datum_json(&Datum::CumulativeU64 { value: 42 }).to_string(),
            "42"
        );
        assert_eq!(datum_json(&Datum::GaugeU64 { value: 7 }).to_string(), "7");
        assert_eq!(
            datum_json(&Datum::GaugeF64 { value: 1.5 }).to_string(),
            "1.5"
        );
    }

    #[test]
    fn timeseries_key_is_deterministic() {
        let id = SampleIdentity {
            cn_id: Uuid::nil(),
            tenant_id: None,
            project_id: None,
            instance_id: Some(Uuid::nil()),
            series: Some("user".into()),
            device: None,
        };
        let a = timeseries_key("triton.cpu_per_zone", &id);
        let b = timeseries_key("triton.cpu_per_zone", &id);
        assert_eq!(a, b);
        // Different series → different key.
        let id2 = SampleIdentity {
            series: Some("system".into()),
            ..id.clone()
        };
        assert_ne!(timeseries_key("triton.cpu_per_zone", &id2), a);
    }

    #[test]
    fn datum_table_routing() {
        assert_eq!(
            datum_table(&Datum::CumulativeU64 { value: 1 }),
            "measurements_cumulative_u64"
        );
        assert_eq!(
            datum_table(&Datum::GaugeF64 { value: 1.0 }),
            "measurements_gauge_f64"
        );
        assert_eq!(
            datum_table(&Datum::GaugeU64 { value: 1 }),
            "measurements_gauge_u64"
        );
        assert_eq!(
            schema_table("triton.cpu_per_zone"),
            "measurements_cumulative_u64"
        );
        assert_eq!(
            schema_table("triton.something_else"),
            "measurements_gauge_f64"
        );
    }
}
