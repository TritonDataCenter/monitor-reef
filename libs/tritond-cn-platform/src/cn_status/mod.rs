// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! CN-status collection: builds the periodic status payload (vmadm + zfs +
//! kstat + sysinfo + disk-usage accounting) and drives the heartbeat /
//! status loops. The transport is pluggable via [`StatusSink`] so this
//! crate stays independent of any specific control-plane HTTP client.

pub mod collector;
pub mod disk_usage;
pub mod heartbeater;
pub mod watchers;

pub use collector::{LiveSysinfo, StatusCollector, StatusReport, SysinfoLoader};
pub use disk_usage::{
    AcceptAllImageFilter, DiskUsage, DiskUsageError, DiskUsageSampler, ImageDatasetFilter,
    UuidNamedImageFilter, VmSnapshot,
};
pub use heartbeater::{
    HEARTBEAT_INTERVAL, Heartbeater, HeartbeaterHandle, STATUS_CHECK_INTERVAL,
    STATUS_MAX_INTERVAL, StatusSink, StatusSinkError,
};
pub use watchers::{DirtyFlag, ZoneeventWatcher};
