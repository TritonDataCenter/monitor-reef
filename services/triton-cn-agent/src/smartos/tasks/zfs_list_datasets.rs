// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `zfs_list_datasets` task: `zfs list -t all` with optional dataset filter.

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
    #[serde(default)]
    recursive: Option<bool>,
}

pub struct ZfsListDatasetsTask {
    tool: Arc<ZfsTool>,
}

impl ZfsListDatasetsTask {
    pub fn new(tool: Arc<ZfsTool>) -> Self {
        Self { tool }
    }
}

#[async_trait]
impl TaskHandler for ZfsListDatasetsTask {
    async fn run(&self, params: serde_json::Value) -> Result<TaskResult, TaskError> {
        // Accept both an empty body ({}) and an absent body (Null), matching
        // the legacy behavior where `req.params.dataset || ''` coerced the
        // missing case to "list everything".
        let params: Params = if params.is_null() {
            Params::default()
        } else {
            serde_json::from_value(params)
                .map_err(|e| TaskError::new(format!("invalid params: {e}")))?
        };

        let opts = ListDatasetsOptions {
            dataset: params.dataset,
            // Legacy hardcoded `{type: 'all'}` (see
            // lib/backends/smartos/tasks/zfs_list_datasets.js).
            kind: DatasetType::All,
            recursive: params.recursive.unwrap_or(false),
        };

        let rows = self
            .tool
            .list_datasets(&opts)
            .await
            .map_err(|e| TaskError::new(format!("failed to list ZFS datasets: {e}")))?;

        Ok(serde_json::Value::Array(
            rows.into_iter().map(serde_json::Value::Object).collect(),
        ))
    }
}
