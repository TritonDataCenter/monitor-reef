// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! The dependency bundle every saga action receives via
//! `ActionContext::user_data()`.
//!
//! For SG-0 the context only carries the SEC fencing tuple and a
//! logger; SG-1 will enrich it with `Arc<dyn tritond_store::Store>`,
//! `Arc<tritond_audit::AuditService>`, etc. Keeping SG-0's context
//! minimal lets `tritond-saga` stay a true leaf crate (no dep on
//! `tritond-store`) and lets us test the engine end-to-end with no
//! tritond build dependencies.

use std::sync::Arc;

use steno::SagaId;
use tritond_auth::IdentityHmacKey;
use tritond_store::Store;

use crate::error::{SagaError, SagaResult};
use crate::secstore::TritondSecStore;
use crate::types::{SagaRequestCtx, SecEpoch, SecId};

/// Inner state shared by all action bodies for a given saga. Cheap
/// to clone (one `Arc`).
#[derive(Clone)]
pub struct SagaContext {
    inner: Arc<SagaContextInner>,
}

struct SagaContextInner {
    /// This `tritond` instance's stable SEC id.
    sec_id: SecId,
    /// Current fence epoch, captured at SagaContext build time. The
    /// executor rebuilds `SagaContext` whenever the SEC adopts new
    /// sagas, so the value here is the latest known epoch for *this*
    /// SEC, suitable for stamping outbound writes.
    sec_epoch: SecEpoch,
    /// The saga this context is bound to. The executor builds a
    /// fresh context per saga at `saga_execute` / resume time so
    /// each action knows which saga it belongs to. SG-0 trivial
    /// test sagas pass `None`.
    saga_id: Option<SagaId>,
    /// SecStore handle used by `verify_fence` to read the saga's
    /// current owner. SG-0 trivial test passes `None`; SG-2 catalog
    /// modules carry the real handle through the executor.
    sec_store: Option<Arc<dyn TritondSecStore>>,
    /// State store catalog actions reach for. SG-0's trivial test
    /// passes `None`; SG-2 onwards always carry a real handle.
    store: Option<Arc<dyn Store>>,
    /// HMAC key the blueprint identity stamping uses (RFD 00003).
    /// SG-0 trivial test passes `None`; SG-2 catalog modules carry
    /// the real key through.
    identity_hmac_key: Option<Arc<IdentityHmacKey>>,
    log: slog::Logger,
}

impl SagaContext {
    pub fn new(sec_id: SecId, sec_epoch: SecEpoch, log: slog::Logger) -> Self {
        Self {
            inner: Arc::new(SagaContextInner {
                sec_id,
                sec_epoch,
                saga_id: None,
                sec_store: None,
                store: None,
                identity_hmac_key: None,
                log,
            }),
        }
    }

    /// Builder: bind this context to a specific saga. The executor
    /// calls this once per `saga_execute` / `saga_resume` so each
    /// action gets a context whose `saga_id()` is its own.
    #[must_use]
    pub fn with_saga_id(mut self, saga_id: SagaId) -> Self {
        let inner = Arc::make_mut(&mut self.inner);
        inner.saga_id = Some(saga_id);
        self
    }

    /// Builder: attach the SecStore handle used by [`Self::verify_fence`]
    /// to check whether this action's fence is still current.
    #[must_use]
    pub fn with_sec_store(mut self, sec_store: Arc<dyn TritondSecStore>) -> Self {
        let inner = Arc::make_mut(&mut self.inner);
        inner.sec_store = Some(sec_store);
        self
    }

    /// Builder: attach the state store catalog actions need.
    #[must_use]
    pub fn with_store(mut self, store: Arc<dyn Store>) -> Self {
        let inner = Arc::make_mut(&mut self.inner);
        inner.store = Some(store);
        self
    }

    /// Builder: attach the identity HMAC key.
    #[must_use]
    pub fn with_identity_hmac_key(mut self, key: Arc<IdentityHmacKey>) -> Self {
        let inner = Arc::make_mut(&mut self.inner);
        inner.identity_hmac_key = Some(key);
        self
    }

    pub fn sec_id(&self) -> SecId {
        self.inner.sec_id
    }

    pub fn sec_epoch(&self) -> SecEpoch {
        self.inner.sec_epoch
    }

    pub fn log(&self) -> &slog::Logger {
        &self.inner.log
    }

    /// The state store. Panics if `with_store` was never called;
    /// catalog action bodies that reach for `store` are always run
    /// from a `tritond`-built executor that wires it.
    pub fn store(&self) -> &Arc<dyn Store> {
        self.inner
            .store
            .as_ref()
            .unwrap_or_else(|| panic!("SagaContext::store called without with_store"))
    }

