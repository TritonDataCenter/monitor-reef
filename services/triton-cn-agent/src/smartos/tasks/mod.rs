// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! SmartOS-backed task handlers.

pub mod machine_destroy;
pub mod machine_info;
pub mod machine_lifecycle;
pub mod machine_load;
pub mod machine_snapshots;
pub mod machine_update;
pub mod server_sysinfo;
pub mod zfs_get_properties;
pub mod zfs_list_datasets;
pub mod zfs_list_pools;
pub mod zfs_list_snapshots;
pub mod zfs_mutations;
