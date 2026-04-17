// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Lifecycle task handlers that wrap `vmadm start/stop/reboot/kill`.
//!
//! Each handler:
//! 1. Parses its params (uuid, idempotent, command-specific knobs).
//! 2. Runs the underlying vmadm mutation.
//! 3. On `idempotent=true`, treats "already in target state" as success.
//! 4. Reloads the VM via `vmadm get` and returns `{vm: <machine>}` to mirror
//!    the legacy task response shape.

use std::sync::Arc;

use async_trait::async_trait;
use cn_agent_api::{TaskError, TaskResult};
use serde::Deserialize;

use crate::registry::TaskHandler;
use crate::smartos::tasks::machine_load::vmadm_error_to_task;
use crate::smartos::vmadm::{
    KillOptions, LoadOptions, RebootOptions, StartOptions, StopOptions, VmadmError, VmadmTool,
};

// ---------------------------------------------------------------------------
// Params
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, Default)]
struct StartParams {
    uuid: String,
    #[serde(default)]
    idempotent: Option<bool>,
    #[serde(default)]
    include_dni: Option<bool>,
    #[serde(default)]
    cdrom: Option<Vec<String>>,
    #[serde(default)]
    disk: Option<Vec<String>>,
    #[serde(default)]
    order: Option<Vec<String>>,
    #[serde(default)]
    once: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, Default)]
struct StopParams {
    uuid: String,
    #[serde(default)]
    idempotent: Option<bool>,
    #[serde(default)]
    include_dni: Option<bool>,
    #[serde(default)]
    force: Option<bool>,
    #[serde(default)]
    timeout: Option<u32>,
}

#[derive(Debug, Deserialize, Default)]
struct RebootParams {
    uuid: String,
    #[serde(default)]
    idempotent: Option<bool>,
    #[serde(default)]
    include_dni: Option<bool>,
    #[serde(default)]
    force: Option<bool>,
}

