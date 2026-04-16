// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! SmartOS backend: shells out to illumos-only binaries.
//!
//! Everything in this module compiles on any platform, but the commands it
//! runs (`/usr/bin/sysinfo`, `/lib/sdc/config.sh`) only exist on a SmartOS
//! compute node. Tests here operate on captured JSON fixtures so they run on
//! developer laptops without the real binaries.

pub mod config;
pub mod sysinfo;
pub mod tasks;

pub use config::{AgentConfig, SdcConfig};
pub use sysinfo::Sysinfo;
