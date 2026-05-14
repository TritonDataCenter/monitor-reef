// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! FoundationDB-backed `SecStore` (RFD 00004 SG-1b).
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
//! ```
//!
//! Conventions mirror `tritond-store/src/fdb.rs`: every write is one
//! `db.run(|tr, _| async move { ... })` transaction (FDB retries
//! conflicts for us); keys are byte strings with fixed,
//! sort-friendly encodings. `node_u32_be` is the
//! `SagaNodeId`'s inner `u32` in big-endian so event scans for a
//! saga arrive in node order; `<event_kind>` is the
//! `SagaNodeEventType` variant name so the `(saga_id, node_id,
//! event_kind)` triple is the idempotency key for re-writing the
//! same event.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use foundationdb::{Database, FdbBindingError, KeySelector, RangeOption};
use steno::{SagaCachedState, SagaCreateParams, SagaId, SagaNodeEvent, SagaNodeEventType};

use crate::error::{SagaError, SagaResult};
use crate::secstore::TritondSecStore;
use crate::types::{
    RecoverableSaga, SagaCachedStatePersist, SagaRecord, SecEpoch, SecHeartbeat, SecId,
};

/// FoundationDB-backed `SecStore`.
pub struct FdbSecStore {
    db: Arc<Database>,
}

impl std::fmt::Debug for FdbSecStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FdbSecStore").finish_non_exhaustive()
    }
}

impl FdbSecStore {
    pub fn new(db: Arc<Database>) -> Arc<Self> {
        Arc::new(Self { db })
    }

    // ── Key helpers ──────────────────────────────────────────────

    fn by_id_key(id: SagaId) -> Vec<u8> {
        format!("saga/by_id/{}", id.0).into_bytes()
    }

    fn by_id_prefix() -> &'static [u8] {
        b"saga/by_id/"
    }

    /// `saga/event/<saga_uuid>/<node_u32_be_hex>/<event_kind>` — the
    /// `node_u32_be_hex` segment is the `SagaNodeId`'s inner u32 in
    /// 8-hex-digit big-endian form (zero-padded). The hex padding
    /// keeps the key lexicographically sortable in node-id order
    /// without escaping concerns, mirroring `tritond-store`'s
    /// `job_pending_key` trick.
    fn event_key(saga_id: SagaId, node_id: u32, event_kind: &str) -> Vec<u8> {
        format!("saga/event/{}/{:08x}/{}", saga_id.0, node_id, event_kind).into_bytes()
    }

    fn events_prefix(saga_id: SagaId) -> Vec<u8> {
        format!("saga/event/{}/", saga_id.0).into_bytes()
    }

    fn by_sec_key(sec_id: SecId, saga_id: SagaId) -> Vec<u8> {
        format!("saga/by_sec/{}/{}", sec_id.0, saga_id.0).into_bytes()
    }

    fn by_sec_prefix(sec_id: SecId) -> Vec<u8> {
        format!("saga/by_sec/{}/", sec_id.0).into_bytes()
    }

    fn heartbeat_key(sec_id: SecId) -> Vec<u8> {
        format!("saga/sec_heartbeat/{}", sec_id.0).into_bytes()
    }

    fn heartbeat_prefix() -> &'static [u8] {
        b"saga/sec_heartbeat/"
    }
}

// ── Helpers ──────────────────────────────────────────────────────

fn prefix_range(prefix: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let mut end = prefix.to_vec();
    // Increment the last byte to bound the scan. If every byte is
    // 0xff the scan runs to the global tail, which is fine for our
    // prefixed keyspace.
    for i in (0..end.len()).rev() {
        if end[i] < 0xff {
            end[i] += 1;
            return (prefix.to_vec(), end);
        }
        end[i] = 0;
    }
    (prefix.to_vec(), end)
}

fn cached_state_str(s: SagaCachedState) -> &'static str {
    match s {
        SagaCachedState::Running => "running",
        SagaCachedState::Unwinding => "unwinding",
        SagaCachedState::Done => "done",
    }
}

