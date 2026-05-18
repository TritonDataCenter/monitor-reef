// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Output kubeconfig for a cluster

use anyhow::{Context, Result};
use clap::Args;

use super::state::{ClusterState, NodeRole};

#[derive(Args, Clone)]
pub struct KubeconfigArgs {
    /// Cluster name or UUID
    pub cluster: String,

    /// Use specific control plane endpoint IP
    #[arg(long)]
    pub endpoint: Option<String>,

    /// Use specific control plane node by name
    #[arg(long)]
    pub node: Option<String>,

    /// Use direct IP instead of CNS hostname (disables load balancing)
    #[arg(long)]
    pub no_cns: bool,

    /// List available control plane endpoints
    #[arg(long)]
    pub list_endpoints: bool,
}

pub async fn run(args: KubeconfigArgs) -> Result<()> {
    let cluster = ClusterState::load_by_name_or_uuid(&args.cluster).await?;

    // Get CNS suffix from cluster state (if available)
    let cns_suffix = cluster
        .control_plane
        .as_ref()
        .and_then(|cp| cp.cns_suffix.as_deref());

    // Collect control plane nodes, sorted by name for consistent ordering
    let mut control_nodes: Vec<(&String, &str)> = cluster
        .nodes
        .iter()
        .filter(|(_, info)| info.role == NodeRole::Control)
        .filter_map(|(name, info)| info.primary_ip.as_deref().map(|ip| (name, ip)))
        .collect();
    control_nodes.sort_by_key(|(name, _)| *name);

    // Handle --list-endpoints
    if args.list_endpoints {
        list_control_endpoints(&cluster.name, &control_nodes, cns_suffix);
        return Ok(());
    }

    let kubeconfig_path = cluster.cluster_dir()?.join("kubeconfig");

    if !kubeconfig_path.exists() {
        anyhow::bail!("Kubeconfig not found for cluster {}", cluster.name);
    }

    let kubeconfig = tokio::fs::read_to_string(&kubeconfig_path).await?;

    // Determine which endpoint to use based on args and current cluster state
    let target_endpoint = determine_target_endpoint(&args, &control_nodes, cns_suffix)?;

    // Always update the kubeconfig with the resolved endpoint
    let output = update_kubeconfig_endpoint(&kubeconfig, &target_endpoint)?;

    print!("{}", output);

    // Print informational message based on what endpoint type we're using
    let using_cns =
        cns_suffix.is_some() && !args.no_cns && args.endpoint.is_none() && args.node.is_none();

    if using_cns && control_nodes.len() > 1 {
        eprintln!(
            "Note: Using CNS hostname for load-balanced access to {} control plane nodes.",
            control_nodes.len()
        );
        eprintln!("      Use --no-cns or --endpoint <IP> to target a specific node.");
    } else if !using_cns
        && control_nodes.len() > 1
        && args.endpoint.is_none()
        && args.node.is_none()
    {
        eprintln!(
            "Note: Cluster has {} control plane nodes. Use --list-endpoints to see all.",
            control_nodes.len()
        );
        eprintln!("      Use --endpoint <IP> or --node <name> to target a specific node.");
    }

    Ok(())
}

/// List control plane endpoints to stderr
fn list_control_endpoints(
    cluster_name: &str,
    control_nodes: &[(&String, &str)],
    cns_suffix: Option<&str>,
) {
    eprintln!("Control plane endpoints for cluster {}:", cluster_name);

    // Show CNS load-balanced endpoint if available
    if let Some(suffix) = cns_suffix {
        eprintln!("  CNS (load-balanced): ctrl.{}", suffix);
    }

    if control_nodes.is_empty() {
        eprintln!("  (no control plane nodes found)");
    } else {
        eprintln!("  Direct endpoints:");
        for (name, ip) in control_nodes {
            eprintln!("    {} - {}", name, ip);
        }
    }
}

/// Determine which endpoint to use based on args and current cluster state.
///
/// Priority:
/// 1. Explicit `--endpoint` flag
/// 2. Explicit `--node` flag
/// 3. CNS hostname (if available and `--no-cns` not specified)
/// 4. First available control plane node (sorted alphabetically by name)
fn determine_target_endpoint(
    args: &KubeconfigArgs,
    control_nodes: &[(&String, &str)],
    cns_suffix: Option<&str>,
) -> Result<String> {
    if let Some(ref endpoint) = args.endpoint {
        // Validate that the endpoint is a known control plane IP
        let valid = control_nodes.iter().any(|(_, ip)| *ip == endpoint);
        if !valid {
            let known_ips: Vec<&str> = control_nodes.iter().map(|(_, ip)| *ip).collect();
            anyhow::bail!(
                "Endpoint {} is not a known control plane IP. Known IPs: {}",
                endpoint,
                known_ips.join(", ")
            );
        }
        return Ok(endpoint.clone());
    }

    if let Some(ref node_name) = args.node {
        // Find the node by name
        let found = control_nodes
            .iter()
            .find(|(name, _)| name.as_str() == node_name);
        match found {
            Some((_, ip)) => return Ok((*ip).to_string()),
            None => {
                let known_names: Vec<&str> = control_nodes
                    .iter()
                    .map(|(name, _)| name.as_str())
                    .collect();
                anyhow::bail!(
                    "Node {} is not a known control plane node. Known nodes: {}",
                    node_name,
                    known_names.join(", ")
                );
            }
        }
    }

    // Use CNS hostname if available (unless --no-cns was specified)
    if !args.no_cns
        && let Some(suffix) = cns_suffix
    {
        return Ok(format!("ctrl.{}", suffix));
    }

    // Fall back to first available control plane node
    match control_nodes.first() {
        Some((_, ip)) => Ok((*ip).to_string()),
        None => anyhow::bail!("No control plane nodes found in cluster"),
    }
}

/// Update the server URL in a kubeconfig to use a different endpoint
fn update_kubeconfig_endpoint(kubeconfig: &str, new_endpoint: &str) -> Result<String> {
    let mut doc: serde_yaml::Value =
        serde_yaml::from_str(kubeconfig).context("Failed to parse kubeconfig as YAML")?;

    if let Some(clusters) = doc.get_mut("clusters").and_then(|c| c.as_sequence_mut()) {
        for cluster in clusters {
            if let Some(cluster_data) = cluster.get_mut("cluster")
                && let Some(server) = cluster_data.get_mut("server")
            {
                *server = serde_yaml::Value::String(format!("https://{}:6443", new_endpoint));
            }
        }
    }

    serde_yaml::to_string(&doc).context("Failed to serialize modified kubeconfig")
}
