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

use std::sync::Arc;

use cn_agent_api::TaskName;

use crate::registry::{TaskRegistry, TaskRegistryBuilder};
use crate::smartos::ZfsTool;
use crate::smartos::tasks::{
    server_sysinfo::ServerSysinfoTask, zfs_get_properties::ZfsGetPropertiesTask,
    zfs_list_datasets::ZfsListDatasetsTask, zfs_list_pools::ZfsListPoolsTask,
    zfs_list_snapshots::ZfsListSnapshotsTask,
};

/// Register platform-neutral tasks that every backend exposes.
pub fn register_common_tasks(builder: TaskRegistryBuilder) -> TaskRegistryBuilder {
    builder
        .register(TaskName::Nop, nop::NopTask)
        .register(TaskName::Sleep, sleep::SleepTask)
}

/// Register the SmartOS ZFS query handlers.
///
/// Takes a shared [`ZfsTool`] so tests can inject mock binaries for the
/// entire ZFS suite with one call.
pub fn register_zfs_query_tasks(
    builder: TaskRegistryBuilder,
    tool: Arc<ZfsTool>,
) -> TaskRegistryBuilder {
    builder
        .register(TaskName::ZfsListPools, ZfsListPoolsTask::new(tool.clone()))
        .register(
            TaskName::ZfsListDatasets,
            ZfsListDatasetsTask::new(tool.clone()),
        )
        .register(
            TaskName::ZfsListSnapshots,
            ZfsListSnapshotsTask::new(tool.clone()),
        )
        .register(TaskName::ZfsGetProperties, ZfsGetPropertiesTask::new(tool))
}

/// Build a registry containing only the platform-neutral tasks.
///
/// Useful for tests and for the `dummy` backend used during development.
pub fn common_registry() -> TaskRegistry {
    register_common_tasks(TaskRegistry::builder()).build()
}

/// Build a registry containing the tasks the SmartOS backend exposes.
///
/// Today that's the platform-neutral set plus `server_sysinfo` and the
/// read-only ZFS query tasks. Mutating ZFS, vmadm, imgadm, Docker, and agent
/// tasks get added here as they're ported.
pub fn smartos_registry() -> TaskRegistry {
    let zfs = Arc::new(ZfsTool::new());
    let mut builder = register_common_tasks(TaskRegistry::builder())
        .register(TaskName::ServerSysinfo, ServerSysinfoTask::new());
    builder = register_zfs_query_tasks(builder, zfs);
    builder.build()
}