fn event_kind_str(t: &SagaNodeEventType) -> &'static str {
    match t {
        SagaNodeEventType::Started => "started",
        SagaNodeEventType::Succeeded(_) => "succeeded",
        SagaNodeEventType::Failed(_) => "failed",
        SagaNodeEventType::UndoStarted => "undo_started",
        SagaNodeEventType::UndoFinished => "undo_finished",
        SagaNodeEventType::UndoFailed(_) => "undo_failed",
    }
}

fn node_id_u32(node_id: steno::SagaNodeId) -> u32 {
    // SagaNodeId is `#[serde(transparent)]` over u32; round-trip
    // through serde to extract the inner number without reaching for
    // a private field.
    serde_json::to_value(node_id)
        .ok()
        .and_then(|v| v.as_u64())
        .map(|v| v as u32)
        .unwrap_or(0)
}

fn ser_record(rec: &SagaRecord) -> Result<Vec<u8>, FdbBindingError> {
    serde_json::to_vec(rec)
        .map_err(|e| FdbBindingError::CustomError(format!("serialize SagaRecord: {e}").into()))
}

fn deser_record(bytes: &[u8]) -> Result<SagaRecord, FdbBindingError> {
    serde_json::from_slice(bytes)
        .map_err(|e| FdbBindingError::CustomError(format!("deserialize SagaRecord: {e}").into()))
}

fn ser_event(ev: &SagaNodeEvent) -> Result<Vec<u8>, FdbBindingError> {
    serde_json::to_vec(ev)
        .map_err(|e| FdbBindingError::CustomError(format!("serialize SagaNodeEvent: {e}").into()))
}

fn deser_event(bytes: &[u8]) -> Result<SagaNodeEvent, FdbBindingError> {
    serde_json::from_slice(bytes)
        .map_err(|e| FdbBindingError::CustomError(format!("deserialize SagaNodeEvent: {e}").into()))
}

fn ser_heartbeat(hb: &SecHeartbeat) -> Result<Vec<u8>, FdbBindingError> {
    serde_json::to_vec(hb)
        .map_err(|e| FdbBindingError::CustomError(format!("serialize SecHeartbeat: {e}").into()))
}

fn deser_heartbeat(bytes: &[u8]) -> Result<SecHeartbeat, FdbBindingError> {
    serde_json::from_slice(bytes)
        .map_err(|e| FdbBindingError::CustomError(format!("deserialize SecHeartbeat: {e}").into()))
}

fn backend(e: impl std::fmt::Display) -> SagaError {
    SagaError::Backend(format!("FDB: {e}"))
}

// ── steno::SecStore ──────────────────────────────────────────────

#[async_trait]
impl steno::SecStore for FdbSecStore {
    async fn saga_create(&self, params: SagaCreateParams) -> Result<(), anyhow::Error> {
        // Sentinel record on first write. SagaExecutor::saga_execute
        // calls `stamp_create` immediately after to fill in
        // `(creator_sec, current_sec, current_epoch, version, name)`.
        // The two-write pattern matches MemSecStore's semantics so
        // recovery sees the same field shape regardless of backend.
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
        let by_id_key = Self::by_id_key(params.id);
        let value = serde_json::to_vec(&record)
            .map_err(|e| anyhow::anyhow!("serialize SagaRecord: {e}"))?;
        self.db
            .run(|tr, _| {
                let by_id_key = by_id_key.clone();
                let value = value.clone();
                async move {
                    // Only write if absent — Steno may retry the
                    // call after a transient error; we keep the
                    // first record (which may have already been
                    // stamped by `stamp_create`).
                    if tr.get(&by_id_key, false).await?.is_none() {
                        tr.set(&by_id_key, &value);
                    }
                    Ok(())
                }
            })
            .await
            .map_err(|e: FdbBindingError| anyhow::anyhow!("FDB saga_create: {e}"))?;
        Ok(())
    }

