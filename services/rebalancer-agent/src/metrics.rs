// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Prometheus metrics for the rebalancer agent
//!
//! Exports metrics for monitoring object transfer operations including:
//! - Total bytes transferred
//! - Objects processed by status (completed, failed, skipped)
//! - Errors by type
//! - Assignment completion time

use prometheus::{Counter, CounterVec, Histogram, HistogramOpts, Opts, Registry, TextEncoder};

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
        /// Registry for all agent metrics
        pub static ref REGISTRY: Registry = Registry::new();

        /// Total bytes transferred (downloaded) by this agent
        pub static ref BYTES_TOTAL: Counter = Counter::with_opts(
            Opts::new("rebalancer_agent_bytes_total", "Total bytes transferred")
        ).expect("valid metric name");

        /// Objects processed by status (completed, failed, skipped)
        pub static ref OBJECTS_TOTAL: CounterVec = CounterVec::new(
            Opts::new("rebalancer_agent_objects_total", "Objects processed by status"),
            &["status"]
        ).expect("valid metric name and labels");

        /// Errors by type
        pub static ref ERRORS_TOTAL: CounterVec = CounterVec::new(
            Opts::new("rebalancer_agent_errors_total", "Errors by type"),
            &["error_type"]
        ).expect("valid metric name and labels");

        /// Assignment completion time histogram
        pub static ref ASSIGNMENT_DURATION: Histogram = Histogram::with_opts(
            HistogramOpts::new(
                "rebalancer_agent_assignment_duration_seconds",
                "Assignment completion time in seconds"
            )
            // Buckets: 1s, 5s, 10s, 30s, 1m, 2m, 5m, 10m, 30m, 1h
            .buckets(vec![1.0, 5.0, 10.0, 30.0, 60.0, 120.0, 300.0, 600.0, 1800.0, 3600.0])
        ).expect("valid histogram opts");

        /// Counter for cleanup failures (e.g., failed to remove temp files)
        ///
        /// These failures indicate that temporary file cleanup could not be completed.
        /// The primary operation (object download/verification) still completed or
        /// failed as expected, but cleanup was degraded.
        pub static ref CLEANUP_FAILURES: Counter = Counter::with_opts(
            Opts::new(
                "rebalancer_agent_cleanup_failures_total",
                "Total cleanup failures (e.g., failed to remove temp files)"
            )
        ).expect("valid metric name");
    }
}

pub use metrics_impl::{
    ASSIGNMENT_DURATION, BYTES_TOTAL, CLEANUP_FAILURES, ERRORS_TOTAL, OBJECTS_TOTAL, REGISTRY,
};

/// Register all metrics with the registry
///
/// Should be called once during application startup.
/// Panics if registration fails (indicates a programming error).
#[allow(clippy::expect_used)]
pub fn register_metrics() {
    REGISTRY
        .register(Box::new(BYTES_TOTAL.clone()))
        .expect("Failed to register BYTES_TOTAL");
    REGISTRY
        .register(Box::new(OBJECTS_TOTAL.clone()))
        .expect("Failed to register OBJECTS_TOTAL");
    REGISTRY
        .register(Box::new(ERRORS_TOTAL.clone()))
        .expect("Failed to register ERRORS_TOTAL");
    REGISTRY
        .register(Box::new(ASSIGNMENT_DURATION.clone()))
        .expect("Failed to register ASSIGNMENT_DURATION");
    REGISTRY
        .register(Box::new(CLEANUP_FAILURES.clone()))
        .expect("Failed to register CLEANUP_FAILURES");
}

/// Get metrics in Prometheus text format
pub fn gather_metrics() -> String {
    let encoder = TextEncoder::new();
    let metric_families = REGISTRY.gather();
    encoder
        .encode_to_string(&metric_families)
        .unwrap_or_default()
}

/// Record a completed object transfer
pub fn record_object_completed(bytes: u64) {
    BYTES_TOTAL.inc_by(bytes as f64);
    OBJECTS_TOTAL.with_label_values(&["completed"]).inc();
}

/// Record a failed object transfer
pub fn record_object_failed(error_type: &str) {
    OBJECTS_TOTAL.with_label_values(&["failed"]).inc();
    ERRORS_TOTAL.with_label_values(&[error_type]).inc();
}

/// Record a skipped object (already exists with correct checksum)
pub fn record_object_skipped() {
    OBJECTS_TOTAL.with_label_values(&["skipped"]).inc();
}

