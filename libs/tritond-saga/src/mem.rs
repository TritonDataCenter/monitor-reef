// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! In-memory `SecStore`. Mirrors `tritond_store::MemStore`'s posture:
//! always compiled in, behind an `Arc<RwLock>`, no transactions
//! (the lock is the transaction).
//!
//! Used by unit tests, integration tests, dev daemons running
//! without FDB, and `make docker-up` without `libfdb_c`. A test
//! fixture that hands the *same* `Arc<MemSecStore>` to two
//! `SagaExecutor`s in sequence reproduces the restart case (the
//! events from the first run are still in the map when the second
//! executor recovers).

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;
use steno::{SagaCachedState, SagaId, SagaNodeEvent};
use tokio::sync::RwLock;

use crate::error::{SagaError, SagaResult};
use crate::secstore::TritondSecStore;
use crate::types::{
    RecoverableSaga, SagaCachedStatePersist, SagaRecord, SecEpoch, SecHeartbeat, SecId,
};

#[derive(Default)]
struct Inner {
    // steno::SagaId derives Ord but not Hash; use BTreeMap.
    sagas: BTreeMap<SagaId, SagaRecord>,
    events: BTreeMap<SagaId, Vec<SagaNodeEvent>>,
    heartbeats: HashMap<SecId, SecHeartbeat>,
}

/// Always-available in-memory `SecStore`.
pub struct MemSecStore {
    inner: RwLock<Inner>,
}

impl std::fmt::Debug for MemSecStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemSecStore").finish_non_exhaustive()
    }
}

impl MemSecStore {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: RwLock::new(Inner::default()),
        })
    }

    /// Test helper: how many events were recorded for a saga.
    pub async fn event_count(&self, id: SagaId) -> usize {
        let g = self.inner.read().await;
        g.events.get(&id).map(|v| v.len()).unwrap_or(0)
    }

    /// Test helper: snapshot every saga's cached state. Useful for
    /// asserting "this saga ended Done with these events".
    pub async fn snapshot_states(&self) -> Vec<(SagaId, SagaCachedStatePersist)> {
        let g = self.inner.read().await;
        g.sagas.iter().map(|(id, r)| (*id, r.state)).collect()
    }
}

impl Default for MemSecStore {
    fn default() -> Self {
        Self {
            inner: RwLock::new(Inner::default()),
        }
    }
}

#[async_trait]
impl steno::SecStore for MemSecStore {
    async fn saga_create(&self, params: steno::SagaCreateParams) -> Result<(), anyhow::Error> {
        let mut g = self.inner.write().await;
        // SG-0 stub: no fencing context is provided by Steno here.
        // SG-1 will replace this with an executor-side
        // `saga_create_with_ctx` helper that stamps the creating SEC
        // and the epoch before Steno's saga_create runs. For the
        // crate tests, we fill the SEC/epoch with a sentinel so the
        // record is queryable. Real callers go through
        // [`MemSecStore::register_new_saga`].
        let now = Utc::now();
        let record = SagaRecord {
            id: params.id,
            name: params.name.to_string(),
            version: 0,
            creator_sec: SecId::new(uuid::Uuid::nil()),
            current_sec: SecId::new(uuid::Uuid::nil()),
            current_epoch: SecEpoch::ZERO,
            adopt_generation: 0,
            dag: params.dag.clone(),
            state: SagaCachedStatePersist::from(params.state),
            time_created: now,
            time_done: None,
            stuck_reason: None,
        };
        g.sagas.entry(params.id).or_insert(record);
        g.events.entry(params.id).or_default();
        Ok(())
    }

    async fn record_event(&self, event: SagaNodeEvent) {
        let mut g = self.inner.write().await;
        g.events.entry(event.saga_id).or_default().push(event);
    }

    async fn saga_update(&self, id: SagaId, update: SagaCachedState) {
        let mut g = self.inner.write().await;
        if let Some(rec) = g.sagas.get_mut(&id) {
            rec.state = SagaCachedStatePersist::from(update);
            if matches!(update, SagaCachedState::Done) {
                rec.time_done = Some(Utc::now());
            }
        }
    }
}