    async fn record_event(&self, event: SagaNodeEvent) {
        let key = Self::event_key(
            event.saga_id,
            node_id_u32(event.node_id),
            event_kind_str(&event.event_type),
        );
        let value = match ser_event(&event) {
            Ok(v) => v,
            Err(e) => {
                // Steno's trait returns `()`; log + drop is the
                // documented behaviour for serialisation failures
                // in the in-memory store too. A persistent
                // serialise failure would mean a Steno upgrade
                // changed the shape — surface via slog so it's
                // operator-visible.
                eprintln!("FdbSecStore::record_event: serialise failed: {e}");
                return;
            }
        };
        let res = self
            .db
            .run(|tr, _| {
                let key = key.clone();
                let value = value.clone();
                async move {
                    // Idempotent on the (saga_id, node_id,
                    // event_kind) triple — re-writing the same
                    // event is a no-op for downstream readers.
                    tr.set(&key, &value);
                    Ok::<(), FdbBindingError>(())
                }
            })
            .await;
        if let Err(e) = res {
            eprintln!("FdbSecStore::record_event: FDB error: {e}");
        }
    }

    async fn saga_update(&self, id: SagaId, update: SagaCachedState) {
        let by_id_key = Self::by_id_key(id);
        let res: Result<(), FdbBindingError> = self
            .db
            .run(|tr, _| {
                let by_id_key = by_id_key.clone();
                async move {
                    let Some(bytes) = tr.get(&by_id_key, false).await? else {
                        return Ok(());
                    };
                    let mut rec = deser_record(&bytes)?;
                    rec.state = SagaCachedStatePersist::from(update);
                    if matches!(update, SagaCachedState::Done) {
                        rec.time_done = Some(Utc::now());
                        // Remove the `by_sec` marker on Done so
                        // `load_recoverable` and `reassign_sagas`
                        // skip terminal sagas without an extra
                        // state check.
                        let by_sec = Self::by_sec_key(rec.current_sec, id);
                        tr.clear(&by_sec);
                    }
                    let value = ser_record(&rec)?;
                    tr.set(&by_id_key, &value);
                    Ok(())
                }
            })
            .await;
        if let Err(e) = res {
            eprintln!(
                "FdbSecStore::saga_update: FDB error (saga={id}, update={}): {e}",
                cached_state_str(update)
            );
        }
    }
}

// ── TritondSecStore ──────────────────────────────────────────────

#[async_trait]
impl TritondSecStore for FdbSecStore {
    async fn stamp_create(
        &self,
        saga_id: SagaId,
        name: &str,
        version: u32,
        sec: SecId,
        epoch: SecEpoch,
    ) -> SagaResult<()> {
        let by_id_key = Self::by_id_key(saga_id);
        let by_sec_key = Self::by_sec_key(sec, saga_id);
        let name = name.to_string();
        self.db
            .run(|tr, _| {
                let by_id_key = by_id_key.clone();
                let by_sec_key = by_sec_key.clone();
                let name = name.clone();
                async move {
                    let bytes = match tr.get(&by_id_key, false).await? {
                        Some(b) => b,
                        None => {
                            // saga_create hasn't landed yet (or
                            // never will because Steno errored
                            // out). Synthesise a fresh record so
                            // the operator-visible view doesn't
                            // miss this saga; future record_event
                            // calls land in the right place.
                            let now = Utc::now();
                            let rec = SagaRecord {
                                id: saga_id,
                                name: name.clone(),
                                version,
                                creator_sec: sec,
                                current_sec: sec,
                                current_epoch: epoch,
                                adopt_generation: 0,
                                dag: serde_json::Value::Null,
                                state: SagaCachedStatePersist::Running,
                                time_created: now,
                                time_done: None,
                                stuck_reason: None,
                            };
                            let value = ser_record(&rec)?;
                            tr.set(&by_id_key, &value);
                            tr.set(&by_sec_key, b"");
                            return Ok(());
                        }
                    };
                    let mut rec = deser_record(&bytes)?;
                    rec.name = name;
                    rec.version = version;
                    rec.creator_sec = sec;
                    rec.current_sec = sec;
                    rec.current_epoch = epoch;
                    let value = ser_record(&rec)?;
                    tr.set(&by_id_key, &value);
                    tr.set(&by_sec_key, b"");
                    Ok::<(), FdbBindingError>(())
                }
            })
            .await
            .map_err(backend)
    }

