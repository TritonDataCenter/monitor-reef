// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `agents_uninstall` — uninstall one or more Triton SDC agents.
//!
//! For each name in `params.agents`, run `Apm::uninstall`. Errors are
//! collected and re-surfaced as a single combined message, matching the
//! legacy task's `VError.errorForEach` join. After the sweep, we re-post
//! the agents list to CNAPI so it reflects the uninstall.

use std::sync::Arc;

use async_trait::async_trait;
use cn_agent_api::{TaskError, TaskResult};
use serde::Deserialize;

use crate::cnapi::CnapiClient;
use crate::heartbeater::AgentsCollector;
use crate::registry::TaskHandler;
use crate::smartos::apm::Apm;
use crate::smartos::sysinfo::Sysinfo;

#[derive(Debug, Deserialize)]
struct Params {
    agents: Vec<String>,
}

pub struct AgentsUninstallTask {
    apm: Arc<Apm>,
    cnapi: Arc<CnapiClient>,
    collector: AgentsCollector,
}

impl AgentsUninstallTask {
    pub fn new(apm: Arc<Apm>, cnapi: Arc<CnapiClient>, collector: AgentsCollector) -> Self {
        Self {
            apm,
            cnapi,
            collector,
        }
    }
}

#[async_trait]
impl TaskHandler for AgentsUninstallTask {
    async fn run(&self, params: serde_json::Value) -> Result<TaskResult, TaskError> {
        let p: Params = serde_json::from_value(params)
            .map_err(|e| TaskError::new(format!("invalid params: {e}")))?;

        let mut errors: Vec<String> = Vec::new();
        for name in &p.agents {
            if let Err(e) = self.apm.uninstall(name).await {
                tracing::error!(agent = %name, error = %e, "agent uninstall failed");
                errors.push(format!("{name}: {e}"));
            }
        }

        // Re-register agents list. Legacy runs this regardless of whether
        // any individual uninstall failed — CNAPI needs to see whatever
        // state the CN is actually in.
        match Sysinfo::collect().await {
            Ok(sysinfo) => match self.collector.collect(&sysinfo.raw).await {
                Ok(agents) => {
                    if let Err(e) = self.cnapi.post_agents(&agents).await {
                        tracing::error!(error = %e, "Error refreshing agent info in CNAPI");
                        errors.push(format!("Error refreshing agent info in CNAPI: {e}"));
                    }
                }
                Err(e) => {
                    tracing::error!(error = %e, "Error collecting agent info");
                    errors.push(format!("Error collecting agent info: {e}"));
                }
            },
            Err(e) => {
                tracing::error!(error = %e, "Error reading sysinfo during agent refresh");
                errors.push(format!("Error reading sysinfo: {e}"));
            }
        }

        if !errors.is_empty() {
            return Err(TaskError::new(format!(
                "AgentsUninstallTask error: {}",
                errors.join("; ")
            )));
        }
        Ok(serde_json::json!({}))
    }
}
