// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `test_subtask` — diagnostic task that echoes its params back as the
//! result. Mirrors the legacy task's behavior of validating the
//! subtask-dispatch code path end-to-end.

use async_trait::async_trait;
use cn_agent_api::{TaskError, TaskResult};

use crate::registry::TaskHandler;

pub struct TestSubtaskTask;

#[async_trait]
impl TaskHandler for TestSubtaskTask {
    async fn run(&self, params: serde_json::Value) -> Result<TaskResult, TaskError> {
        // Echo the params back so the caller can confirm round-tripping.
        // The legacy task just calls `self.finish({echo: params})`.
        Ok(serde_json::json!({ "echo": params }))
    }
}