    async fn get_record(&self, id: SagaId) -> SagaResult<SagaRecord> {
        let by_id_key = Self::by_id_key(id);
        let bytes: Option<Vec<u8>> = self
            .db
            .run(|tr, _| {
                let by_id_key = by_id_key.clone();
                async move {
                    let v = tr.get(&by_id_key, false).await?;
                    Ok::<Option<Vec<u8>>, FdbBindingError>(v.map(|b| b.to_vec()))
                }
            })
            .await
            .map_err(backend)?;
        let bytes = bytes.ok_or(SagaError::NotFound)?;
        deser_record(&bytes).map_err(backend)
    }

    async fn load_recoverable(&self, sec: SecId) -> SagaResult<Vec<RecoverableSaga>> {
        // Step 1: scan `saga/by_sec/<sec>/` for the saga ids this SEC owns.
        let by_sec_prefix = Self::by_sec_prefix(sec);
        let (begin, end) = prefix_range(&by_sec_prefix);
        let raw_keys: Vec<Vec<u8>> = self
            .db
            .run(|tr, _| {
                let begin = begin.clone();
                let end = end.clone();
                async move {
                    let opt = RangeOption {
                        begin: KeySelector::first_greater_or_equal(begin),
                        end: KeySelector::first_greater_or_equal(end),
                        ..RangeOption::default()
                    };
                    let kvs = tr.get_range(&opt, 1, false).await?;
                    Ok::<Vec<Vec<u8>>, FdbBindingError>(
                        kvs.iter().map(|kv| kv.key().to_vec()).collect(),
                    )
                }
            })
            .await
            .map_err(backend)?;

        // Parse the trailing `<saga_uuid>` segment back out of each
        // key.
        let mut saga_ids: Vec<SagaId> = Vec::with_capacity(raw_keys.len());
        for k in raw_keys {
            let s = std::str::from_utf8(&k).map_err(backend)?;
            let Some(tail) = s.rsplit('/').next() else {
                continue;
            };
            let uuid = uuid::Uuid::parse_str(tail).map_err(backend)?;
            saga_ids.push(SagaId(uuid));
        }

        // Step 2: for each saga id, read its record + paginate the
        // event log. We page within each saga to avoid blowing the
        // 10 MB single-transaction limit on a long-running saga.
        let mut out = Vec::with_capacity(saga_ids.len());
        for saga_id in saga_ids {
            let Ok(record) = self.get_record(saga_id).await else {
                continue;
            };
            if matches!(record.state, SagaCachedStatePersist::Done) {
                continue;
            }
            let events = self.load_events_paged(saga_id).await?;
            out.push(RecoverableSaga { record, events });
        }
        Ok(out)
    }

