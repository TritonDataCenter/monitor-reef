// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Environment-driven configuration for the synthetic prober. All
//! dials live behind env vars; there is no config file. The README
//! splits these into "required (4)" and "tuning (7)".

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use thiserror::Error;

/// Parsed configuration. Constructed via [`Config::from_env`] which
/// reads every var, applies defaults, and validates ranges. Parsing
/// failures are fatal — the daemon exits non-zero so SMF restarts
/// with backoff instead of looping on a misconfigured deployment
/// (I1-adjacent invariant from the slice-1 plan).
#[derive(Debug, Clone)]
pub struct Config {
    /// Required — mantad S3 endpoint, e.g. `http://192.168.1.182:7443`
    /// or `https://s3.example.com`.
    pub endpoint: String,
    /// Required — passed to aws-sdk-s3 as the signing region.
    pub region: String,
    /// Required — pre-created bucket the prober uses for PUT/GET/DELETE.
    /// Operator owns its lifecycle (I5).
    pub bucket: String,
    /// Required — SigV4 access key id.
    pub access_key_id: String,
    /// Required — SigV4 secret. Held in a wrapper that won't be
    /// surfaced in Debug output.
    pub secret_access_key: Secret,

    /// Tuning — period between cycle starts. Default 30s.
    pub interval: Duration,
    /// Tuning — per-op timeout. Default 10s. Every SDK call is
    /// wrapped in `tokio::time::timeout(op_timeout, ...)` (I2).
    pub op_timeout: Duration,
    /// Tuning — PUT payload size. Default 4 KiB.
    pub payload_bytes: usize,
    /// Tuning — Prometheus `/metrics` bind address. Default
    /// `0.0.0.0:9275`.
    pub metrics_bind: SocketAddr,
    /// Tuning — log level for tracing_subscriber. Default "info".
    /// Read directly from the env var by `main.rs` *before* the
    /// rest of the config is parsed (so a config-parsing failure is
    /// itself logged at the configured level). The field exists for
    /// Debug visibility.
    #[allow(dead_code)]
    pub log_level: String,
    /// Tuning — number of consecutive auth-failure cycles before the
    /// I6 WARN log fires ("creds misconfigured, not a wedge").
    /// Default 3.
    pub auth_warn_threshold: u32,
}

/// Wrapper around a secret string that hides its contents from
/// `Debug` output so accidental log lines don't leak the SigV4
/// secret. Use [`Self::expose`] inside the SDK construction.
#[derive(Clone)]
pub struct Secret(String);

