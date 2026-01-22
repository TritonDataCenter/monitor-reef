// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Prometheus metrics for the rebalancer manager
//!
//! Exports metrics for monitoring manager operations including:
//! - DB operation failures (when counter updates fail)

use prometheus::{Counter, Opts, Registry, TextEncoder};

// Static metric initialization uses expect because these are compile-time
// constant definitions that cannot fail in practice. If they do fail, it indicates
// a programming error (e.g., invalid metric name) that should cause a panic at startup.
//
// This module exists to scope the clippy allow attributes to just the metric definitions.
#[allow(clippy::expect_used)]
mod metrics_impl {
    use super::*;
    use lazy_static::lazy_static;

    lazy_static! {
        /// Registry for all manager metrics
        pub static ref REGISTRY: Registry = Registry::new();

        /// Counter for DB operation failures (e.g., increment_result_count errors)
        ///
        /// These failures indicate that job progress counters could not be updated
        /// in the database. The core operation (object processing) still completed,
        /// but progress tracking is degraded.
        pub static ref DB_OPERATION_FAILURES: Counter = Counter::with_opts(
            Opts::new(
                "rebalancer_manager_db_operation_failures_total",
                "Total DB operation failures (e.g., failed to increment result counters)"
            )
        ).expect("valid metric name");
    }
}

pub use metrics_impl::{DB_OPERATION_FAILURES, REGISTRY};

/// Register all metrics with the registry
///
/// Should be called once during application startup.
/// Panics if registration fails (indicates a programming error).
#[allow(clippy::expect_used)]
#[allow(dead_code)] // Will be used when manager adds metrics server
pub fn register_metrics() {
    REGISTRY
        .register(Box::new(DB_OPERATION_FAILURES.clone()))
        .expect("Failed to register DB_OPERATION_FAILURES");
}

/// Get metrics in Prometheus text format
#[allow(dead_code)] // Will be used when manager adds metrics server
pub fn gather_metrics() -> String {
    let encoder = TextEncoder::new();
    let metric_families = REGISTRY.gather();
    encoder
        .encode_to_string(&metric_families)
        .unwrap_or_default()
}

/// Record a DB operation failure
///
/// Call this when a database operation like `increment_result_count` fails.
/// This provides observability into database health issues without stopping
/// the core operation.
pub fn record_db_operation_failure() {
    DB_OPERATION_FAILURES.inc();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_db_operation_failure_counter() {
        // Record some failures
        let before = DB_OPERATION_FAILURES.get();
        record_db_operation_failure();
        record_db_operation_failure();
        let after = DB_OPERATION_FAILURES.get();

        assert_eq!(after - before, 2.0);
    }
}
