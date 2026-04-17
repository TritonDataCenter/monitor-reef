// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `image_get` — fetch an installed image's manifest from imgadm.

use std::sync::Arc;

use async_trait::async_trait;
use cn_agent_api::{TaskError, TaskResult};
use serde::Deserialize;

use crate::registry::TaskHandler;
use crate::smartos::imgadm_tool::ImgadmTool;

#[derive(Debug, Deserialize)]
struct Params {
    uuid: String,
}

pub struct ImageGetTask {
    tool: Arc<ImgadmTool>,
}

impl ImageGetTask {
    pub fn new(tool: Arc<ImgadmTool>) -> Self {
        Self { tool }
    }
}

#[async_trait]
impl TaskHandler for ImageGetTask {
    async fn run(&self, params: serde_json::Value) -> Result<TaskResult, TaskError> {
        let p: Params = serde_json::from_value(params)
            .map_err(|e| TaskError::new(format!("invalid params: {e}")))?;
        self.tool
            .get(&p.uuid)
            .await
            .map_err(|e| TaskError::new(format!("Image.get error: {e}")))
    }
}
