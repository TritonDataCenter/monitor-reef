// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Per-instance token-bucket rate limiter for the IMDS daemon.
//!
//! `IMDS_DESIGN.md` §3 (the "rate limit" bullet) + §6 ("no accidental
//! coordination plane" -- the structural backstop against a runaway
//! guest hammering writeback). A guest that holds a valid IMDSv2
//! token can otherwise burn through tritond fetches and writeback
//! relays as fast as it can dial the listener; we cap that on a
//! per-`instance_id` basis so one noisy VM can't drag down its
//! tenants on the same CN.
//!
//! Hand-rolled because the `governor` crate would pull in a new
//! workspace dependency for ~50 lines of arithmetic. Token-bucket
//! semantics: each bucket has `capacity` tokens; refill drips at
//! `refill_per_sec` until full; each request takes one token (or
//! returns false if the bucket is empty -- 429 in the middleware
//! layer).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

use uuid::Uuid;

/// Production defaults per `IMDS_DESIGN.md` §3 ("e.g. 100 req/s,
/// burst 200"). Override via [`PerInstanceRateLimiter::with`] in
/// tests or future config plumbing.
pub const DEFAULT_BURST: u32 = 200;
pub const DEFAULT_REFILL_PER_SEC: u32 = 100;

#[derive(Debug)]
struct Bucket {
    tokens: f64,
    last_refill: Instant,
}

impl Bucket {
    fn full(capacity: u32) -> Self {
        Self {
            tokens: capacity as f64,
            last_refill: Instant::now(),
        }
    }

    fn try_take(&mut self, capacity: u32, refill_per_sec: f64) -> bool {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.tokens = (self.tokens + elapsed * refill_per_sec).min(capacity as f64);
        self.last_refill = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// Per-`instance_id` rate limiter. Cheaply cloneable (`Arc` inside).
pub struct PerInstanceRateLimiter {
    inner: std::sync::Arc<PerInstanceRateLimiterInner>,
}

struct PerInstanceRateLimiterInner {
    capacity: u32,
    refill_per_sec: f64,
    buckets: Mutex<HashMap<Uuid, Bucket>>,
}

impl Clone for PerInstanceRateLimiter {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl Default for PerInstanceRateLimiter {
    fn default() -> Self {
        Self::with(DEFAULT_BURST, DEFAULT_REFILL_PER_SEC)
    }
}

impl PerInstanceRateLimiter {
    /// New limiter with the production defaults.
    pub fn new() -> Self {
        Self::default()
    }

    /// New limiter with a custom (burst, refill_per_sec). Tests use
    /// `with(2, 0)` to verify the burst-then-deny pattern without
    /// having to `sleep`.
    pub fn with(burst: u32, refill_per_sec: u32) -> Self {
        Self {
            inner: std::sync::Arc::new(PerInstanceRateLimiterInner {
                capacity: burst,
                refill_per_sec: refill_per_sec as f64,
                buckets: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// Try to consume one token for `instance_id`. Returns `true` if
    /// the request is allowed, `false` if rate-limited (the caller
    /// returns 429). Mutex poison falls through with `true` -- a
    /// panic elsewhere in tritonagent must not lock IMDS down.
    pub fn check(&self, instance_id: Uuid) -> bool {
        let mut g = match self.inner.buckets.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        let bucket = g
            .entry(instance_id)
            .or_insert_with(|| Bucket::full(self.inner.capacity));
        bucket.try_take(self.inner.capacity, self.inner.refill_per_sec)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;
    use std::time::Duration;

    #[test]
    fn burst_then_deny() {
        let rl = PerInstanceRateLimiter::with(2, 0); // 2-burst, no refill
        let id = Uuid::new_v4();
        assert!(rl.check(id));
        assert!(rl.check(id));
        assert!(!rl.check(id)); // bucket empty
    }

    #[test]
    fn refill_recovers_capacity() {
        let rl = PerInstanceRateLimiter::with(1, 1000); // 1-burst, 1000 r/s
        let id = Uuid::new_v4();
        assert!(rl.check(id));
        assert!(!rl.check(id));
        sleep(Duration::from_millis(5)); // > 1ms; should have 1+ tokens back
        assert!(rl.check(id));
    }

    #[test]
    fn separate_instances_have_independent_buckets() {
        let rl = PerInstanceRateLimiter::with(1, 0);
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        assert!(rl.check(a));
        assert!(rl.check(b)); // b has its own bucket
        assert!(!rl.check(a)); // a is empty
        assert!(!rl.check(b)); // b is empty
    }

    #[test]
    fn clone_shares_state() {
        let rl = PerInstanceRateLimiter::with(1, 0);
        let rl2 = rl.clone();
        let id = Uuid::new_v4();
        assert!(rl.check(id));
        assert!(!rl2.check(id)); // shared state
    }
}
