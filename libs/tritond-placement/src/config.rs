// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! FDB-backed cluster-settings shape for the chain config + the
//! strategy preset weight vectors.
//!
//! RFD 00005 doc 02 §"The chain config" is the canonical reference.
//! The actual cluster-settings glue (read on tritond boot, watch for
//! changes, rebuild [`crate::ChainRunner`]) lives in PL-3 + PL-5;
//! PL-1 ships the serialisable shapes the runner consumes.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::types::{Strategy, StrategyWeights};

/// The active placement configuration. Lives in FDB-backed cluster
/// settings (same substrate `tcadm config` already uses); a setting
/// change rebuilds the runner without restarting `tritond`.
///
/// An active filter / scorer name that is not in the in-process
/// registry is a hard error at load time — the load fails loudly,
/// the setting change is rejected, and the response names the
/// unknown name (RFD 00005 doc 02 §"The chain config", D-Pl-1).
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PlacementConfig {
    /// Kebab-case filter names in evaluation order. The first
    /// rejecting filter wins for each CN; later filters still run
    /// so the `ExplainReport` carries the full picture.
    pub active_filters: Vec<String>,

    pub active_scorers: Vec<ScorerConfig>,

    /// Default strategy for requests that don't override.
    pub strategy: Strategy,

    pub overprovision: OverprovisionDefaults,

    pub materialiser: MaterialiserConfig,

    pub updated_at: DateTime<Utc>,

    /// Principal who applied this config. Carries a `String` here
    /// to keep the placement crate from taking a path dep on
    /// `tritond-audit`; the writer site (tritond Settings handler)
    /// fills it in by calling `principal.to_audit_string()` or
    /// similar.
    pub updated_by: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ScorerConfig {
    pub name: String,

    /// Resolved weight applied on top of the strategy preset. Use
    /// `None` to fall back to the strategy preset / scorer default.
    #[serde(default)]
    pub weight: Option<f32>,
}

/// Cluster-default overprovision ratios. Per-CN overrides on
/// `PlacementPolicyView` take precedence when set (D-Pl-3).
///
/// The ratio is a multiplier: 1.0 == no oversubscription;
/// >1.0 == oversubscribe; <1.0 == conservative safety margin.
/// Effective capacity for placement = total * ratio.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct OverprovisionDefaults {
    pub cpu: f32,
    pub ram: f32,
    pub disk: f32,
}

impl Default for OverprovisionDefaults {
    /// Cluster defaults: CPU oversubscription 4.0 (matches legacy
    /// DAPI's typical deployment), RAM 1.0 (no oversubscription
    /// -- can't physically over-commit memory on this platform),
    /// disk 1.0. Operators bump these via cluster settings.
    fn default() -> Self {
        Self {
            cpu: 4.0,
            ram: 1.0,
            disk: 1.0,
        }
    }
}

/// Cluster-settings shape for the load materialiser (PL-6). PL-1
/// carries the shape so callers can round-trip configs without the
/// materialiser feature compiled in.
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MaterialiserConfig {
    /// How often the materialiser polls ClickHouse. Default 60s
    /// (RFD 00005 doc 02 §"The load materialiser").
    pub interval_seconds: u32,

    /// A `cn-load-summary` row whose `last_refreshed_at` is older
    /// than `staleness_ticks × interval_seconds` is marked
    /// `stale = true` and load-history scorers contribute zero.
    pub staleness_ticks: u32,

    pub clickhouse_url: String,

    pub min_samples_5m: u32,
    pub min_samples_1d: u32,
    pub min_samples_7d: u32,
}

impl Default for MaterialiserConfig {
    fn default() -> Self {
        Self {
            interval_seconds: 60,
            staleness_ticks: 3,
            clickhouse_url: String::new(),
            min_samples_5m: 3,
            min_samples_1d: 12,
            min_samples_7d: 24,
        }
    }
}

/// Default strategy → weight vector mapping.
///
/// PL-1 carries the shape and the structural defaults; the actual
/// scorer names land in PL-4 alongside the scorers themselves.
/// Strategy presets are layered on top of each scorer's
/// `default_weight()`: the runner builds a [`StrategyWeights`] by
/// (1) seeding every registered scorer at its `default_weight()`,
/// (2) applying the strategy override below, (3) applying any
/// per-scorer operator overrides from [`PlacementConfig::active_scorers`].
pub fn strategy_weights(strategy: Strategy) -> StrategyWeights {
    let mut w = StrategyWeights::new();
    match strategy {
        Strategy::Spread => {
            // Defaults already favour spread; explicitly disable
            // the pack scorer.
            w.set("score-pack-by-fault-domain", 0.0);
        }
        Strategy::Pack => {
            w.set("score-spread-by-fault-domain", 0.0);
            w.set("score-pack-by-fault-domain", 1.5);
        }
        Strategy::Balanced => {
            w.set("score-spread-by-fault-domain", 0.75);
            w.set("score-pack-by-fault-domain", 0.75);
        }
    }
    w
}
