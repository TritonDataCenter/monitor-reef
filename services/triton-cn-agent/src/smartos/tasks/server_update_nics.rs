// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `server_update_nics` — reconcile the CN's nic-tag assignments with
//! the list CNAPI provides.
//!
//! The caller supplies a list of nics, each with a MAC and an array of
//! `nic_tags_provided`. We compare that against the current output of
//! `nictagadm list -p -d '|'` and issue `add`/`update`/`delete`
//! invocations to close the gap. Algorithm mirrors the legacy task,
//! line for line.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use async_trait::async_trait;
use cn_agent_api::{TaskError, TaskResult};
use serde::Deserialize;

use crate::registry::TaskHandler;
use crate::smartos::nictagadm::NictagadmTool;

#[derive(Debug, Deserialize)]
struct Params {
    nics: Vec<Nic>,
}

/// A single NIC entry from the task params. `pub` because
/// [`plan_nictagadm_commands`] is public for testability and Rust
/// won't expose a function whose argument type is private.
#[derive(Debug, Deserialize)]
pub struct Nic {
    pub mac: String,
    #[serde(default)]
    pub nic_tags_provided: Option<Vec<String>>,
}

/// The reconciliation command we're about to issue. Surfaced so tests
/// can verify the plan without actually running nictagadm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NicTagCommand {
    Add { tag: String, mac: String },
    Update { tag: String, mac: String },
    Delete { tag: String },
}

/// Build the list of nictagadm operations required to move from
/// `tags_before` to the assignment implied by `nics`.
///
/// Algorithm:
/// 1. For each nic's `nic_tags_provided`:
///    * If the tag is already present with this MAC, do nothing.
///    * If it's present with a different MAC, emit `update`.
///    * If it's absent, emit `add`.
/// 2. For any tag left in `tags_before` whose MAC belongs to one of the
///    nics we saw, emit `delete` — the operator wants that tag gone.
///
/// Matches the legacy `server_update_nics.js` flow exactly.
pub fn plan_nictagadm_commands(
    mut tags_before: BTreeMap<String, String>,
    nics: &[Nic],
) -> Vec<NicTagCommand> {
    let mut commands = Vec::new();
    let mut seen_macs: BTreeSet<&str> = BTreeSet::new();
    let mut seen_tags: BTreeSet<String> = BTreeSet::new();

    for nic in nics {
        seen_macs.insert(nic.mac.as_str());
        let Some(tags) = nic.nic_tags_provided.as_ref() else {
            continue;
        };
        for tag in tags {
            if !seen_tags.insert(tag.clone()) {
                continue;
            }
            match tags_before.remove(tag) {
                Some(old_mac) if old_mac == nic.mac => {
                    // unchanged
                }
                Some(_old_mac_different) => {
                    commands.push(NicTagCommand::Update {
                        tag: tag.clone(),
                        mac: nic.mac.clone(),
                    });
                }
                None => {
                    commands.push(NicTagCommand::Add {
                        tag: tag.clone(),
                        mac: nic.mac.clone(),
                    });
                }
            }
        }
    }

    // Whatever's left in tags_before is a tag we haven't been told to
    // keep; delete it if its MAC belongs to one of the nics we saw.
    for (tag, mac) in tags_before {
        if seen_macs.contains(mac.as_str()) {
            commands.push(NicTagCommand::Delete { tag });
        }
    }

    commands
}

pub struct ServerUpdateNicsTask {
    tool: Arc<NictagadmTool>,
}

impl ServerUpdateNicsTask {
    pub fn new(tool: Arc<NictagadmTool>) -> Self {
        Self { tool }
    }
}

#[async_trait]
impl TaskHandler for ServerUpdateNicsTask {
    async fn run(&self, params: serde_json::Value) -> Result<TaskResult, TaskError> {
        let p: Params = serde_json::from_value(params)
            .map_err(|e| TaskError::new(format!("invalid params: {e}")))?;
        if p.nics.iter().all(|n| n.nic_tags_provided.is_none()) && !p.nics.is_empty() {
            tracing::warn!("server_update_nics: no nic_tags_provided on any nic");
        }

        let tags_before = self
            .tool
            .list()
            .await
            .map_err(|e| TaskError::new(format!("nic tag list error: {e}")))?;

        let commands = plan_nictagadm_commands(tags_before, &p.nics);

        for cmd in &commands {
            match cmd {
                NicTagCommand::Add { tag, mac } => self.tool.add(tag, mac).await,
                NicTagCommand::Update { tag, mac } => self.tool.update(tag, mac).await,
                NicTagCommand::Delete { tag } => self.tool.delete(tag).await,
            }
            .map_err(|e| TaskError::new(e.to_string()))?;
        }

        Ok(serde_json::json!({ "commands_executed": commands.len() }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nic(mac: &str, tags: &[&str]) -> Nic {
        Nic {
            mac: mac.to_string(),
            nic_tags_provided: Some(tags.iter().map(|s| s.to_string()).collect()),
        }
    }

    #[test]
    fn plan_adds_missing_tags() {
        let before: BTreeMap<String, String> = BTreeMap::new();
        let nics = vec![nic("aa:bb:cc:dd:ee:01", &["admin"])];
        let plan = plan_nictagadm_commands(before, &nics);
        assert_eq!(
            plan,
            vec![NicTagCommand::Add {
                tag: "admin".into(),
                mac: "aa:bb:cc:dd:ee:01".into()
            }]
        );
    }

    #[test]
    fn plan_updates_tag_that_moved_macs() {
        let mut before = BTreeMap::new();
        before.insert("admin".to_string(), "aa:aa:aa:aa:aa:01".to_string());
        let nics = vec![nic("aa:bb:cc:dd:ee:01", &["admin"])];
        let plan = plan_nictagadm_commands(before, &nics);
        assert_eq!(
            plan,
            vec![NicTagCommand::Update {
                tag: "admin".into(),
                mac: "aa:bb:cc:dd:ee:01".into()
            }]
        );
    }

    #[test]
    fn plan_deletes_tags_belonging_to_seen_nic() {
        let mut before = BTreeMap::new();
        // This nic exists, but the "external" tag assigned to it is no
        // longer in the desired nic_tags_provided list.
        before.insert("admin".to_string(), "aa:bb:cc:dd:ee:01".to_string());
        before.insert("external".to_string(), "aa:bb:cc:dd:ee:01".to_string());
        let nics = vec![nic("aa:bb:cc:dd:ee:01", &["admin"])];
        let plan = plan_nictagadm_commands(before, &nics);
        assert_eq!(
            plan,
            vec![NicTagCommand::Delete {
                tag: "external".into()
            }]
        );
    }

    #[test]
    fn plan_leaves_unrelated_tags_alone() {
        // Tag belongs to a MAC we haven't seen: not ours to delete.
        let mut before = BTreeMap::new();
        before.insert("admin".to_string(), "aa:bb:cc:dd:ee:01".to_string());
        before.insert("internal".to_string(), "99:99:99:99:99:99".to_string());
        let nics = vec![nic("aa:bb:cc:dd:ee:01", &["admin"])];
        let plan = plan_nictagadm_commands(before, &nics);
        assert!(
            plan.is_empty(),
            "should not touch tags on unseen MACs: {plan:?}"
        );
    }

    #[test]
    fn plan_ignores_nic_without_provided_tags() {
        let before = BTreeMap::new();
        let nics = vec![Nic {
            mac: "aa:bb:cc:dd:ee:01".into(),
            nic_tags_provided: None,
        }];
        let plan = plan_nictagadm_commands(before, &nics);
        assert!(plan.is_empty());
    }
}
