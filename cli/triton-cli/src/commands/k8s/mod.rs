// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Kubernetes cluster management commands

use anyhow::Result;
use clap::Subcommand;
use triton_gateway_client::TypedClient;
use triton_gateway_client::types::Cluster;

pub mod add_nodes;
pub mod bootstrap;
pub mod create;
pub mod delete;
pub mod get;
pub mod kubeconfig;
pub mod lb;
pub mod list;
pub mod relay_bridge;
pub mod upgrade;
pub mod worker;

#[derive(Subcommand, Clone)]
pub enum K8sCommand {
    /// Create a new cluster record
    Create(create::CreateArgs),
    /// List all cluster records
    #[command(visible_alias = "ls")]
    List(list::ListArgs),
    /// Get a cluster record
    Get(get::GetArgs),
    /// Delete a cluster record
    #[command(visible_alias = "rm")]
    Delete(delete::DeleteArgs),
    /// Print the kubeconfig for a running cluster
    Kubeconfig(kubeconfig::KubeconfigArgs),
    /// Bootstrap a cluster (apply Talos configs, bootstrap etcd, retrieve kubeconfig)
    Bootstrap(bootstrap::BootstrapArgs),
    /// Add nodes to a running cluster
    #[command(name = "add-nodes")]
    AddNodes(add_nodes::AddNodesArgs),
    /// Upgrade Talos on all cluster nodes
    Upgrade(upgrade::UpgradeArgs),
    /// Start a local relay bridge for kubectl access to a cluster
    #[command(name = "relay-bridge")]
    RelayBridge(relay_bridge::RelayBridgeArgs),
    /// Manage worker nodes
    Worker {
        #[command(subcommand)]
        command: worker::WorkerCommand,
    },
    /// Manage the Triton LB controller
    Lb {
        #[command(subcommand)]
        command: lb::LbCommand,
    },
}

impl K8sCommand {
    pub async fn run(self, client: &TypedClient, json: bool) -> Result<()> {
        match self {
            Self::Create(args) => create::run(args, client, json).await,
            Self::List(args) => list::run(args, client, json).await,
            Self::Get(args) => get::run(args, client, json).await,
            Self::Delete(args) => delete::run(args, client).await,
            Self::Kubeconfig(args) => kubeconfig::run(args, client).await,
            Self::Bootstrap(args) => bootstrap::run(args, client, json).await,
            Self::AddNodes(args) => add_nodes::run(args, client, json).await,
            Self::Upgrade(args) => upgrade::run(args, client, json).await,
            Self::RelayBridge(_) => unreachable!("relay-bridge is handled before K8sCommand::run"),
            Self::Worker { command } => command.run(client, json).await,
            Self::Lb { command } => command.run(client, json).await,
        }
    }
}

/// Resolve a cluster name, short UUID prefix, or full UUID to a `Cluster`.
///
/// Resolution order:
/// 1. If the input parses as a full UUID, call GET directly.
/// 2. Otherwise, list all clusters and search by exact name or 8-char ID prefix.
pub async fn resolve_cluster(input: &str, client: &TypedClient) -> Result<Cluster> {
    // Try parsing as a full UUID first.
    if let Ok(uuid) = uuid::Uuid::parse_str(input) {
        let cluster = client
            .inner()
            .k8s_clusters_get()
            .cluster(uuid)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("cluster not found: {}", e))?
            .into_inner();
        return Ok(cluster);
    }

    // Fall back to listing and searching by name or prefix.
    let list = client
        .inner()
        .k8s_clusters_list()
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("failed to list clusters: {}", e))?
        .into_inner();

    // Exact name match first.
    let by_name: Vec<&Cluster> = list.items.iter().filter(|c| c.name == input).collect();
    if by_name.len() == 1 {
        return Ok(by_name[0].clone());
    }
    if by_name.len() > 1 {
        let ids: Vec<String> = by_name
            .iter()
            .map(|c| c.id.to_string()[..8].to_string())
            .collect();
        anyhow::bail!(
            "ambiguous cluster name '{}' matches multiple clusters: {}",
            input,
            ids.join(", ")
        );
    }

    // Short ID prefix match.
    let by_prefix: Vec<&Cluster> = list
        .items
        .iter()
        .filter(|c| c.id.to_string().starts_with(input))
        .collect();
    match by_prefix.len() {
        1 => Ok(by_prefix[0].clone()),
        0 => Err(
            crate::errors::ResourceNotFoundError(format!("cluster not found: {}", input)).into(),
        ),
        n => {
            let ids: Vec<String> = by_prefix
                .iter()
                .map(|c| c.id.to_string()[..8].to_string())
                .collect();
            anyhow::bail!(
                "ambiguous short ID '{}' matches {} clusters: {}",
                input,
                n,
                ids.join(", ")
            )
        }
    }
}
