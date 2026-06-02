// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `RingBufferStore`: lossy in-memory sample store keyed by
//! `(schema, identity)`.
//!
//! Used for dev environments and tests where standing up ClickHouse is
//! overkill. Keeps the most recent samples per series within a
//! retention window, evicts older points lazily on insert. Loses data
//! on restart -- intentional, this is not the production sink.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::Mutex;
use std::time::SystemTime;

use chrono::{DateTime, Duration, Utc};

use super::{
    MetricsHealth, MetricsStore, MetricsStoreError, RangeQuery, RangeResult, SeriesPoint,
    SeriesPoints,
};
use crate::sample::Sample;

/// Default retention -- enough to cover the V5 dashboard's longest
/// time-range button (30d) only when running against a real backend;
/// the ring buffer trades that for low memory and is intended for the
/// 1h..24h common case in dev.
const DEFAULT_RETENTION: Duration = Duration::hours(24);

/// Per-series sample log key. Schemas with no series identity (CN-only)
/// produce one entry with `series == ""`.
type SeriesKey = (String, IdentityKey);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct IdentityKey {
    cn_id: uuid::Uuid,
    tenant_id: Option<uuid::Uuid>,
    project_id: Option<uuid::Uuid>,
    instance_id: Option<uuid::Uuid>,
    series: String,
    device: Option<String>,
}

impl IdentityKey {
    fn from(s: &Sample) -> Self {
        IdentityKey {
            cn_id: s.identity.cn_id,
            tenant_id: s.identity.tenant_id,
            project_id: s.identity.project_id,
            instance_id: s.identity.instance_id,
            series: s.identity.series.clone().unwrap_or_default(),
            device: s.identity.device.clone(),
        }
    }
}

/// In-memory sample store.
pub struct RingBufferStore {
    inner: Mutex<Inner>,
    retention: Duration,
}

struct Inner {
    /// `(schema, identity)` -> chronological series log.
    series: HashMap<SeriesKey, VecDeque<Sample>>,
}

impl RingBufferStore {
    /// New store with the default retention (24h).
    pub fn new() -> Self {
        Self::with_retention(DEFAULT_RETENTION)
    }

    /// New store with an explicit retention window.
    pub fn with_retention(retention: Duration) -> Self {
        Self {
            inner: Mutex::new(Inner {
                series: HashMap::new(),
            }),
            retention,
        }
    }

    fn now() -> DateTime<Utc> {
        // Avoid drifting against the OS clock during long-running
        // tests; chrono::Utc::now reads CLOCK_REALTIME under the hood.
        let now = SystemTime::now();
        DateTime::<Utc>::from(now)
    }

    fn evict_older_than(log: &mut VecDeque<Sample>, cutoff: DateTime<Utc>) {
        while let Some(front) = log.front() {
            if front.timestamp < cutoff {
                log.pop_front();
            } else {
                break;
            }
        }
    }
}

impl Default for RingBufferStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl MetricsStore for RingBufferStore {
    async fn insert(&self, samples: &[Sample]) -> Result<(), MetricsStoreError> {
        let now = Self::now();
        let cutoff = now - self.retention;

        let mut guard = self
            .inner
            .lock()
            .map_err(|_| MetricsStoreError::Unavailable("ring mutex poisoned".into()))?;

        for s in samples {
            let key: SeriesKey = (s.schema.0.clone(), IdentityKey::from(s));
            let log = guard.series.entry(key).or_default();
            log.push_back(s.clone());
            Self::evict_older_than(log, cutoff);
        }
        Ok(())
    }

