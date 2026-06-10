// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Throttled migration progress posting (LM-3).
//!
//! The data-plane streams (`MigrateZfsSend` source role, the
//! `MigrateVmmStream` RAM push) tick a shared cumulative byte
//! counter on every chunk; a detached ticker task samples it and
//! POSTs `/v1/agent/migrations/{id}/progress` at most once per
//! [`POST_INTERVAL`] or per [`POST_BYTES_DELTA`] of new bytes,
//! whichever comes first. The split keeps the per-chunk hook down
//! to one atomic store while the network side stays bounded no
//! matter how fast the stream runs.
//!
//! Progress is observability: a failed POST is logged and dropped,
//! never surfaced into the transfer's own result.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use tokio::task::JoinHandle;
use tracing::warn;
use tritond_client::Client;
use tritond_client::types::MigrationProgressReport;
use uuid::Uuid;

/// Minimum wall-clock spacing between posts.
const POST_INTERVAL: Duration = Duration::from_secs(5);
/// Byte delta that forces a post before [`POST_INTERVAL`] elapses.
const POST_BYTES_DELTA: u64 = 256 * 1024 * 1024;
/// Counter sampling cadence. Bounds how far past a 256 MiB
/// threshold the stream can run before the post fires.
const TICK: Duration = Duration::from_secs(1);

/// Everything a post needs; shared between the ticker task and
/// [`ProgressReporter::finish`].
struct PosterCtx {
    client: Arc<Client>,
    migration_id: Uuid,
    /// Estimated stream total (`zfs send -nP` dry run for ZFS,
    /// guest RAM size for the vmm stream). `None` posts progress
    /// without a percentage or ETA.
    total_bytes: Option<u64>,
    /// Operator-log label, e.g. the snapshot being streamed.
    label: String,
}

/// Handle owning the ticker task for one transfer. Created before
/// the stream starts, fed via [`ProgressReporter::observer`], and
/// closed with [`ProgressReporter::finish`] on success. Failure
/// paths just drop it; `Drop` aborts the detached ticker so it
/// cannot outlive the stream.
pub(crate) struct ProgressReporter {
    bytes: Arc<AtomicU64>,
    ticker: JoinHandle<()>,
    ctx: Arc<PosterCtx>,
}

impl ProgressReporter {
    pub(crate) fn start(
        client: Arc<Client>,
        migration_id: Uuid,
        total_bytes: Option<u64>,
        label: String,
    ) -> Self {
        let bytes = Arc::new(AtomicU64::new(0));
        let ctx = Arc::new(PosterCtx {
            client,
            migration_id,
            total_bytes,
            label,
        });
        let ticker = tokio::spawn(run_ticker(Arc::clone(&ctx), Arc::clone(&bytes)));
        Self { bytes, ticker, ctx }
    }

    /// The cumulative-byte cell the stream's per-chunk callback
    /// stores into. One relaxed store per chunk; the ticker does
    /// the expensive part.
    pub(crate) fn observer(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.bytes)
    }

    /// Post the terminal sample and stop the ticker. Bypasses the
    /// throttle deliberately: the last event must land so the
    /// operator log ends at the true byte total.
    pub(crate) async fn finish(&self) {
        self.ticker.abort();
        let current = self.bytes.load(Ordering::Relaxed);
        post(&self.ctx, current, None, Some(0)).await;
    }
}

impl Drop for ProgressReporter {
    fn drop(&mut self) {
        self.ticker.abort();
    }
}

/// Whether a throttle window has been crossed. No new bytes means
/// no post regardless of elapsed time: an idle stream should not
/// spam identical samples.
fn should_post(elapsed: Duration, bytes_delta: u64) -> bool {
    bytes_delta > 0 && (elapsed >= POST_INTERVAL || bytes_delta >= POST_BYTES_DELTA)
}

/// Bytes/second over the window since the previous post. The
/// trailing window (rather than a since-start average) tracks the
/// link's current behaviour, which is what an ETA should reflect.
fn trailing_rate(bytes_delta: u64, elapsed: Duration) -> Option<u64> {
    let ms = elapsed.as_millis() as u64;
    if ms == 0 {
        return None;
    }
    Some(bytes_delta.saturating_mul(1000) / ms)
}

fn eta_ms(rate: Option<u64>, current: u64, total: Option<u64>) -> Option<u64> {
    let rate = rate.filter(|r| *r > 0)?;
    let total = total?;
    if total <= current {
        return None;
    }
    Some((total - current).saturating_mul(1000) / rate)
}

async fn run_ticker(ctx: Arc<PosterCtx>, bytes: Arc<AtomicU64>) {
    let mut last_post_at = Instant::now();
    let mut last_post_bytes = 0u64;
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        let current = bytes.load(Ordering::Relaxed);
        let elapsed = last_post_at.elapsed();
        let delta = current.saturating_sub(last_post_bytes);
        if !should_post(elapsed, delta) {
            continue;
        }
        let rate = trailing_rate(delta, elapsed);
        let eta = eta_ms(rate, current, ctx.total_bytes);
        post(&ctx, current, rate, eta).await;
        last_post_at = Instant::now();
        last_post_bytes = current;
    }
}

async fn post(ctx: &PosterCtx, current: u64, rate: Option<u64>, eta: Option<u64>) {
    let body = MigrationProgressReport {
        // tritond stamps the record's current phase/state; the
        // agent only sees its own stream.
        phase: None,
        state: None,
        current_progress: Some(current),
        total_progress: ctx.total_bytes,
        transfer_bytes_second: rate,
        eta_ms: eta,
        message: Some(ctx.label.clone()),
    };
    if let Err(e) = ctx
        .client
        .agent_report_migration_progress()
        .migration_id(ctx.migration_id)
        .body(body)
        .send()
        .await
    {
        warn!(
            migration_id = %ctx.migration_id,
            error = %e,
            "migration progress post failed; dropping sample",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_new_bytes_never_posts() {
        assert!(!should_post(Duration::from_secs(60), 0));
    }

    #[test]
    fn time_threshold_triggers_post() {
        assert!(!should_post(Duration::from_secs(4), 1));
        assert!(should_post(Duration::from_secs(5), 1));
    }

    #[test]
    fn byte_threshold_triggers_post_before_interval() {
        assert!(!should_post(Duration::from_secs(1), POST_BYTES_DELTA - 1));
        assert!(should_post(Duration::from_secs(1), POST_BYTES_DELTA));
    }

    #[test]
    fn trailing_rate_is_bytes_per_second() {
        assert_eq!(
            trailing_rate(10 * 1024 * 1024, Duration::from_secs(5)),
            Some(2 * 1024 * 1024),
        );
        // A zero-length window cannot produce a rate.
        assert_eq!(trailing_rate(1024, Duration::ZERO), None);
    }

    #[test]
    fn eta_needs_rate_and_remaining_bytes() {
        assert_eq!(eta_ms(Some(100), 500, Some(1000)), Some(5_000));
        assert_eq!(eta_ms(Some(0), 500, Some(1000)), None);
        assert_eq!(eta_ms(None, 500, Some(1000)), None);
        assert_eq!(eta_ms(Some(100), 500, None), None);
        // Past the estimate (it undershot): no ETA rather than a
        // negative one.
        assert_eq!(eta_ms(Some(100), 1500, Some(1000)), None);
    }
}
