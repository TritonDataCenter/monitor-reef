// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `refresh_agents` — re-read the installed agents list from disk and
//! POST `/servers/{uuid}` to CNAPI.
//!
//! The legacy task used a copy of the startup `getAgents` path; we
//! re-use the [`AgentsCollector`] and the running [`CnapiClient`] directly.

use std::sync::Arc;

use async_trait::async_trait;
use cn_agent_api::{TaskError, TaskResult};

use crate::cnapi::CnapiClient;
use crate::heartbeater::AgentsCollector;
use crate::registry::TaskHandler;
use crate::smartos::sysinfo::Sysinfo;

pub struct RefreshAgentsTask {
    cnapi: Arc<CnapiClient>,
    collector: AgentsCollector,
    /// Optional injected sysinfo source for tests. Production always
    /// runs `/usr/bin/sysinfo`.
    sysinfo_override: Option<Arc<SysinfoOverride>>,
}

/// Test hook that returns a canned sysinfo value.
pub struct SysinfoOverride(pub Sysinfo);

impl RefreshAgentsTask {
    pub fn new(cnapi: Arc<CnapiClient>, collector: AgentsCollector) -> Self {
        Self {
            cnapi,
            collector,
            sysinfo_override: None,
        }
    }

    pub fn with_sysinfo(mut self, sysinfo: Sysinfo) -> Self {
        self.sysinfo_override = Some(Arc::new(SysinfoOverride(sysinfo)));
        self
    }
}

#[async_trait]
impl TaskHandler for RefreshAgentsTask {
    async fn run(&self, _params: serde_json::Value) -> Result<TaskResult, TaskError> {
        let sysinfo = if let Some(ov) = &self.sysinfo_override {
            ov.0.clone()
        } else {
            Sysinfo::collect()
                .await
                .map_err(|e| TaskError::new(format!("AgentInstall error: {e}")))?
        };

        let agents = self
            .collector
            .collect(&sysinfo.raw)
            .await
            .map_err(|e| TaskError::new(format!("AgentInstall error: {e}")))?;

        self.cnapi
            .post_agents(&agents)
            .await
            .map_err(|e| TaskError::new(format!("AgentInstall error: {e}")))?;

        Ok(serde_json::json!({}))
    }
}
