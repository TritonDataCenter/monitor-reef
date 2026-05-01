// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Tamper-evident audit log primitives for the Triton Cloud control plane.
//!
//! Each event is a link in an append-only hash chain. The application
//! layer carries the chain (hashes, sequence numbers, prev pointers);
//! the [`Chain`] trait abstracts the durable substrate underneath so
//! the same wire shape lives on different storage backends without the
//! emission code knowing which.
//!
//! # Substrate today vs. v1
//!
//! For Phase 0e the audit chain lives in **FoundationDB inside
//! `tritond` itself**, under the `audit/operator/...` keyspace. The
//! hash chain is application-layer; durability is FDB's. This is
//! sufficient for "who did what when" compliance posture but is not
//! WORM-attestable.
//!
//! At Phase 0/1 of the `manta-storage` substrate roadmap (see
//! `manta-storage/docs/plan/15-phasing.md`) the live tail moves to
//! the substrate's `("e", region, tenant_shard, versionstamp, ...)`
//! events keyspace, and sealed segments tier onto the `manta-s3`
//! surface in Object Lock compliance mode for WORM-attestable
//! retention. The hash-chain bytes survive the migration unchanged;
//! only the [`Chain`] backend swaps. Today's `FdbChain` becomes a
//! `MantaSubstrateChain` and emission code is unaffected.
//!
//! # Modules
//!
//! * [`types`] — [`AuditEvent`], [`PendingEvent`], [`Actor`],
//!   [`Decision`], [`Outcome`], hash compute helpers.
//! * [`mem`] — In-memory [`Chain`] for tests.
//! * [`fdb`] — FDB-backed [`Chain`] (gated behind the `foundationdb`
//!   cargo feature, like the equivalent in `tritond-store`).

#[cfg(feature = "foundationdb")]
pub mod fdb;
pub mod mem;
mod types;

#[cfg(feature = "foundationdb")]
pub use fdb::FdbChain;
pub use mem::MemChain;
pub use types::{
    Actor, AuditEvent, ChainHead, Decision, EventHash, Outcome, PendingEvent, VerifyOutcome,
};

use async_trait::async_trait;

/// Errors returned by [`Chain`] implementations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AuditError {
    /// Storage backend reported a failure.
    #[error("audit backend error: {0}")]
    Backend(String),

    /// Serialisation of an event failed (programmer error).
    #[error("audit serialise error: {0}")]
    Serialise(String),

    /// Verification found a chain inconsistency at the given seq.
    #[error("chain integrity broken at seq {seq}: {message}")]
    ChainBroken { seq: u64, message: String },

    /// The requested seq is past the current head.
    #[error("requested seq {seq} is past chain head {head}")]
    PastHead { seq: u64, head: u64 },
}

/// Append-only hash-chained event log. Implementations must enforce
/// strict sequential ordering: every appended event's `prev_hash`
/// equals the prior event's `hash`, and `seq` is monotonic from 0.
#[async_trait]
pub trait Chain: Send + Sync + 'static {
    /// Append a new event to the chain. The implementation assigns
    /// the `seq`, computes `prev_hash` from the current head, and
    /// computes `hash` from the canonical serialisation.
    async fn append(&self, event: PendingEvent) -> Result<AuditEvent, AuditError>;

    /// Fetch a single event by sequence number.
    async fn get(&self, seq: u64) -> Result<AuditEvent, AuditError>;

    /// Page through events. Returns at most `limit` events with
    /// `seq > after_seq`. Order is ascending by `seq`.
    async fn list(&self, after_seq: u64, limit: usize) -> Result<Vec<AuditEvent>, AuditError>;

    /// Read the current chain head — the last appended event's seq
    /// and hash. Returns `None` if the chain is empty.
    async fn head(&self) -> Result<Option<ChainHead>, AuditError>;

    /// Walk events in `[from, to]` and recompute hashes; return the
    /// first divergence (if any) plus the head observed at the end
    /// of the walk.
    async fn verify(&self, from: u64, to: u64) -> Result<VerifyOutcome, AuditError>;
}
