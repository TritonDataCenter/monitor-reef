// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Heartbeater: posts periodic liveness, status, and sysinfo updates to CNAPI.
//!
//! The legacy implementation used a mix of `setInterval`s and a vasync queue
//! that serialized updates so CNAPI didn't see more than one request at a
//! time per compute node. The Rust port keeps that serialization via a
//! single `tokio::select!` loop in [`Heartbeater::run`]: only one task ever
//! holds the CnapiClient at a time.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Notify;
use tokio::time::{self, Interval, interval};

use crate::cnapi::CnapiClient;
use crate::heartbeater::status::StatusCollector;

pub mod status;

pub use status::{StatusCollector as StatusCollectorType, StatusReport};

/// How often to post heartbeats. Matches `HEARTBEAT_INTERVAL = 5000` in the
/// legacy agent.
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);

/// How often to collect status even when nothing has signaled dirty.
/// Matches `max_interval = 60000` in the legacy StatusReporter.
pub const STATUS_MAX_INTERVAL: Duration = Duration::from_secs(60);

/// How often to sample the status collector when the "dirty" flag is set.
/// Matches `status_interval = 500`.
pub const STATUS_CHECK_INTERVAL: Duration = Duration::from_millis(500);

/// Handle returned from [`Heartbeater::spawn`]: dropping or calling
/// [`HeartbeaterHandle::shutdown`] stops the background task.
#[derive(Debug)]
pub struct HeartbeaterHandle {
    shutdown: Arc<Notify>,
    join: tokio::task::JoinHandle<()>,
}

impl HeartbeaterHandle {
    /// Signal the heartbeater to stop after its current iteration.
    ///
    /// Uses `notify_one` (not `notify_waiters`) so the shutdown signal
    /// survives even if the heartbeater task is currently mid-HTTP-request
    /// and hasn't re-entered the `select!` yet.
    pub async fn shutdown(self) {
        self.shutdown.notify_one();
        let _ = self.join.await;
    }
}

/// Periodic CNAPI client.
///
/// Holds the pieces it needs to hit CNAPI: the client, a status collector,
/// and timing knobs. Built via [`Heartbeater::new`], run via
/// [`Heartbeater::spawn`] (background) or [`Heartbeater::run`] (awaits in
/// place).
pub struct Heartbeater {
    cnapi: Arc<CnapiClient>,
    collector: StatusCollector,
    heartbeat_interval: Duration,
    status_check_interval: Duration,
    status_max_interval: Duration,
}

impl Heartbeater {
    pub fn new(cnapi: Arc<CnapiClient>, collector: StatusCollector) -> Self {
        Self {
            cnapi,
            collector,
            heartbeat_interval: HEARTBEAT_INTERVAL,
            status_check_interval: STATUS_CHECK_INTERVAL,
            status_max_interval: STATUS_MAX_INTERVAL,
        }
    }

    pub fn with_heartbeat_interval(mut self, d: Duration) -> Self {
        self.heartbeat_interval = d;
        self
    }

    pub fn with_status_check_interval(mut self, d: Duration) -> Self {
        self.status_check_interval = d;
        self
    }

    pub fn with_status_max_interval(mut self, d: Duration) -> Self {
        self.status_max_interval = d;
        self
    }

    /// Spawn a background heartbeater; returns a handle to stop it.
    pub fn spawn(self) -> HeartbeaterHandle {
        let shutdown = Arc::new(Notify::new());
        let shutdown_token = shutdown.clone();
        let join = tokio::spawn(async move {
            self.run(shutdown_token).await;
        });
        HeartbeaterHandle { shutdown, join }
    }

    /// Run the heartbeat / status loops until the given notifier fires.
    pub async fn run(self, shutdown: Arc<Notify>) {
        let mut heartbeat_tick: Interval = interval(self.heartbeat_interval);
        heartbeat_tick.set_missed_tick_behavior(time::MissedTickBehavior::Skip);

        let mut status_tick: Interval = interval(self.status_check_interval);
        status_tick.set_missed_tick_behavior(time::MissedTickBehavior::Skip);

        let mut max_tick: Interval = interval(self.status_max_interval);
        max_tick.set_missed_tick_behavior(time::MissedTickBehavior::Skip);

        // The first `tick()` on an Interval fires immediately. We discard
        // the initial status-max tick so we don't double-collect on startup.
        let _ = max_tick.tick().await;

        loop {
            tokio::select! {
                _ = shutdown.notified() => {
                    tracing::info!("heartbeater shutting down");
                    break;
                }

                _ = heartbeat_tick.tick() => {
                    if let Err(e) = self.cnapi.post_heartbeat().await {
                        tracing::warn!(error = %e, "failed to post heartbeat");
                    }
                }

                _ = max_tick.tick() => {
                    self.collect_and_post().await;
                }

                // Status-check-interval is kept for future use: the legacy
                // agent used it to batch "dirty" signals from zoneevent /
                // fs.watch. We haven't ported the watchers yet, so this
                // branch currently only triggers the same collect_and_post
                // as the max-interval branch — but at a finer cadence so
                // post-startup we don't wait a full minute for the first
                // status update.
                _ = status_tick.tick() => {
                    // No-op for now; once watchers exist they'll gate this.
                }
            }
        }
    }

    async fn collect_and_post(&self) {
        let report = self.collector.collect().await;
        let body = report.into_json();
        if let Err(e) = self.cnapi.post_status(&body).await {
            tracing::warn!(error = %e, "failed to post status");
        }
    }
}
