// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `SagaExecutor` — the `tritond`-facing wrapper around Steno's
//! `SecClient` + our `TritondSecStore`.
//!
//! Responsibilities:
//!
//! * Build the SEC at startup (`new`), register the catalog of
//!   actions, take an `Arc<dyn TritondSecStore>` and Steno's
//!   `SecClient` over the same underlying store.
//! * Drive a saga end-to-end (`saga_execute`) — used by SG-2's
//!   synchronous handler entry; SG-4 will add the
//!   fire-and-forget `saga_start` variant returning an operation
//!   handle.
//! * Recover this SEC's unfinished sagas at startup
//!   (`recover_all_for_sec`).
//! * Drive a sweeper-style pass that adopts stale SECs' sagas
//!   (`reassign_stale_sec_sagas`).
//! * Heartbeat write (`touch_heartbeat`).

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use steno::{SagaDag, SagaId, SagaResult as StenoSagaResult};

use crate::context::{ActionRegistry, SagaContext};
use crate::error::{SagaError, SagaResult};
use crate::mem::MemSecStore;
use crate::secstore::TritondSecStore;
use crate::types::{RecoverableSaga, SecEpoch, SecHeartbeat, SecId};

/// The `tritond`-side wrapper around Steno's `SecClient`.
pub struct SagaExecutor {
    sec_id: SecId,
    sec_epoch: SecEpoch,
    sec_client: steno::SecClient,
    sec_store: Arc<dyn TritondSecStore>,
    registry: Arc<ActionRegistry>,
    /// Catalog: `(name, version)` pairs the executor will accept on
    /// recovery. Builders call `register_saga_version` per catalog
    /// module. D-Sg-10.
    saga_versions: HashMap<&'static str, u32>,
    log: slog::Logger,
}

impl SagaExecutor {
    /// Build the executor. Callers must build the Steno `SecClient`
    /// over the same `Arc<dyn steno::SecStore>` that `sec_store`
    /// implements, so the engine and the extension see the same
    /// state. `new_for_test` below wires this for `MemSecStore`.
    pub fn new(
        sec_id: SecId,
        sec_epoch: SecEpoch,
        sec_client: steno::SecClient,
        sec_store: Arc<dyn TritondSecStore>,
        registry: ActionRegistry,
        log: slog::Logger,
    ) -> Self {
        Self {
            sec_id,
            sec_epoch,
            sec_client,
            sec_store,
            registry: Arc::new(registry),
            saga_versions: HashMap::new(),
            log,
        }
    }

    /// Tell the executor "this catalog module is registered at this
    /// version" (D-Sg-10). Recovery rejects sagas whose persisted
    /// `(name, version)` isn't in this table.
    pub fn register_saga_version(&mut self, name: &'static str, version: u32) {
        self.saga_versions.insert(name, version);
    }

    pub fn sec_id(&self) -> SecId {
        self.sec_id
    }

    pub fn sec_epoch(&self) -> SecEpoch {
        self.sec_epoch
    }

    pub fn log(&self) -> &slog::Logger {
        &self.log
    }

    /// Build a `SagaContext` pinned to this SEC's `(id, epoch)`.
    /// SG-1 will replace this with the richer `tritond` context
    /// bundle (Store / AuditService / clients) but the fencing
    /// fields stay the same.
    pub fn make_context(&self) -> SagaContext {
        SagaContext::new(self.sec_id, self.sec_epoch, self.log.clone())
    }

