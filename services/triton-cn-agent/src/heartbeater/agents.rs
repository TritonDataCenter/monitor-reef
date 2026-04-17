// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Collect the list of SDC agents installed on a compute node.
//!
//! CNAPI keeps a per-CN view of which SDC agents are installed and at
//! which image/version. cn-agent posts this list once on startup and
//! again whenever sysinfo changes. The data comes from three places:
//!
//! * `/opt/smartdc/agents/lib/node_modules/<name>/image_uuid` — image each
//!   agent was installed from.
//! * `/opt/smartdc/agents/etc/<name>` — single-line file containing the
//!   agent's instance UUID (not every agent has one).
//! * `sysinfo['SDC Agents']` — authoritative list of name/version pairs.
//!
//! We start from sysinfo and enrich each entry with the image/instance
//! fields, matching the legacy `SmartosBackend.getAgents` contract.

use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::cnapi::AgentInfo;

pub const DEFAULT_AGENTS_DIR: &str = "/opt/smartdc/agents/lib/node_modules";
pub const DEFAULT_AGENTS_ETC: &str = "/opt/smartdc/agents/etc";

#[derive(Debug, Error)]
pub enum AgentsError {
    #[error("failed to read {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("sysinfo missing 'SDC Agents' field")]
    MissingSdcAgents,
    #[error("sysinfo 'SDC Agents' is not an array")]
    InvalidSdcAgents,
}

/// Collector for agent metadata. Paths are injectable for testing.
#[derive(Debug, Clone)]
pub struct AgentsCollector {
    pub agents_dir: PathBuf,
    pub etc_dir: PathBuf,
}

impl Default for AgentsCollector {
    fn default() -> Self {
        Self {
            agents_dir: PathBuf::from(DEFAULT_AGENTS_DIR),
            etc_dir: PathBuf::from(DEFAULT_AGENTS_ETC),
        }
    }
}

impl AgentsCollector {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_dirs(agents_dir: impl Into<PathBuf>, etc_dir: impl Into<PathBuf>) -> Self {
        Self {
            agents_dir: agents_dir.into(),
            etc_dir: etc_dir.into(),
        }
    }

    /// Collect agents from the given sysinfo payload.
    ///
    /// `sysinfo` must be the JSON object returned by `/usr/bin/sysinfo`;
    /// we read the `SDC Agents` array and enrich each entry.
    pub async fn collect(
        &self,
        sysinfo: &serde_json::Value,
    ) -> Result<Vec<AgentInfo>, AgentsError> {
        let sdc_agents = sysinfo
            .get("SDC Agents")
            .ok_or(AgentsError::MissingSdcAgents)?
            .as_array()
            .ok_or(AgentsError::InvalidSdcAgents)?;

        // Start from a Vec<AgentInfo> seeded with whatever sysinfo has
        // (name + version). We'll then enrich with image_uuid and instance
        // uuid from disk.
        let mut agents: Vec<AgentInfo> = sdc_agents
            .iter()
            .filter_map(|entry| {
                let obj = entry.as_object()?;
                Some(AgentInfo {
                    name: obj.get("name").and_then(|v| v.as_str())?.to_string(),
                    image_uuid: String::new(),
                    uuid: None,
                    version: obj
                        .get("version")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                })
            })
            .collect();

        // Walk node_modules once and enrich in-place.
        let mut entries = match tokio::fs::read_dir(&self.agents_dir).await {
            Ok(e) => e,
            Err(source) => {
                return Err(AgentsError::Read {
                    path: self.agents_dir.clone(),
                    source,
                });
            }
        };
        while let Some(entry) = next_dir_entry(&mut entries).await? {
            let file_name = entry.file_name();
            let Some(name) = file_name.to_str() else {
                continue;
            };
            let image_uuid = match read_image_uuid(&self.agents_dir, name).await {
                Ok(Some(uuid)) => uuid,
                Ok(None) => continue,
                Err(e) => {
                    tracing::warn!(agent = %name, error = %e, "skipping agent without image_uuid");
                    continue;
                }
            };
            let instance_uuid = read_instance_uuid(&self.etc_dir, name).await?;
            for agent in agents.iter_mut().filter(|a| a.name == name) {
                agent.image_uuid = image_uuid.clone();
                if instance_uuid.is_some() {
                    agent.uuid = instance_uuid.clone();
                }
            }
        }

        Ok(agents)
    }
}