    async fn query_range(&self, q: &RangeQuery) -> Result<RangeResult, MetricsStoreError> {
        q.validate()?;

        let guard = self
            .inner
            .lock()
            .map_err(|_| MetricsStoreError::Unavailable("ring mutex poisoned".into()))?;

        // Group samples by series label, filtering by the requested
        // identity fields and time window. BTreeMap sorts series names
        // alphabetically, which keeps the JSON deterministic.
        let mut grouped: BTreeMap<String, Vec<&Sample>> = BTreeMap::new();
        for ((schema, ident), log) in guard.series.iter() {
            if schema != &q.schema {
                continue;
            }
            if let Some(want) = q.cn_id
                && ident.cn_id != want
            {
                continue;
            }
            if let Some(want) = q.tenant_id
                && ident.tenant_id != Some(want)
            {
                continue;
            }
            if let Some(want) = q.instance_id
                && ident.instance_id != Some(want)
            {
                continue;
            }
            if let Some(want) = &q.device
                && ident.device.as_deref() != Some(want.as_str())
            {
                continue;
            }
            for s in log.iter() {
                if s.timestamp < q.since || s.timestamp > q.until {
                    continue;
                }
                grouped.entry(ident.series.clone()).or_default().push(s);
            }
        }

        // Each group is now a flat list of samples within [since, until]
        // for one (schema, series) pair (across one or more identities,
        // which we tolerate for the fleet/admin path). Bucket and emit.
        let step = q.step;
        let mut series_out = Vec::with_capacity(grouped.len());
        for (name, mut samples) in grouped {
            samples.sort_by_key(|s| s.timestamp);
            let points = bucket_samples(&samples, q.since, q.until, step);
            series_out.push(SeriesPoints { name, points });
        }

        let step_seconds = step.num_seconds().max(1) as u64;
        Ok(RangeResult {
            schema: q.schema.clone(),
            since: q.since,
            until: q.until,
            step_seconds,
            series: series_out,
        })
    }

    async fn latest_for_schema(&self, schema: &str) -> Result<Vec<Sample>, MetricsStoreError> {
        let guard = self
            .inner
            .lock()
            .map_err(|_| MetricsStoreError::Unavailable("ring mutex poisoned".into()))?;

        let mut out = Vec::new();
        for ((s_schema, _ident), log) in guard.series.iter() {
            if s_schema != schema {
                continue;
            }
            if let Some(last) = log.back() {
                out.push(last.clone());
            }
        }
        Ok(out)
    }

    async fn health(&self) -> MetricsHealth {
        MetricsHealth {
            backend: "memory".to_string(),
            endpoint: None,
            reachable: true,
            detail: None,
        }
    }
}

