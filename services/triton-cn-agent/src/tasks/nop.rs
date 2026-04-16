// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! No-op task: returns immediately.

use async_trait::async_trait;
use cn_agent_api::{TaskError, TaskResult};

use crate::registry::TaskHandler;

pub struct NopTask;

#[async_trait]
impl TaskHandler for NopTask {
    async fn run(&self, _params: serde_json::Value) -> Result<TaskResult, TaskError> {
        Ok(serde_json::json!({}))
    }
}
