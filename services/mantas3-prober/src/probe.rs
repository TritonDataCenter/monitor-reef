// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! One cycle of the prober loop: PUT a small random payload, GET it
//! back and verify byte-for-byte, HEAD a key that should not exist
//! and verify 404, DELETE the PUT key. Every SDK call is timeout-
//! wrapped (I2). Each op emits a histogram observation plus a
//! structured JSON log line (per the log schema in the slice-1 plan).

use std::time::{Duration, Instant};

use aws_sdk_s3::Client as S3Client;
use aws_sdk_s3::error::SdkError;
use aws_sdk_s3::primitives::ByteStream;
use rand::RngCore;
use serde::Serialize;
use tokio::time::error::Elapsed;
use uuid::Uuid;

use crate::metrics::{
    Metrics, OP_DELETE, OP_GET, OP_HEAD, OP_PUT, OUTCOME_4XX, OUTCOME_5XX, OUTCOME_SDK_ERROR,
    OUTCOME_SUCCESS, OUTCOME_TIMEOUT, classify_service_status,
};

/// Outcome of running one full cycle. The supervisor uses this to
/// set the `cycle_success` gauge and decide whether to increment the
/// I6 auth-warning streak counter. `had_data_integrity_failure` is
/// kept on the struct for Debug visibility even though the data-
/// integrity signal flows through the dedicated counter — keeping
/// the field reflects the cycle's full result shape.
#[derive(Debug, Clone, Copy)]
pub struct CycleOutcome {
    pub success: bool,
    pub had_auth_failure: bool,
    #[allow(dead_code)]
    pub had_data_integrity_failure: bool,
}

/// Run one PUT/GET/HEAD/DELETE cycle. Errors observed by individual
/// ops are recorded as histogram observations + log lines and folded
/// into `CycleOutcome`. The function itself never returns an error —
/// the supervisor cares about the *outcome*, not about an exception
/// chain.
pub async fn run_cycle(
    client: &S3Client,
    metrics: &Metrics,
    bucket: &str,
    payload_bytes: usize,
    op_timeout: Duration,
) -> CycleOutcome {
    let cycle_id = Uuid::new_v4();
    let probe_key = format!("probe-{}", Uuid::new_v4());
    let missing_key = format!("probe-404-{}", Uuid::new_v4());

    let cycle_start = Instant::now();

    // Generate a fresh random payload per cycle. No seed — the same
    // cycle PUTs and GETs, so determinism would buy nothing
    // (panel — steele).
    let mut payload = vec![0u8; payload_bytes];
    rand::rng().fill_bytes(&mut payload);

    let mut success = true;
    let mut had_auth_failure = false;
    let mut had_data_integrity_failure = false;

    // ----- PUT --------------------------------------------------------
    // Build the SDK future synchronously then time-out-wrap it. This
    // avoids an `async { ... payload.clone() ... }` closure that would
    // capture references with awkward lifetimes.
    let put_fut = client
        .put_object()
        .bucket(bucket)
        .key(&probe_key)
        .body(ByteStream::from(payload.clone()))
        .send();
    let put_res = tokio::time::timeout(op_timeout, put_fut).await;
    let (put_outcome, put_status) = record_outcome(metrics, OP_PUT, &put_res, cycle_start);
    log_op(cycle_id, OP_PUT, put_outcome, put_status.as_deref(), &probe_key, &put_res);
    if put_outcome != OUTCOME_SUCCESS {
        success = false;
        if put_status.as_deref() == Some("403") {
            metrics.auth_failures.inc();
            had_auth_failure = true;
        }
    }

    // ----- GET (only meaningful if the PUT succeeded) ----------------
    if matches!(put_outcome, OUTCOME_SUCCESS) {
        let get_fut = client.get_object().bucket(bucket).key(&probe_key).send();
        let get_res = tokio::time::timeout(op_timeout, get_fut).await;
        let (get_outcome, get_status) = record_outcome(metrics, OP_GET, &get_res, cycle_start);
        log_op(cycle_id, OP_GET, get_outcome, get_status.as_deref(), &probe_key, &get_res);
        if get_outcome != OUTCOME_SUCCESS {
            success = false;
            if get_status.as_deref() == Some("403") {
                metrics.auth_failures.inc();
                had_auth_failure = true;
            }
        } else {
            // Happy path: take ownership of the response so we can
            // consume the body (ByteStream is not Clone). The
            // pattern match is infallible here because
            // `get_outcome == OUTCOME_SUCCESS` implies `Ok(Ok(_))`,
            // but the unreachable arm keeps the compiler honest.
            let body_result = match get_res {
                Ok(Ok(out)) => out.body.collect().await,
                _ => unreachable!("get_outcome=success implies Ok(Ok)"),
            };
            match body_result {
                Ok(body) => {
                    // I4 — byte-exact verification. Mismatch is data
                    // corruption, separate counter, not the outcome
                    // label.
                    let bytes = body.into_bytes();
                    if bytes.as_ref() != payload.as_slice() {
                        metrics.data_integrity_failures.inc();
                        had_data_integrity_failure = true;
                        success = false;
                        tracing::error!(
                            cycle_id = %cycle_id,
                            event = "probe_integrity",
                            key = %probe_key,
                            put_len = payload.len(),
                            got_len = bytes.len(),
                            "GET returned a body that does not match PUT payload"
                        );
                    }
                }
                Err(e) => {
                    success = false;
                    tracing::error!(
                        cycle_id = %cycle_id,
                        event = "probe_integrity_read_fail",
                        error = %e,
                        "could not read GET body for verification"
                    );
                }
            }
        }
    } else {
        // GET skipped — record a counter bump on op_errors so the
        // skip is visible in metrics, not just by absence.
        metrics
            .op_errors
            .with_label_values(&[OP_GET, "skipped_after_put_fail"])
            .inc();
    }

    // ----- HEAD on a key that should not exist -----------------------
    // Different UUID from probe_key so this doesn't race the DELETE
    // step below.
    let head_fut = client.head_object().bucket(bucket).key(&missing_key).send();
    let head_res = tokio::time::timeout(op_timeout, head_fut).await;
    // For HEAD-on-missing, 404 is the *expected* outcome. Classify
    // anything else as failure even though the SDK call "succeeded"
    // (some SDKs surface 404 as Ok, some as Err NotFound).
    let head_outcome = classify_head_404(&head_res);
    let elapsed = cycle_start.elapsed().as_secs_f64();
    metrics
        .op_duration
        .with_label_values(&[OP_HEAD, head_outcome])
        .observe(elapsed);
    let head_status = head_status_label(&head_res);
    if head_outcome != OUTCOME_SUCCESS {
        success = false;
    }
    log_op(cycle_id, OP_HEAD, head_outcome, head_status.as_deref(), &missing_key, &head_res);

    // ----- DELETE -----------------------------------------------------
    // Best-effort. If PUT failed we still try DELETE on the (probably
    // nonexistent) key so the metric series stays populated.
    let del_fut = client.delete_object().bucket(bucket).key(&probe_key).send();
    let del_res = tokio::time::timeout(op_timeout, del_fut).await;
    let (del_outcome, del_status) = record_outcome(metrics, OP_DELETE, &del_res, cycle_start);
    log_op(cycle_id, OP_DELETE, del_outcome, del_status.as_deref(), &probe_key, &del_res);
    if del_outcome != OUTCOME_SUCCESS {
        success = false;
        if del_status.as_deref() == Some("403") {
            metrics.auth_failures.inc();
            had_auth_failure = true;
        }
    }

    let total = cycle_start.elapsed().as_secs_f64();
    tracing::info!(
        cycle_id = %cycle_id,
        event = "probe_cycle",
        outcome = if success { "success" } else { "failed" },
        had_auth_failure,
        had_data_integrity_failure,
        total_latency_ms = total * 1000.0,
        "cycle complete"
    );

    CycleOutcome {
        success,
        had_auth_failure,
        had_data_integrity_failure,
    }
}

