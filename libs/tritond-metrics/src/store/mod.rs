// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Storage backend trait + reference in-memory implementation.
//!
//! Backends consume `Sample`s and answer two kinds of read:
//!
//! * `query_range` -- bucket the requested `[since, until]` window into
//!   `step`-sized bins, return one numeric value per bin per series.
//!   Counters (`CumulativeU64`) return per-bucket deltas; gauges
//!   return the last sample in the bucket.
//!
//! * `latest_for_schema` -- one sample per `(identity)` pairing, used
//!   by the per-tenant Prometheus exposition where we want the most
//!   recent value irrespective of bucket alignment.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::sample::Sample;

mod ring;
pub use ring::RingBufferStore;

#[cfg(feature = "clickhouse")]
pub mod clickhouse;
#[cfg(feature = "clickhouse")]
pub use clickhouse::ClickHouseStore;

/// All ways a metrics store call can fail.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum MetricsStoreError {
    /// Backend is offline / unreachable / mid-failover.
    #[error("metrics storage unavailable: {0}")]
    Unavailable(String),
    /// Query was structurally invalid (e.g. step <= 0).
    #[error("invalid metrics query: {0}")]
    InvalidQuery(String),
    /// Schema name unknown to the backend's registered set.
    #[error("unknown schema: {0}")]
    UnknownSchema(String),
}

/// Self-description of the live metrics backend, so tritond can answer
/// "what backend am I actually running, where, and is it up?" without
/// the caller needing out-of-band knowledge. Reported by the live
/// `Arc<dyn MetricsStore>`, so it reflects the **effective** backend
/// (e.g. an in-memory fallback after a failed ClickHouse bootstrap),
/// not just the configured intent.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MetricsHealth {
    /// Backend kind: `memory` or `clickhouse`.
    pub backend: String,
    /// Endpoint URL for remote backends; `None` for in-memory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    /// Whether the backend answered a cheap liveness probe.
    pub reachable: bool,
    /// Error detail when `reachable` is false.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Storage backend for `Sample`s. Implemented by [`RingBufferStore`]
/// (in-memory) and `ClickHouseStore`.
#[async_trait::async_trait]
pub trait MetricsStore: Send + Sync {
    /// Insert a batch of samples. Implementations buffer + batch
    /// internally as appropriate; this call does not need to be
    /// durable to satisfy the slot.
    async fn insert(&self, samples: &[Sample]) -> Result<(), MetricsStoreError>;

    /// Bucket-aligned range query. Returns one numeric series per
    /// distinct `series` identity field encountered in the window.
    async fn query_range(&self, q: &RangeQuery) -> Result<RangeResult, MetricsStoreError>;

    /// Most recent sample per `(identity)` pairing for the given
    /// schema. Used by the Prometheus exposition path.
    async fn latest_for_schema(&self, schema: &str) -> Result<Vec<Sample>, MetricsStoreError>;

    /// Describe the live backend + probe its reachability. The default
    /// covers test mocks; real backends override it. Used by tritond's
    /// metrics-status endpoint so clients ask tritond, not ClickHouse.
    async fn health(&self) -> MetricsHealth {
        MetricsHealth {
            backend: "unknown".to_string(),
            endpoint: None,
            reachable: true,
            detail: None,
        }
    }
}

/// Range query parameters. Filters narrow which `Sample`s contribute
/// to the result; missing filters match anything.
#[derive(Debug, Clone)]
pub struct RangeQuery {
    pub schema: String,
    pub instance_id: Option<Uuid>,
    pub tenant_id: Option<Uuid>,
    pub cn_id: Option<Uuid>,
    /// Device/pool filter for storage schemas (matches `identity.device`).
    pub device: Option<String>,
    pub since: DateTime<Utc>,
    pub until: DateTime<Utc>,
    pub step: Duration,
}

impl RangeQuery {
    /// Validate the query before handing it to a backend.
    pub fn validate(&self) -> Result<(), MetricsStoreError> {
        if self.until <= self.since {
            return Err(MetricsStoreError::InvalidQuery(
                "until must be after since".into(),
            ));
        }
        if self.step <= Duration::zero() {
            return Err(MetricsStoreError::InvalidQuery(
                "step must be positive".into(),
            ));
        }
        Ok(())
    }
}

/// Result of a range query. One entry per distinct series label seen
/// in the window (e.g. `user`, `system`, `iowait`).
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RangeResult {
    pub schema: String,
    pub since: DateTime<Utc>,
    pub until: DateTime<Utc>,
    pub step_seconds: u64,
    pub series: Vec<SeriesPoints>,
}

/// One named series of `(timestamp, value)` pairs. Timestamps are
/// bucket-start-aligned; values for cumulative datums are deltas
/// over the bucket interval.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SeriesPoints {
    /// Series label (e.g. `user`, `system`). Empty when the schema
    /// has no `series` identity field.
    pub name: String,
    /// Bucket-aligned points in chronological order. Counters return
    /// per-bucket deltas; gauges return the bucket's last value.
    pub points: Vec<SeriesPoint>,
}

/// One sample on a series. Modeled as a struct rather than a tuple
/// because OpenAPI v3.0 (which the OpenAPI manager emits) does not
/// support fixed-position tuple arrays -- they degrade to
/// `array<oneOf>` and lose the column meaning.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SeriesPoint {
    pub timestamp: DateTime<Utc>,
    pub value: f64,
}
