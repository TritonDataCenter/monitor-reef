// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Synthetic-request prober for the mantas3 S3 surface. See
//! `~/.claude/plans/zy0v-slice1-prober.md` for the design and the
//! README in this crate for operator-facing docs.

#![allow(clippy::expect_used)] // expect() at startup is acceptable for fatal-config paths

mod config;
mod metrics;
mod probe;
mod supervise;

use std::process::ExitCode;
use std::sync::Arc;

use aws_config::BehaviorVersion;
use aws_credential_types::Credentials;
use aws_sdk_s3::Client as S3Client;
use aws_sdk_s3::config::Region;

use config::Config;
use metrics::Metrics;

#[tokio::main]
async fn main() -> ExitCode {
    // tracing init before *anything* else so even early fatals land
    // on stdout in the structured shape downstream log shippers
    // expect.
    let level = std::env::var("MANTAS3_PROBER_LOG_LEVEL").unwrap_or_else(|_| "info".to_string());
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_new(&level)
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .json()
        .init();

    let cfg = match Config::from_env() {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(event = "config_error", error = %e, "fatal: misconfigured deployment; exiting so SMF restarts with backoff");
            return ExitCode::from(2);
        }
    };

    tracing::info!(
        event = "prober_starting",
        endpoint = %cfg.endpoint,
        bucket = %cfg.bucket,
        region = %cfg.region,
        interval_secs = cfg.interval.as_secs(),
        op_timeout_secs = cfg.op_timeout.as_secs(),
        payload_bytes = cfg.payload_bytes,
        metrics_bind = %cfg.metrics_bind,
        "mantas3-prober starting"
    );

    let metrics = match Metrics::new(cfg.interval.as_secs_f64()) {
        Ok(m) => Arc::new(m),
        Err(e) => {
            tracing::error!(event = "metrics_init_error", error = %e, "fatal: cannot initialize Prometheus registry");
            return ExitCode::from(3);
        }
    };

    let client = build_s3_client(&cfg).await;

    // I1 — bucket-missing-at-startup is fatal.
    if let Err(code) = startup_head_bucket(&client, &cfg).await {
        return code;
    }

    // R2 — supervise the /metrics listener. If the handle resolves
    // (for any reason), exit non-zero so SMF restarts the daemon.
    let mut listener_handle = supervise::spawn_metrics_listener(cfg.metrics_bind, metrics.clone());

    let mut consecutive_auth_failures: u32 = 0;
    let mut tick = tokio::time::interval(cfg.interval);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            biased;
            // R2: listener died → SEV-1, exit non-zero.
            join_result = &mut listener_handle => {
                tracing::error!(
                    event = "metrics_listener_exited",
                    detail = ?join_result,
                    "fatal: /metrics listener task ended; exiting so SMF restarts the daemon"
                );
                return ExitCode::from(6);
            }
            _ = tick.tick() => {
                let client = client.clone();
                let metrics_for_cycle = metrics.clone();
                let bucket = cfg.bucket.clone();
                let payload = cfg.payload_bytes;
                let timeout = cfg.op_timeout;

                let outcome = supervise::run_cycle_supervised(
                    &metrics,
                    async move {
                        probe::run_cycle(&client, &metrics_for_cycle, &bucket, payload, timeout).await
                    },
                ).await;

                match outcome {
                    Some(o) => {
                        metrics.cycle_success.set(if o.success { 1.0 } else { 0.0 });
                        if o.had_auth_failure {
                            consecutive_auth_failures += 1;
                            if consecutive_auth_failures == cfg.auth_warn_threshold {
                                // I6: structurally distinct WARN so
                                // an operator paged on cycle_success
                                // == 0 can rule out creds from logs.
                                tracing::warn!(
                                    event = "auth_failure_streak",
                                    streak = consecutive_auth_failures,
                                    "{} consecutive cycles with 403; check prober credentials, not target wedge",
                                    consecutive_auth_failures,
                                );
                            }
                        } else {
                            consecutive_auth_failures = 0;
                        }
                    }
                    None => {
                        // R1: panic was caught + counted; loop continues.
                        tracing::error!(event = "cycle_dropped", "cycle dropped due to panic; loop continues");
                    }
                }
            }
        }
    }
}

async fn startup_head_bucket(client: &S3Client, cfg: &Config) -> Result<(), ExitCode> {
    match tokio::time::timeout(cfg.op_timeout, client.head_bucket().bucket(&cfg.bucket).send()).await {
        Ok(Ok(_)) => {
            tracing::info!(event = "head_bucket_ok", bucket = %cfg.bucket, "startup bucket check passed");
            Ok(())
        }
        Ok(Err(e)) => {
            tracing::error!(
                event = "head_bucket_failed",
                bucket = %cfg.bucket,
                error = %e,
                "fatal: HeadBucket failed at startup (bucket missing, creds wrong, or endpoint unreachable). \
                 Exiting non-zero so SMF backoff-restarts; fix the deployment before retrying."
            );
            Err(ExitCode::from(4))
        }
        Err(_) => {
            tracing::error!(
                event = "head_bucket_timeout",
                bucket = %cfg.bucket,
                "fatal: HeadBucket timed out at startup; endpoint unreachable or wedged. \
                 Exiting non-zero so SMF backoff-restarts."
            );
            Err(ExitCode::from(5))
        }
    }
}

async fn build_s3_client(cfg: &Config) -> S3Client {
    let creds = Credentials::from_keys(
        cfg.access_key_id.clone(),
        cfg.secret_access_key.expose().to_string(),
        None,
    );
    let sdk_cfg = aws_config::defaults(BehaviorVersion::latest())
        .region(Region::new(cfg.region.clone()))
        .credentials_provider(creds)
        .load()
        .await;
    let s3_cfg = aws_sdk_s3::config::Builder::from(&sdk_cfg)
        .endpoint_url(cfg.endpoint.clone())
        .force_path_style(true)
        .build();
    S3Client::from_conf(s3_cfg)
}
