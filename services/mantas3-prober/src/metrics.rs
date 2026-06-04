// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Prometheus metrics registry and handles. Three orthogonal signal
//! classes — transport (histogram), data integrity (counter), auth
//! (counter) — plus the cycle gauge and a panic counter for prober-
//! self-health.

use prometheus::{
    Encoder, Gauge, HistogramOpts, HistogramVec, IntCounter, IntCounterVec, Opts, Registry,
    TextEncoder,
};
use thiserror::Error;

/// Op label values for the histogram.
pub const OP_PUT: &str = "put";
pub const OP_GET: &str = "get";
pub const OP_HEAD: &str = "head";
pub const OP_DELETE: &str = "delete";

/// Outcome label values for the histogram.
pub const OUTCOME_SUCCESS: &str = "success";
pub const OUTCOME_TIMEOUT: &str = "timeout";
pub const OUTCOME_4XX: &str = "error_4xx";
pub const OUTCOME_5XX: &str = "error_5xx";
pub const OUTCOME_SDK_ERROR: &str = "sdk_error";

/// All metric handles the prober writes to. Wrapped in `Arc` by the
/// caller so the cycle task can share them across awaits.
#[derive(Clone)]
pub struct Metrics {
    pub registry: Registry,
    /// Per-op duration with `{op, outcome}` labels. The histogram's
    /// `_count` and `_sum` series cover what a separate counter
    /// would — there is no `ops_total` counter (panel — thompson).
    pub op_duration: HistogramVec,
    /// Body mismatch between PUT payload and GET response. This is
    /// data corruption, structurally distinct from transport failure
    /// (panel — steele).
    pub data_integrity_failures: IntCounter,
    /// 403 on any op. Tracked separately so an operator paged on
    /// `cycle_success == 0` can rule out misconfigured creds from
    /// logs alone (I6).
    pub auth_failures: IntCounter,
    /// Panics caught by R1's cycle-body supervisor. Distinct from
    /// `cycle_success == 0` because this represents prober brokenness
    /// (SEV-1: monitoring itself is failing) not target brokenness.
    pub cycle_panics: IntCounter,
    /// 1 if every op in the most recent cycle succeeded, 0 otherwise.
    /// Single binary signal per cycle; alert rules derive
    /// "consecutive failures" from this via PromQL.
    pub cycle_success: Gauge,
    /// Static gauge exposing the configured cycle interval so alert
    /// rules can self-describe (`for: 3 * cycle_interval_seconds`)
    /// without hardcoding (panel — hickey). Set once during
    /// `new()`; the handle is retained for Debug visibility and to
    /// keep the gauge alive (the registry holds a clone but the
    /// owned handle makes the lifetime intent explicit).
    #[allow(dead_code)]
    pub cycle_interval_seconds: Gauge,
    /// Per-op error code distribution. Auxiliary to the histogram —
    /// when the histogram's `outcome="error_4xx"` fires, this records
    /// which 4xx code so operators can distinguish 403 from 404 from
    /// 400 in PromQL without scraping logs.
    pub op_errors: IntCounterVec,
}

#[derive(Debug, Error)]
pub enum MetricsError {
    #[error("prometheus registration failed: {0}")]
    Register(#[from] prometheus::Error),
}

impl Metrics {
    /// Build a fresh registry with all prober metrics registered. The
    /// registry is per-Metrics-instance rather than the
    /// `prometheus::default_registry()` so tests can construct
    /// isolated registries.
    pub fn new(cycle_interval_secs: f64) -> Result<Self, MetricsError> {
        let registry = Registry::new();

        let op_duration = HistogramVec::new(
            HistogramOpts::new(
                "mantas3_prober_op_duration_seconds",
                "S3 op latency observed by the synthetic prober, labelled by op and outcome.",
            )
            // Buckets cover the SLO ranges in the parent bead: small
            // PUT/GET targets are <50ms / <20ms, larger objects up to
            // 4 MiB are <500ms / <200ms. The default histogram
            // buckets (5ms..10s) plus a 250ms / 1s rail catch the
            // shapes we care about.
            .buckets(vec![
                0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
            ]),
            &["op", "outcome"],
        )?;
        registry.register(Box::new(op_duration.clone()))?;

        let data_integrity_failures = IntCounter::with_opts(Opts::new(
            "mantas3_prober_data_integrity_failures_total",
            "PUT-then-GET returned a body that does not match what was PUT. Data corruption.",
        ))?;
        registry.register(Box::new(data_integrity_failures.clone()))?;

        let auth_failures = IntCounter::with_opts(Opts::new(
            "mantas3_prober_auth_failures_total",
            "Op returned 403. Indicates credentials issue, distinct from wedge.",
        ))?;
        registry.register(Box::new(auth_failures.clone()))?;

        let cycle_panics = IntCounter::with_opts(Opts::new(
            "mantas3_prober_cycle_panics_total",
            "Cycle body panicked and was recovered by the supervisor. Prober itself is unstable.",
        ))?;
        registry.register(Box::new(cycle_panics.clone()))?;

        let cycle_success = Gauge::with_opts(Opts::new(
            "mantas3_prober_cycle_success",
            "1 if every op in the most recent cycle succeeded, 0 otherwise.",
        ))?;
        registry.register(Box::new(cycle_success.clone()))?;

        let cycle_interval_seconds = Gauge::with_opts(Opts::new(
            "mantas3_prober_cycle_interval_seconds",
            "Configured period between cycle starts, in seconds. Used by alert rules to self-describe thresholds.",
        ))?;
        cycle_interval_seconds.set(cycle_interval_secs);
        registry.register(Box::new(cycle_interval_seconds.clone()))?;

        let op_errors = IntCounterVec::new(
            Opts::new(
                "mantas3_prober_op_errors_total",
                "Per-op error distribution by HTTP status code or error class.",
            ),
            &["op", "code"],
        )?;
        registry.register(Box::new(op_errors.clone()))?;

        Ok(Self {
            registry,
            op_duration,
            data_integrity_failures,
            auth_failures,
            cycle_panics,
            cycle_success,
            cycle_interval_seconds,
            op_errors,
        })
    }

