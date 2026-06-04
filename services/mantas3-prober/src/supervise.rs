// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Reliability mechanisms from the slice-1 plan's "Prober reliability"
//! section:
//!
//! - **R1**: cycle-body panic recovery. The cycle future is run inside
//!   a `tokio::spawn`; if the task panics, we log, bump
//!   `cycle_panics_total`, and continue the loop.
//! - **R2**: metrics listener supervision. The `/metrics` HTTP listener
//!   is a `tokio::spawn`'d task whose `JoinHandle` we observe; if it
//!   terminates we exit the daemon non-zero so SMF restarts the whole
//!   process. Better to take the ~5s SMF restart hit than to silently
//!   run a prober nobody can scrape.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;

use http_body_util::Full;
use hyper::body::Bytes;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

use crate::metrics::Metrics;

/// Run one cycle inside a panic-recovering task. Returns the
/// inner cycle outcome on success; on panic, returns `None` after
/// logging at ERROR and bumping `cycle_panics_total`. The loop
/// continues either way — a panic in one cycle must not silently
/// kill the prober (R1).
pub async fn run_cycle_supervised<F, T>(
    metrics: &Metrics,
    fut: F,
) -> Option<T>
where
    F: std::future::Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    let handle: JoinHandle<T> = tokio::spawn(fut);
    match handle.await {
        Ok(outcome) => Some(outcome),
        Err(join_err) => {
            metrics.cycle_panics.inc();
            if join_err.is_panic() {
                // Extract the panic payload if it's a String / &str.
                // For other payload types we still log at ERROR with
                // the JoinError debug form so the operator sees
                // *something*.
                let panic_payload: String = match join_err.try_into_panic() {
                    Ok(p) => {
                        if let Some(s) = p.downcast_ref::<&'static str>() {
                            (*s).to_string()
                        } else if let Some(s) = p.downcast_ref::<String>() {
                            s.clone()
                        } else {
                            "<non-string panic payload>".to_string()
                        }
                    }
                    Err(e) => format!("{e:?}"),
                };
                tracing::error!(
                    event = "cycle_panic",
                    panic = %panic_payload,
                    "cycle body panicked; supervisor caught it, prober continues"
                );
            } else {
                tracing::error!(
                    event = "cycle_join_error",
                    error = %join_err,
                    "cycle body task terminated without panic; continuing"
                );
            }
            None
        }
    }
}

/// Spawn the supervised `/metrics` HTTP listener. Returns a
/// `JoinHandle<()>` the main loop awaits as part of `tokio::select!`;
/// if this handle resolves, the listener has died and the daemon
/// exits non-zero (R2).
pub fn spawn_metrics_listener(
    bind: SocketAddr,
    metrics: Arc<Metrics>,
) -> JoinHandle<anyhow::Result<()>> {
    tokio::spawn(async move {
        let listener = TcpListener::bind(bind).await.map_err(|e| {
            anyhow::anyhow!("failed to bind metrics listener on {bind}: {e}")
        })?;
        tracing::info!(
            event = "metrics_listener_ready",
            bind = %bind,
            "/metrics endpoint listening"
        );
        loop {
            let (stream, peer) = match listener.accept().await {
                Ok(p) => p,
                Err(e) => {
                    tracing::error!(
                        event = "metrics_accept_failed",
                        error = %e,
                        "transient accept() failure; continuing"
                    );
                    // accept() failures on a listening socket usually
                    // mean fd exhaustion or similar. Don't tight-loop.
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    continue;
                }
            };
            let metrics = metrics.clone();
            tokio::spawn(async move {
                let io = TokioIo::new(stream);
                let service = service_fn(move |req: Request<hyper::body::Incoming>| {
                    let metrics = metrics.clone();
                    async move { Ok::<_, Infallible>(serve(metrics, req)) }
                });
                if let Err(e) = http1::Builder::new().serve_connection(io, service).await {
                    tracing::debug!(
                        event = "metrics_conn_error",
                        peer = %peer,
                        error = %e,
                        "/metrics connection ended with error"
                    );
                }
            });
        }
    })
}

fn serve(
    metrics: Arc<Metrics>,
    req: Request<hyper::body::Incoming>,
) -> Response<Full<Bytes>> {
    if req.uri().path() != "/metrics" {
        return Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Full::new(Bytes::from("not found\n")))
            .unwrap_or_else(|_| Response::new(Full::new(Bytes::from("not found\n"))));
    }
    match metrics.render() {
        Ok(buf) => Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "text/plain; version=0.0.4")
            .body(Full::new(Bytes::from(buf)))
            .unwrap_or_else(|_| Response::new(Full::new(Bytes::new()))),
        Err(e) => Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .body(Full::new(Bytes::from(format!("encode failed: {e}\n"))))
            .unwrap_or_else(|_| Response::new(Full::new(Bytes::new()))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn run_cycle_supervised_catches_panic() {
        let metrics = Metrics::new(30.0).unwrap();
        let before = metrics.cycle_panics.get();
        let result = run_cycle_supervised::<_, ()>(&metrics, async {
            panic!("synthetic panic for the supervisor test");
        })
        .await;
        assert!(result.is_none(), "panic must yield None");
        assert_eq!(
            metrics.cycle_panics.get(),
            before + 1,
            "cycle_panics_total must increment on panic"
        );
    }

    #[tokio::test]
    async fn run_cycle_supervised_passes_through_success() {
        let metrics = Metrics::new(30.0).unwrap();
        let before = metrics.cycle_panics.get();
        let result = run_cycle_supervised(&metrics, async { 42i32 }).await;
        assert_eq!(result, Some(42));
        assert_eq!(
            metrics.cycle_panics.get(),
            before,
            "cycle_panics_total must not change on clean success"
        );
    }
}
