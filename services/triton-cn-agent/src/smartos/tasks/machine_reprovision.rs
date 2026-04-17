// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `machine_reprovision` — re-install a VM's root dataset from a
//! (possibly different) image, preserving its config.
//!
//! Pipeline matches the legacy task:
//! 1. Guard + assert VM exists.
//! 2. Ensure the target image dataset is present; import if not.
//! 3. `vmadm reprovision <uuid>` with the payload on stdin.
//! 4. Reload and return `{vm: ...}`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use cn_agent_api::{TaskError, TaskResult};

use crate::registry::TaskHandler;
use crate::smartos::imgadm_tool::{
    DEFAULT_ZPOOL, ImgadmTool, ImportOptions, default_install_dataset,
};
use crate::smartos::tasks::machine_create::GUARD_FILE_PREFIX;
use crate::smartos::tasks::machine_load::vmadm_error_to_task;
use crate::smartos::vmadm::{LoadOptions, VmadmTool};
use crate::smartos::zfs::{DatasetType, ListDatasetsOptions, ZfsError, ZfsTool};

/// Default directory for the reprovision-in-progress guard. Same as
/// machine_create's so a `cleanupStaleLocks` pass catches both.
pub const DEFAULT_GUARD_DIR: &str = "/var/tmp";

pub struct MachineReprovisionTask {
    vmadm: Arc<VmadmTool>,
    zfs: Arc<ZfsTool>,
    imgadm: Arc<ImgadmTool>,
    guard_dir: PathBuf,
}

impl MachineReprovisionTask {
    pub fn new(vmadm: Arc<VmadmTool>, zfs: Arc<ZfsTool>, imgadm: Arc<ImgadmTool>) -> Self {
        Self {
            vmadm,
            zfs,
            imgadm,
            guard_dir: PathBuf::from(DEFAULT_GUARD_DIR),
        }
    }

    pub fn with_guard_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.guard_dir = dir.into();
        self
    }
}

#[async_trait]
impl TaskHandler for MachineReprovisionTask {
    async fn run(&self, params: serde_json::Value) -> Result<TaskResult, TaskError> {
        let uuid = params
            .get("uuid")
            .and_then(|v| v.as_str())
            .ok_or_else(|| TaskError::new("missing params.uuid".to_string()))?
            .to_string();
        let zpool = params
            .get("zfs_storage_pool_name")
            .and_then(|v| v.as_str())
            .unwrap_or(DEFAULT_ZPOOL)
            .to_string();
        let include_dni = params
            .get("include_dni")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        // Require VM exists up front so the error message matches the
        // legacy "VM <uuid> does not exist." phrasing.
        if let Err(e) = self
            .vmadm
            .load(
                &uuid,
                &LoadOptions {
                    include_dni,
                    fields: None,
                },
            )
            .await
        {
            return Err(vmadm_error_to_task(e, "pre-check error"));
        }

        let guard = GuardFile::create(&self.guard_dir, &uuid).await?;

        let result = self.reprovision(&params, &uuid, &zpool, include_dni).await;

        guard.unlink().await;
        result
    }
}

impl MachineReprovisionTask {
    async fn reprovision(
        &self,
        params: &serde_json::Value,
        uuid: &str,
        zpool: &str,
        include_dni: bool,
    ) -> Result<TaskResult, TaskError> {
        self.ensure_image_present(params, zpool).await?;

        let payload = scrub_payload(params.clone());
        self.vmadm
            .reprovision(uuid, &payload, include_dni)
            .await
            .map_err(|e| vmadm_error_to_task(e, "vmadm.reprovision error"))?;

        let vm = self
            .vmadm
            .load(
                uuid,
                &LoadOptions {
                    include_dni,
                    fields: None,
                },
            )
            .await
            .map_err(|e| vmadm_error_to_task(e, "vmadm.load error"))?;

        Ok(serde_json::json!({ "vm": vm }))
    }

    async fn ensure_image_present(
        &self,
        params: &serde_json::Value,
        zpool: &str,
    ) -> Result<(), TaskError> {
        let image_uuid = params
            .get("image_uuid")
            .and_then(|v| v.as_str())
            .or_else(|| {
                params
                    .get("disks")
                    .and_then(|d| d.as_array())
                    .and_then(|arr| arr.first())
                    .and_then(|first| first.get("image_uuid"))
                    .and_then(|v| v.as_str())
            });
        let Some(image_uuid) = image_uuid else {
            return Ok(());
        };
        let dataset = default_install_dataset(zpool, image_uuid);
        let opts = ListDatasetsOptions {
            dataset: Some(dataset.clone()),
            kind: DatasetType::All,
            recursive: false,
        };
        let exists = match self.zfs.list_datasets(&opts).await {
            Ok(rows) => !rows.is_empty(),
            Err(ZfsError::NonZeroExit { stderr, .. }) if stderr.contains("does not exist") => false,
            Err(e) => {
                return Err(TaskError::new(format!(
                    "failed to check for image dataset {dataset}: {e}"
                )));
            }
        };
        if exists {
            return Ok(());
        }
        self.imgadm
            .import(zpool, image_uuid, &ImportOptions::default())
            .await
            .map_err(|e| TaskError::new(format!("imgadm import failed: {e}")))
    }
}

struct GuardFile {
    path: PathBuf,
}

impl GuardFile {
    async fn create(dir: &Path, uuid: &str) -> Result<Self, TaskError> {
        let path = dir.join(format!("{GUARD_FILE_PREFIX}{uuid}"));
        match tokio::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
            .await
        {
            Ok(_) => Ok(Self { path }),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(TaskError::new(
                format!("another machine_create/reprovision is in progress for {uuid}"),
            )),
            Err(e) => Err(TaskError::new(format!(
                "failed to create guard file {}: {e}",
                path.display()
            ))),
        }
    }

    async fn unlink(self) {
        if let Err(e) = tokio::fs::remove_file(&self.path).await {
            tracing::warn!(
                path = %self.path.display(),
                error = %e,
                "failed to remove reprovision guard"
            );
        }
    }
}

fn scrub_payload(mut params: serde_json::Value) -> serde_json::Value {
    if let Some(obj) = params.as_object_mut() {
        for key in [
            "firewall_rules",
            "include_dni",
            "zfs_storage_pool_name",
            "imgapiPeers",
            "req_id",
        ] {
            obj.remove(key);
        }
    }
    params
}
