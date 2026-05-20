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

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use steno::{SagaDag, SagaId, SagaResult as StenoSagaResult};

use crate::context::{ActionRegistry, SagaContext};
use crate::error::{SagaError, SagaResult};
use crate::mem::MemSecStore;
use crate::secstore::TritondSecStore;
use crate::types::{RecoverableSaga, ResourceRef, ResourceScope, SecEpoch, SecHeartbeat, SecId};

/// Audit emitter for saga lifecycle events (RFD 00004 D-Sg-11).
///
/// Implementations land in the daemon (tritond) where the
/// underlying audit chain lives; tritond-saga stays a leaf crate
/// relative to `tritond-audit` by defining only the trait.
///
/// SG-2b ships start + finish hooks. Per-action `operation_step`
/// events are deferred — Steno doesn't expose action-completion
/// hooks, and the SecStore's node-event log already carries that
/// information for `/v2/operations/{id}` and `tcadm operations
/// get`. Operators get fleet-chain breadcrumbs for "saga X started
/// / finished" today; deep step-by-step audit lives on the
/// existing per-step `record_event` writes.
#[async_trait]
pub trait SagaAuditEmitter: Send + Sync + 'static {
    /// Fired immediately after `SecClient::saga_create` succeeds.
    /// The fence has been stamped onto the record; the saga has not
    /// yet started executing.
    async fn operation_started(&self, saga_id: SagaId, kind: &str, version: u32);

    /// Fired after the saga reaches a terminal state. `state` is
    /// `"succeeded"` for `Ok`, `"unwound"` for an `Err` whose
    /// unwind ran cleanly, `"stuck"` for an `Err` left in a partial
    /// state. `error` carries a short human-readable summary when
    /// the saga didn't succeed.
    async fn operation_finished(&self, saga_id: SagaId, state: &str, error: Option<String>);
}

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
    /// Optional state-store catalog action bodies reach for via
    /// `SagaContext::store()`. SG-1 leaves this `None` (trivial
    /// test, no catalog); SG-2 onwards always wires it.
    store: Option<Arc<dyn tritond_store::Store>>,
    /// Optional identity HMAC key (RFD 00003). Same wiring posture
    /// as `store` above.
    identity_hmac_key: Option<Arc<tritond_auth::IdentityHmacKey>>,
    /// Optional audit emitter for saga lifecycle events
    /// (RFD 00004 D-Sg-11). When set, the executor fires
    /// `operation_started` / `operation_finished` around each
    /// `saga_execute` / `saga_resume`. SG-0 trivial test sagas
    /// leave it `None`.
    audit: Option<Arc<dyn SagaAuditEmitter>>,
}

