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

pub mod agents;
pub mod disk_usage;
pub mod status;
pub mod watchers;

pub use agents::{AgentsCollector, AgentsError};
pub use disk_usage::{DiskUsage, DiskUsageError, DiskUsageSampler, VmSnapshot};
pub use status::{StatusCollector as StatusCollectorType, StatusReport};
pub use watchers::DirtyFlag;

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
/// a shared [`DirtyFlag`] that watchers can poke to force a status sample,
/// and timing knobs. Built via [`Heartbeater::new`], run via
/// [`Heartbeater::spawn`] (background) or [`Heartbeater::run`] (awaits in
/// place).
pub struct Heartbeater {
    cnapi: Arc<CnapiClient>,
    collector: StatusCollector,
    dirty: DirtyFlag,
    heartbeat_interval: Duration,
    status_check_interval: Duration,
    status_max_interval: Duration,
}

impl Heartbeater {
    pub fn new(cnapi: Arc<CnapiClient>, collector: StatusCollector) -> Self {
        Self {
            cnapi,
            collector,
            dirty: DirtyFlag::new(),
            heartbeat_interval: HEARTBEAT_INTERVAL,
            status_check_interval: STATUS_CHECK_INTERVAL,
            status_max_interval: STATUS_MAX_INTERVAL,
        }
    }

    /// Use a specific [`DirtyFlag`] — typically one shared with the
    /// zoneevent / zone-config watchers.
    pub fn with_dirty_flag(mut self, dirty: DirtyFlag) -> Self {
        self.dirty = dirty;
        self
    }

    /// Shared dirty flag watchers should poke.
    pub fn dirty_flag(&self) -> DirtyFlag {
        self.dirty.clone()
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
    ///
    /// Three timers drive the loop, mirroring the legacy
    /// `StatusReporter.start()` behavior:
    ///
    /// * `heartbeat_interval` (default 5s) — post a heartbeat to CNAPI.
    /// * `status_max_interval` (default 60s) — force a status sample
    ///   even if nothing has signaled dirty, so CNAPI never sees stale
    ///   data longer than a minute.
    /// * `status_check_interval` (default 500ms) — check the dirty flag;
    ///   if any watcher has raised it since the last sample, take a new
    ///   one.
    ///
    /// The dirty flag is shared with the zoneevent + /etc/zones watchers
    /// (see [`DirtyFlag`] and the `watchers` module).
    pub async fn run(self, shutdown: Arc<Notify>) {
        let mut heartbeat_tick: Interval = interval(self.heartbeat_interval);
        heartbeat_tick.set_missed_tick_behavior(time::MissedTickBehavior::Skip);

        let mut status_tick: Interval = interval(self.status_check_interval);
        status_tick.set_missed_tick_behavior(time::MissedTickBehavior::Skip);

        let mut max_tick: Interval = interval(self.status_max_interval);
        max_tick.set_missed_tick_behavior(time::MissedTickBehavior::Skip);

        // The first `tick()` on an Interval fires immediately. Consume the
        // initial status-max tick so we don't double-sample on startup;
        // the status-check arm below already covers the dirty-flag path.
        let _ = max_tick.tick().await;

        // Mark dirty once at startup so CNAPI gets a first status sample
        // well before the 60s `status_max_interval` has a chance to fire.
        self.dirty.mark();

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
                    // Hard floor: sample at least every
                    // `status_max_interval`, even if nothing signaled dirty.
                    self.dirty.take();
                    self.collect_and_post().await;
                }

                _ = status_tick.tick() => {
                    if self.dirty.take() {
                        self.collect_and_post().await;
                    }
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
