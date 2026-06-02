// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! SmartOS host-tool adapters: thin wrappers around `vmadm`, `zoneadm`,
//! `zfs`/`zpool`, `kstat`, and `/usr/bin/sysinfo`. Each wrapper carries
//! the binary path so tests can swap in mock scripts.

pub mod disks;
pub mod kstat;
pub mod reservoir;
pub mod sysinfo;
pub mod vmadm;
pub mod zfs;
pub mod zoneadm;

pub use disks::{DiskHealth, DiskTool};
pub use kstat::KstatTool;
pub use reservoir::{ReservoirState, ReservoirTool};
pub use sysinfo::Sysinfo;
pub use vmadm::VmadmTool;
pub use zfs::ZfsTool;
pub use zoneadm::{ZoneInfo, ZoneadmTool};
