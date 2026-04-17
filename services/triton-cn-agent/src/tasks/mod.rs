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

use crate::cnapi::CnapiClient;
use crate::heartbeater::AgentsCollector;
use crate::registry::{TaskRegistry, TaskRegistryBuilder};
use crate::smartos::apm::Apm;
use crate::smartos::nictagadm::NictagadmTool;
use crate::smartos::tasks::{
    agent_install::AgentInstallTask,
    agents_uninstall::AgentsUninstallTask,
    command_execute::CommandExecuteTask,
    image_ensure_present::ImageEnsurePresentTask,
    image_get::ImageGetTask,
    machine_create::MachineCreateTask,
    machine_create_image::MachineCreateImageTask,
    machine_destroy::MachineDestroyTask,
    machine_info::MachineInfoTask,
    machine_lifecycle::{MachineBootTask, MachineKillTask, MachineRebootTask, MachineShutdownTask},
    machine_load::MachineLoadTask,
    machine_proc::MachineProcTask,
    machine_reprovision::MachineReprovisionTask,
    machine_screenshot::MachineScreenshotTask,
    machine_snapshots::{
        MachineCreateSnapshotTask, MachineDeleteSnapshotTask, MachineRollbackSnapshotTask,
    },
    machine_update::MachineUpdateTask,
    machine_update_nics::MachineUpdateNicsTask,
    recovery_config::RecoveryConfigTask,
    refresh_agents::RefreshAgentsTask,
    server_overprovision_ratio::ServerOverprovisionRatioTask,
    server_reboot::ServerRebootTask,
    server_sysinfo::ServerSysinfoTask,
    server_update_nics::ServerUpdateNicsTask,
    shutdown_cn_agent_update::ShutdownCnAgentUpdateTask,
    test_subtask::TestSubtaskTask,
    zfs_get_properties::ZfsGetPropertiesTask,
    zfs_list_datasets::ZfsListDatasetsTask,
    zfs_list_pools::ZfsListPoolsTask,
    zfs_list_snapshots::ZfsListSnapshotsTask,
    zfs_mutations::{
        ZfsCloneDatasetTask, ZfsCreateDatasetTask, ZfsDestroyDatasetTask, ZfsRenameDatasetTask,
        ZfsRollbackDatasetTask, ZfsSetPropertiesTask, ZfsSnapshotDatasetTask,
    },
};
use crate::smartos::{ImgadmTool, VmadmTool, ZfsTool};

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

/// Register the SmartOS ZFS mutation handlers.
pub fn register_zfs_mutation_tasks(
    builder: TaskRegistryBuilder,
    tool: Arc<ZfsTool>,
) -> TaskRegistryBuilder {
    builder
        .register(
            TaskName::ZfsCreateDataset,
            ZfsCreateDatasetTask::new(tool.clone()),
        )
        .register(
            TaskName::ZfsDestroyDataset,
            ZfsDestroyDatasetTask::new(tool.clone()),
        )
        .register(
            TaskName::ZfsRenameDataset,
            ZfsRenameDatasetTask::new(tool.clone()),
        )
        .register(
            TaskName::ZfsSnapshotDataset,
            ZfsSnapshotDatasetTask::new(tool.clone()),
        )
        .register(
            TaskName::ZfsRollbackDataset,
            ZfsRollbackDatasetTask::new(tool.clone()),
        )
        .register(
            TaskName::ZfsCloneDataset,
            ZfsCloneDatasetTask::new(tool.clone()),
        )
        .register(TaskName::ZfsSetProperties, ZfsSetPropertiesTask::new(tool))
}

/// Build a registry containing only the platform-neutral tasks.
///
/// Useful for tests and for the `dummy` backend used during development.
pub fn common_registry() -> TaskRegistry {
    register_common_tasks(TaskRegistry::builder()).build()
}

/// Register read-only vmadm wrappers (`machine_load`, `machine_info`).
pub fn register_vmadm_query_tasks(
    builder: TaskRegistryBuilder,
    tool: Arc<VmadmTool>,
) -> TaskRegistryBuilder {
    builder
        .register(TaskName::MachineLoad, MachineLoadTask::new(tool.clone()))
        .register(TaskName::MachineInfo, MachineInfoTask::new(tool))
}

