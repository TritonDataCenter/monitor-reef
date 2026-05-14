// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! FoundationDB-backed `SecStore`. **SG-0 stub.**
//!
//! Keyspace under `saga/` (disjoint from `tritond-store`'s keys so
//! the migration/debug tooling can read both over one handle, same
//! rationale as RFD 00003 D-Id-10):
//!
//! ```text
//! saga/by_id/<saga_uuid>                              -> JSON SagaRecord
//! saga/event/<saga_uuid>/<node_u32_be>/<event_kind>   -> JSON SagaNodeEvent
//! saga/by_sec/<sec_uuid>/<saga_uuid>                  -> "" (empty marker)
//! saga/sec_heartbeat/<sec_uuid>                       -> JSON SecHeartbeat
//! saga/idempotency/<tenant>/<kind>/<sha256(key)>      -> saga_uuid  (SG-4)
//! saga/op_index/<terminal_at_millis>/<saga_uuid>      -> "" (SG-4)
//! ```
//!
//! SG-0 ships the skeleton + the [`FdbSecStore`] type but does not
//! implement the persistence (every method returns
//! `SagaError::Backend("FdbSecStore not yet implemented")`). SG-1
//! fills this in.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use foundationdb::Database;
use steno::{SagaCachedState, SagaCreateParams, SagaId, SagaNodeEvent};

use crate::error::{SagaError, SagaResult};
use crate::secstore::TritondSecStore;
use crate::types::{RecoverableSaga, SagaRecord, SecEpoch, SecHeartbeat, SecId};

/// FoundationDB-backed `SecStore`. SG-0 skeleton.
pub struct FdbSecStore {
    _db: Arc<Database>,
}

impl FdbSecStore {
    pub fn new(db: Arc<Database>) -> Arc<Self> {
        Arc::new(Self { _db: db })
    }
}

const NOT_YET: &str = "FdbSecStore not yet implemented (SG-1 deliverable)";

#[async_trait]
impl steno::SecStore for FdbSecStore {
    async fn saga_create(&self, _params: SagaCreateParams) -> Result<(), anyhow::Error> {
        Err(anyhow::anyhow!(NOT_YET))
    }

    async fn record_event(&self, _event: SagaNodeEvent) {
        // No-op stub; SG-1 wires this to the FDB single-transaction
        // append on `saga/event/...`.
    }

    async fn saga_update(&self, _id: SagaId, _update: SagaCachedState) {
        // No-op stub; SG-1 wires this to the conditional rewrite of
        // `saga/by_id/...`.
    }
}

#[async_trait]
impl TritondSecStore for FdbSecStore {
    async fn stamp_create(
        &self,
        _saga_id: SagaId,
        _name: &str,
        _version: u32,
        _sec: SecId,
        _epoch: SecEpoch,
    ) -> SagaResult<()> {
        Err(SagaError::Backend(NOT_YET.into()))
    }

    async fn get_record(&self, _id: SagaId) -> SagaResult<SagaRecord> {
        Err(SagaError::Backend(NOT_YET.into()))
    }

    async fn load_recoverable(&self, _sec: SecId) -> SagaResult<Vec<RecoverableSaga>> {
        Err(SagaError::Backend(NOT_YET.into()))
    }

    async fn reassign_sagas(
        &self,
        _stale_secs: &[SecId],
        _new_sec: SecId,
    ) -> SagaResult<Vec<RecoverableSaga>> {
        Err(SagaError::Backend(NOT_YET.into()))
    }

    async fn touch_sec(&self, _hb: SecHeartbeat) -> SagaResult<()> {
        Err(SagaError::Backend(NOT_YET.into()))
    }

    async fn stale_secs(&self, _before: DateTime<Utc>) -> SagaResult<Vec<SecId>> {
        Err(SagaError::Backend(NOT_YET.into()))
    }

    async fn current_owner(&self, _saga_id: SagaId) -> SagaResult<Option<(SecId, SecEpoch)>> {
        Err(SagaError::Backend(NOT_YET.into()))
    }

    async fn mark_stuck(&self, _saga_id: SagaId, _reason: String) -> SagaResult<()> {
        Err(SagaError::Backend(NOT_YET.into()))
    }
}
