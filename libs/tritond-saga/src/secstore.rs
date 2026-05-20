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
use steno::{SagaId, SagaNodeEvent};

use crate::error::SagaResult;
use crate::types::{
    RecoverableSaga, ResourceRef, ResourceScope, SagaRecord, SecEpoch, SecHeartbeat, SecId,
};

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
    /// Stamp the creating SEC / epoch / version / resource refs
    /// onto a saga's record. Called by `SagaExecutor::saga_execute`
    /// immediately after `SecClient::saga_create` so the record
    /// carries the fence fields and resource index the rest of the
    /// system relies on (Invariant 8 / D-Sg-10 / RFD 00004 SG-4
    /// resource indexing). Idempotent on `saga_id`.
    ///
    /// `references` is the catalog module's `build_references(...)`
    /// output — every resource known at create-time. FDB
    /// implementations write secondary keys
    /// `saga/by_ref/<scope>/<id>/<inv_ts>/<saga_id>` in the same
    /// transaction so the index is atomic with the saga record.
    async fn stamp_create(
        &self,
        saga_id: SagaId,
        name: &str,
        version: u32,
        sec: SecId,
        epoch: SecEpoch,
        references: &[ResourceRef],
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

    /// Page through every saga the store knows about, regardless of
    /// owning SEC or terminal status. Used by the operator-visible
    /// `/v2/operations` surface and the adminUI Operations tab
    /// (RFD 00004 SG-4). `marker` is an opaque continuation
    /// token; pass `None` for the first page.
    ///
    /// Implementations are free to pick any stable ordering as
    /// long as `marker` produces a deterministic next-page
    /// boundary; the SG-4 contract is "operators can walk the
    /// catalog", not "newest-first". The wire shape is stable
    /// across implementations.
    async fn list_sagas(&self, marker: Option<SagaId>, limit: usize)
    -> SagaResult<Vec<SagaRecord>>;

    /// Read every node event recorded for a saga, in node-id order
    /// (then by event_kind for ties). Implementations are expected
    /// to paginate internally so they don't exceed the FDB single-
    /// transaction limit (see `load_events_paged` in the FDB impl).
    /// Used by the operator-visible `/v2/operations/{id}` surface
    /// to project step-by-step progress (RFD 00004 D-Sg-13).
    async fn load_events(&self, saga_id: SagaId) -> SagaResult<Vec<SagaNodeEvent>>;

    /// Retention GC: delete every saga record + its event log + its
    /// `by_sec` marker for sagas that are terminal (state == Done)
    /// and whose `time_done` is older than `before`. Returns the
    /// number of sagas pruned. Idempotent — re-running the same
    /// sweep is a no-op. RFD 00004 SG-4 retention pass.
    ///
    /// Implementations must skip sagas whose state is non-terminal
    /// (a slow Done write may still be in flight) and must not
    /// touch the `Stuck` set — `stuck_reason` is operator-actionable
    /// and never expires.
    async fn prune_terminal_sagas_older_than(&self, before: DateTime<Utc>) -> SagaResult<usize>;

    /// Page through every saga that touches the given resource.
    /// Ordering is newest-first (the FDB index keys the saga id
    /// under an inverted millisecond timestamp). `marker` is the
    /// saga id of the last entry from the prior page; pass `None`
    /// for the first page.
    ///
    /// Operator-visible endpoint: `GET /v2/operations?resource_scope=
    /// &resource_id=` (RFD 00004 SG-4 resource indexing).
    async fn list_sagas_by_reference(
        &self,
        scope: ResourceScope,
        id: uuid::Uuid,
        marker: Option<SagaId>,
        limit: usize,
    ) -> SagaResult<Vec<SagaRecord>>;
}
