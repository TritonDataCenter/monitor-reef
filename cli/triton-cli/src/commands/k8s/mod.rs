// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Kubernetes cluster management commands

use anyhow::Result;
use clap::Subcommand;
use cloudapi_client::TypedClient;

use crate::config::profile::Profile;

pub mod bootstrap;
pub mod control;
pub mod create;
pub mod delete;
pub mod get;
pub mod kube_client;
pub mod kubeconfig;
pub mod lb;
pub mod list;
pub mod logging;
pub mod network;
pub mod provisioning;
pub mod state;
pub mod talos;
pub mod worker;

#[derive(Subcommand, Clone)]
pub enum K8sCommand {
    /// Create a new cluster (metadata only, no provisioning)
    Create(create::CreateArgs),

    /// Bootstrap cluster nodes (provision and configure)
    Bootstrap(bootstrap::BootstrapArgs),

    /// List all clusters
    #[command(visible_alias = "ls")]
    List(list::ListArgs),

    /// Get cluster details
    Get(get::GetArgs),

    /// Delete a cluster
    #[command(visible_alias = "rm")]
    Delete(delete::DeleteArgs),

    /// Output kubeconfig for a cluster
    Kubeconfig(kubeconfig::KubeconfigArgs),

    /// Manage control plane nodes
    #[command(subcommand)]
    Control(control::ControlCommand),

    /// Manage worker nodes
    #[command(subcommand)]
    Worker(worker::WorkerCommand),

    /// Manage LoadBalancer controller
    #[command(subcommand, visible_alias = "loadbalancer")]
    Lb(lb::LbCommand),
}

impl K8sCommand {
    pub async fn run(self, client: &TypedClient, profile: &Profile, json: bool) -> Result<()> {
        match self {
            Self::Create(args) => create::run(args, client, json).await,
            Self::Bootstrap(args) => bootstrap::run(args, client, json).await,
            Self::List(args) => list::run(args, json).await,
            Self::Get(args) => get::run(args, client, json).await,
            Self::Delete(args) => delete::run(args, client).await,
            Self::Kubeconfig(args) => kubeconfig::run(args).await,
            Self::Control(cmd) => cmd.run(client, json).await,
            Self::Worker(cmd) => cmd.run(client, json).await,
            Self::Lb(cmd) => cmd.run(client, profile, json).await,
        }
    }
}