#[derive(Debug, Deserialize, Default)]
struct KillParams {
    uuid: String,
    #[serde(default)]
    idempotent: Option<bool>,
    #[serde(default)]
    include_dni: Option<bool>,
    #[serde(default)]
    signal: Option<String>,
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Load the VM and wrap it as `{vm: ...}` for the task response.
///
/// Mirrors the legacy lifecycle tasks which all return the freshly-reloaded
/// machine regardless of whether the mutation was idempotent.
async fn reload_and_respond(
    tool: &VmadmTool,
    uuid: &str,
    include_dni: bool,
) -> Result<TaskResult, TaskError> {
    let load_opts = LoadOptions {
        include_dni,
        fields: None,
    };
    match tool.load(uuid, &load_opts).await {
        Ok(vm) => Ok(serde_json::json!({ "vm": vm })),
        Err(err) => Err(vmadm_error_to_task(err, "vmadm.load error")),
    }
}

/// Translate the mutation result (which may be `AlreadyInState`) into a
/// success or task error, honoring the `idempotent` flag.
fn handle_mutation_error(err: VmadmError, idempotent: bool, prefix: &str) -> Result<(), TaskError> {
    if matches!(err, VmadmError::AlreadyInState { .. }) && idempotent {
        tracing::info!(error = %err, "ignoring already-in-state (idempotent)");
        return Ok(());
    }
    Err(vmadm_error_to_task(err, prefix))
}

// ---------------------------------------------------------------------------
// MachineBoot
// ---------------------------------------------------------------------------

pub struct MachineBootTask {
    tool: Arc<VmadmTool>,
}

impl MachineBootTask {
    pub fn new(tool: Arc<VmadmTool>) -> Self {
        Self { tool }
    }
}

#[async_trait]
impl TaskHandler for MachineBootTask {
    async fn run(&self, params: serde_json::Value) -> Result<TaskResult, TaskError> {
        let p: StartParams = serde_json::from_value(params)
            .map_err(|e| TaskError::new(format!("invalid params: {e}")))?;
        let idempotent = p.idempotent.unwrap_or(false);
        let include_dni = p.include_dni.unwrap_or(false);
        let opts = StartOptions {
            include_dni,
            cdrom: p.cdrom.unwrap_or_default(),
            disk: p.disk.unwrap_or_default(),
            order: p.order.unwrap_or_default(),
            once: p.once.unwrap_or_default(),
        };

        if let Err(err) = self.tool.start(&p.uuid, &opts).await {
            handle_mutation_error(err, idempotent, "vmadm.start error")?;
        }

        reload_and_respond(&self.tool, &p.uuid, include_dni).await
    }
}

// ---------------------------------------------------------------------------
// MachineShutdown
// ---------------------------------------------------------------------------

pub struct MachineShutdownTask {
    tool: Arc<VmadmTool>,
}

impl MachineShutdownTask {
    pub fn new(tool: Arc<VmadmTool>) -> Self {
        Self { tool }
    }
}

#[async_trait]
impl TaskHandler for MachineShutdownTask {
    async fn run(&self, params: serde_json::Value) -> Result<TaskResult, TaskError> {
        let p: StopParams = serde_json::from_value(params)
            .map_err(|e| TaskError::new(format!("invalid params: {e}")))?;
        let idempotent = p.idempotent.unwrap_or(false);
        let include_dni = p.include_dni.unwrap_or(false);
        let opts = StopOptions {
            include_dni,
            force: p.force.unwrap_or(false),
            timeout: p.timeout,
        };

        if let Err(err) = self.tool.stop(&p.uuid, &opts).await {
            handle_mutation_error(err, idempotent, "vmadm.stop error")?;
        }

        reload_and_respond(&self.tool, &p.uuid, include_dni).await
    }
}

// ---------------------------------------------------------------------------
// MachineReboot
// ---------------------------------------------------------------------------

pub struct MachineRebootTask {
    tool: Arc<VmadmTool>,
}

impl MachineRebootTask {
    pub fn new(tool: Arc<VmadmTool>) -> Self {
        Self { tool }
    }
}

#[async_trait]
impl TaskHandler for MachineRebootTask {
    async fn run(&self, params: serde_json::Value) -> Result<TaskResult, TaskError> {
        let p: RebootParams = serde_json::from_value(params)
            .map_err(|e| TaskError::new(format!("invalid params: {e}")))?;
        let idempotent = p.idempotent.unwrap_or(false);
        let include_dni = p.include_dni.unwrap_or(false);
        let opts = RebootOptions {
            include_dni,
            force: p.force.unwrap_or(false),
        };

        if let Err(err) = self.tool.reboot(&p.uuid, &opts).await {
            handle_mutation_error(err, idempotent, "vmadm.reboot error")?;
        }

        reload_and_respond(&self.tool, &p.uuid, include_dni).await
    }
}

// ---------------------------------------------------------------------------
// MachineKill
// ---------------------------------------------------------------------------

pub struct MachineKillTask {
    tool: Arc<VmadmTool>,
}

impl MachineKillTask {
    pub fn new(tool: Arc<VmadmTool>) -> Self {
        Self { tool }
    }
}

#[async_trait]
impl TaskHandler for MachineKillTask {
    async fn run(&self, params: serde_json::Value) -> Result<TaskResult, TaskError> {
        let p: KillParams = serde_json::from_value(params)
            .map_err(|e| TaskError::new(format!("invalid params: {e}")))?;
        let idempotent = p.idempotent.unwrap_or(false);
        let include_dni = p.include_dni.unwrap_or(false);
        let opts = KillOptions {
            include_dni,
            signal: p.signal,
        };

        if let Err(err) = self.tool.kill(&p.uuid, &opts).await {
            handle_mutation_error(err, idempotent, "vmadm.kill error")?;
        }

        reload_and_respond(&self.tool, &p.uuid, include_dni).await
    }
}
