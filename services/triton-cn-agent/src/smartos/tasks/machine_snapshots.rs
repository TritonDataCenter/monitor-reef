// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `machine_create_snapshot` / `machine_delete_snapshot` /
//! `machine_rollback_snapshot` tasks.
//!
//! All three follow the same pattern: run the corresponding `vmadm <op>`
//! subcommand, then reload the VM and return `{vm: <machine>}`.

use std::sync::Arc;

use async_trait::async_trait;
use cn_agent_api::{TaskError, TaskResult};
use serde::Deserialize;

use crate::registry::TaskHandler;
use crate::smartos::tasks::machine_load::vmadm_error_to_task;
use crate::smartos::vmadm::{LoadOptions, SnapshotOptions, VmadmTool};

#[derive(Debug, Deserialize)]
struct Params {
    uuid: String,
    snapshot_name: String,
    #[serde(default)]
    include_dni: Option<bool>,
}

async fn reload(tool: &VmadmTool, uuid: &str, include_dni: bool) -> Result<TaskResult, TaskError> {
    let load_opts = LoadOptions {
        include_dni,
        fields: None,
    };
    let vm = tool
        .load(uuid, &load_opts)
        .await
        .map_err(|err| vmadm_error_to_task(err, "vmadm.load error"))?;
    Ok(serde_json::json!({ "vm": vm }))
}

// ---------------------------------------------------------------------------
// Create
// ---------------------------------------------------------------------------

pub struct MachineCreateSnapshotTask {
    tool: Arc<VmadmTool>,
}

impl MachineCreateSnapshotTask {
    pub fn new(tool: Arc<VmadmTool>) -> Self {
        Self { tool }
    }
}

#[async_trait]
impl TaskHandler for MachineCreateSnapshotTask {
    async fn run(&self, params: serde_json::Value) -> Result<TaskResult, TaskError> {
        let p: Params = serde_json::from_value(params)
            .map_err(|e| TaskError::new(format!("invalid params: {e}")))?;
        let include_dni = p.include_dni.unwrap_or(false);
        let opts = SnapshotOptions { include_dni };

        self.tool
            .create_snapshot(&p.uuid, &p.snapshot_name, &opts)
            .await
            .map_err(|err| vmadm_error_to_task(err, "vmadm.create_snapshot error"))?;
        reload(&self.tool, &p.uuid, include_dni).await
    }
}

// ---------------------------------------------------------------------------
// Delete
// ---------------------------------------------------------------------------

pub struct MachineDeleteSnapshotTask {
    tool: Arc<VmadmTool>,
}

impl MachineDeleteSnapshotTask {
    pub fn new(tool: Arc<VmadmTool>) -> Self {
        Self { tool }
    }
}

#[async_trait]
impl TaskHandler for MachineDeleteSnapshotTask {
    async fn run(&self, params: serde_json::Value) -> Result<TaskResult, TaskError> {
        let p: Params = serde_json::from_value(params)
            .map_err(|e| TaskError::new(format!("invalid params: {e}")))?;
        let include_dni = p.include_dni.unwrap_or(false);
        let opts = SnapshotOptions { include_dni };

        self.tool
            .delete_snapshot(&p.uuid, &p.snapshot_name, &opts)
            .await
            .map_err(|err| vmadm_error_to_task(err, "vmadm.delete_snapshot error"))?;
        reload(&self.tool, &p.uuid, include_dni).await
    }
}

// ---------------------------------------------------------------------------
// Rollback
// ---------------------------------------------------------------------------

pub struct MachineRollbackSnapshotTask {
    tool: Arc<VmadmTool>,
}

impl MachineRollbackSnapshotTask {
    pub fn new(tool: Arc<VmadmTool>) -> Self {
        Self { tool }
    }
}

#[async_trait]
impl TaskHandler for MachineRollbackSnapshotTask {
    async fn run(&self, params: serde_json::Value) -> Result<TaskResult, TaskError> {
        let p: Params = serde_json::from_value(params)
            .map_err(|e| TaskError::new(format!("invalid params: {e}")))?;
        let include_dni = p.include_dni.unwrap_or(false);
        let opts = SnapshotOptions { include_dni };

        self.tool
            .rollback_snapshot(&p.uuid, &p.snapshot_name, &opts)
            .await
            .map_err(|err| vmadm_error_to_task(err, "vmadm.rollback_snapshot error"))?;
        reload(&self.tool, &p.uuid, include_dni).await
    }
}