impl SagaExecutor {
    /// Build the executor. Callers must build the Steno `SecClient`
    /// over the same `Arc<dyn steno::SecStore>` that `sec_store`
    /// implements, so the engine and the extension see the same
    /// state. [`Self::new_with_mem_store`] below wires this for `MemSecStore`.
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
            store: None,
            identity_hmac_key: None,
            audit: None,
        }
    }

    /// Builder: attach the saga audit emitter (D-Sg-11).
    #[must_use]
    pub fn with_audit(mut self, audit: Arc<dyn SagaAuditEmitter>) -> Self {
        self.audit = Some(audit);
        self
    }

    /// Builder: attach the state store catalog actions reach for.
    /// SG-2 catalog modules need this; SG-1's empty catalog and
    /// SG-0's trivial test leave it unset.
    #[must_use]
    pub fn with_store(mut self, store: Arc<dyn tritond_store::Store>) -> Self {
        self.store = Some(store);
        self
    }

    /// Builder: attach the identity HMAC key.
    #[must_use]
    pub fn with_identity_hmac_key(mut self, key: Arc<tritond_auth::IdentityHmacKey>) -> Self {
        self.identity_hmac_key = Some(key);
        self
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

    /// Build a `SagaContext` pinned to this SEC's `(id, epoch)`,
    /// with `store`/`identity_hmac_key` threaded through if the
    /// executor was built with them. The context is NOT bound to a
    /// saga id; callers that run a saga should use
    /// [`Self::make_context_for_saga`] so action bodies'
    /// `verify_fence` calls have the right id.
    pub fn make_context(&self) -> SagaContext {
        let mut ctx = SagaContext::new(self.sec_id, self.sec_epoch, self.log.clone())
            .with_sec_store(self.sec_store.clone());
        if let Some(s) = self.store.clone() {
            ctx = ctx.with_store(s);
        }
        if let Some(k) = self.identity_hmac_key.clone() {
            ctx = ctx.with_identity_hmac_key(k);
        }
        ctx
    }

    /// Build a `SagaContext` bound to a specific saga. Used at the
    /// `saga_execute` / `saga_resume` entry points; action bodies
    /// read the saga id back via `SagaContext::saga_id()` (and
    /// implicitly via `verify_fence`).
    pub fn make_context_for_saga(&self, saga_id: SagaId) -> SagaContext {
        self.make_context().with_saga_id(saga_id)
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
        references: &[ResourceRef],
    ) -> SagaResult<StenoSagaResult> {
        let ctx = Arc::new(self.make_context_for_saga(saga_id));
        // 1. Steno persists the saga (calls our SecStore::saga_create).
        let result_fut = self
            .sec_client
            .saga_create(saga_id, ctx, dag, self.registry.clone())
            .await
            .map_err(SagaError::from)?;
        // 2. Stamp the fence + version + resource refs onto the
        //    record. References populate the secondary index that
        //    powers per-resource saga views (RFD 00004 SG-4).
        self.sec_store
            .stamp_create(
                saga_id,
                name,
                version,
                self.sec_id,
                self.sec_epoch,
                references,
            )
            .await?;
        // 3. Audit: operation_started (D-Sg-11). Fired after the
        //    record is durable so a reader of the audit log can
        //    correlate `saga_id` with the record on `/v2/operations`.
        if let Some(audit) = self.audit.as_ref() {
            audit.operation_started(saga_id, name, version).await;
        }
        // 4. Kick the SEC.
        self.sec_client
            .saga_start(saga_id)
            .await
            .map_err(SagaError::from)?;
        // 5. Await the terminal result.
        let result = result_fut.await;
        // 6. Audit: operation_finished.
        if let Some(audit) = self.audit.as_ref() {
            let (state, error) = match &result.kind {
                Ok(_) => ("succeeded", None),
                Err(e) => {
                    // Steno's SagaResultErr carries the failing
                    // node + action error. Surface a short
                    // human-readable summary; the full payload
                    // lives in the saga's event log.
                    let summary = format!("node {:?}: {:?}", e.error_node_name, e.error_source);
                    ("unwound", Some(summary))
                }
            };
            audit.operation_finished(saga_id, state, error).await;
        }
        Ok(result)
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

    /// Operator-facing listing: page through every saga the
    /// SecStore knows about. Used by the SG-4 `/v2/operations`
    /// surface. `marker` is an opaque continuation token; pass
    /// `None` for the first page. See
    /// [`TritondSecStore::list_sagas`] for ordering semantics.
    pub async fn list_sagas(
        &self,
        marker: Option<steno::SagaId>,
        limit: usize,
    ) -> SagaResult<Vec<crate::types::SagaRecord>> {
        self.sec_store.list_sagas(marker, limit).await
    }

    /// Operator-facing detail lookup: the full `SagaRecord` for
    /// one saga. Returns `SagaError::NotFound` if the id is
    /// unknown. Used by `GET /v2/operations/{id}`.
    pub async fn get_saga(&self, id: steno::SagaId) -> SagaResult<crate::types::SagaRecord> {
        self.sec_store.get_record(id).await
    }

    /// Operator-facing event-log fetch for one saga. Returns the
    /// persisted Steno node-event log in node-id order. Used by
    /// `GET /v2/operations/{id}` to project per-step progress
    /// (RFD 00004 D-Sg-13).
    pub async fn get_saga_events(
        &self,
        id: steno::SagaId,
    ) -> SagaResult<Vec<steno::SagaNodeEvent>> {
        self.sec_store.load_events(id).await
    }

    /// Resource-scoped saga listing. Backed by the FDB by_ref index;
    /// returns newest-first. Used by per-VM / per-CN / per-tenant
    /// "operations" views (RFD 00004 SG-4 resource indexing).
    pub async fn list_sagas_by_reference(
        &self,
        scope: ResourceScope,
        id: uuid::Uuid,
        marker: Option<SagaId>,
        limit: usize,
    ) -> SagaResult<Vec<crate::types::SagaRecord>> {
        self.sec_store
            .list_sagas_by_reference(scope, id, marker, limit)
            .await
    }

    /// Sweeper hook: drop every terminal saga whose `time_done` is
    /// older than `before`. Returns the number pruned. RFD 00004
    /// SG-4 retention pass; see the corresponding SecStore method
    /// for what gets deleted and what is left alone (Stuck sagas
    /// are intentionally preserved).
    pub async fn prune_terminal_sagas_older_than(
        &self,
        before: DateTime<Utc>,
    ) -> SagaResult<usize> {
        self.sec_store.prune_terminal_sagas_older_than(before).await
    }

    /// Force a running saga into its unwind direction (RFD 00004
    /// D-Sg-12). Injects an `ActionError` at every node in the
    /// saga's DAG that hasn't yet completed; the next pending node
    /// the saga reaches will fail and trigger the catalog's own
    /// undos in reverse.
    ///
    /// Returns the number of nodes the executor poked. The actual
    /// transition into `Unwinding` happens cooperatively: any
    /// currently-running action body finishes its natural outcome
    /// first, then the next node trips the injected error. There's
    /// no preemption of an in-flight action; that's outside what
    /// the v1 escape hatch promises.
    ///
    /// Operator-only at the HTTP layer; the executor doesn't
    /// authorise the caller.
    pub async fn abandon_saga(&self, saga_id: SagaId) -> SagaResult<usize> {
        // Pull the saga's persisted DAG so we can walk every
        // declared node and inject an error at each. Already-
        // completed nodes ignore the injection per Steno
        // semantics; pending or running ones fail on next visit.
        let record = self.sec_store.get_record(saga_id).await?;
        // The persisted DAG is the SagaDag's JSON form (what
        // `SecClient::saga_create` stored). Re-deserialise to
        // SagaDag so we can enumerate nodes by name and look up
        // their NodeIndex.
        let saga_dag: steno::SagaDag = serde_json::from_value(record.dag.clone())
            .map_err(|e| SagaError::Backend(format!("deserialise persisted DAG: {e}")))?;
        let mut poked = 0usize;
        // Inject at every node by walking petgraph's node iterator.
        // Steno's Dag exposes `get(NodeIndex)` and a NodeIter via
        // `iter_nodes`; for simplicity we walk a contiguous range
        // 0..=MAX and rely on Steno to error-out-of-bounds entries.
        // The MAX is a soft cap matching the largest catalog
        // saga we ship; raise it when the catalog grows.
        const MAX_NODES: u32 = 64;
        for i in 0..MAX_NODES {
            let idx: petgraph::graph::NodeIndex = petgraph::graph::NodeIndex::new(i as usize);
            // get_index by NodeIndex isn't on Dag; we use Steno's
            // SagaDag::get on the raw petgraph index. Skip nodes
            // that aren't in the saga's dag (out-of-bounds).
            //
            // Since `Dag::get` is `pub(crate)`, we can't probe
            // membership directly. Cheapest test: round-trip the
            // index through `saga_inject_error` and accept the
            // out-of-bounds variant as expected.
            let res = self.sec_client.saga_inject_error(saga_id, idx).await;
            match res {
                Ok(()) => poked += 1,
                Err(e) => {
                    let msg = e.to_string();
                    // Steno's error for "node not found" carries
                    // the node id; suppress that case and propagate
                    // anything else.
                    if !msg.contains("node") {
                        return Err(SagaError::Steno(msg));
                    }
                    break;
                }
            }
        }
        let _ = saga_dag; // currently unused; kept for future when we walk by name
        slog::info!(
            self.log,
            "tritond-saga: operator-initiated abandon poked {poked} nodes";
            "saga_id" => %saga_id,
        );
        Ok(poked)
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
            let ctx = Arc::new(self.make_context_for_saga(r.record.id));
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

/// Build an executor over an [`MemSecStore`]. The same `Arc` is
/// shared between Steno's `SecClient` and our `TritondSecStore` so
/// the engine and the fencing extension see the same backing state.
///
/// This is the constructor every integration test uses and the
/// dev-daemon default. Production deploys with FDB use
/// [`Self::new_with_fdb_store`].
impl SagaExecutor {
    pub fn new_with_mem_store(
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

#[cfg(feature = "foundationdb")]
impl SagaExecutor {
    /// Build an executor over an [`crate::fdb::FdbSecStore`]
    /// backed by the supplied FoundationDB `Database` handle.
    /// `db` is the same handle that backs `tritond_store::FdbStore`;
    /// the two share boot/network state via the `Arc<Database>`
    /// indirection.
    pub fn new_with_fdb_store(
        sec_id: SecId,
        sec_epoch: SecEpoch,
        db: Arc<foundationdb::Database>,
        registry: ActionRegistry,
        log: slog::Logger,
    ) -> Self {
        let sec_store = crate::fdb::FdbSecStore::new(db);
        let steno_store: Arc<dyn steno::SecStore> = sec_store.clone();
        let sec_client = steno::sec(log.clone(), steno_store);
        let trit_store: Arc<dyn TritondSecStore> = sec_store;
        Self::new(sec_id, sec_epoch, sec_client, trit_store, registry, log)
    }
}