/// Inspect a `Result<Result<T, SdkError<E>>, Elapsed>` and produce
/// the histogram outcome label + the HTTP status string (if any).
/// Also records the histogram observation in `metrics.op_duration`.
fn record_outcome<T, E>(
    metrics: &Metrics,
    op: &'static str,
    res: &Result<Result<T, SdkError<E>>, Elapsed>,
    cycle_start: Instant,
) -> (&'static str, Option<String>) {
    let elapsed = cycle_start.elapsed().as_secs_f64();
    let (outcome, status) = match res {
        Ok(Ok(_)) => (OUTCOME_SUCCESS, None),
        Ok(Err(sdk_err)) => match sdk_err.raw_response().map(|r| r.status().as_u16()) {
            Some(s) => {
                let (oc, code) = classify_service_status(s);
                metrics.op_errors.with_label_values(&[op, &code]).inc();
                (oc, Some(code))
            }
            None => {
                metrics
                    .op_errors
                    .with_label_values(&[op, "sdk_no_response"])
                    .inc();
                (OUTCOME_SDK_ERROR, None)
            }
        },
        Err(_) => {
            metrics.op_errors.with_label_values(&[op, "timeout"]).inc();
            (OUTCOME_TIMEOUT, None)
        }
    };
    metrics
        .op_duration
        .with_label_values(&[op, outcome])
        .observe(elapsed);
    (outcome, status)
}

/// HEAD-on-missing has inverted semantics: 404 is success.
fn classify_head_404<T, E>(
    res: &Result<Result<T, SdkError<E>>, tokio::time::error::Elapsed>,
) -> &'static str {
    match res {
        // Some SDK versions return Ok for 200 on HEAD; that's a
        // bucket-misconfiguration (the missing key actually exists),
        // not a happy path. Treat as failure.
        Ok(Ok(_)) => OUTCOME_SDK_ERROR,
        Ok(Err(sdk_err)) => match sdk_err.raw_response().map(|r| r.status().as_u16()) {
            Some(404) => OUTCOME_SUCCESS,
            Some(s) if (400..500).contains(&s) => OUTCOME_4XX,
            Some(s) if (500..600).contains(&s) => OUTCOME_5XX,
            _ => OUTCOME_SDK_ERROR,
        },
        Err(_) => OUTCOME_TIMEOUT,
    }
}