/// Bucket `samples` (already filtered to `[since, until]`) into
/// `step`-aligned bins. For cumulative datums returns
/// `last - first` per bucket; for gauges returns the last value.
///
/// Buckets that contain no samples are still emitted so the returned
/// series has a stable cadence -- the UI uses these for x-axis tick
/// alignment. Empty cumulative buckets emit `0.0`; empty gauge buckets
/// repeat the previous value (or `0.0` if none seen yet).
///
/// Shared with the ClickHouse backend (see `store::clickhouse`) so the
/// UI sees identical bucketing whichever store is configured.
pub(crate) fn bucket_samples(
    samples: &[&Sample],
    since: DateTime<Utc>,
    until: DateTime<Utc>,
    step: Duration,
) -> Vec<SeriesPoint> {
    let mut out = Vec::new();
    let is_cumulative = samples
        .first()
        .map(|s| s.datum.is_cumulative())
        .unwrap_or(false);

    let mut bucket_start = since;
    let mut idx = 0;
    // For counters: the cumulative value at the end of the previous
    // bucket; deltas read against this so per-bucket emissions give
    // a Prometheus-style rate without requiring two samples per
    // bucket (which the agent's 15s tick frequently doesn't deliver
    // when the dashboard's range bucketing is finer-grained than the
    // emission cadence).
    let mut prev_cumulative: Option<f64> = None;
    let mut last_gauge: Option<f64> = None;

    while bucket_start < until {
        let bucket_end = bucket_start + step;
        let mut bucket: Vec<&Sample> = Vec::new();
        while idx < samples.len() && samples[idx].timestamp < bucket_end {
            bucket.push(samples[idx]);
            idx += 1;
        }

        let value = if is_cumulative {
            let last_now = bucket.last().map(|s| s.datum.as_f64());
            let delta = match (prev_cumulative, last_now) {
                (Some(prev), Some(last)) => (last - prev).max(0.0),
                _ => 0.0,
            };
            if let Some(last) = last_now {
                prev_cumulative = Some(last);
            }
            delta
        } else {
            // Gauge: take the last sample's value as the bucket's
            // representative value; forward-fill from the previous
            // bucket if this one is empty.
            match bucket.last().map(|s| s.datum.as_f64()) {
                Some(v) => {
                    last_gauge = Some(v);
                    v
                }
                None => last_gauge.unwrap_or(0.0),
            }
        };

        out.push(SeriesPoint {
            timestamp: bucket_start,
            value,
        });
        bucket_start = bucket_end;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sample::{Datum, SampleIdentity};
    use crate::schema::SchemaName;
    use uuid::Uuid;

    fn cpu_sample(ts: DateTime<Utc>, cn: Uuid, instance: Uuid, mode: &str, ns: u64) -> Sample {
        Sample {
            schema: SchemaName::from("triton.cpu_per_zone"),
            identity: SampleIdentity {
                cn_id: cn,
                tenant_id: None,
                project_id: None,
                instance_id: Some(instance),
                series: Some(mode.to_string()),
                device: None,
            },
            timestamp: ts,
            datum: Datum::CumulativeU64 { value: ns },
        }
    }

    #[tokio::test]
    async fn cumulative_buckets_emit_deltas() {
        let store = RingBufferStore::new();
        let cn = Uuid::nil();
        let inst = Uuid::nil();
        // Anchor inside the ring's retention window (24h) -- a
        // hardcoded absolute date drifts out of range as the calendar
        // advances and the samples get evicted on insert.
        let t0 = Utc::now() - Duration::hours(1);

        // 4 samples 30s apart, counter advances by 1e9 ns/sample.
        // 30s buckets aligned to t0 give us 3 emitted points; the
        // first bucket has no baseline so emits 0, then each
        // subsequent bucket emits the per-bucket delta of 1e9.
        let samples = vec![
            cpu_sample(t0, cn, inst, "user", 0),
            cpu_sample(t0 + Duration::seconds(30), cn, inst, "user", 1_000_000_000),
            cpu_sample(t0 + Duration::seconds(60), cn, inst, "user", 2_000_000_000),
            cpu_sample(t0 + Duration::seconds(90), cn, inst, "user", 3_000_000_000),
        ];
        store.insert(&samples).await.expect("insert");

        let q = RangeQuery {
            schema: "triton.cpu_per_zone".into(),
            instance_id: Some(inst),
            tenant_id: None,
            cn_id: None,
            device: None,
            since: t0,
            until: t0 + Duration::seconds(120),
            step: Duration::seconds(30),
        };
        let r = store.query_range(&q).await.expect("query");
        assert_eq!(r.series.len(), 1);
        assert_eq!(r.series[0].name, "user");
        let pts = &r.series[0].points;
        assert_eq!(pts.len(), 4, "4 buckets across [t0, t0+120s)");
        // First bucket: no baseline yet → 0.
        assert!((pts[0].value - 0.0).abs() < f64::EPSILON);
        // Subsequent buckets each emit a 1e9 delta.
        for p in &pts[1..] {
            assert!(
                (p.value - 1_000_000_000.0).abs() < 1.0,
                "expected ~1e9 delta, got {}",
                p.value
            );
        }
    }

    #[tokio::test]
    async fn invalid_query_rejected() {
        let store = RingBufferStore::new();
        let now = Utc::now();
        let q = RangeQuery {
            schema: "x".into(),
            instance_id: None,
            tenant_id: None,
            cn_id: None,
            device: None,
            since: now,
            until: now,
            step: Duration::seconds(60),
        };
        let err = store.query_range(&q).await.unwrap_err();
        assert!(matches!(err, MetricsStoreError::InvalidQuery(_)));
    }
}
