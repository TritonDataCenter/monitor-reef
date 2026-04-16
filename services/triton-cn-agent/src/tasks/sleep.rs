// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Sleep task (test helper): sleeps for N seconds, optionally returning an
//! error. Mirrors `lib/backends/smartos/tasks/nop.js` in the legacy agent.

use std::time::Duration;

use async_trait::async_trait;
use cn_agent_api::{SleepParams, TaskError, TaskResult};

use crate::registry::TaskHandler;

pub struct SleepTask;

#[async_trait]
impl TaskHandler for SleepTask {
    async fn run(&self, params: serde_json::Value) -> Result<TaskResult, TaskError> {
        let params: SleepParams = serde_json::from_value(params)
            .map_err(|e| TaskError::new(format!("invalid sleep params: {e}")))?;

        if let Some(seconds) = params.sleep {
            tokio::time::sleep(Duration::from_secs(seconds)).await;
        }

        if let Some(err) = params.error {
            return Err(TaskError::new(err));
        }

        Ok(serde_json::json!({}))
    }
}
