// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Tritond-side v2p invalidation ring.
//!
//! Implements item 8 of `PROTEUS_PLAN.md` §11.7.1: when a NIC is
//! torn down or migrated, tritond pushes an `(vni, peer_ip)`
//! invalidation directive onto a per-process ring. The bound CN
//! agent polls `GET /v2/agent/peer-invalidations?since=<seq>` on a
//! fixed cadence; the response contains every entry with `seq >
//! since` and a `tail_seq` cursor for the next poll.
//!
//! Phase A shape: a single global ring broadcast to every CN
//! (every CN's poll returns the same batch). This is correct for
//! low-NIC-churn deployments (the resolver re-queries on the next
//! packet anyway, so over-broadcasting is wasted work but not a
//! correctness problem). Phase B narrows to per-CN filtering once
//! tritond tracks which CNs have queried `/v2/agent/peer`.
//!
//! Bounded: the ring keeps the most recent
//! [`MAX_RING_ENTRIES`]; older entries fall off. A CN whose
//! `since` cursor is older than the ring's tail catches up to
//! whatever's still in the buffer (the older invalidations have
//! already aged out of any cache that would care -- the kmod TTL
//! handles those). Process restart drops the ring, which is OK
//! because:
//! 1. Agents will re-poll with `since=0` and pick up whatever
//!    landed post-restart;
//! 2. Any invalidation we lost across a restart will be re-issued
//!    by the next NIC teardown OR will TTL-out of the kmod cache
//!    within DEFAULT_PEER_ENTRY_TTL_SECS.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use tritond_api::AgentPeerInvalidation;

/// Cap the ring at this many entries. Sized so a 1000-NIC subnet
/// can absorb a complete teardown (1000 invalidations) without
/// truncating, with headroom.
pub const MAX_RING_ENTRIES: usize = 4096;

/// How long to keep an invalidation in the ring after it was
/// pushed. Bounded above by the kmod's default cache TTL -- after
/// this window every receiver has either polled (and removed) or
/// TTL-evicted (and doesn't need the invalidation). 10 minutes is
/// 2× the default 5-minute TTL.
pub const ENTRY_RETENTION: Duration = Duration::from_secs(600);

#[derive(Clone, Debug)]
struct RingEntry {
    invalidation: AgentPeerInvalidation,
    pushed_at: Instant,
}

/// Process-local invalidation ring. Behind a `Mutex` -- contention
/// is low (one writer per NIC teardown event, N readers polling at
/// ~10s intervals).
pub struct Ring {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    entries: std::collections::VecDeque<RingEntry>,
    next_seq: u64,
}

impl Default for Ring {
    fn default() -> Self {
        Self::new()
    }
}

impl Ring {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                entries: std::collections::VecDeque::new(),
                next_seq: 1,
            }),
        }
    }

    /// Push a fresh invalidation onto the ring. Returns the
    /// assigned sequence number.
    pub fn push(&self, vni: u32, peer_ip: String) -> u64 {
        let mut inner = self
            .inner
            .lock()
            .expect("peer_invalidations: lock poisoned");
        let seq = inner.next_seq;
        inner.next_seq = seq.wrapping_add(1);
        let entry = RingEntry {
            invalidation: AgentPeerInvalidation { seq, vni, peer_ip },
            pushed_at: Instant::now(),
        };
        inner.entries.push_back(entry);
        // Drop expired + oversized.
        Self::trim(&mut inner);
        seq
    }

    /// Return every invalidation strictly after `since`, plus the
    /// highest seq returned (for the agent's next `since` cursor).
    /// Empty list with `tail_seq = since` when nothing's new.
    pub fn drain_since(&self, since: u64) -> (Vec<AgentPeerInvalidation>, u64) {
        let mut inner = self
            .inner
            .lock()
            .expect("peer_invalidations: lock poisoned");
        Self::trim(&mut inner);
        let mut out: Vec<AgentPeerInvalidation> = inner
            .entries
            .iter()
            .filter(|e| e.invalidation.seq > since)
            .map(|e| e.invalidation.clone())
            .collect();
        out.sort_by_key(|i| i.seq);
        let tail_seq = out.last().map(|i| i.seq).unwrap_or(since);
        (out, tail_seq)
    }

    fn trim(inner: &mut Inner) {
        let cutoff = Instant::now().checked_sub(ENTRY_RETENTION);
        while let Some(front) = inner.entries.front() {
            let expired = cutoff.is_some_and(|c| front.pushed_at < c);
            let overflow = inner.entries.len() > MAX_RING_ENTRIES;
            if !(expired || overflow) {
                break;
            }
            inner.entries.pop_front();
        }
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.inner.lock().unwrap().entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_assigns_increasing_seqs() {
        let r = Ring::new();
        let a = r.push(0x1234, "10.0.0.1".to_string());
        let b = r.push(0x1234, "10.0.0.2".to_string());
        assert!(b > a);
    }

    #[test]
    fn drain_since_returns_new_entries_only() {
        let r = Ring::new();
        let a = r.push(0x1234, "10.0.0.1".to_string());
        let b = r.push(0x1234, "10.0.0.2".to_string());
        let (entries, tail) = r.drain_since(a);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].seq, b);
        assert_eq!(tail, b);
    }

    #[test]
    fn drain_since_with_no_new_returns_cursor_unchanged() {
        let r = Ring::new();
        let a = r.push(0x1234, "10.0.0.1".to_string());
        let (entries, tail) = r.drain_since(a);
        assert!(entries.is_empty());
        assert_eq!(tail, a);
    }

    #[test]
    fn drain_since_from_zero_returns_everything() {
        let r = Ring::new();
        let _ = r.push(0x1234, "10.0.0.1".to_string());
        let _ = r.push(0x1234, "10.0.0.2".to_string());
        let (entries, _) = r.drain_since(0);
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn ring_caps_at_max_entries() {
        let r = Ring::new();
        for i in 0..(MAX_RING_ENTRIES + 100) {
            r.push(0x1234, format!("10.0.0.{}", i % 256));
        }
        assert!(r.len() <= MAX_RING_ENTRIES);
    }
}
