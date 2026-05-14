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
    log: slog::Logger,
}

impl SagaContext {
    pub fn new(sec_id: SecId, sec_epoch: SecEpoch, log: slog::Logger) -> Self {
        Self {
            inner: Arc::new(SagaContextInner {
                sec_id,
                sec_epoch,
                log,
            }),
        }
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

    /// Build the fencing tuple action bodies pass into store
    /// mutations and `enqueue_job` calls.
    pub fn request_ctx(&self, saga_id: SagaId) -> SagaRequestCtx {
        SagaRequestCtx::new(saga_id, self.inner.sec_id, self.inner.sec_epoch)
    }
}

impl std::fmt::Debug for SagaContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SagaContext")
            .field("sec_id", &self.inner.sec_id)
            .field("sec_epoch", &self.inner.sec_epoch)
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