    async fn reassign_sagas(
        &self,
        stale_secs: &[SecId],
        new_sec: SecId,
    ) -> SagaResult<Vec<RecoverableSaga>> {
        if stale_secs.is_empty() {
            return Ok(Vec::new());
        }
        let mut moved_ids: Vec<SagaId> = Vec::new();
        for stale in stale_secs.iter().copied() {
            let prefix = Self::by_sec_prefix(stale);
            let (begin, end) = prefix_range(&prefix);
            let keys: Vec<Vec<u8>> = self
                .db
                .run(|tr, _| {
                    let begin = begin.clone();
                    let end = end.clone();
                    async move {
                        let opt = RangeOption {
                            begin: KeySelector::first_greater_or_equal(begin),
                            end: KeySelector::first_greater_or_equal(end),
                            ..RangeOption::default()
                        };
                        let kvs = tr.get_range(&opt, 1, false).await?;
                        Ok::<Vec<Vec<u8>>, FdbBindingError>(
                            kvs.iter().map(|kv| kv.key().to_vec()).collect(),
                        )
                    }
                })
                .await
                .map_err(backend)?;
            for k in keys {
                let s = std::str::from_utf8(&k).map_err(backend)?;
                let Some(tail) = s.rsplit('/').next() else {
                    continue;
                };
                let Ok(uuid) = uuid::Uuid::parse_str(tail) else {
                    continue;
                };
                let saga_id = SagaId(uuid);

                // CAS the by_id record over to `new_sec` and bump
                // the epoch + adopt_generation. The CAS check (the
                // read inside the txn) lets FDB fail the
                // transaction if another SEC adopted first.
                let by_id_key = Self::by_id_key(saga_id);
                let old_by_sec = Self::by_sec_key(stale, saga_id);
                let new_by_sec = Self::by_sec_key(new_sec, saga_id);
                let moved: Result<bool, FdbBindingError> = self
                    .db
                    .run(|tr, _| {
                        let by_id_key = by_id_key.clone();
                        let old_by_sec = old_by_sec.clone();
                        let new_by_sec = new_by_sec.clone();
                        async move {
                            let Some(bytes) = tr.get(&by_id_key, false).await? else {
                                return Ok(false);
                            };
                            let mut rec = deser_record(&bytes)?;
                            if rec.current_sec != stale {
                                // Already adopted by someone else
                                // between our scan and this CAS;
                                // skip cleanly.
                                return Ok(false);
                            }
                            if matches!(rec.state, SagaCachedStatePersist::Done) {
                                tr.clear(&old_by_sec);
                                return Ok(false);
                            }
                            rec.current_sec = new_sec;
                            rec.current_epoch = rec.current_epoch.bump();
                            rec.adopt_generation = rec.adopt_generation.saturating_add(1);
                            let value = ser_record(&rec)?;
                            tr.set(&by_id_key, &value);
                            tr.clear(&old_by_sec);
                            tr.set(&new_by_sec, b"");
                            Ok(true)
                        }
                    })
                    .await;
                if matches!(moved, Ok(true)) {
                    moved_ids.push(saga_id);
                }
            }
        }
        // Now build RecoverableSaga payloads for every successfully
        // adopted saga.
        let mut out = Vec::with_capacity(moved_ids.len());
        for saga_id in moved_ids {
            let Ok(record) = self.get_record(saga_id).await else {
                continue;
            };
            let events = self.load_events_paged(saga_id).await?;
            out.push(RecoverableSaga { record, events });
        }
        Ok(out)
    }

    async fn touch_sec(&self, hb: SecHeartbeat) -> SagaResult<()> {
        let key = Self::heartbeat_key(hb.sec_id);
        let value = ser_heartbeat(&hb).map_err(backend)?;
        self.db
            .run(|tr, _| {
                let key = key.clone();
                let value = value.clone();
                async move {
                    tr.set(&key, &value);
                    Ok::<(), FdbBindingError>(())
                }
            })
            .await
            .map_err(backend)
    }

    async fn stale_secs(&self, before: DateTime<Utc>) -> SagaResult<Vec<SecId>> {
        let prefix = Self::heartbeat_prefix().to_vec();
        let (begin, end) = prefix_range(&prefix);
        let raws: Vec<Vec<u8>> = self
            .db
            .run(|tr, _| {
                let begin = begin.clone();
                let end = end.clone();
                async move {
                    let opt = RangeOption {
                        begin: KeySelector::first_greater_or_equal(begin),
                        end: KeySelector::first_greater_or_equal(end),
                        ..RangeOption::default()
                    };
                    let kvs = tr.get_range(&opt, 1, false).await?;
                    Ok::<Vec<Vec<u8>>, FdbBindingError>(
                        kvs.iter().map(|kv| kv.value().to_vec()).collect(),
                    )
                }
            })
            .await
            .map_err(backend)?;
        Ok(raws
            .into_iter()
            .filter_map(|b| deser_heartbeat(&b).ok())
            .filter(|hb| hb.at < before)
            .map(|hb| hb.sec_id)
            .collect())
    }

    async fn current_owner(&self, saga_id: SagaId) -> SagaResult<Option<(SecId, SecEpoch)>> {
        let rec = self.get_record(saga_id).await?;
        if matches!(rec.state, SagaCachedStatePersist::Done) {
            return Ok(None);
        }
        Ok(Some((rec.current_sec, rec.current_epoch)))
    }