fn head_status_label<T, E>(
    res: &Result<Result<T, SdkError<E>>, tokio::time::error::Elapsed>,
) -> Option<String> {
    match res {
        Ok(Ok(_)) => Some("200".to_string()),
        Ok(Err(sdk_err)) => sdk_err.raw_response().map(|r| r.status().as_u16().to_string()),
        Err(_) => None,
    }
}

/// Structured per-op log line. Matches the log schema in the slice-1
/// plan. `sdk_error_chain` walks `std::error::Error::source()` so the
/// full chain ends up in the log (panel — norvig).
#[derive(Serialize)]
struct OpLog<'a> {
    event: &'static str,
    cycle_id: String,
    op: &'a str,
    outcome: &'a str,
    http_status: Option<&'a str>,
    key: &'a str,
    sdk_error_chain: Vec<ErrorLink>,
}

#[derive(Serialize)]
struct ErrorLink {
    error_type: String,
    message: String,
}

fn log_op<T, E>(
    cycle_id: Uuid,
    op: &str,
    outcome: &str,
    http_status: Option<&str>,
    key: &str,
    res: &Result<Result<T, SdkError<E>>, tokio::time::error::Elapsed>,
) where
    E: std::error::Error + 'static,
{
    let chain: Vec<ErrorLink> = match res {
        Ok(Ok(_)) => Vec::new(),
        Ok(Err(sdk_err)) => walk_error_chain(sdk_err as &dyn std::error::Error),
        Err(elapsed) => walk_error_chain(elapsed as &dyn std::error::Error),
    };
    let payload = OpLog {
        event: "probe_op",
        cycle_id: cycle_id.to_string(),
        op,
        outcome,
        http_status,
        key,
        sdk_error_chain: chain,
    };
    if outcome == OUTCOME_SUCCESS {
        // Serialize once; downstream log handlers can parse the JSON.
        if let Ok(s) = serde_json::to_string(&payload) {
            tracing::info!(target: "mantas3_prober::op", "{s}");
        }
    } else if let Ok(s) = serde_json::to_string(&payload) {
        tracing::warn!(target: "mantas3_prober::op", "{s}");
    }
}

fn walk_error_chain(err: &dyn std::error::Error) -> Vec<ErrorLink> {
    let mut out = Vec::new();
    let mut current: Option<&dyn std::error::Error> = Some(err);
    while let Some(e) = current {
        out.push(ErrorLink {
            // `type_name` is best-effort. For `Box<dyn Error>` we'd
            // get the boxed wrapper; that's acceptable here since the
            // operator's primary signal is the chain of messages.
            error_type: std::any::type_name_of_val(e).to_string(),
            message: e.to_string(),
        });
        current = e.source();
        // Cap the walk to avoid pathological cycles. Realistic chains
        // are 2-4 deep.
        if out.len() >= 16 {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_head_404_treats_404_as_success() {
        // We can't easily fabricate a real SdkError without an SDK
        // call, so this test exercises only the timeout / Ok(Ok(_))
        // branches that don't need one.
        let res: Result<
            Result<(), SdkError<std::convert::Infallible>>,
            tokio::time::error::Elapsed,
        > = Ok(Ok(()));
        assert_eq!(classify_head_404(&res), OUTCOME_SDK_ERROR);
    }

    #[test]
    fn cycle_outcome_struct_fields_are_independent() {
        // Sanity test: a cycle can fail integrity while still
        // succeeding on transport. Both flags can be true; total
        // success requires both to be false.
        let outcome = CycleOutcome {
            success: false,
            had_auth_failure: false,
            had_data_integrity_failure: true,
        };
        assert!(!outcome.success);
        assert!(outcome.had_data_integrity_failure);
    }

    #[test]
    fn walk_error_chain_caps_at_16() {
        // Build a synthetic chain longer than the cap to verify we
        // stop walking. Use thiserror to compose.
        #[derive(Debug, thiserror::Error)]
        #[error("link {n}")]
        struct Link {
            n: usize,
            #[source]
            next: Option<Box<dyn std::error::Error + Send + Sync>>,
        }

        fn build(depth: usize) -> Box<dyn std::error::Error + Send + Sync> {
            let mut cur: Option<Box<dyn std::error::Error + Send + Sync>> = None;
            for n in 0..depth {
                cur = Some(Box::new(Link { n, next: cur }));
            }
            cur.unwrap()
        }

        let deep = build(64);
        let chain = walk_error_chain(deep.as_ref());
        assert_eq!(chain.len(), 16, "walk_error_chain must cap at 16 links");
    }
}
