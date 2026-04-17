// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `machine_load` task: `vmadm get <uuid>`.

use std::sync::Arc;

use async_trait::async_trait;
use cn_agent_api::{TaskError, TaskResult};
use serde::Deserialize;

use crate::registry::TaskHandler;
use crate::smartos::vmadm::{LoadOptions, VmadmError, VmadmTool};

#[derive(Debug, Deserialize)]
struct Params {
    uuid: String,
    #[serde(default)]
    include_dni: Option<bool>,
    #[serde(default)]
    fields: Option<Vec<String>>,
}

pub struct MachineLoadTask {
    tool: Arc<VmadmTool>,
}

impl MachineLoadTask {
    pub fn new(tool: Arc<VmadmTool>) -> Self {
        Self { tool }
    }
}

#[async_trait]
impl TaskHandler for MachineLoadTask {
    async fn run(&self, params: serde_json::Value) -> Result<TaskResult, TaskError> {
        let params: Params = serde_json::from_value(params)
            .map_err(|e| TaskError::new(format!("invalid params: {e}")))?;

        let opts = LoadOptions {
            include_dni: params.include_dni.unwrap_or(false),
            fields: params.fields,
        };

        match self.tool.load(&params.uuid, &opts).await {
            Ok(vm) => Ok(vm),
            Err(err) => {
                let mut task_err = TaskError::new(format!("VM.load error: {err}"));
                task_err.rest_code = err.rest_code().map(|s| s.to_string());
                Err(task_err)
            }
        }
    }
}

/// Expose the internal error-mapping helper so machine_info's
/// `ifExists` path can share it.
pub fn vmadm_error_to_task(err: VmadmError, prefix: &str) -> TaskError {
    let mut task_err = TaskError::new(format!("{prefix}: {err}"));
    task_err.rest_code = err.rest_code().map(|s| s.to_string());
    task_err
}
