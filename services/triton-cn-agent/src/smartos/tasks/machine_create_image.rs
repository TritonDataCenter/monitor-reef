// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `machine_create_image` — create a new image from a running VM and
//! publish it to the provided IMGAPI URL.
//!
//! Thin wrapper over [`ImgadmTool::create_image`]. On failure, we clip
//! long error messages the same way the legacy task did — CNAPI uses
//! the error body verbatim in workflow history and unbounded stderr
//! blobs would bloat its storage.

use std::sync::Arc;

use async_trait::async_trait;
use cn_agent_api::{TaskError, TaskResult};
use serde::Deserialize;

use crate::registry::TaskHandler;
use crate::smartos::imgadm_tool::{CreateImageOptions, ImgadmCliError, ImgadmTool};

/// Maximum error body size returned to CNAPI. Matches the legacy
/// `LIMIT = 20000`.
const ERROR_BODY_LIMIT: usize = 20_000;

#[derive(Debug, Deserialize)]
struct Params {
    /// Source VM UUID.
    uuid: String,
    /// Compression passed to `imgadm -c`. Required by legacy.
    compression: String,
    /// IMGAPI URL the new image publishes to. Required by legacy.
    imgapi_url: String,
    /// Manifest object for the new image. Must contain `uuid`.
    manifest: serde_json::Value,
    #[serde(default)]
    incremental: Option<bool>,
    #[serde(default)]
    max_origin_depth: Option<u32>,
    #[serde(default)]
    prepare_image_script: Option<String>,
}

pub struct MachineCreateImageTask {
    tool: Arc<ImgadmTool>,
}

impl MachineCreateImageTask {
    pub fn new(tool: Arc<ImgadmTool>) -> Self {
        Self { tool }
    }
}

#[async_trait]
impl TaskHandler for MachineCreateImageTask {
    async fn run(&self, params: serde_json::Value) -> Result<TaskResult, TaskError> {
        let p: Params = serde_json::from_value(params)
            .map_err(|e| TaskError::new(format!("invalid params: {e}")))?;

        let manifest_uuid = p
            .manifest
            .get("uuid")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .ok_or_else(|| TaskError::new("manifest must contain a uuid field".to_string()))?;

        let opts = CreateImageOptions {
            vm_uuid: p.uuid.clone(),
            compression: p.compression,
            imgapi_url: p.imgapi_url,
            manifest: p.manifest,
            incremental: p.incremental.unwrap_or(false),
            max_origin_depth: p.max_origin_depth,
            prepare_image_script: p.prepare_image_script,
        };

        match self.tool.create_image(&opts).await {
            Ok(()) => {
                tracing::info!(
                    vm_uuid = %p.uuid,
                    manifest_uuid = %manifest_uuid,
                    "imgadm create succeeded"
                );
                Ok(serde_json::json!({}))
            }
            Err(ImgadmCliError::ImgadmReported(body)) => {
                // Legacy passes the structured error JSON back verbatim.
                Err(TaskError::new(clip_message(&body, ERROR_BODY_LIMIT)))
            }
            Err(e) => Err(TaskError::new(clip_message(
                &e.to_string(),
                ERROR_BODY_LIMIT,
            ))),
        }
    }
}

/// Truncate long error messages, replacing the middle with a
/// human-readable elision marker. Matches the legacy `clip()` helper so
/// CNAPI workflow history storage stays bounded.
fn clip_message(s: &str, limit: usize) -> String {
    if s.len() <= limit {
        return s.to_string();
    }
    let elide = format!(
        "\n... content elided (full message was {} characters) ...\n",
        s.len()
    );
    let front = (limit.saturating_sub(elide.len())) / 2;
    let back = limit.saturating_sub(front).saturating_sub(elide.len());
    let start = &s[..front.min(s.len())];
    let tail_start = s.len().saturating_sub(back);
    let end = &s[tail_start..];
    format!("{start}{elide}{end}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clip_message_is_no_op_when_under_limit() {
        assert_eq!(clip_message("hi", 100), "hi");
    }

    #[test]
    fn clip_message_elides_middle_when_over_limit() {
        let s = "x".repeat(1000);
        let clipped = clip_message(&s, 200);
        assert!(clipped.len() <= 200);
        assert!(clipped.contains("content elided"));
        // Keeps both head and tail
        assert!(clipped.starts_with('x'));
        assert!(clipped.ends_with('x'));
    }
}
