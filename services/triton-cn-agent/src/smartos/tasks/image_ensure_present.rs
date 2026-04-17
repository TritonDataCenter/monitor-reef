// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `image_ensure_present` — idempotently install an image dataset.
//!
//! Steps:
//! 1. Check if `<zpool>/<uuid>` already exists via `zfs list`. If so,
//!    the image is installed and we short-circuit.
//! 2. Otherwise, optionally pick an IMGAPI peer URL (if `imgapiPeers` is
//!    set and the installed imgadm is recent enough).
//! 3. Call `imgadm import -q -P <zpool> <uuid>` (possibly with `-S
//!    <peer>` and `--zstream`), waiting for any concurrent import to
//!    clear its `<pool>/<uuid>-partial` dataset first.

use std::sync::Arc;

use async_trait::async_trait;
use cn_agent_api::{TaskError, TaskResult};
use serde::Deserialize;

use crate::registry::TaskHandler;
use crate::smartos::imgadm_tool::{
    DEFAULT_ZPOOL, ImgadmTool, ImportOptions, default_install_dataset,
};
use crate::smartos::zfs::{DatasetType, ListDatasetsOptions, ZfsError, ZfsTool};

/// Default cn-agent port peer URLs assume when `imgapiPeers` is supplied.
pub const DEFAULT_CN_AGENT_PORT: u16 = 5309;

#[derive(Debug, Deserialize, Default)]
struct Params {
    image_uuid: String,
    #[serde(default)]
    zfs_storage_pool_name: Option<String>,
    /// When set, use the first peer's IP as an `imgadm -S` source. Shape
    /// mirrors the legacy CNAPI payload.
    #[serde(default, alias = "imgapiPeers")]
    imgapi_peers: Option<Vec<ImgapiPeer>>,
}

#[derive(Debug, Deserialize)]
struct ImgapiPeer {
    ip: String,
    #[allow(dead_code)]
    #[serde(default)]
    // Present in the legacy payload (UUID, role) but we only use `ip`.
    uuid: Option<String>,
}

pub struct ImageEnsurePresentTask {
    imgadm: Arc<ImgadmTool>,
    zfs: Arc<ZfsTool>,
}

impl ImageEnsurePresentTask {
    pub fn new(imgadm: Arc<ImgadmTool>, zfs: Arc<ZfsTool>) -> Self {
        Self { imgadm, zfs }
    }
}

#[async_trait]
impl TaskHandler for ImageEnsurePresentTask {
    async fn run(&self, params: serde_json::Value) -> Result<TaskResult, TaskError> {
        let p: Params = serde_json::from_value(params)
            .map_err(|e| TaskError::new(format!("invalid params: {e}")))?;

        let zpool = p
            .zfs_storage_pool_name
            .clone()
            .unwrap_or_else(|| DEFAULT_ZPOOL.to_string());
        let dataset = default_install_dataset(&zpool, &p.image_uuid);

        // Step 1: short-circuit if already installed.
        if image_already_installed(&self.zfs, &dataset).await? {
            tracing::info!(dataset = %dataset, "image already installed, skipping import");
            return Ok(serde_json::json!({}));
        }

        // Step 2: pick a source URL if peers were provided.
        let source = p
            .imgapi_peers
            .as_ref()
            .and_then(|peers| peers.first())
            .map(|peer| format!("http://{}:{}", peer.ip, DEFAULT_CN_AGENT_PORT));

        let import_opts = ImportOptions {
            source: source.clone(),
            zstream: source.is_some(),
            lock_timeout: None,
        };

        // Step 3: run the import.
        self.imgadm
            .import(&zpool, &p.image_uuid, &import_opts)
            .await
            .map_err(|e| TaskError::new(format!("imgadm import failed: {e}")))?;

        Ok(serde_json::json!({}))
    }
}

/// Returns true if the `<pool>/<uuid>` dataset already exists.
async fn image_already_installed(zfs: &ZfsTool, dataset: &str) -> Result<bool, TaskError> {
    let opts = ListDatasetsOptions {
        dataset: Some(dataset.to_string()),
        kind: DatasetType::All,
        recursive: false,
    };
    match zfs.list_datasets(&opts).await {
        Ok(rows) => Ok(!rows.is_empty()),
        Err(ZfsError::NonZeroExit { stderr, .. }) if stderr.contains("does not exist") => Ok(false),
        Err(e) => Err(TaskError::new(format!(
            "failed to check if image is installed: {e}"
        ))),
    }
}