/// Register vmadm lifecycle wrappers (boot/shutdown/reboot/kill).
pub fn register_vmadm_lifecycle_tasks(
    builder: TaskRegistryBuilder,
    tool: Arc<VmadmTool>,
) -> TaskRegistryBuilder {
    builder
        .register(TaskName::MachineBoot, MachineBootTask::new(tool.clone()))
        .register(
            TaskName::MachineShutdown,
            MachineShutdownTask::new(tool.clone()),
        )
        .register(
            TaskName::MachineReboot,
            MachineRebootTask::new(tool.clone()),
        )
        .register(TaskName::MachineKill, MachineKillTask::new(tool))
}

/// Register vmadm mutation wrappers (destroy/update + snapshot operations).
pub fn register_vmadm_mutation_tasks(
    builder: TaskRegistryBuilder,
    tool: Arc<VmadmTool>,
) -> TaskRegistryBuilder {
    builder
        .register(
            TaskName::MachineDestroy,
            MachineDestroyTask::new(tool.clone()),
        )
        .register(
            TaskName::MachineUpdate,
            MachineUpdateTask::new(tool.clone()),
        )
        .register(
            TaskName::MachineCreateSnapshot,
            MachineCreateSnapshotTask::new(tool.clone()),
        )
        .register(
            TaskName::MachineDeleteSnapshot,
            MachineDeleteSnapshotTask::new(tool.clone()),
        )
        .register(
            TaskName::MachineRollbackSnapshot,
            MachineRollbackSnapshotTask::new(tool.clone()),
        )
        .register(
            TaskName::MachineScreenshot,
            MachineScreenshotTask::new(tool),
        )
}

/// Register server-level operational tasks (reboot, command_execute,
/// overprovision ratio, cn-agent-update shutdown, diagnostic helpers,
/// recovery_config).
pub fn register_server_ops_tasks(builder: TaskRegistryBuilder) -> TaskRegistryBuilder {
    builder
        .register(TaskName::ServerReboot, ServerRebootTask::new())
        .register(TaskName::CommandExecute, CommandExecuteTask::new())
        .register(
            TaskName::ServerOverprovisionRatio,
            ServerOverprovisionRatioTask::new(),
        )
        .register(
            TaskName::ShutdownCnAgentUpdate,
            ShutdownCnAgentUpdateTask::new(),
        )
        .register(TaskName::RecoveryConfig, RecoveryConfigTask::new())
        .register(TaskName::TestSubtask, TestSubtaskTask)
}

/// Register agent-management tasks (refresh_agents,
/// agent_install, agents_uninstall). All three post to CNAPI when
/// their operation completes.
pub fn register_agent_tasks(
    builder: TaskRegistryBuilder,
    cnapi: Arc<CnapiClient>,
    collector: AgentsCollector,
    apm: Arc<Apm>,
    bind_port: u16,
) -> TaskRegistryBuilder {
    builder
        .register(
            TaskName::RefreshAgents,
            RefreshAgentsTask::new(cnapi.clone(), collector.clone()),
        )
        .register(
            TaskName::AgentInstall,
            AgentInstallTask::new(apm.clone(), cnapi.clone(), collector.clone(), bind_port),
        )
        .register(
            TaskName::AgentsUninstall,
            AgentsUninstallTask::new(apm, cnapi, collector),
        )
}

/// Register image tasks (`image_get` + `image_ensure_present`).
pub fn register_image_tasks(
    builder: TaskRegistryBuilder,
    imgadm: Arc<ImgadmTool>,
    zfs: Arc<ZfsTool>,
) -> TaskRegistryBuilder {
    builder
        .register(TaskName::ImageGet, ImageGetTask::new(imgadm.clone()))
        .register(
            TaskName::ImageEnsurePresent,
            ImageEnsurePresentTask::new(imgadm, zfs),
        )
}