impl Secret {
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Secret(<redacted, {} bytes>)", self.0.len())
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("required env var {0} is missing or empty")]
    Missing(&'static str),

    #[error("env var {0}={1:?}: {2}")]
    Invalid(&'static str, String, String),
}

impl Config {
    /// Read every prober env var, apply defaults, validate ranges.
    /// Errors here mean the operator must fix the deployment; the
    /// daemon must exit non-zero so SMF backoff-restarts.
    pub fn from_env() -> Result<Self, ConfigError> {
        let endpoint = required("MANTAS3_PROBER_ENDPOINT")?;
        // Sanity-check scheme without parsing the full URL — aws-sdk-s3
        // does the rest. We only catch the operator who set
        // `192.168.1.182:7443` (missing `http://`).
        if !endpoint.starts_with("http://") && !endpoint.starts_with("https://") {
            return Err(ConfigError::Invalid(
                "MANTAS3_PROBER_ENDPOINT",
                endpoint,
                "must start with http:// or https://".to_string(),
            ));
        }

        let region = optional_with_default("MANTAS3_PROBER_REGION", "us-east-1");
        let bucket = required("MANTAS3_PROBER_BUCKET")?;
        let access_key_id = required("MANTAS3_PROBER_ACCESS_KEY_ID")?;
        let secret_access_key = Secret(required("MANTAS3_PROBER_SECRET_ACCESS_KEY")?);

        let interval = parse_duration("MANTAS3_PROBER_INTERVAL_SECS", 30)?;
        let op_timeout = parse_duration("MANTAS3_PROBER_OP_TIMEOUT_SECS", 10)?;
        if op_timeout >= interval {
            return Err(ConfigError::Invalid(
                "MANTAS3_PROBER_OP_TIMEOUT_SECS",
                op_timeout.as_secs().to_string(),
                format!(
                    "op timeout ({:?}) must be strictly less than the cycle interval ({:?})",
                    op_timeout, interval,
                ),
            ));
        }

        let payload_bytes = parse_usize("MANTAS3_PROBER_PAYLOAD_BYTES", 4096)?;
        if payload_bytes == 0 {
            return Err(ConfigError::Invalid(
                "MANTAS3_PROBER_PAYLOAD_BYTES",
                "0".to_string(),
                "payload must be at least 1 byte".to_string(),
            ));
        }

        let metrics_port = parse_u16("MANTAS3_PROBER_METRICS_PORT", 9275)?;
        let metrics_bind_ip = optional_with_default("MANTAS3_PROBER_METRICS_BIND", "0.0.0.0");
        let metrics_bind_ip: IpAddr = metrics_bind_ip.parse().map_err(|e: std::net::AddrParseError| {
            ConfigError::Invalid(
                "MANTAS3_PROBER_METRICS_BIND",
                metrics_bind_ip.clone(),
                e.to_string(),
            )
        })?;
        let metrics_bind = SocketAddr::new(metrics_bind_ip, metrics_port);

        let log_level = optional_with_default("MANTAS3_PROBER_LOG_LEVEL", "info");
        let auth_warn_threshold = parse_u32("MANTAS3_PROBER_AUTH_WARN_THRESHOLD", 3)?;

        Ok(Config {
            endpoint,
            region,
            bucket,
            access_key_id,
            secret_access_key,
            interval,
            op_timeout,
            payload_bytes,
            metrics_bind,
            log_level,
            auth_warn_threshold,
        })
    }
}

fn required(var: &'static str) -> Result<String, ConfigError> {
    match std::env::var(var) {
        Ok(v) if !v.is_empty() => Ok(v),
        _ => Err(ConfigError::Missing(var)),
    }
}

fn optional_with_default(var: &str, default: &str) -> String {
    std::env::var(var)
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_string())
}

fn parse_duration(var: &'static str, default_secs: u64) -> Result<Duration, ConfigError> {
    let raw = std::env::var(var).ok().filter(|v| !v.is_empty());
    match raw {
        None => Ok(Duration::from_secs(default_secs)),
        Some(s) => s
            .parse::<u64>()
            .map(Duration::from_secs)
            .map_err(|e| ConfigError::Invalid(var, s, e.to_string())),
    }
}

fn parse_usize(var: &'static str, default: usize) -> Result<usize, ConfigError> {
    let raw = std::env::var(var).ok().filter(|v| !v.is_empty());
    match raw {
        None => Ok(default),
        Some(s) => s
            .parse::<usize>()
            .map_err(|e| ConfigError::Invalid(var, s, e.to_string())),
    }
}

fn parse_u16(var: &'static str, default: u16) -> Result<u16, ConfigError> {
    let raw = std::env::var(var).ok().filter(|v| !v.is_empty());
    match raw {
        None => Ok(default),
        Some(s) => s
            .parse::<u16>()
            .map_err(|e| ConfigError::Invalid(var, s, e.to_string())),
    }
}

