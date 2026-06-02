// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `Sample` and friends -- the wire-stable shape that tritonagent posts
//! to tritond's metrics ingest endpoint.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::schema::SchemaName;

/// A single measurement at a single point in time, addressed by the
/// `(schema, identity)` tuple.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Sample {
    /// Schema name (e.g. `triton.cpu_per_zone`).
    pub schema: SchemaName,
    /// Identity fields (CN, tenant, project, instance, series label).
    pub identity: SampleIdentity,
    /// Wall-clock time the sample was taken.
    pub timestamp: DateTime<Utc>,
    /// The measured value.
    pub datum: Datum,
}

/// Identity tuple. Open-coded rather than a `Vec<(String, FieldValue)>`
/// because tritond's dimensions are small and fixed; surfacing them as
/// named fields keeps the wire format obvious to humans reading
/// captures.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SampleIdentity {
    /// CN that produced the sample.
    pub cn_id: Uuid,
    /// Tenant the resource belongs to. `None` for host-only metrics
    /// (e.g. fleet-wide CN samples).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<Uuid>,
    /// Project under that tenant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<Uuid>,
    /// Instance (VM) UUID for VM-scoped samples.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance_id: Option<Uuid>,
    /// Series label inside the schema (e.g. CPU mode, NIC name).
    /// Cardinality stays bounded because each schema fixes its
    /// allowed values.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub series: Option<String>,
    /// Device or pool label for storage samples (e.g. `c1t2d0`, a pool
    /// name). Kept distinct from `series` so a storage schema can carry
    /// both a device *and* a sub-metric (device `c1t2d0`, series
    /// `write_lat`). `None` for non-storage schemas. Cardinality is the
    /// CN's disk/pool count -- bounded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
}

/// Single-value datum. Schemas state which variant they emit; storage
/// backends route each variant to the right physical table.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Datum {
    /// Monotonic counter. Range queries return per-bucket deltas.
    CumulativeU64 { value: u64 },
    /// Instantaneous gauge.
    GaugeF64 { value: f64 },
    /// Instantaneous gauge, integer-typed.
    GaugeU64 { value: u64 },
}

impl Datum {
    /// Lossy projection used for downsampling / averaging. Counter
    /// values returned here are cumulative; rate computation lives in
    /// the storage layer's range query.
    pub fn as_f64(&self) -> f64 {
        match *self {
            Datum::CumulativeU64 { value } => value as f64,
            Datum::GaugeF64 { value } => value,
            Datum::GaugeU64 { value } => value as f64,
        }
    }

    /// Whether this datum is a monotonic counter.
    pub fn is_cumulative(&self) -> bool {
        matches!(self, Datum::CumulativeU64 { .. })
    }
}

/// Wire shape for the metrics ingest endpoint. Agents batch samples
/// per tick to amortize the round-trip; tritond rejects batches larger
/// than [`SampleBatch::MAX_SAMPLES`].
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SampleBatch {
    pub samples: Vec<Sample>,
}

impl SampleBatch {
    /// Conservative upper bound. One CN with ~200 zones x 3 CPU modes
    /// fits well under this.
    pub const MAX_SAMPLES: usize = 5_000;
}
