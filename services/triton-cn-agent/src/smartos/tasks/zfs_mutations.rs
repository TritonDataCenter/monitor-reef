// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! ZFS mutation task handlers.
//!
//! Each handler is a thin wrapper that parses params, calls the
//! corresponding [`ZfsTool`] method, and returns `{}` on success. The
//! error-message wording is preserved from the legacy tasks so log-scraping
//! tooling keeps working.

use std::sync::Arc;

use async_trait::async_trait;
use cn_agent_api::{TaskError, TaskResult};
use serde::Deserialize;

use crate::registry::TaskHandler;
use crate::smartos::zfs::ZfsTool;

#[derive(Debug, Deserialize)]
struct DatasetParams {
    dataset: String,
}

#[derive(Debug, Deserialize)]
struct RenameParams {
    dataset: String,
    newname: String,
}

#[derive(Debug, Deserialize)]
struct CloneParams {
    snapshot: String,
    dataset: String,
}

#[derive(Debug, Deserialize)]
struct SetPropertiesParams {
    dataset: String,
    properties: serde_json::Map<String, serde_json::Value>,
}

fn parse<T: serde::de::DeserializeOwned>(v: serde_json::Value) -> Result<T, TaskError> {
    serde_json::from_value(v).map_err(|e| TaskError::new(format!("invalid params: {e}")))
}

// ---------------------------------------------------------------------------
// create
// ---------------------------------------------------------------------------

pub struct ZfsCreateDatasetTask {
    tool: Arc<ZfsTool>,
}

impl ZfsCreateDatasetTask {
    pub fn new(tool: Arc<ZfsTool>) -> Self {
        Self { tool }
    }
}

#[async_trait]
impl TaskHandler for ZfsCreateDatasetTask {
    async fn run(&self, params: serde_json::Value) -> Result<TaskResult, TaskError> {
        let p: DatasetParams = parse(params)?;
        self.tool.create_dataset(&p.dataset).await.map_err(|e| {
            TaskError::new(format!(
                "failed to create ZFS dataset \"{}\": {e}",
                p.dataset
            ))
        })?;
        Ok(serde_json::json!({}))
    }
}

// ---------------------------------------------------------------------------
// destroy
// ---------------------------------------------------------------------------

pub struct ZfsDestroyDatasetTask {
    tool: Arc<ZfsTool>,
}

impl ZfsDestroyDatasetTask {
    pub fn new(tool: Arc<ZfsTool>) -> Self {
        Self { tool }
    }
}

#[async_trait]
impl TaskHandler for ZfsDestroyDatasetTask {
    async fn run(&self, params: serde_json::Value) -> Result<TaskResult, TaskError> {
        let p: DatasetParams = parse(params)?;
        self.tool.destroy_dataset(&p.dataset).await.map_err(|e| {
            TaskError::new(format!(
                "failed to destroy ZFS dataset \"{}\": {e}",
                p.dataset
            ))
        })?;
        Ok(serde_json::json!({}))
    }
}

// ---------------------------------------------------------------------------
// rename
// ---------------------------------------------------------------------------

pub struct ZfsRenameDatasetTask {
    tool: Arc<ZfsTool>,
}

impl ZfsRenameDatasetTask {
    pub fn new(tool: Arc<ZfsTool>) -> Self {
        Self { tool }
    }
}

#[async_trait]
impl TaskHandler for ZfsRenameDatasetTask {
    async fn run(&self, params: serde_json::Value) -> Result<TaskResult, TaskError> {
        let p: RenameParams = parse(params)?;
        self.tool
            .rename_dataset(&p.dataset, &p.newname)
            .await
            .map_err(|e| {
                TaskError::new(format!(
                    "failed to rename ZFS dataset \"{}\" to \"{}\": {e}",
                    p.dataset, p.newname
                ))
            })?;
        Ok(serde_json::json!({}))
    }
}

// ---------------------------------------------------------------------------
// snapshot
// ---------------------------------------------------------------------------

pub struct ZfsSnapshotDatasetTask {
    tool: Arc<ZfsTool>,
}

impl ZfsSnapshotDatasetTask {
    pub fn new(tool: Arc<ZfsTool>) -> Self {
        Self { tool }
    }
}

#[async_trait]
impl TaskHandler for ZfsSnapshotDatasetTask {
    async fn run(&self, params: serde_json::Value) -> Result<TaskResult, TaskError> {
        let p: DatasetParams = parse(params)?;
        self.tool.snapshot_dataset(&p.dataset).await.map_err(|e| {
            TaskError::new(format!(
                "failed to snapshot ZFS dataset \"{}\": {e}",
                p.dataset
            ))
        })?;
        Ok(serde_json::json!({}))
    }
}

// ---------------------------------------------------------------------------
// rollback
// ---------------------------------------------------------------------------

pub struct ZfsRollbackDatasetTask {
    tool: Arc<ZfsTool>,
}

impl ZfsRollbackDatasetTask {
    pub fn new(tool: Arc<ZfsTool>) -> Self {
        Self { tool }
    }
}

#[async_trait]
impl TaskHandler for ZfsRollbackDatasetTask {
    async fn run(&self, params: serde_json::Value) -> Result<TaskResult, TaskError> {
        let p: DatasetParams = parse(params)?;
        self.tool.rollback_dataset(&p.dataset).await.map_err(|e| {
            TaskError::new(format!(
                "failed to rollback ZFS dataset \"{}\": {e}",
                p.dataset
            ))
        })?;
        Ok(serde_json::json!({}))
    }
}

// ---------------------------------------------------------------------------
// clone
// ---------------------------------------------------------------------------

pub struct ZfsCloneDatasetTask {
    tool: Arc<ZfsTool>,
}

impl ZfsCloneDatasetTask {
    pub fn new(tool: Arc<ZfsTool>) -> Self {
        Self { tool }
    }
}

#[async_trait]
impl TaskHandler for ZfsCloneDatasetTask {
    async fn run(&self, params: serde_json::Value) -> Result<TaskResult, TaskError> {
        let p: CloneParams = parse(params)?;
        self.tool
            .clone_dataset(&p.snapshot, &p.dataset)
            .await
            .map_err(|e| {
                TaskError::new(format!(
                    "failed to clone ZFS snapshot \"{}\" into dataset \"{}\": {e}",
                    p.snapshot, p.dataset
                ))
            })?;
        Ok(serde_json::json!({}))
    }
}

// ---------------------------------------------------------------------------
// set_properties
// ---------------------------------------------------------------------------

pub struct ZfsSetPropertiesTask {
    tool: Arc<ZfsTool>,
}

impl ZfsSetPropertiesTask {
    pub fn new(tool: Arc<ZfsTool>) -> Self {
        Self { tool }
    }
}

#[async_trait]
impl TaskHandler for ZfsSetPropertiesTask {
    async fn run(&self, params: serde_json::Value) -> Result<TaskResult, TaskError> {
        let p: SetPropertiesParams = parse(params)?;
        self.tool
            .set_properties(&p.dataset, &p.properties)
            .await
            .map_err(|e| {
                TaskError::new(format!(
                    "failed to set ZFS properties for dataset \"{}\": {e}",
                    p.dataset
                ))
            })?;
        Ok(serde_json::json!({}))
    }
}
