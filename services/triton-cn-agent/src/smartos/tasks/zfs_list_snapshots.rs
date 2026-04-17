// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `zfs_list_snapshots` task: `zfs list -t snapshot -r`.

use std::sync::Arc;

use async_trait::async_trait;
use cn_agent_api::{TaskError, TaskResult};
use serde::Deserialize;

use crate::registry::TaskHandler;
use crate::smartos::zfs::{DatasetType, ListDatasetsOptions, ZfsTool};

#[derive(Debug, Deserialize, Default)]
struct Params {
    #[serde(default)]
    dataset: Option<String>,
}

pub struct ZfsListSnapshotsTask {
    tool: Arc<ZfsTool>,
}

impl ZfsListSnapshotsTask {
    pub fn new(tool: Arc<ZfsTool>) -> Self {
        Self { tool }
    }
}

#[async_trait]
impl TaskHandler for ZfsListSnapshotsTask {
    async fn run(&self, params: serde_json::Value) -> Result<TaskResult, TaskError> {
        let params: Params = if params.is_null() {
            Params::default()
        } else {
            serde_json::from_value(params)
                .map_err(|e| TaskError::new(format!("invalid params: {e}")))?
        };

        let opts = ListDatasetsOptions {
            dataset: params.dataset,
            // Legacy sets `{type: 'snapshot', recursive: true}`.
            kind: DatasetType::Snapshot,
            recursive: true,
        };

        let rows = self
            .tool
            .list_datasets(&opts)
            .await
            .map_err(|e| TaskError::new(format!("failed to list ZFS snapshots: {e}")))?;

        Ok(serde_json::Value::Array(
            rows.into_iter().map(serde_json::Value::Object).collect(),
        ))
    }
}
