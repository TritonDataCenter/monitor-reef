// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `machine_screenshot` — ask vmadm to take a PPM screenshot of a KVM
//! VM's framebuffer, then return the image as base64.
//!
//! Implementation mirrors the legacy task exactly: `vmadm sysrq <uuid>
//! screenshot`, then read `/zones/<uuid>/root/tmp/vm.ppm`, return the
//! bytes base64-encoded in the response.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine;
use cn_agent_api::{TaskError, TaskResult};
use serde::Deserialize;

use crate::registry::TaskHandler;
use crate::smartos::tasks::machine_load::vmadm_error_to_task;
use crate::smartos::vmadm::VmadmTool;

/// Default path vmadm writes the screenshot PPM to. Templated on VM UUID.
pub const SCREENSHOT_PATH_TEMPLATE: &str = "/zones/{uuid}/root/tmp/vm.ppm";

#[derive(Debug, Deserialize)]
struct Params {
    uuid: String,
    #[serde(default)]
    include_dni: Option<bool>,
}

pub struct MachineScreenshotTask {
    tool: Arc<VmadmTool>,
    /// Base dir where zone roots live. Defaults to `/zones`; tests
    /// override.
    zones_root: PathBuf,
}

impl MachineScreenshotTask {
    pub fn new(tool: Arc<VmadmTool>) -> Self {
        Self {
            tool,
            zones_root: PathBuf::from("/zones"),
        }
    }

    pub fn with_zones_root(tool: Arc<VmadmTool>, root: impl Into<PathBuf>) -> Self {
        Self {
            tool,
            zones_root: root.into(),
        }
    }
}

#[async_trait]
impl TaskHandler for MachineScreenshotTask {
    async fn run(&self, params: serde_json::Value) -> Result<TaskResult, TaskError> {
        let p: Params = serde_json::from_value(params)
            .map_err(|e| TaskError::new(format!("invalid params: {e}")))?;
        let include_dni = p.include_dni.unwrap_or(false);

        self.tool
            .sysrq_screenshot(&p.uuid, include_dni)
            .await
            .map_err(|err| vmadm_error_to_task(err, "vmadm.sysrq error"))?;

        let path = self.zones_root.join(&p.uuid).join("root/tmp/vm.ppm");
        let bytes = tokio::fs::read(&path).await.map_err(|e| {
            TaskError::new(format!(
                "failed to read screenshot at {}: {e}",
                path.display()
            ))
        })?;

        let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
        Ok(serde_json::json!({ "screenshot": encoded }))
    }
}
