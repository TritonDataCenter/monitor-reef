// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! bhyve live-migration data-plane (LM-2).
//!
//! Lifted from the legacy `vmm-migrate-agent` standalone Rust
//! binary into a tokio-async, transport-generic library so the
//! tritonagent migrate module (LM-3) can drive it over WebSocket
//! while tests use in-memory channels. The legacy agent's blocking
//! std-thread main, unix-socket control API, and pre-copy
//! convergence loop are intentionally dropped per
//! `we-need-to-build-ancient-scone.md` §F-6 (pause-first only).
//!
//! Module map:
//!
//! * [`codec`] — wire-format messages, encode/decode.
//! * [`protocol`] — constants + xxh3 hashing helper.
//! * [`vmm_dev`] — `VmmDev` trait + `mock::MockVmm`. The SmartOS
//!   ioctl-backed implementation lands as a follow-up
//!   (`smartos` module) so this slice ships review-ready.
//! * [`bhyve_ctl`] — async client for the in-zone bhyve control
//!   socket (status / pause-devices / pause-vm / drain-devices /
//!   export-state / import-state / resume-vm).
//! * [`transport`] — `Transport` trait + in-memory channel pair for
//!   loopback testing.
//! * [`state_machine`] — `OutboundMigration` (source) +
//!   `InboundMigration` (target), the two top-level state machines
//!   the tritonagent module instantiates.

pub mod bhyve_ctl;
pub mod codec;
pub mod protocol;
pub mod state_machine;
pub mod transport;
pub mod vmm_dev;
pub mod zfs_stream;

pub use codec::{DecodeError, Message, MigrationPreamble, PAGE_BATCH_FLAG_ZSTD};
pub use protocol::{
    BATCH_SIZE, HIGHMEM_BASE_GPA, PAGE_SIZE, PROTOCOL_V0, ZFS_CHUNK_SIZE, ZSTD_LEVEL,
    hash_guest_ram, hash_region,
};
pub use state_machine::{
    InboundMigration, MigrateError, NoopSourceHooks, NoopTargetHooks, OutboundMigration, Phase,
    SourceHooks, StateBlobs, TargetCaptured, TargetHooks,
};
pub use transport::{Transport, inmem};
pub use vmm_dev::{MemLayout, MemRegion, SharedVmm, VmmDev};
pub use zfs_stream::{ZfsReceiver, ZfsSender};
