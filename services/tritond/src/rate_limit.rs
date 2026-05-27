// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Per-source-IP rate limiting for the operator-auth surface.
//!
//! Today this fronts only `POST /v1/auth/login`. Refresh and API-key
//! creation already require an authenticated principal, so the
//! brute-force surface is small. Login takes a username + password
//! straight off the wire and the password is the most valuable secret
//! in the system, so this is the path that needs throttling.
//!
//! ## Why per-source-IP and not per-username
//!
//! Per-username throttling protects accounts but lets an attacker
//! enumerate users by hammering thousands of guessed names from a
//! single IP. Per-source-IP catches that scan and the standard
//! "rotate passwords against one valid username" attack as long as
//! the attacker stays on one IP. A mature deployment wants both;
//! we'll add per-username when credential-stuffing patterns appear.
//!
//! ## Why we don't trust X-Forwarded-For
//!
//! [`dropshot::RequestContext::remote_addr`] returns the TCP peer's
//! address. In a deployment behind a load balancer or reverse proxy
//! the real source IP appears in `X-Forwarded-For`, but trusting that
//! header by default would let an attacker spoof their source IP to
//! evade rate limiting. Until the deployment story includes a
//! configured list of trusted proxies, the conservative call is to
//! rate-limit by peer address. (Tracked in `STATUS.md` deferred items.)
//!
//! ## Why in-memory and not FDB
//!
//! Phase 0 runs a single tritond process; an in-memory limiter is
//! correct and cheap. Once we fan out to multiple processes behind a
//! load balancer, attackers can spread load to dodge per-process
//! limits. Cluster-wide rate limiting via FDB or sticky-routing at
//! the load balancer is a Phase 1 concern.

use std::net::IpAddr;
use std::num::NonZeroU32;
use std::time::Duration;

use governor::clock::{Clock, DefaultClock};
use governor::state::keyed::DefaultKeyedStateStore;
use governor::{Quota, RateLimiter};

/// Default per-IP login quota: ten attempts per minute, burst ten.
/// After the bucket is drained, each additional attempt has to wait
/// ~6s for one token to refill — slow enough to be useless to brute
/// force, fast enough that a human typing their password wrong a few
/// times in a row never trips it.
pub const DEFAULT_LOGIN_QUOTA_PER_MINUTE: u32 = 10;

/// Default per-IP CN-approve quota: matches login's bucket. The
/// approve endpoint takes a 6-character claim code (~30 bits of
/// entropy) under a 1h TTL; the per-IP limit + the entropy +
/// the TTL together leave the brute-force search space far out
/// of reach.
pub const DEFAULT_CN_APPROVE_QUOTA_PER_MINUTE: u32 = 10;

const DEFAULT_LOGIN_QUOTA_NZ: NonZeroU32 = match NonZeroU32::new(DEFAULT_LOGIN_QUOTA_PER_MINUTE) {
    Some(n) => n,
    None => panic!("DEFAULT_LOGIN_QUOTA_PER_MINUTE must be non-zero"),
};

const DEFAULT_CN_APPROVE_QUOTA_NZ: NonZeroU32 =
    match NonZeroU32::new(DEFAULT_CN_APPROVE_QUOTA_PER_MINUTE) {
        Some(n) => n,
        None => panic!("DEFAULT_CN_APPROVE_QUOTA_PER_MINUTE must be non-zero"),
    };

/// Generic per-source-IP token bucket. Used by both the login
/// throttle and the CN-approve throttle; each gets its own
/// instance with its own bucket-set so attackers hammering one
/// surface don't drain the other's budget.
pub struct IpRateLimiter {
    inner: RateLimiter<IpAddr, DefaultKeyedStateStore<IpAddr>, DefaultClock>,
    clock: DefaultClock,
}

impl IpRateLimiter {
    /// Build a limiter at an explicit quota.
    #[must_use]
    pub fn with_quota(quota: Quota) -> Self {
        let clock = DefaultClock::default();
        let inner = RateLimiter::keyed(quota);
        Self { inner, clock }
    }

    /// Try to take one token for `source`. On success returns
    /// `Ok(())`. On rejection returns `Err(retry_after)`, the
    /// duration the caller should wait before trying again.
    pub fn check(&self, source: IpAddr) -> Result<(), Duration> {
        match self.inner.check_key(&source) {
            Ok(()) => Ok(()),
            Err(not_until) => Err(not_until.wait_time_from(self.clock.now())),
        }
    }
}

/// Type alias preserved for the existing login-handler call sites;
/// internally identical to [`IpRateLimiter`] but constructed with
/// the login quota by default.
pub type LoginRateLimiter = IpRateLimiter;

impl IpRateLimiter {
    /// Build a limiter at the default login quota.
    #[must_use]
    pub fn new() -> Self {
        Self::with_quota(Quota::per_minute(DEFAULT_LOGIN_QUOTA_NZ))
    }

    /// Build a limiter at the default CN-approve quota.
    #[must_use]
    pub fn for_cn_approve() -> Self {
        Self::with_quota(Quota::per_minute(DEFAULT_CN_APPROVE_QUOTA_NZ))
    }
}

impl Default for IpRateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn first_n_attempts_pass_then_reject() {
        let q = Quota::per_minute(NonZeroU32::new(3).unwrap_or(NonZeroU32::MIN));
        let lim = LoginRateLimiter::with_quota(q);
        let ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
        for _ in 0..3 {
            assert!(lim.check(ip).is_ok());
        }
        let err = lim.check(ip).expect_err("4th attempt should be throttled");
        assert!(err.as_secs() < 60);
    }

    #[test]
    fn separate_ips_have_separate_buckets() {
        let q = Quota::per_minute(NonZeroU32::new(2).unwrap_or(NonZeroU32::MIN));
        let lim = LoginRateLimiter::with_quota(q);
        let a = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
        let b = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2));
        assert!(lim.check(a).is_ok());
        assert!(lim.check(a).is_ok());
        assert!(lim.check(a).is_err());
        // b's bucket is unaffected.
        assert!(lim.check(b).is_ok());
    }
}