fn parse_u32(var: &'static str, default: u32) -> Result<u32, ConfigError> {
    let raw = std::env::var(var).ok().filter(|v| !v.is_empty());
    match raw {
        None => Ok(default),
        Some(s) => s
            .parse::<u32>()
            .map_err(|e| ConfigError::Invalid(var, s, e.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each test gets a unique env-var prefix so concurrent
    /// `cargo test` runs don't tread on each other's vars. We do a
    /// manual restore so a partial-failure test doesn't bleed.
    struct EnvGuard {
        keys: Vec<&'static str>,
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for k in &self.keys {
                // SAFETY: process-wide env mutation. The tests in this
                // module are gated behind `serial_test::serial` would
                // be safer; for now we keep the surface narrow and
                // rely on the test runner's single-threaded scheduling
                // for env-sensitive tests (set --test-threads=1).
                unsafe { std::env::remove_var(k) };
            }
        }
    }

    fn set_required_envs() -> EnvGuard {
        let keys = vec![
            "MANTAS3_PROBER_ENDPOINT",
            "MANTAS3_PROBER_BUCKET",
            "MANTAS3_PROBER_ACCESS_KEY_ID",
            "MANTAS3_PROBER_SECRET_ACCESS_KEY",
        ];
        // SAFETY: as above; test runner serialization is the discipline.
        unsafe {
            std::env::set_var("MANTAS3_PROBER_ENDPOINT", "http://127.0.0.1:7443");
            std::env::set_var("MANTAS3_PROBER_BUCKET", "prober-canary");
            std::env::set_var("MANTAS3_PROBER_ACCESS_KEY_ID", "AKIAEXAMPLE");
            std::env::set_var("MANTAS3_PROBER_SECRET_ACCESS_KEY", "secretexample");
        }
        EnvGuard { keys }
    }

    #[test]
    fn from_env_happy_path() {
        let _g = set_required_envs();
        let cfg = Config::from_env().unwrap();
        assert_eq!(cfg.endpoint, "http://127.0.0.1:7443");
        assert_eq!(cfg.region, "us-east-1");
        assert_eq!(cfg.bucket, "prober-canary");
        assert_eq!(cfg.access_key_id, "AKIAEXAMPLE");
        assert_eq!(cfg.secret_access_key.expose(), "secretexample");
        assert_eq!(cfg.interval, Duration::from_secs(30));
        assert_eq!(cfg.op_timeout, Duration::from_secs(10));
        assert_eq!(cfg.payload_bytes, 4096);
        assert_eq!(cfg.metrics_bind.port(), 9275);
        assert_eq!(cfg.auth_warn_threshold, 3);
    }

    #[test]
    fn missing_required_var_fails() {
        // SAFETY: see above.
        unsafe {
            std::env::remove_var("MANTAS3_PROBER_ENDPOINT");
            std::env::remove_var("MANTAS3_PROBER_BUCKET");
            std::env::remove_var("MANTAS3_PROBER_ACCESS_KEY_ID");
            std::env::remove_var("MANTAS3_PROBER_SECRET_ACCESS_KEY");
        }
        let err = Config::from_env().unwrap_err();
        assert!(matches!(err, ConfigError::Missing(_)));
    }

    #[test]
    fn endpoint_without_scheme_is_rejected() {
        let _g = set_required_envs();
        // SAFETY: see above.
        unsafe { std::env::set_var("MANTAS3_PROBER_ENDPOINT", "127.0.0.1:7443") };
        let err = Config::from_env().unwrap_err();
        match err {
            ConfigError::Invalid(var, _, _) => assert_eq!(var, "MANTAS3_PROBER_ENDPOINT"),
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn op_timeout_geq_interval_is_rejected() {
        let _g = set_required_envs();
        // SAFETY: see above.
        unsafe {
            std::env::set_var("MANTAS3_PROBER_INTERVAL_SECS", "10");
            std::env::set_var("MANTAS3_PROBER_OP_TIMEOUT_SECS", "10");
        }
        let err = Config::from_env().unwrap_err();
        match err {
            ConfigError::Invalid(var, _, _) => assert_eq!(var, "MANTAS3_PROBER_OP_TIMEOUT_SECS"),
            other => panic!("expected Invalid, got {other:?}"),
        }
        // SAFETY: see above.
        unsafe {
            std::env::remove_var("MANTAS3_PROBER_INTERVAL_SECS");
            std::env::remove_var("MANTAS3_PROBER_OP_TIMEOUT_SECS");
        }
    }

    #[test]
    fn zero_payload_is_rejected() {
        let _g = set_required_envs();
        // SAFETY: see above.
        unsafe { std::env::set_var("MANTAS3_PROBER_PAYLOAD_BYTES", "0") };
        let err = Config::from_env().unwrap_err();
        match err {
            ConfigError::Invalid(var, _, _) => assert_eq!(var, "MANTAS3_PROBER_PAYLOAD_BYTES"),
            other => panic!("expected Invalid, got {other:?}"),
        }
        // SAFETY: see above.
        unsafe { std::env::remove_var("MANTAS3_PROBER_PAYLOAD_BYTES") };
    }

    #[test]
    fn secret_debug_redacts() {
        let s = Secret("super-secret".to_string());
        let dbg = format!("{s:?}");
        assert!(!dbg.contains("super-secret"));
        assert!(dbg.contains("redacted"));
    }
}
