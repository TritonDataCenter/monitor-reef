// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Schema-versioned timeseries primitives shared by tritond and
//! tritonagent.
//!
//! The data model borrows oxide/oximeter's vocabulary -- a `Sample`
//! carries a schema name, identity fields (tenant / project / instance
//! / cn / series), a timestamp, and a single `Datum`. Counters use
//! [`Datum::CumulativeU64`]; gauges use [`Datum::GaugeF64`] or
//! [`Datum::GaugeU64`]. Storage backends consume `Sample`s and answer
//! range queries shaped for the admin UI's multi-series charts.
//!
//! Two storage backends ship with this crate:
//!
//! * [`store::RingBufferStore`] -- in-memory ring keeping the last
//!   `retention` of samples per `(schema, instance)`. Default for dev /
//!   tests; loses data on restart. Cheap, zero deps.
//! * (planned) `ClickHouseStore` -- production sink writing to
//!   per-datum-type MergeTree tables. Lives in a sibling crate so this
//!   one stays dep-light.

#![forbid(unsafe_code)]

pub mod prometheus;
pub mod sample;
pub mod schema;
pub mod store;

pub use sample::{Datum, Sample, SampleBatch, SampleIdentity};
pub use schema::{SchemaName, cpu_mode, schemas, series};
pub use store::{
    MetricsStore, MetricsStoreError, RangeQuery, RangeResult, SeriesPoint, SeriesPoints,
};