    /// Run a saga end-to-end: persist the create, stamp the fence,
    /// start it, await the terminal result. Synchronous handler
    /// path used by SG-2; SG-4 will add `saga_start`
    /// (fire-and-forget) that returns the operation handle
    /// immediately.
    pub async fn saga_execute(
        &self,
        saga_id: SagaId,
        name: &'static str,
        version: u32,
        dag: Arc<SagaDag>,
    ) -> SagaResult<StenoSagaResult> {
        let ctx = Arc::new(self.make_context());
        // 1. Steno persists the saga (calls our SecStore::saga_create).
        let result_fut = self
            .sec_client
            .saga_create(saga_id, ctx, dag, self.registry.clone())
            .await
            .map_err(SagaError::from)?;
        // 2. Stamp the fence + version onto the record.
        self.sec_store
            .stamp_create(saga_id, name, version, self.sec_id, self.sec_epoch)
            .await?;
        // 3. Kick the SEC.
        self.sec_client
            .saga_start(saga_id)
            .await
            .map_err(SagaError::from)?;
        // 4. Await the terminal result.
        Ok(result_fut.await)
    }

    /// Load every not-`Done` saga this SEC owns, resume each
    /// through Steno, return the count resumed. A saga whose
    /// persisted `(name, version)` isn't registered is marked
    /// Stuck and skipped (D-Sg-10).
    pub async fn recover_all_for_sec(&self) -> SagaResult<usize> {
        let recoverables = self.sec_store.load_recoverable(self.sec_id).await?;
        self.resume_many(recoverables).await
    }

    /// Sweeper hook: find every SEC whose heartbeat is older than
    /// `before`, CAS its sagas over to *this* SEC (bumping the
    /// fence epoch on each), and resume them.
    pub async fn reassign_stale_sec_sagas(&self, before: DateTime<Utc>) -> SagaResult<usize> {
        let stale = self.sec_store.stale_secs(before).await?;
        if stale.is_empty() {
            return Ok(0);
        }
        let moved = self.sec_store.reassign_sagas(&stale, self.sec_id).await?;
        self.resume_many(moved).await
    }

    /// Heartbeat-write the local SEC's `(epoch, now)`. The
    /// heartbeat task in SG-1 calls this on a cadence.
    pub async fn touch_heartbeat(&self) -> SagaResult<()> {
        self.sec_store
            .touch_sec(SecHeartbeat {
                sec_id: self.sec_id,
                epoch: self.sec_epoch,
                at: Utc::now(),
            })
            .await
    }

    async fn resume_many(&self, sagas: Vec<RecoverableSaga>) -> SagaResult<usize> {
        let mut resumed = 0usize;
        for r in sagas {
            let known = self.saga_versions.get(r.record.name.as_str()).copied();
            match known {
                Some(v) if v == r.record.version => { /* fall through */ }
                _ => {
                    let reason = format!(
                        "version not registered: {}@{} (N=2 deprecation window does not cover this version)",
                        r.record.name, r.record.version
                    );
                    self.sec_store.mark_stuck(r.record.id, reason).await?;
                    slog::warn!(
                        self.log,
                        "saga version not registered, leaving Stuck";
                        "saga_id" => %r.record.id,
                        "name" => %r.record.name,
                        "version" => r.record.version,
                    );
                    continue;
                }
            }
            let ctx = Arc::new(self.make_context());
            let _resume_fut = self
                .sec_client
                .saga_resume(
                    r.record.id,
                    ctx,
                    r.record.dag.clone(),
                    self.registry.clone(),
                    r.events,
                )
                .await
                .map_err(SagaError::from)?;
            self.sec_client
                .saga_start(r.record.id)
                .await
                .map_err(SagaError::from)?;
            resumed += 1;
        }
        Ok(resumed)
    }
}

/// Test-only constructor that wires a `MemSecStore` to both Steno's
/// `SecClient` and our `TritondSecStore` from the same `Arc`, so the
/// engine and the fencing extension see the same backing state.
impl SagaExecutor {
    pub fn new_for_test(
        sec_id: SecId,
        sec_epoch: SecEpoch,
        sec_store: Arc<MemSecStore>,
        registry: ActionRegistry,
        log: slog::Logger,
    ) -> Self {
        let steno_store: Arc<dyn steno::SecStore> = sec_store.clone();
        let sec_client = steno::sec(log.clone(), steno_store);
        let trit_store: Arc<dyn TritondSecStore> = sec_store;
        Self::new(sec_id, sec_epoch, sec_client, trit_store, registry, log)
    }
}
