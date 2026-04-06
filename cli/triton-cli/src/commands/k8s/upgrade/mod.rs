// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Talos version upgrade commands for Kubernetes clusters
//!
//! This module provides commands to upgrade Talos versions on cluster nodes
//! using a rolling upgrade strategy (one node at a time) to maintain cluster
//! availability.

use anyhow::Result;
use clap::Args;
use cloudapi_client::TypedClient;

pub mod execute;

/// Default Talos installer image registry
const DEFAULT_INSTALLER_REGISTRY: &str = "ghcr.io/siderolabs/installer";

#[derive(Args, Clone)]
pub struct UpgradeArgs {
    /// Cluster name or UUID
    pub cluster: String,

    /// Target Talos version (e.g. v1.9.0)
    #[arg(long, conflicts_with = "image")]
    pub version: Option<String>,

    /// Full installer image URL (e.g. ghcr.io/siderolabs/installer:v1.9.0)
    #[arg(long, conflicts_with = "version")]
    pub image: Option<String>,

    /// Upgrade control plane nodes only
    #[arg(long, conflicts_with = "workers")]
    pub control_plane: bool,

    /// Upgrade worker nodes only
    #[arg(long, conflicts_with = "control_plane")]
    pub workers: bool,

    /// Preview changes without applying (dry-run mode)
    #[arg(long)]
    pub dry_run: bool,

    /// Skip pre-flight health checks
    #[arg(long)]
    pub force: bool,

    /// Preserve data during upgrade
    #[arg(long, default_value = "true")]
    pub preserve: bool,

    /// Stage the upgrade (download but don't apply until reboot)
    #[arg(long)]
    pub stage: bool,

    /// Timeout for pre-flight health check (e.g. 30s, 2m, 5m)
    #[arg(long, default_value = "2m")]
    pub health_timeout: String,

    /// Timeout waiting for node to come back after upgrade (e.g. 5m, 10m)
    #[arg(long, default_value = "5m")]
    pub reboot_timeout: String,
}

impl UpgradeArgs {
    /// Get the installer image URL from either --version or --image
    pub fn installer_image(&self) -> Result<String> {
        if let Some(ref image) = self.image {
            Ok(image.clone())
        } else if let Some(ref version) = self.version {
            // Normalize version to include 'v' prefix
            let version = if version.starts_with('v') {
                version.clone()
            } else {
                format!("v{}", version)
            };
            Ok(format!("{}:{}", DEFAULT_INSTALLER_REGISTRY, version))
        } else {
            anyhow::bail!("Either --version or --image must be specified")
        }
    }

    /// Get the target version string for display/tracking
    pub fn target_version(&self) -> Result<String> {
        if let Some(ref version) = self.version {
            // Normalize version to include 'v' prefix
            Ok(if version.starts_with('v') {
                version.clone()
            } else {
                format!("v{}", version)
            })
        } else if let Some(ref image) = self.image {
            // Extract version from image URL (e.g. ghcr.io/siderolabs/installer:v1.9.0 -> v1.9.0)
            if let Some(tag) = image.split(':').next_back() {
                Ok(tag.to_string())
            } else {
                Ok(image.clone())
            }
        } else {
            anyhow::bail!("Either --version or --image must be specified")
        }
    }
}

pub async fn run(args: UpgradeArgs, _client: &TypedClient, json: bool) -> Result<()> {
    execute::run(args, json).await
}
