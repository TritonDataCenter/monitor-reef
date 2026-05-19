// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `tritond-placement` — the VM placement engine for the Triton
//! Cloud control plane. See RFD 00005.
//!
//! ## Crate posture
//!
//! Modelled on `tritond-saga`: same `lib.rs` re-exports + `types.rs`
//! shape, same "no HTTP, no service-level deps" rule so the crate is
//! a clean leaf in the workspace.
//!
//! ## What this crate ships (PL-1, scaffolding)
//!
//! * [`Filter`] / [`Scorer`] — the two extension traits the engine
//!   composes. Pure functions over [`CnView`] /
//!   [`PlacementRequest`] / [`ChainContext`].
//! * [`Verdict`] — what a filter returns (`Accept` / `Reject` /
//!   `Skip`).
//! * [`Strategy`] — Spread / Pack / Balanced preset weight vectors
//!   applied on top of the same scorer set.
//! * [`ChainRunner`] — the two-phase (filter then score) pick loop,
//!   producing an [`ExplainReport`] for every call. PL-1 ships a
//!   trivial stub that returns the first eligible CN (no filters or
//!   scorers registered yet — PL-3 + PL-4 fill them in).
//! * [`CnView`] / [`PlacementRequest`] / [`PlacementConfig`] — the
//!   shapes filters and scorers operate on, plus the FDB-backed
//!   cluster-settings shape that names the active chain and weights.
//!
//! ## What it does *not* do (yet)
//!
//! * No built-in filters or scorers — those land in PL-3 / PL-4
//!   under [`filter`] and [`scorer`].
//! * No `designate` saga action — that lives in
//!   `services/tritond/src/sagas/designate.rs` and lands in PL-5.
//! * No ClickHouse load materialiser — that lives in
//!   [`load_materializer`] behind the `materializer` cargo feature
//!   and lands in PL-6.
//! * No `tritond-store` path dep yet. PL-1's [`CnView`] embeds
//!   placement-engine projection types defined here; PL-2 adds the
//!   canonical FDB row shapes (`CnCapacity`, `CnPlacement`,
//!   `CnReservation`, `CnLoadSummary`, `InstanceAffinity`) to
//!   `tritond-store` and provides Store methods that produce a
//!   `CnView` projection from them. The engine reads the projection,
//!   not the raw rows, so the trait surface here is stable across
//!   that addition.
//! * No `vnext` in identifiers (D-Pl-8).

pub mod config;
pub mod engine;
pub mod filter;
#[cfg(feature = "materializer")]
pub mod load_materializer;
pub mod scorer;
pub mod types;

pub use config::{
    MaterialiserConfig, OverprovisionDefaults, PlacementConfig, ScorerConfig, strategy_weights,
};
pub use engine::{ChainRunner, ExplainPerCn, ExplainReport, ScorerContribution};
pub use filter::{
    CnAffinityRequired, CnApprovedAndLive, CnCapacityPresent, CnCpuAvailable, CnDeviceAvailable,
    CnHvmSupported, CnLoadNotOverheating, CnNicTags, CnNotCordoned, CnNotEvacuating, CnNotReserved,
    CnNumaFits, CnPlatformMin, CnRamAvailable, CnRoleMatches, CnScopeMatch, CnTraitsRequired,
    CnUnderlay, CnZpoolHasSpace, default_filter_chain,
};
pub use types::{
    AssignedInstanceView, CapacityView, ChainContext, CnLoadSummaryView, CnRoleView, CnStateView,
    CnView, DeviceKind, DeviceView, Filter, NumaNodeView, PlacementPolicyView, PlacementRequest,
    ReservationView, Scorer, SiblingInstanceView, Strategy, StrategyWeights, UnderlayCapability,
    Verdict, ZpoolView,
};