    /// The identity HMAC key. See [`Self::store`] for the
    /// "always-wired in production" invariant.
    pub fn identity_hmac_key(&self) -> &Arc<IdentityHmacKey> {
        self.inner.identity_hmac_key.as_ref().unwrap_or_else(|| {
            panic!("SagaContext::identity_hmac_key called without with_identity_hmac_key")
        })
    }

    /// Build the fencing tuple action bodies pass into store
    /// mutations and `enqueue_job` calls.
    pub fn request_ctx(&self, saga_id: SagaId) -> SagaRequestCtx {
        SagaRequestCtx::new(saga_id, self.inner.sec_id, self.inner.sec_epoch)
    }

    /// The saga this context is bound to. Panics if the context
    /// was built without a saga id (only the SG-0 trivial test
    /// path); production catalog actions always have one.
    pub fn saga_id(&self) -> SagaId {
        self.inner
            .saga_id
            .unwrap_or_else(|| panic!("SagaContext::saga_id called without with_saga_id"))
    }

    /// Best-effort fence check (RFD 00004 D-Sg-8 / Invariant 8).
    ///
    /// Reads the saga's current owner from the SecStore and
    /// compares to this context's `(sec_id, sec_epoch)`. Returns
    /// `Err(SagaError::FencedOut)` when another SEC has adopted
    /// the saga between this context's build time and this call.
    ///
    /// Catalog actions call this immediately before any
    /// externally-visible side effect (`store.enqueue_job`,
    /// `store.create_*`, …) to keep the fence-violation window
    /// small. The window between `verify_fence` and the side
    /// effect is unavoidable without threading the fence ctx into
    /// every Store mutation and embedding the check in the same
    /// FDB transaction as the write; that deeper refactor is
    /// tracked as a follow-up. For the current race profile
    /// (heartbeat-driven reassignment, short action bodies) this
    /// best-effort check closes the operationally-realistic
    /// fraction of the window.
    pub async fn verify_fence(&self) -> SagaResult<()> {
        let (Some(saga_id), Some(sec_store)) = (self.inner.saga_id, self.inner.sec_store.as_ref())
        else {
            // Context wasn't built with fencing wired (SG-0 trivial
            // test path). Treat as a no-op so the test surface
            // doesn't have to build a real SecStore just to call
            // verify_fence.
            return Ok(());
        };
        let owner = sec_store.current_owner(saga_id).await?;
        let Some((actual_sec, actual_epoch)) = owner else {
            // Terminal saga: no live owner. The unwind tail
            // shouldn't fire further side effects, so treat this
            // as fenced.
            return Err(SagaError::FencedOut {
                saga_id: saga_id.to_string(),
                expected_sec: self.inner.sec_id.to_string(),
                expected_epoch: self.inner.sec_epoch.0,
                actual_sec: "<terminal>".to_string(),
                actual_epoch: 0,
            });
        };
        if actual_sec != self.inner.sec_id || actual_epoch != self.inner.sec_epoch {
            return Err(SagaError::FencedOut {
                saga_id: saga_id.to_string(),
                expected_sec: self.inner.sec_id.to_string(),
                expected_epoch: self.inner.sec_epoch.0,
                actual_sec: actual_sec.to_string(),
                actual_epoch: actual_epoch.0,
            });
        }
        Ok(())
    }
}

impl Clone for SagaContextInner {
    fn clone(&self) -> Self {
        Self {
            sec_id: self.sec_id,
            sec_epoch: self.sec_epoch,
            saga_id: self.saga_id,
            sec_store: self.sec_store.clone(),
            store: self.store.clone(),
            identity_hmac_key: self.identity_hmac_key.clone(),
            log: self.log.clone(),
        }
    }
}

impl std::fmt::Debug for SagaContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SagaContext")
            .field("sec_id", &self.inner.sec_id)
            .field("sec_epoch", &self.inner.sec_epoch)
            .field("saga_id", &self.inner.saga_id)
            .field("has_sec_store", &self.inner.sec_store.is_some())
            .field("has_store", &self.inner.store.is_some())
            .field(
                "has_identity_hmac_key",
                &self.inner.identity_hmac_key.is_some(),
            )
            .finish()
    }
}

/// The Steno `SagaType` for the `tritond-saga` catalog. Fixed once;
/// every saga the engine runs uses this type.
#[derive(Debug)]
pub struct TritondSagaType;

impl steno::SagaType for TritondSagaType {
    type ExecContextType = SagaContext;
}

/// Type alias for action bodies: `async fn foo(ctx: SagaActionContext) -> ...`.
pub type SagaActionContext = steno::ActionContext<TritondSagaType>;

/// Type alias for `Arc<dyn Action<TritondSagaType>>` so call sites
/// in the catalog stay short.
pub type SagaAction = Arc<dyn steno::Action<TritondSagaType>>;

/// Type alias for the action registry the executor builds at start
/// up.
pub type ActionRegistry = steno::ActionRegistry<TritondSagaType>;