/// Record assignment completion time
pub fn record_assignment_duration(duration_secs: f64) {
    ASSIGNMENT_DURATION.observe(duration_secs);
}

/// Record a cleanup failure
///
/// Call this when cleanup operations like temp file removal fail.
/// This provides observability into cleanup issues without affecting
/// the primary operation's outcome.
pub fn record_cleanup_failure() {
    CLEANUP_FAILURES.inc();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metrics_registration() {
        // Create a fresh registry for testing
        let registry = Registry::new();

        let bytes = Counter::with_opts(Opts::new("test_bytes", "test")).unwrap();
        registry.register(Box::new(bytes.clone())).unwrap();

        bytes.inc_by(100.0);

        let metric_families = registry.gather();
        assert!(!metric_families.is_empty());
    }

    #[test]
    fn test_counter_vec_labels() {
        let objects =
            CounterVec::new(Opts::new("test_objects", "test objects"), &["status"]).unwrap();

        objects.with_label_values(&["completed"]).inc();
        objects.with_label_values(&["failed"]).inc();
        objects.with_label_values(&["completed"]).inc();

        // Verify different labels track separately
        assert_eq!(objects.with_label_values(&["completed"]).get(), 2.0);
        assert_eq!(objects.with_label_values(&["failed"]).get(), 1.0);
    }

    #[test]
    fn test_record_object_completed() {
        let before_bytes = BYTES_TOTAL.get();
        let before_completed = OBJECTS_TOTAL.with_label_values(&["completed"]).get();

        record_object_completed(1024);

        let after_bytes = BYTES_TOTAL.get();
        let after_completed = OBJECTS_TOTAL.with_label_values(&["completed"]).get();

        // Due to parallel test execution, other tests may increment counters.
        // We verify that at least our increment was applied.
        assert!(
            after_bytes - before_bytes >= 1024.0,
            "BYTES_TOTAL should increase by at least 1024, got {}",
            after_bytes - before_bytes
        );
        assert!(
            after_completed - before_completed >= 1.0,
            "OBJECTS_TOTAL[completed] should increase by at least 1, got {}",
            after_completed - before_completed
        );
    }

    #[test]
    fn test_record_object_failed() {
        let before_failed = OBJECTS_TOTAL.with_label_values(&["failed"]).get();
        let before_network_errors = ERRORS_TOTAL.with_label_values(&["network"]).get();

        record_object_failed("network");

        let after_failed = OBJECTS_TOTAL.with_label_values(&["failed"]).get();
        let after_network_errors = ERRORS_TOTAL.with_label_values(&["network"]).get();

        assert!(
            after_failed - before_failed >= 1.0,
            "OBJECTS_TOTAL[failed] should increase by at least 1"
        );
        assert!(
            after_network_errors - before_network_errors >= 1.0,
            "ERRORS_TOTAL[network] should increase by at least 1"
        );
    }

    #[test]
    fn test_record_object_skipped() {
        let before_skipped = OBJECTS_TOTAL.with_label_values(&["skipped"]).get();

        record_object_skipped();

        let after_skipped = OBJECTS_TOTAL.with_label_values(&["skipped"]).get();

        assert!(
            after_skipped - before_skipped >= 1.0,
            "OBJECTS_TOTAL[skipped] should increase by at least 1"
        );
    }

    #[test]
    fn test_record_assignment_duration() {
        // The histogram doesn't have a simple "get" method, so we verify
        // it doesn't panic and the sample count increases
        let before_count = ASSIGNMENT_DURATION.get_sample_count();

        record_assignment_duration(5.0);

        assert_eq!(ASSIGNMENT_DURATION.get_sample_count() - before_count, 1);
    }

    #[test]
    fn test_record_cleanup_failure() {
        let before = CLEANUP_FAILURES.get();

        record_cleanup_failure();
        record_cleanup_failure();

        assert_eq!(CLEANUP_FAILURES.get() - before, 2.0);
    }

    #[test]
    fn test_gather_metrics_produces_output() {
        // Register metrics first (idempotent if already registered)
        // Note: In production, register_metrics is called once at startup
        // In tests, it may fail on re-registration, but that's okay for this test
        let _ = std::panic::catch_unwind(register_metrics);

        // Record something to ensure there's data
        record_object_completed(100);

        let output = gather_metrics();

        // Should contain prometheus-formatted metrics
        assert!(output.contains("rebalancer_agent"));
    }
}