    async fn mark_stuck(&self, saga_id: SagaId, reason: String) -> SagaResult<()> {
        let by_id_key = Self::by_id_key(saga_id);
        self.db
            .run(|tr, _| {
                let by_id_key = by_id_key.clone();
                let reason = reason.clone();
                async move {
                    let Some(bytes) = tr.get(&by_id_key, false).await? else {
                        return Ok(());
                    };
                    let mut rec = deser_record(&bytes)?;
                    rec.stuck_reason = Some(reason);
                    let value = ser_record(&rec)?;
                    tr.set(&by_id_key, &value);
                    Ok::<(), FdbBindingError>(())
                }
            })
            .await
            .map_err(backend)
    }

    async fn list_sagas(
        &self,
        marker: Option<SagaId>,
        limit: usize,
    ) -> SagaResult<Vec<SagaRecord>> {
        let prefix = Self::by_id_prefix().to_vec();
        let (mut begin, end) = prefix_range(&prefix);
        if let Some(m) = marker {
            // Step past the marker: scan from `saga/by_id/<m>\0`
            // (one byte higher than the marker's exact key).
            let mut start = Self::by_id_key(m);
            start.push(0u8);
            begin = start;
        }
        let raws: Vec<Vec<u8>> = self
            .db
            .run(|tr, _| {
                let begin = begin.clone();
                let end = end.clone();
                async move {
                    let opt = RangeOption {
                        begin: KeySelector::first_greater_or_equal(begin),
                        end: KeySelector::first_greater_or_equal(end),
                        limit: Some(limit),
                        ..RangeOption::default()
                    };
                    let kvs = tr.get_range(&opt, 1, false).await?;
                    Ok::<Vec<Vec<u8>>, FdbBindingError>(
                        kvs.iter().map(|kv| kv.value().to_vec()).collect(),
                    )
                }
            })
            .await
            .map_err(backend)?;
        Ok(raws
            .into_iter()
            .filter_map(|b| deser_record(&b).ok())
            .collect())
    }
}

impl FdbSecStore {
    /// Paginate the event log for one saga. FDB transactions cap at
    /// 10 MB; a long-running saga's log can exceed that, so we read
    /// in pages keyed by the highest `(node_id, event_kind)` seen
    /// so far. Each page is one FDB transaction.
    async fn load_events_paged(&self, saga_id: SagaId) -> SagaResult<Vec<SagaNodeEvent>> {
        const PAGE: usize = 256;
        let prefix = Self::events_prefix(saga_id);
        let (initial_begin, end) = prefix_range(&prefix);
        let mut begin = initial_begin;
        let mut out: Vec<SagaNodeEvent> = Vec::new();
        loop {
            let begin_c = begin.clone();
            let end_c = end.clone();
            let kvs: Vec<(Vec<u8>, Vec<u8>)> = self
                .db
                .run(|tr, _| {
                    let begin = begin_c.clone();
                    let end = end_c.clone();
                    async move {
                        let opt = RangeOption {
                            begin: KeySelector::first_greater_or_equal(begin),
                            end: KeySelector::first_greater_or_equal(end),
                            limit: Some(PAGE),
                            ..RangeOption::default()
                        };
                        let kvs = tr.get_range(&opt, 1, false).await?;
                        Ok::<Vec<(Vec<u8>, Vec<u8>)>, FdbBindingError>(
                            kvs.iter()
                                .map(|kv| (kv.key().to_vec(), kv.value().to_vec()))
                                .collect(),
                        )
                    }
                })
                .await
                .map_err(backend)?;
            if kvs.is_empty() {
                break;
            }
            let last_key = kvs.last().map(|(k, _)| k.clone());
            for (_, v) in &kvs {
                if let Ok(ev) = deser_event(v) {
                    out.push(ev);
                }
            }
            if kvs.len() < PAGE {
                break;
            }
            // Continue strictly past the last key we read.
            let Some(mut next) = last_key else { break };
            next.push(0u8);
            begin = next;
        }
        Ok(out)
    }
}

// ── Time helpers (unused export keeps `chrono::TimeZone` referenced
// for places where we might want it later) ─────────────────────────

#[allow(dead_code)]
fn epoch_millis(at: DateTime<Utc>) -> i64 {
    Utc.from_utc_datetime(&at.naive_utc()).timestamp_millis()
}
