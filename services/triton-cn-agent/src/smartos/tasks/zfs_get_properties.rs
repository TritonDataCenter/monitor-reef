// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `zfs_get_properties` task: runs `zfs get` for a set of properties.

use std::sync::Arc;

use async_trait::async_trait;
use cn_agent_api::{TaskError, TaskResult};
use serde::Deserialize;

use crate::registry::TaskHandler;
use crate::smartos::zfs::ZfsTool;

#[derive(Debug, Deserialize, Default)]
struct Params {
    #[serde(default)]
    dataset: Option<String>,
    /// Properties to fetch. Legacy default is `["all"]` if absent.
    #[serde(default)]
    properties: Option<Vec<String>>,
}

pub struct ZfsGetPropertiesTask {
    tool: Arc<ZfsTool>,
}

impl ZfsGetPropertiesTask {
    pub fn new(tool: Arc<ZfsTool>) -> Self {
        Self { tool }
    }
}

#[async_trait]
impl TaskHandler for ZfsGetPropertiesTask {
    async fn run(&self, params: serde_json::Value) -> Result<TaskResult, TaskError> {
        let params: Params = if params.is_null() {
            Params::default()
        } else {
            serde_json::from_value(params)
                .map_err(|e| TaskError::new(format!("invalid params: {e}")))?
        };

        let properties = params
            .properties
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| vec!["all".to_string()]);
        let prop_refs: Vec<&str> = properties.iter().map(String::as_str).collect();

        let values = self
            .tool
            .get_properties(params.dataset.as_deref(), &prop_refs)
            .await
            .map_err(|e| TaskError::new(format!("failed to get ZFS properties: {e}")))?;

        Ok(serde_json::Value::Object(values))
    }
}
