// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `machine_destroy` task: `vmadm delete <uuid>`.
//!
//! Idempotent: if the VM is already missing, we report success (matches the
//! legacy `stderrLines.match(': No such zone')` special case).
//!
//! The legacy task also ran `logadm -r <path>` on platforms older than
//! 20171125T020845Z to paper over OS-6053. Every SmartOS platform we ship
//! today is well past that fix, so the workaround isn't ported.

use std::sync::Arc;

use async_trait::async_trait;
use cn_agent_api::{TaskError, TaskResult};
use serde::Deserialize;

use crate::registry::TaskHandler;
use crate::smartos::tasks::machine_load::vmadm_error_to_task;
use crate::smartos::vmadm::{DeleteOptions, VmadmTool};

#[derive(Debug, Deserialize)]
struct Params {
    uuid: String,
    #[serde(default)]
    include_dni: Option<bool>,
}

pub struct MachineDestroyTask {
    tool: Arc<VmadmTool>,
}

impl MachineDestroyTask {
    pub fn new(tool: Arc<VmadmTool>) -> Self {
        Self { tool }
    }
}

#[async_trait]
impl TaskHandler for MachineDestroyTask {
    async fn run(&self, params: serde_json::Value) -> Result<TaskResult, TaskError> {
        let p: Params = serde_json::from_value(params)
            .map_err(|e| TaskError::new(format!("invalid params: {e}")))?;
        let opts = DeleteOptions {
            include_dni: p.include_dni.unwrap_or(false),
        };
        self.tool
            .delete(&p.uuid, &opts)
            .await
            .map_err(|err| vmadm_error_to_task(err, "delete error"))?;
        Ok(serde_json::json!({}))
    }
}