    /// Encode the current registry contents as Prometheus text
    /// exposition format. Called by the `/metrics` HTTP handler.
    pub fn render(&self) -> Result<Vec<u8>, MetricsError> {
        let encoder = TextEncoder::new();
        let mut buf = Vec::with_capacity(8192);
        let metric_families = self.registry.gather();
        encoder.encode(&metric_families, &mut buf)?;
        Ok(buf)
    }
}

/// Categorize an aws-sdk-s3 service error into the prober's outcome
/// label vocabulary. Returns `(outcome, error_code)` so the
/// `op_errors` counter can carry the specific code while the
/// histogram carries the category.
pub fn classify_service_status(status: u16) -> (&'static str, String) {
    match status {
        // 403 is the auth case (also bumps auth_failures separately).
        // It still lands in the 4xx bucket here so the transport
        // histogram is complete.
        c if (400..500).contains(&c) => (OUTCOME_4XX, c.to_string()),
        c if (500..600).contains(&c) => (OUTCOME_5XX, c.to_string()),
        c => (OUTCOME_SDK_ERROR, c.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_renders_expected_series() {
        let m = Metrics::new(30.0).unwrap();
        // Touch every series at least once so the Prometheus encoder
        // emits its TYPE/HELP lines + a data point. An IntCounterVec
        // with zero observations is omitted from the rendered text.
        m.op_duration
            .with_label_values(&[OP_PUT, OUTCOME_SUCCESS])
            .observe(0.012);
        m.op_errors
            .with_label_values(&[OP_PUT, "200"])
            .inc();
        m.cycle_success.set(1.0);
        // Counters without labels render their zero value from
        // construction, so no manual nudge needed.

        let text = String::from_utf8(m.render().unwrap()).unwrap();
        assert!(text.contains("mantas3_prober_op_duration_seconds"));
        assert!(text.contains("mantas3_prober_op_duration_seconds_count"));
        assert!(text.contains("mantas3_prober_data_integrity_failures_total"));
        assert!(text.contains("mantas3_prober_auth_failures_total"));
        assert!(text.contains("mantas3_prober_cycle_panics_total"));
        assert!(text.contains("mantas3_prober_cycle_success"));
        assert!(text.contains("mantas3_prober_cycle_interval_seconds 30"));
        assert!(text.contains("mantas3_prober_op_errors_total"));
        // The {op="put", outcome="success"} permutation should appear.
        assert!(text.contains(r#"op="put",outcome="success""#));
    }

    #[test]
    fn classify_status_buckets_4xx_5xx_correctly() {
        assert_eq!(classify_service_status(403).0, OUTCOME_4XX);
        assert_eq!(classify_service_status(404).0, OUTCOME_4XX);
        assert_eq!(classify_service_status(412).0, OUTCOME_4XX);
        assert_eq!(classify_service_status(500).0, OUTCOME_5XX);
        assert_eq!(classify_service_status(503).0, OUTCOME_5XX);
        assert_eq!(classify_service_status(200).0, OUTCOME_SDK_ERROR);
    }

    #[test]
    fn cycle_interval_gauge_exposes_configured_value() {
        let m = Metrics::new(45.5).unwrap();
        let text = String::from_utf8(m.render().unwrap()).unwrap();
        assert!(
            text.contains("mantas3_prober_cycle_interval_seconds 45.5"),
            "expected cycle_interval_seconds = 45.5; got:\n{text}"
        );
    }

    /// Helper-correctness test (panel — knuth): a 200 status should
    /// NOT classify as 4xx or 5xx — catches a future refactor that
    /// accidentally widens the 4xx range.
    #[test]
    fn classify_status_does_not_widen_4xx_range() {
        let (outcome, _) = classify_service_status(200);
        assert_ne!(outcome, OUTCOME_4XX);
        assert_ne!(outcome, OUTCOME_5XX);
    }
}
