// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Triton LoadBalancer controller management for Kubernetes clusters
//!
//! This module provides commands to install, check status, and remove the
//! Triton LoadBalancer controller from a Kubernetes cluster. The controller
//! watches for Services of type LoadBalancer and provisions Triton instances
//! to handle the load balancing.

use anyhow::Result;
use clap::Subcommand;
use cloudapi_client::TypedClient;

use crate::config::profile::Profile;

pub mod install;
pub mod remove;
pub mod status;

/// Embedded RBAC manifest for the controller
pub const RBAC_YAML: &str = include_str!("manifests/rbac.yaml");

/// Embedded deployment manifest template for the controller
pub const DEPLOYMENT_YAML_TEMPLATE: &str = include_str!("manifests/deployment.yaml");

/// Default controller image
pub const DEFAULT_CONTROLLER_IMAGE: &str = "travispaul/triton-lb-controller:latest";

/// Default package for LB instances
pub const DEFAULT_LB_PACKAGE: &str = "sample-1G";

/// Default image name pattern for LB instances
pub const DEFAULT_LB_IMAGE_NAME: &str = "cloud-load-balancer";

#[derive(Subcommand, Clone)]
pub enum LbCommand {
    /// Install the Triton LoadBalancer controller
    Install(install::InstallArgs),

    /// Show LoadBalancer controller status
    Status(status::StatusArgs),

    /// Remove the LoadBalancer controller
    Remove(remove::RemoveArgs),
}

impl LbCommand {
    pub async fn run(self, client: &TypedClient, profile: &Profile, json: bool) -> Result<()> {
        match self {
            Self::Install(args) => install::run(args, client, profile, json).await,
            Self::Status(args) => status::run(args, json).await,
            Self::Remove(args) => remove::run(args, json).await,
        }
    }
}
