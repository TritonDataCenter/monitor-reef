// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `zfs_list_pools` task: runs `zpool list` and returns an array of objects.

use std::sync::Arc;

use async_trait::async_trait;
use cn_agent_api::{TaskError, TaskResult};

use crate::registry::TaskHandler;
use crate::smartos::zfs::ZfsTool;

pub struct ZfsListPoolsTask {
    tool: Arc<ZfsTool>,
}

impl ZfsListPoolsTask {
    pub fn new(tool: Arc<ZfsTool>) -> Self {
        Self { tool }
    }
}

#[async_trait]
impl TaskHandler for ZfsListPoolsTask {
    async fn run(&self, _params: serde_json::Value) -> Result<TaskResult, TaskError> {
        let rows = self
            .tool
            .list_pools()
            .await
            .map_err(|e| TaskError::new(format!("failed to list ZFS pools: {e}")))?;
        Ok(serde_json::Value::Array(
            rows.into_iter().map(serde_json::Value::Object).collect(),
        ))
    }
}