#[async_trait]
impl TritondSecStore for MemSecStore {
    async fn stamp_create(
        &self,
        saga_id: SagaId,
        name: &str,
        version: u32,
        sec: SecId,
        epoch: SecEpoch,
    ) -> SagaResult<()> {
        let mut g = self.inner.write().await;
        // Steno's `saga_create` has already inserted a sentinel
        // record. Refresh the fence fields onto it. If for any
        // reason the sentinel is missing (a test calling stamp_create
        // standalone), insert a record so subsequent lookups succeed.
        let rec = g.sagas.entry(saga_id).or_insert_with(|| SagaRecord {
            id: saga_id,
            name: name.to_string(),
            version,
            creator_sec: sec,
            current_sec: sec,
            current_epoch: epoch,
            adopt_generation: 0,
            dag: Value::Null,
            state: SagaCachedStatePersist::Running,
            time_created: Utc::now(),
            time_done: None,
            stuck_reason: None,
        });
        rec.name = name.to_string();
        rec.version = version;
        rec.creator_sec = sec;
        rec.current_sec = sec;
        rec.current_epoch = epoch;
        g.events.entry(saga_id).or_default();
        Ok(())
    }

    async fn get_record(&self, id: SagaId) -> SagaResult<SagaRecord> {
        let g = self.inner.read().await;
        g.sagas.get(&id).cloned().ok_or(SagaError::NotFound)
    }

    async fn load_recoverable(&self, sec: SecId) -> SagaResult<Vec<RecoverableSaga>> {
        let g = self.inner.read().await;
        let mut out = Vec::new();
        for rec in g.sagas.values() {
            if rec.current_sec != sec {
                continue;
            }
            if matches!(rec.state, SagaCachedStatePersist::Done) {
                continue;
            }
            let events = g.events.get(&rec.id).cloned().unwrap_or_default();
            out.push(RecoverableSaga {
                record: rec.clone(),
                events,
            });
        }
        Ok(out)
    }

    async fn reassign_sagas(
        &self,
        stale_secs: &[SecId],
        new_sec: SecId,
    ) -> SagaResult<Vec<RecoverableSaga>> {
        let mut g = self.inner.write().await;
        let stale: std::collections::HashSet<SecId> = stale_secs.iter().copied().collect();
        let mut moved = Vec::new();
        for rec in g.sagas.values_mut() {
            if !stale.contains(&rec.current_sec) {
                continue;
            }
            if matches!(rec.state, SagaCachedStatePersist::Done) {
                continue;
            }
            rec.current_sec = new_sec;
            rec.current_epoch = rec.current_epoch.bump();
            rec.adopt_generation = rec.adopt_generation.saturating_add(1);
            moved.push(rec.clone());
        }
        // Second pass: read events for each moved record.
        let mut out = Vec::with_capacity(moved.len());
        for rec in moved {
            let events = g.events.get(&rec.id).cloned().unwrap_or_default();
            out.push(RecoverableSaga {
                record: rec,
                events,
            });
        }
        Ok(out)
    }

    async fn touch_sec(&self, hb: SecHeartbeat) -> SagaResult<()> {
        let mut g = self.inner.write().await;
        g.heartbeats.insert(hb.sec_id, hb);
        Ok(())
    }

    async fn stale_secs(&self, before: DateTime<Utc>) -> SagaResult<Vec<SecId>> {
        let g = self.inner.read().await;
        Ok(g.heartbeats
            .values()
            .filter(|hb| hb.at < before)
            .map(|hb| hb.sec_id)
            .collect())
    }

    async fn current_owner(&self, saga_id: SagaId) -> SagaResult<Option<(SecId, SecEpoch)>> {
        let g = self.inner.read().await;
        let Some(rec) = g.sagas.get(&saga_id) else {
            return Err(SagaError::NotFound);
        };
        if matches!(rec.state, SagaCachedStatePersist::Done) {
            return Ok(None);
        }
        Ok(Some((rec.current_sec, rec.current_epoch)))
    }

    async fn mark_stuck(&self, saga_id: SagaId, reason: String) -> SagaResult<()> {
        let mut g = self.inner.write().await;
        if let Some(rec) = g.sagas.get_mut(&saga_id) {
            rec.stuck_reason = Some(reason);
        }
        Ok(())
    }
}