async fn next_dir_entry(
    entries: &mut tokio::fs::ReadDir,
) -> Result<Option<tokio::fs::DirEntry>, AgentsError> {
    entries
        .next_entry()
        .await
        .map_err(|source| AgentsError::Read {
            path: PathBuf::from("<ReadDir>"),
            source,
        })
}

async fn read_image_uuid(agents_dir: &Path, name: &str) -> Result<Option<String>, AgentsError> {
    let path = agents_dir.join(name).join("image_uuid");
    match tokio::fs::read_to_string(&path).await {
        Ok(s) => Ok(Some(s.trim().to_string())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(AgentsError::Read { path, source }),
    }
}

async fn read_instance_uuid(etc_dir: &Path, name: &str) -> Result<Option<String>, AgentsError> {
    let path = etc_dir.join(name);
    match tokio::fs::read_to_string(&path).await {
        Ok(s) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                Ok(None)
            } else {
                Ok(Some(trimmed.to_string()))
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(AgentsError::Read { path, source }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn prepare(tmp: &Path) -> AgentsCollector {
        let agents = tmp.join("node_modules");
        let etc = tmp.join("etc");
        fs::create_dir_all(&agents).expect("agents dir");
        fs::create_dir_all(&etc).expect("etc dir");
        AgentsCollector::with_dirs(agents, etc)
    }

    #[tokio::test]
    async fn collect_enriches_sysinfo_entries() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = prepare(tmp.path());
        fs::create_dir(c.agents_dir.join("net-agent")).expect("mkdir");
        fs::write(c.agents_dir.join("net-agent/image_uuid"), "img-1\n").expect("image_uuid");
        fs::write(c.etc_dir.join("net-agent"), "inst-1\n").expect("instance");

        let sysinfo = serde_json::json!({
            "SDC Agents": [
                {"name": "net-agent", "version": "2.2.0"},
                {"name": "cn-agent", "version": "2.15.0"}
            ]
        });
        let mut agents = c.collect(&sysinfo).await.expect("collect");
        agents.sort_by(|a, b| a.name.cmp(&b.name));

        assert_eq!(agents.len(), 2);
        let net = agents.iter().find(|a| a.name == "net-agent").expect("net");
        assert_eq!(net.image_uuid, "img-1");
        assert_eq!(net.uuid.as_deref(), Some("inst-1"));
        assert_eq!(net.version.as_deref(), Some("2.2.0"));
        let cn = agents.iter().find(|a| a.name == "cn-agent").expect("cn");
        // No image_uuid file for cn-agent in this test, so it stays empty.
        assert_eq!(cn.image_uuid, "");
        assert_eq!(cn.uuid, None);
    }

    #[tokio::test]
    async fn collect_tolerates_missing_instance_uuid_file() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = prepare(tmp.path());
        fs::create_dir(c.agents_dir.join("amon-agent")).expect("mkdir");
        fs::write(c.agents_dir.join("amon-agent/image_uuid"), "img-a\n").expect("image_uuid");

        let sysinfo = serde_json::json!({
            "SDC Agents": [{"name": "amon-agent", "version": "1.0.0"}]
        });
        let agents = c.collect(&sysinfo).await.expect("collect");
        assert_eq!(agents[0].image_uuid, "img-a");
        assert_eq!(agents[0].uuid, None);
    }

    #[tokio::test]
    async fn collect_errors_when_sysinfo_missing_field() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = prepare(tmp.path());
        let err = c.collect(&serde_json::json!({})).await.unwrap_err();
        assert!(matches!(err, AgentsError::MissingSdcAgents));
    }
}
