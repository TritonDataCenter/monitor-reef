// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Per-VM log tail primitives shared by tritond and tritonagent.
//!
//! Two log sources per zone, both files under `/zones/<uuid>/logs/`
//! on the SmartOS host:
//!
//! * `console.log`  -- the guest's serial console output. For KVM
//!   and bhyve VMs this is whatever the guest writes to the virtual
//!   serial port; for LX zones it's the init system's stdout.
//! * `platform.log` -- the SmartOS platform's log about the zone
//!   (boot transitions, vmadm output, sysidcfg processing, etc.).
//!
//! Architecture parallels `tritond-metrics`:
//!
//! * tritonagent's `log_tailer` reads new bytes from each file
//!   every few seconds, parses lines, batches them, and POSTs to
//!   tritond's `/v2/agent/logs` ingest endpoint.
//! * tritond's [`LogStore`] consumes batches and answers tail-read
//!   queries scoped to a `(instance_id, source)` pair.
//! * Two backends: an in-memory ring buffer (default, dev) and a
//!   future ClickHouse sink (for retention beyond ring capacity).
//!
//! This crate stays lean -- no Dropshot, no HTTP -- so it's safe
//! for both client (agent) and server (tritond) to depend on.

#![forbid(unsafe_code)]

pub mod store;
pub mod types;

pub use store::{LogStore, LogStoreError, LogTailQuery, LogTailResult, RingBufferLogStore};
pub use types::{LogBatch, LogLine, LogSource};