/// Register the heavy provisioning tasks (`machine_create`,
/// `machine_reprovision`, `machine_create_image`, `machine_update_nics`).
/// Needs the admin IP so the firewaller client can dial the local
/// firewaller on port 2021.
pub fn register_provisioning_tasks(
    builder: TaskRegistryBuilder,
    vmadm: Arc<VmadmTool>,
    zfs: Arc<ZfsTool>,
    imgadm: Arc<ImgadmTool>,
    admin_ip: std::net::Ipv4Addr,
) -> TaskRegistryBuilder {
    builder
        .register(
            TaskName::MachineCreate,
            MachineCreateTask::new(vmadm.clone(), zfs.clone(), imgadm.clone(), admin_ip),
        )
        .register(
            TaskName::MachineReprovision,
            MachineReprovisionTask::new(vmadm.clone(), zfs, imgadm.clone()),
        )
        .register(
            TaskName::MachineCreateImage,
            MachineCreateImageTask::new(imgadm),
        )
        .register(
            TaskName::MachineUpdateNics,
            MachineUpdateNicsTask::new(vmadm.clone()),
        )
        .register(TaskName::MachineProc, MachineProcTask::new(vmadm))
}

/// Register `server_update_nics` (nictagadm wrapper).
pub fn register_server_nic_tasks(
    builder: TaskRegistryBuilder,
    tool: Arc<NictagadmTool>,
) -> TaskRegistryBuilder {
    builder.register(TaskName::ServerUpdateNics, ServerUpdateNicsTask::new(tool))
}

/// Build a registry containing the tasks the SmartOS backend exposes.
///
/// This is the "offline" variant — no CNAPI client available, so
/// refresh_agents is not registered, and no admin IP is provided so
/// machine_create is also not registered. Callers that have a running
/// CNAPI client should use [`smartos_registry_with`] instead.
pub fn smartos_registry() -> TaskRegistry {
    let vmadm = Arc::new(VmadmTool::new());
    let zfs = Arc::new(ZfsTool::new());
    let imgadm = Arc::new(ImgadmTool::new(zfs.clone()));
    let mut builder = register_common_tasks(TaskRegistry::builder())
        .register(TaskName::ServerSysinfo, ServerSysinfoTask::new());
    builder = register_zfs_query_tasks(builder, zfs.clone());
    builder = register_zfs_mutation_tasks(builder, zfs.clone());
    builder = register_vmadm_query_tasks(builder, vmadm.clone());
    builder = register_vmadm_lifecycle_tasks(builder, vmadm.clone());
    builder = register_vmadm_mutation_tasks(builder, vmadm);
    builder = register_image_tasks(builder, imgadm, zfs);
    builder = register_server_nic_tasks(builder, Arc::new(NictagadmTool::new()));
    builder = register_server_ops_tasks(builder);
    builder.build()
}

/// Full SmartOS registry: every task we ship today. Accepts injectable
/// tool instances so the binary can share them with the heartbeater, and
/// a CNAPI client + agents collector for tasks that post back to CNAPI
/// (currently just `refresh_agents`), plus the admin IP for the
/// firewaller wiring in `machine_create`.
pub fn smartos_registry_with(
    vmadm: Arc<VmadmTool>,
    zfs: Arc<ZfsTool>,
    cnapi: Arc<CnapiClient>,
    agents_collector: AgentsCollector,
    admin_ip: std::net::Ipv4Addr,
    bind_port: u16,
) -> TaskRegistry {
    let imgadm = Arc::new(ImgadmTool::new(zfs.clone()));
    let apm = Arc::new(Apm::production());
    let mut builder = register_common_tasks(TaskRegistry::builder())
        .register(TaskName::ServerSysinfo, ServerSysinfoTask::new());
    builder = register_zfs_query_tasks(builder, zfs.clone());
    builder = register_zfs_mutation_tasks(builder, zfs.clone());
    builder = register_vmadm_query_tasks(builder, vmadm.clone());
    builder = register_vmadm_lifecycle_tasks(builder, vmadm.clone());
    builder = register_vmadm_mutation_tasks(builder, vmadm.clone());
    builder = register_image_tasks(builder, imgadm.clone(), zfs.clone());
    builder = register_provisioning_tasks(builder, vmadm, zfs, imgadm, admin_ip);
    builder = register_server_nic_tasks(builder, Arc::new(NictagadmTool::new()));
    builder = register_server_ops_tasks(builder);
    builder = register_agent_tasks(builder, cnapi, agents_collector, apm, bind_port);
    builder.build()
}
