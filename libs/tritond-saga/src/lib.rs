// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `tritond-saga` — the durable workflow engine for the Triton
//! Cloud control plane. Wraps `steno` with an FDB-backed `SecStore`,
//! the `TritondSagaType` Steno generic, and the executor `tritond`
//! drives at startup and via its sweeper. See RFD 00004.
//!
//! ## Crate posture
//!
//! Modelled on `tritond_store`: same `lib.rs` re-exports +
//! `types.rs` + `mem.rs` + (feature-gated) `fdb.rs` shape, same
//! "no HTTP, no service-level deps" rule so the crate is a clean
//! leaf in the workspace.
//!
//! ## What this crate ships
//!
//! * [`TritondSagaType`] — the `steno::SagaType` impl that fixes
//!   `ExecContextType = SagaContext`.
//! * [`SagaContext`] — what every action body receives via
//!   `ActionContext::user_data()`. Carries the SEC fencing tuple
//!   (Invariant 8) and a logger. SG-1 will enrich it with the
//!   `Store` / `AuditService` references catalog actions need.
//! * [`MemSecStore`] — in-memory `SecStore`. Always compiled in;
//!   used by unit tests, dev daemons running without FDB, and
//!   `make docker-up` without `libfdb_c`.
//! * `FdbSecStore` — behind the `foundationdb` feature; saga state
//!   under the `saga/...` prefix of the region's single FDB cluster
//!   (Locked Decision #4). Stub at SG-0; filled in at SG-1.
//! * [`SagaExecutor`] — the `tritond`-facing wrapper around
//!   `steno::SecClient` + `TritondSecStore`. Owns recovery, sweeper
//!   reassignment, and heartbeats.
//!
//! ## What it does *not* do
//!
//! * The catalog (the actual sagas) lives in
//!   `services/tritond/src/sagas/`; the crate ships only the engine.
//! * HTTP / the operation API live in `tritond-api` + `tritond`.
//! * No `vnext` in identifiers (D-Sg-7).

pub mod context;
pub mod error;
pub mod executor;
#[cfg(feature = "foundationdb")]
pub mod fdb;
pub mod mem;
pub mod secstore;
pub mod types;

pub use context::{ActionRegistry, SagaAction, SagaActionContext, SagaContext, TritondSagaType};
pub use error::{SagaError, SagaResult};
pub use executor::SagaExecutor;
pub use mem::MemSecStore;
pub use secstore::TritondSecStore;
pub use types::{
    RecoverableSaga, SagaCachedStatePersist, SagaRecord, SagaRequestCtx, SecEpoch, SecHeartbeat,
    SecId,
};

// Re-export the steno surface most call sites need so consumers
// can import everything from `tritond_saga::*` without dragging in
// the `steno` crate directly.
pub use steno::{
    ActionContext, ActionError, ActionFunc, ActionFuncResult, ActionResult, Dag, DagBuilder, Node,
    NodeName, SagaCachedState, SagaDag, SagaId, SagaName, SagaNodeEvent, SagaResult as StenoResult,
    SagaResultErr, SagaResultOk, UndoResult,
};
