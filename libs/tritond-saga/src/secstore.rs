// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! The `SecStore` extension trait + supporting types.
//!
//! Steno itself requires a `steno::SecStore` impl with three
//! methods: `saga_create`, `record_event`, `saga_update`. That's
//! enough to *run* a saga but not enough to recover one across a
//! restart or reassign one when its owner SEC goes stale. This
//! module defines the extra operations RFD 00004 needs:
//!
//! * `load_recoverable` — every not-`Done` saga the given SEC owns,
//!   returned with its DAG + log so the executor can call
//!   `SecClient::saga_resume`.
//! * `reassign_sagas` — CAS-move every not-`Done` saga from a stale
//!   SEC to a live SEC, bumping the fence epoch (D-Sg-4 / D-Sg-8).
//! * `touch_sec` + `stale_secs` — heartbeat side-table.
//! * `current_owner` — the (sec_id, epoch) tuple receivers compare
//!   against when enforcing the fence (Invariant 8).
//!
//! Implementations: [`crate::mem::MemSecStore`] (always available,
//! `Arc<RwLock<HashMap>>` backed; the test/dev backend) and
//! `crate::fdb::FdbSecStore` (behind the `foundationdb` feature;
//! lives under the `saga/...` prefix of the region's single FDB
//! cluster, per Locked Decision #4).

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use steno::SagaId;

use crate::error::SagaResult;
use crate::types::{RecoverableSaga, SagaRecord, SecEpoch, SecHeartbeat, SecId};

/// `TritondSecStore` extends Steno's `SecStore` with the queries the
/// `SagaExecutor` needs for recovery, reassignment, and fence
/// enforcement.
///
/// Implementations must persist `saga_create`, `record_event`, and
/// `saga_update` such that a process restart can rebuild the full
/// state. `MemSecStore` keeps state behind an `Arc<RwLock>` so a
/// test fixture that hands the *same* `Arc<MemSecStore>` to two
/// `SagaExecutor`s in sequence reproduces the restart case.
#[async_trait]
pub trait TritondSecStore: steno::SecStore {
    /// Stamp the creating SEC / epoch / version onto a saga's
    /// record. Called by `SagaExecutor::saga_execute` immediately
    /// after `SecClient::saga_create` so the record carries the
    /// fence fields the rest of the system relies on (Invariant 8 /
    /// D-Sg-10). Idempotent on `saga_id`.
    async fn stamp_create(
        &self,
        saga_id: SagaId,
        name: &str,
        version: u32,
        sec: SecId,
        epoch: SecEpoch,
    ) -> SagaResult<()>;

    /// Look up the persisted record for a saga. Used by
    /// `current_owner` (below) and by operator surfaces.
    async fn get_record(&self, id: SagaId) -> SagaResult<SagaRecord>;

    /// Read every not-`Done` saga owned by `sec`, plus its full
    /// node-event log. The executor feeds each into
    /// `SecClient::saga_resume` on startup (`recover_all_for_sec`)
    /// or after a sweep adoption.
    async fn load_recoverable(&self, sec: SecId) -> SagaResult<Vec<RecoverableSaga>>;

    /// CAS every not-`Done` saga whose `current_sec` is in
    /// `stale_secs` over to `new_sec`, bumping `current_epoch` and
    /// `adopt_generation`. Returns the number of sagas moved.
    ///
    /// CAS is the parallel-execution guard (D-Sg-4); the new epoch
    /// is the side-effect guard (D-Sg-8): any in-flight RPC the
    /// stale SEC dispatched will now be rejected by receivers that
    /// check `(sec_id, epoch)`.
    async fn reassign_sagas(
        &self,
        stale_secs: &[SecId],
        new_sec: SecId,
    ) -> SagaResult<Vec<RecoverableSaga>>;

    /// Heartbeat side-table write. The local SEC calls this on a
    /// cadence (≈ a third of the stale threshold) so other SECs'
    /// sweepers can spot when it's gone.
    async fn touch_sec(&self, hb: SecHeartbeat) -> SagaResult<()>;

    /// Return every SEC whose last heartbeat is older than
    /// `before`. The local SEC excludes itself by writing its own
    /// heartbeat immediately before each sweep pass.
    async fn stale_secs(&self, before: DateTime<Utc>) -> SagaResult<Vec<SecId>>;

    /// The `(sec_id, epoch)` tuple a receiver compares against when
    /// enforcing the fence. Returns `Ok(None)` if the saga is
    /// already terminal (no live owner). Invariant 8.
    async fn current_owner(&self, saga_id: SagaId) -> SagaResult<Option<(SecId, SecEpoch)>>;

    /// Set the `stuck_reason` on a terminal saga. Idempotent. Used
    /// by the executor when an undo errors or when a saga's
    /// persisted version is missing from the registry.
    async fn mark_stuck(&self, saga_id: SagaId, reason: String) -> SagaResult<()>;
}
