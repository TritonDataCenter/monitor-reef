// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `machine_update` task: `vmadm update <uuid>` with JSON payload on stdin.
//!
//! After the update, if `add_nics` / `remove_nics` are present AND the VM
//! is running, the legacy task follows up with `vmadm reboot` so the NIC
//! changes take effect. We preserve that dance.
//!
//! Response shape: `{vm: <loaded VM>}`.

use std::sync::Arc;

use async_trait::async_trait;
use cn_agent_api::{TaskError, TaskResult};

use crate::registry::TaskHandler;
use crate::smartos::tasks::machine_load::vmadm_error_to_task;
use crate::smartos::vmadm::{LoadOptions, RebootOptions, VmadmTool};

pub struct MachineUpdateTask {
    tool: Arc<VmadmTool>,
}

impl MachineUpdateTask {
    pub fn new(tool: Arc<VmadmTool>) -> Self {
        Self { tool }
    }
}

#[async_trait]
impl TaskHandler for MachineUpdateTask {
    async fn run(&self, params: serde_json::Value) -> Result<TaskResult, TaskError> {
        let obj = params
            .as_object()
            .ok_or_else(|| TaskError::new("machine_update params must be an object".to_string()))?;

        // uuid is both in the payload (for vmadm) and needed for our reload.
        let uuid = obj
            .get("uuid")
            .and_then(|v| v.as_str())
            .ok_or_else(|| TaskError::new("missing params.uuid".to_string()))?
            .to_string();
        let include_dni = obj
            .get("include_dni")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let has_nic_changes = obj.contains_key("add_nics") || obj.contains_key("remove_nics");

        // Scrub knobs vmadm doesn't want in its payload — these are
        // task-layer concerns, not vmadm fields. (include_dni is removed
        // because vmadm rejects unknown fields; uuid is fine to keep since
        // `vmadm update <uuid>` takes it on argv but doesn't reject it in
        // payload.)
        let mut payload = params.clone();
        if let Some(map) = payload.as_object_mut() {
            map.remove("include_dni");
        }

        self.tool
            .update(&uuid, &payload, include_dni)
            .await
            .map_err(|err| vmadm_error_to_task(err, "vmadm.update error"))?;

        let load_opts = LoadOptions {
            include_dni,
            fields: None,
        };
        let mut vm = self
            .tool
            .load(&uuid, &load_opts)
            .await
            .map_err(|err| vmadm_error_to_task(err, "vmadm.load error"))?;

        if has_nic_changes {
            let running = vm.get("state").and_then(|v| v.as_str()) == Some("running");
            if running {
                let reboot_opts = RebootOptions {
                    include_dni,
                    force: false,
                };
                self.tool
                    .reboot(&uuid, &reboot_opts)
                    .await
                    .map_err(|err| vmadm_error_to_task(err, "vmadm.reboot error"))?;
                // Reload so the response reflects any state differences after
                // the reboot (e.g., boot_timestamp).
                vm = self
                    .tool
                    .load(&uuid, &load_opts)
                    .await
                    .map_err(|err| vmadm_error_to_task(err, "vmadm.load error"))?;
            }
        }

        Ok(serde_json::json!({ "vm": vm }))
    }
}
