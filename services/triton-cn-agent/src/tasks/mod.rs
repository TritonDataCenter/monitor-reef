// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Task handler implementations.
//!
//! Platform-neutral handlers (nop, sleep, test_subtask) live at the root of
//! this module. SmartOS-specific handlers live in `smartos` and are only
//! compiled for illumos targets.

pub mod nop;
pub mod sleep;

use cn_agent_api::TaskName;

use crate::registry::{TaskRegistry, TaskRegistryBuilder};
use crate::smartos::tasks::server_sysinfo::ServerSysinfoTask;

/// Register platform-neutral tasks that every backend exposes.
pub fn register_common_tasks(builder: TaskRegistryBuilder) -> TaskRegistryBuilder {
    builder
        .register(TaskName::Nop, nop::NopTask)
        .register(TaskName::Sleep, sleep::SleepTask)
}

/// Build a registry containing only the platform-neutral tasks.
///
/// Useful for tests and for the `dummy` backend used during development.
pub fn common_registry() -> TaskRegistry {
    register_common_tasks(TaskRegistry::builder()).build()
}

/// Build a registry containing the tasks the SmartOS backend exposes.
///
/// Today that's the platform-neutral set plus `server_sysinfo`; the rest of
/// the vmadm / zfs / imgadm-backed tasks will be registered here as they are
/// ported.
pub fn smartos_registry() -> TaskRegistry {
    register_common_tasks(TaskRegistry::builder())
        .register(TaskName::ServerSysinfo, ServerSysinfoTask::new())
        .build()
}
