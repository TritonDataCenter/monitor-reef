// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `machine_info` task: `vmadm info <uuid> [types]`.

use std::sync::Arc;

use async_trait::async_trait;
use cn_agent_api::{TaskError, TaskResult};
use serde::Deserialize;

use crate::registry::TaskHandler;
use crate::smartos::tasks::machine_load::vmadm_error_to_task;
use crate::smartos::vmadm::{InfoOptions, VmadmTool};

#[derive(Debug, Deserialize)]
struct Params {
    uuid: String,
    #[serde(default)]
    include_dni: Option<bool>,
    /// Info types to request (e.g., ["net", "block"]). Empty → all.
    #[serde(default)]
    types: Option<Vec<String>>,
}

pub struct MachineInfoTask {
    tool: Arc<VmadmTool>,
}

impl MachineInfoTask {
    pub fn new(tool: Arc<VmadmTool>) -> Self {
        Self { tool }
    }
}

#[async_trait]
impl TaskHandler for MachineInfoTask {
    async fn run(&self, params: serde_json::Value) -> Result<TaskResult, TaskError> {
        let params: Params = serde_json::from_value(params)
            .map_err(|e| TaskError::new(format!("invalid params: {e}")))?;

        let opts = InfoOptions {
            types: params.types.unwrap_or_default(),
            include_dni: params.include_dni.unwrap_or(false),
        };

        self.tool
            .info(&params.uuid, &opts)
            .await
            .map_err(|err| vmadm_error_to_task(err, "vmadm.info error"))
    }
}
