// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! SmartOS-backed task handlers.

pub mod agent_install;
pub mod agents_uninstall;
pub mod command_execute;
pub mod image_ensure_present;
pub mod image_get;
pub mod machine_create;
pub mod machine_create_image;
pub mod machine_destroy;
pub mod machine_info;
pub mod machine_lifecycle;
pub mod machine_load;
pub mod machine_proc;
pub mod machine_reprovision;
pub mod machine_screenshot;
pub mod machine_snapshots;
pub mod machine_update;
pub mod machine_update_nics;
pub mod recovery_config;
pub mod refresh_agents;
pub mod server_overprovision_ratio;
pub mod server_reboot;
pub mod server_sysinfo;
pub mod server_update_nics;
pub mod shutdown_cn_agent_update;
pub mod test_subtask;
pub mod zfs_get_properties;
pub mod zfs_list_datasets;
pub mod zfs_list_pools;
pub mod zfs_list_snapshots;
pub mod zfs_mutations;
